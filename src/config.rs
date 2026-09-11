use anyhow::{Context, Result, ensure};
use std::path::{Path, PathBuf};

pub fn tests_directory(config_path: &Path) -> Result<PathBuf> {
    let text = std::fs::read_to_string(config_path)
        .with_context(|| format!("cannot read {}; set tests_directory in this file or pass a tests directory on the command line", config_path.display()))?;
    let config: toml::Table = toml::from_str(&text)
        .with_context(|| format!("invalid TOML in {}", config_path.display()))?;
    let directory = config
        .get("tests_directory")
        .and_then(toml::Value::as_str)
        .with_context(|| {
            format!(
                "{} must contain a tests_directory string",
                config_path.display()
            )
        })?;
    ensure!(
        !directory.trim().is_empty(),
        "tests_directory in {} must not be empty",
        config_path.display()
    );
    Ok(config_path
        .parent()
        .unwrap_or(Path::new("."))
        .join(directory))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_resolved_relative_to_config() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("test_config.toml");
        std::fs::write(&config, "tests_directory = 'test files' # comment\n").unwrap();
        assert_eq!(
            tests_directory(&config).unwrap(),
            dir.path().join("test files")
        );
        let absolute = dir.path().join("absolute");
        std::fs::write(
            &config,
            format!("tests_directory = '{}'", absolute.display()),
        )
        .unwrap();
        assert_eq!(tests_directory(&config).unwrap(), absolute);
    }

    #[test]
    fn missing_or_invalid_config_is_reported() {
        let dir = tempfile::tempdir().unwrap();
        let config = dir.path().join("test_config.toml");
        assert!(tests_directory(&config).is_err());
        for text in [
            "",
            "tests_directory = 42",
            "tests_directory = ' '",
            "tests_directory = '",
        ] {
            std::fs::write(&config, text).unwrap();
            assert!(tests_directory(&config).is_err(), "{text}");
        }
    }
}
