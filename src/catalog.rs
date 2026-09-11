use anyhow::{Context, Result, bail, ensure};
use regex::Regex;
use std::{
    collections::HashSet,
    fs,
    io::Write,
    path::{Path, PathBuf},
};

const PRODUCERS: &str = "Test|RetryingTest|ParameterizedTest|RepeatedTest|TestFactory|TestTemplate";

#[derive(Debug)]
pub struct Test {
    pub file: usize,
    pub line: usize,
    pub end: usize,
    pub class: String,
    pub method: String,
    pub annotation: String,
    pub original: bool,
    pub enabled: bool,
}

pub struct Source {
    pub path: PathBuf,
    pub text: String,
}

pub struct Catalog {
    pub root: PathBuf,
    pub sources: Vec<Source>,
    pub tests: Vec<Test>,
}

// Preserve byte offsets and newlines while hiding Kotlin comments and literals.
// Also identify real line comments, so examples in strings/block comments stay untouched.
fn code_mask(text: &str) -> (String, HashSet<usize>) {
    let bytes = text.as_bytes();
    let mut out = bytes.to_vec();
    let mut comments = HashSet::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        if bytes[i..].starts_with(b"//") {
            comments.insert(i);
            while i < bytes.len() && bytes[i] != b'\n' {
                i += 1;
            }
        } else if bytes[i..].starts_with(b"/*") {
            i += 2;
            let mut depth = 1;
            while i < bytes.len() && depth > 0 {
                if bytes[i..].starts_with(b"/*") {
                    depth += 1;
                    i += 2;
                } else if bytes[i..].starts_with(b"*/") {
                    depth -= 1;
                    i += 2;
                } else {
                    i += 1;
                }
            }
        } else if bytes[i..].starts_with(b"\"\"\"") {
            i += 3;
            while i < bytes.len() && !bytes[i..].starts_with(b"\"\"\"") {
                i += 1;
            }
            i = (i + 3).min(bytes.len());
        } else if matches!(bytes[i], b'"' | b'\'' | b'`') {
            let quote = bytes[i];
            i += 1;
            while i < bytes.len() {
                if bytes[i] == b'\\' && quote != b'`' {
                    i = (i + 2).min(bytes.len());
                } else if bytes[i] == quote {
                    i += 1;
                    break;
                } else {
                    i += 1;
                }
            }
        } else {
            i += 1;
            continue;
        }
        for byte in &mut out[start..i] {
            if *byte != b'\n' && *byte != b'\r' {
                *byte = b' ';
            }
        }
    }
    (
        String::from_utf8(out).expect("mask preserves UTF-8"),
        comments,
    )
}

fn uncomment(line: &str) -> Result<String> {
    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
    let rest = line[indent..]
        .strip_prefix("//")
        .context("expected a commented annotation line")?;
    Ok(format!(
        "{}{}",
        &line[..indent],
        rest.strip_prefix(' ').unwrap_or(rest)
    ))
}

fn parse(text: &str, file: usize) -> Result<Vec<Test>> {
    let (mask, comments) = code_mask(text);
    let lines: Vec<_> = text.split_inclusive('\n').collect();
    let masked: Vec<_> = mask.split_inclusive('\n').collect();
    let producer = Regex::new(&format!(r"^\s*@(?:[\w]+\.)*({PRODUCERS})\b"))?;
    let function = Regex::new(r"\bfun\b")?;
    let class_re = Regex::new(r"\bclass\s+(\w+)")?;
    let name_re = Regex::new(r"^(?:`([^`]+)`|([\p{L}_][\p{L}\p{N}_]*))\s*\(")?;
    let mut offset = 0;
    let mut result = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let line = lines[i];
        let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
        let disabled = comments.contains(&(offset + indent))
            && producer.is_match(line[indent..].strip_prefix("//").unwrap_or(""));
        let active = producer.is_match(masked[i]);
        if !active && !disabled {
            offset += line.len();
            i += 1;
            continue;
        }
        let start = i;
        let start_offset = offset;
        let mut annotation = String::new();
        loop {
            ensure!(
                i < lines.len(),
                "unterminated test annotation at line {}",
                start + 1
            );
            annotation.push_str(&if disabled {
                uncomment(lines[i])?
            } else {
                lines[i].to_owned()
            });
            offset += lines[i].len();
            i += 1;
            let (annotation_mask, _) = code_mask(&annotation);
            let capture = producer
                .find(&annotation_mask)
                .context("invalid annotation")?;
            let tail = annotation_mask[capture.end()..].trim();
            if tail.is_empty() {
                break;
            }
            ensure!(
                tail.starts_with('('),
                "unsupported annotation layout at line {} (put the annotation on its own lines)",
                start + 1
            );
            let mut depth = 0;
            let mut closing = None;
            for (pos, c) in tail.char_indices() {
                if c == '(' {
                    depth += 1;
                }
                if c == ')' {
                    depth -= 1;
                    if depth == 0 {
                        closing = Some(pos + 1);
                        break;
                    }
                }
            }
            if let Some(end) = closing {
                ensure!(
                    tail[end..].trim().is_empty(),
                    "test annotation shares a line with code at line {}",
                    start + 1
                );
                break;
            }
        }
        let after = &mask[offset..];
        let fun = function
            .find(after)
            .with_context(|| format!("no method after annotation at line {}", start + 1))?;
        ensure!(
            !class_re.is_match(&after[..fun.start()]) && !after[..fun.start()].contains(['{', '}']),
            "cannot associate annotation at line {} with a method",
            start + 1
        );
        let name_start = offset + fun.end();
        let name = name_re
            .captures(text[name_start..].trim_start())
            .with_context(|| format!("unsupported method declaration at line {}", start + 1))?;
        let method = name
            .get(1)
            .or_else(|| name.get(2))
            .unwrap()
            .as_str()
            .to_owned();
        let class = class_re
            .captures_iter(&mask[..start_offset])
            .last()
            .map(|c| c[1].to_owned())
            .unwrap_or_else(|| "<top-level>".into());
        ensure!(
            !result
                .iter()
                .any(|t: &Test| t.class == class && t.method == method),
            "multiple test annotations or duplicate method: {class}.{method}"
        );
        result.push(Test {
            file,
            line: start,
            end: i,
            class,
            method,
            annotation: annotation.trim().to_owned(),
            original: active,
            enabled: active,
        });
    }
    Ok(result)
}

fn collect(path: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_by_key(|e| e.file_name());
    for entry in entries {
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() && !entry.file_name().to_string_lossy().starts_with('.') {
            collect(&entry.path(), files)?;
        } else if kind.is_file() && entry.path().extension().is_some_and(|ext| ext == "kt") {
            files.push(entry.path());
        }
    }
    Ok(())
}

impl Catalog {
    pub fn load(root: &Path) -> Result<Self> {
        let root = root
            .canonicalize()
            .with_context(|| format!("cannot open {}", root.display()))?;
        ensure!(root.is_dir(), "tests path must be a directory");
        let mut paths = Vec::new();
        collect(&root, &mut paths)?;
        let mut catalog = Self {
            root,
            sources: Vec::new(),
            tests: Vec::new(),
        };
        for path in paths {
            let text = fs::read_to_string(&path)?;
            let tests = parse(&text, catalog.sources.len())
                .with_context(|| format!("cannot scan {}", path.display()))?;
            catalog.tests.extend(tests);
            catalog.sources.push(Source { path, text });
        }
        Ok(catalog)
    }

    pub fn label(&self, test: &Test) -> String {
        format!(
            "{}  {}.{}",
            self.sources[test.file]
                .path
                .strip_prefix(&self.root)
                .unwrap()
                .display(),
            test.class,
            test.method
        )
    }

    pub fn pending(&self) -> usize {
        self.tests
            .iter()
            .filter(|t| t.enabled != t.original)
            .count()
    }

    pub fn reset(&mut self) {
        for test in &mut self.tests {
            test.enabled = test.original;
        }
    }

    pub fn render_source(&self, file: usize) -> Result<String> {
        let mut lines: Vec<String> = self.sources[file]
            .text
            .split_inclusive('\n')
            .map(str::to_owned)
            .collect();
        for test in self
            .tests
            .iter()
            .filter(|t| t.file == file && t.enabled != t.original)
        {
            for line in &mut lines[test.line..test.end] {
                if test.enabled {
                    *line = uncomment(line)?;
                } else {
                    let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
                    line.insert_str(indent, "// ");
                }
            }
        }
        Ok(lines.concat())
    }

    pub fn preview(&self) -> Result<String> {
        let mut out = String::new();
        for (file, source) in self.sources.iter().enumerate() {
            let changed: Vec<_> = self
                .tests
                .iter()
                .filter(|t| t.file == file && t.enabled != t.original)
                .collect();
            if changed.is_empty() {
                continue;
            }
            out.push_str(&format!(
                "--- {}\n+++ {}\n",
                source.path.strip_prefix(&self.root).unwrap().display(),
                source.path.strip_prefix(&self.root).unwrap().display()
            ));
            let updated = self.render_source(file)?;
            let old: Vec<_> = source.text.lines().collect();
            let new: Vec<_> = updated.lines().collect();
            for test in changed {
                out.push_str(&format!(
                    "@@ -{},{} +{},{} @@ {}.{}\n",
                    test.line + 1,
                    test.end - test.line,
                    test.line + 1,
                    test.end - test.line,
                    test.class,
                    test.method
                ));
                for line in &old[test.line..test.end] {
                    out.push_str(&format!("-{line}\n"));
                }
                for line in &new[test.line..test.end] {
                    out.push_str(&format!("+{line}\n"));
                }
            }
        }
        Ok(out)
    }

    pub fn save(&mut self) -> Result<usize> {
        let count = self.pending();
        let mut staged = Vec::new();
        // Check every source before any write: external edits require a reload.
        for source in &self.sources {
            ensure!(
                !fs::symlink_metadata(&source.path)?.file_type().is_symlink(),
                "file became a symlink: {}",
                source.path.display()
            );
            ensure!(
                fs::read_to_string(&source.path)? == source.text,
                "file changed outside this app: {}; reload before saving",
                source.path.display()
            );
        }
        for (file, source) in self.sources.iter().enumerate() {
            if !self
                .tests
                .iter()
                .any(|t| t.file == file && t.enabled != t.original)
            {
                continue;
            }
            let updated = self.render_source(file)?;
            let mut temp = tempfile::NamedTempFile::new_in(source.path.parent().unwrap())?;
            temp.as_file()
                .set_permissions(fs::metadata(&source.path)?.permissions())?;
            temp.write_all(updated.as_bytes())?;
            temp.as_file().sync_all()?;
            staged.push((file, updated, temp));
        }
        // Each file replacement is atomic. Record each success even if a later file fails.
        for (file, updated, temp) in staged {
            let source = &mut self.sources[file];
            if fs::read_to_string(&source.path)? != source.text {
                bail!("file changed during save: {}", source.path.display());
            }
            temp.persist(&source.path)
                .map_err(|e| e.error)
                .with_context(|| {
                    format!(
                        "saving {}; some earlier files may already be saved",
                        source.path.display()
                    )
                })?;
            source.text = updated;
            for test in self.tests.iter_mut().filter(|t| t.file == file) {
                test.original = test.enabled;
            }
        }
        Ok(count)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(text: &str) -> (tempfile::TempDir, Catalog) {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("Example.kt"), text).unwrap();
        let catalog = Catalog::load(dir.path()).unwrap();
        (dir, catalog)
    }

    #[test]
    fn discovers_live_and_commented_tests_in_multiple_classes() {
        let text = "class First {\n  @Test\n  @DisplayName(\"hello\")\n  fun `hello world`() {}\n}\nclass Second {\n  //@RetryingTest(3)\n  @DisabledInSplit\n  fun another() {}\n}";
        let (_, catalog) = fixture(text);
        assert_eq!(catalog.tests.len(), 2);
        assert_eq!(catalog.tests[0].method, "hello world");
        assert_eq!(catalog.tests[1].class, "Second");
        assert!(!catalog.tests[1].enabled);
    }

    #[test]
    fn ignores_fake_tests_inside_comments_and_strings() {
        let text = "/*\n@Test\nfun fake() {}\n// @Test\nfun fake2() {}\n*/\nval example = \"\"\"\n@Test\nfun fake3() {}\n// @Test\nfun fake4() {}\n\"\"\"\nclass Real {\n@Test\nfun real() {}\n}";
        let (_, catalog) = fixture(text);
        assert_eq!(catalog.tests.len(), 1);
        assert_eq!(catalog.tests[0].method, "real");
    }

    #[test]
    fn round_trip_preserves_crlf_metadata_and_multiline_annotations() {
        let text = "class Example {\r\n  @ParameterizedTest(\r\n    name = \"value ({0})\",\r\n  ) // reason\r\n  @MethodSource(\"cases\")\r\n  fun example() {}\r\n}";
        let (dir, mut catalog) = fixture(text);
        catalog.tests[0].enabled = false;
        catalog.save().unwrap();
        let mut loaded = Catalog::load(dir.path()).unwrap();
        assert!(!loaded.tests[0].enabled);
        assert!(
            loaded.sources[0]
                .text
                .contains("  @MethodSource(\"cases\")\r\n")
        );
        loaded.tests[0].enabled = true;
        loaded.save().unwrap();
        assert_eq!(
            fs::read_to_string(dir.path().join("Example.kt")).unwrap(),
            text
        );
    }

    #[test]
    fn external_edit_blocks_all_writes() {
        let (dir, mut catalog) = fixture("@Test\nfun example() {}\n");
        catalog.tests[0].enabled = false;
        fs::write(dir.path().join("Example.kt"), "user edit").unwrap();
        assert!(catalog.save().is_err());
        assert_eq!(
            fs::read_to_string(dir.path().join("Example.kt")).unwrap(),
            "user edit"
        );
    }

    #[test]
    fn refuses_inline_code_instead_of_commenting_out_method() {
        assert!(parse("@Test fun example() {}", 0).is_err());
    }

    #[test]
    fn discovery_refreshes_from_disk() {
        let (dir, catalog) = fixture("@Test\nfun first() {}\n");
        assert_eq!(catalog.tests.len(), 1);
        fs::create_dir(dir.path().join("new_feature")).unwrap();
        fs::write(
            dir.path().join("new_feature/New.kt"),
            "@Test\nfun addedLater() {}\n",
        )
        .unwrap();
        assert_eq!(Catalog::load(dir.path()).unwrap().tests.len(), 2);
    }
}
