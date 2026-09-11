use crate::catalog::Catalog;
use anyhow::{Result, ensure};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, List, ListItem, ListState, Paragraph, Wrap},
};
use std::{
    collections::BTreeMap,
    io::{self, IsTerminal},
    path::PathBuf,
};

#[derive(Debug, PartialEq)]
enum Row {
    Directory { path: PathBuf, tests: Vec<usize> },
    Test(usize),
}

enum Mode {
    Browse,
    Search,
    Preview,
    Save,
    Quit,
}

struct App {
    catalog: Catalog,
    query: String,
    selected: usize,
    list: ListState,
    mode: Mode,
    scroll: u16,
    message: String,
}

impl App {
    fn visible(&self) -> Vec<usize> {
        let words: Vec<_> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.catalog
            .tests
            .iter()
            .enumerate()
            .filter(|(_, test)| {
                let label = self.catalog.label(test).to_lowercase();
                words.iter().all(|word| label.contains(word))
            })
            .map(|(index, _)| index)
            .collect()
    }

    fn rows(&self) -> Vec<Row> {
        let visible = self.visible();
        let mut groups: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
        for (index, test) in self.catalog.tests.iter().enumerate() {
            let directory = self.catalog.sources[test.file].path.parent().unwrap();
            groups.entry(directory.to_owned()).or_default().push(index);
        }
        let mut rows = Vec::new();
        for (path, tests) in groups {
            let matching: Vec<_> = tests
                .iter()
                .copied()
                .filter(|index| visible.binary_search(index).is_ok())
                .collect();
            if !matching.is_empty() {
                rows.push(Row::Directory { path, tests });
                rows.extend(matching.into_iter().map(Row::Test));
            }
        }
        rows
    }

    fn directory_label(&self, path: &std::path::Path) -> String {
        let relative = path.strip_prefix(&self.catalog.root).unwrap();
        if relative.as_os_str().is_empty() {
            ".".into()
        } else {
            relative.display().to_string()
        }
    }

    fn bulk(&mut self, enabled: bool) {
        let visible = self.visible();
        for index in &visible {
            self.catalog.tests[*index].enabled = enabled;
        }
        self.message = format!(
            "{} matching tests staged as {}. Press s to review and save.",
            visible.len(),
            if enabled { "enabled" } else { "disabled" }
        );
    }

    fn isolate(&mut self) {
        let visible = self.visible();
        if visible.is_empty() {
            self.message = "No matches; isolation was not applied.".into();
            return;
        }
        for (index, test) in self.catalog.tests.iter_mut().enumerate() {
            test.enabled = visible.contains(&index);
        }
        self.message = format!(
            "Keep only {} matching tests enabled. Review with s; undo with u.",
            visible.len()
        );
    }

    fn draw(&mut self, frame: &mut ratatui::Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(3),
            Constraint::Length(5),
            Constraint::Length(3),
        ])
        .split(frame.area());
        let enabled = self.catalog.tests.iter().filter(|t| t.enabled).count();
        let total = self.catalog.tests.len();
        let title = Line::from(vec![
            Span::styled(
                " RUSTROVER ",
                Style::default()
                    .fg(Color::Black)
                    .bg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                " UI test selector   {enabled}/{total} enabled   {} pending",
                self.catalog.pending()
            )),
        ]);
        frame.render_widget(
            Paragraph::new(title).block(
                Block::default()
                    .borders(Borders::BOTTOM)
                    .title(self.catalog.root.display().to_string()),
            ),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(format!(
                "/ {}{}",
                self.query,
                if matches!(self.mode, Mode::Search) {
                    "▏"
                } else {
                    ""
                }
            ))
            .block(
                Block::bordered()
                    .title(if matches!(self.mode, Mode::Search) {
                        " Search · Enter to finish · Esc to clear "
                    } else {
                        " Filter by path, class, method · / to edit "
                    })
                    .border_style(Style::default().fg(Color::Cyan)),
            ),
            chunks[1],
        );
        if matches!(self.mode, Mode::Preview | Mode::Save | Mode::Quit) {
            let preview = self
                .catalog
                .preview()
                .unwrap_or_else(|error| format!("{error:#}"));
            let lines: Vec<Line> = if preview.is_empty() {
                vec![Line::raw("No pending changes.")]
            } else {
                preview
                    .lines()
                    .map(|line| {
                        Line::styled(
                            line.to_owned(),
                            Style::default().fg(if line.starts_with('+') {
                                Color::Green
                            } else if line.starts_with('-') {
                                Color::Red
                            } else if line.starts_with("@@") {
                                Color::Cyan
                            } else {
                                Color::White
                            }),
                        )
                    })
                    .collect()
            };
            let title = match self.mode {
                Mode::Save => " Review changes · Enter to write files · Esc to cancel ",
                Mode::Quit => " Unsaved changes · Enter to discard and quit · Esc to return ",
                _ => " Change preview · ↑/↓ scroll · Esc to return ",
            };
            let max_scroll = lines
                .len()
                .saturating_sub(chunks[2].height.saturating_sub(2) as usize)
                .min(u16::MAX as usize) as u16;
            self.scroll = self.scroll.min(max_scroll);
            frame.render_widget(
                Paragraph::new(lines)
                    .scroll((self.scroll, 0))
                    .block(Block::bordered().title(title)),
                chunks[2],
            );
        } else {
            let visible = self.rows();
            self.selected = self.selected.min(visible.len().saturating_sub(1));
            self.list.select(if visible.is_empty() {
                None
            } else {
                Some(self.selected)
            });
            let items: Vec<_> = visible
                .iter()
                .map(|row| {
                    let index = match row {
                        Row::Test(index) => *index,
                        Row::Directory { path, tests } => {
                            let enabled = tests
                                .iter()
                                .filter(|&&i| self.catalog.tests[i].enabled)
                                .count();
                            let checkbox = if enabled == tests.len() {
                                'x'
                            } else if enabled == 0 {
                                ' '
                            } else {
                                '-'
                            };
                            let pending = tests.iter().any(|&i| {
                                self.catalog.tests[i].enabled != self.catalog.tests[i].original
                            });
                            return ListItem::new(Line::styled(
                                format!(
                                    " [{}] {}{}/ · {enabled}/{} enabled",
                                    checkbox,
                                    if pending { "* " } else { "  " },
                                    self.directory_label(path),
                                    tests.len()
                                ),
                                Style::default()
                                    .fg(Color::Cyan)
                                    .add_modifier(Modifier::BOLD),
                            ));
                        }
                    };
                    let test = &self.catalog.tests[index];
                    ListItem::new(Line::from(vec![
                        Span::styled(
                            format!("   [{}] ", if test.enabled { 'x' } else { ' ' }),
                            Style::default().fg(if test.enabled {
                                Color::Green
                            } else {
                                Color::DarkGray
                            }),
                        ),
                        Span::styled(
                            if test.original != test.enabled {
                                "* "
                            } else {
                                "  "
                            },
                            Style::default().fg(Color::Yellow),
                        ),
                        Span::raw(format!("{} · {}", test.method, test.class)),
                    ]))
                })
                .collect();
            let list = List::new(items)
                .block(Block::bordered().title(format!(
                    " {} matches · [x] enabled · [-] mixed · * unsaved ",
                    self.visible().len()
                )))
                .highlight_style(
                    Style::default()
                        .bg(Color::DarkGray)
                        .add_modifier(Modifier::BOLD),
                )
                .highlight_symbol("›");
            frame.render_stateful_widget(list, chunks[2], &mut self.list);
            if visible.is_empty() {
                frame.render_widget(
                    Paragraph::new("No tests match. Press / to edit the filter or r to rescan."),
                    Block::bordered().inner(chunks[2]),
                );
            }
        }
        let rows = self.rows();
        let details = rows.get(self.selected).map(|row| match row {
            Row::Directory { path, tests } => format!(
                "{}/ · {} tests\nSpace toggles ALL tests directly in this directory, including hidden matches.\nSubdirectories have separate checkboxes. Press s to review and save.",
                self.directory_label(path), tests.len()
            ),
            Row::Test(index) => {
                let test = &self.catalog.tests[*index];
                format!("{}:{}\n{}.{}  {}\nOnly test-producing annotations are toggled; OS/mode restrictions still apply.", self.catalog.sources[test.file].path.strip_prefix(&self.catalog.root).unwrap().display(), test.line + 1, test.class, test.method, test.annotation.replace('\n', " "))
            }
        }).unwrap_or_else(|| "Tests are discovered from .kt files on every launch and reload.".into());
        frame.render_widget(
            Paragraph::new(details)
                .wrap(Wrap { trim: false })
                .block(Block::bordered().title(" Selected directory / test ")),
            chunks[3],
        );
        let keys = if matches!(self.mode, Mode::Browse) {
            "↑↓/jk move · Space toggle test/directory · e/d enable/disable matches · o keep ONLY matches · / search\np preview · s save · u undo pending · r reload · q quit"
        } else {
            "↑↓ scroll · Enter confirm · Esc return"
        };
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(&self.message, Style::default().fg(Color::Yellow)),
                Line::raw(keys.split('\n').next().unwrap()),
                Line::raw(keys.split('\n').nth(1).unwrap_or("")),
            ]),
            chunks[4],
        );
    }

    fn key(&mut self, code: KeyCode) -> Result<bool> {
        if matches!(self.mode, Mode::Search) {
            match code {
                KeyCode::Enter => self.mode = Mode::Browse,
                KeyCode::Esc => {
                    self.query.clear();
                    self.mode = Mode::Browse;
                }
                KeyCode::Backspace => {
                    self.query.pop();
                    self.selected = 0;
                }
                KeyCode::Char(c) => {
                    self.query.push(c);
                    self.selected = 0;
                }
                _ => {}
            }
            return Ok(false);
        }
        if !matches!(self.mode, Mode::Browse) {
            match code {
                KeyCode::Esc | KeyCode::Char('q') => self.mode = Mode::Browse,
                KeyCode::Down | KeyCode::Char('j') => self.scroll = self.scroll.saturating_add(1),
                KeyCode::Up | KeyCode::Char('k') => self.scroll = self.scroll.saturating_sub(1),
                KeyCode::PageDown => self.scroll = self.scroll.saturating_add(15),
                KeyCode::PageUp => self.scroll = self.scroll.saturating_sub(15),
                KeyCode::Enter => {
                    if matches!(self.mode, Mode::Quit) {
                        return Ok(true);
                    }
                    if matches!(self.mode, Mode::Save) {
                        self.message = match self.catalog.save() {
                            Ok(count) => format!(
                                "Saved {count} test changes. Review the diff in your checkout."
                            ),
                            Err(error) => format!("Save failed: {error:#}"),
                        };
                    }
                    self.mode = Mode::Browse;
                }
                _ => {}
            }
            return Ok(false);
        }
        let visible = self.rows();
        self.selected = self.selected.min(visible.len().saturating_sub(1));
        match code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(visible.len().saturating_sub(1))
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Home => self.selected = 0,
            KeyCode::End => self.selected = visible.len().saturating_sub(1),
            KeyCode::PageDown => {
                self.selected = (self.selected + 15).min(visible.len().saturating_sub(1))
            }
            KeyCode::PageUp => self.selected = self.selected.saturating_sub(15),
            KeyCode::Char(' ') => match visible.get(self.selected) {
                Some(Row::Test(index)) => {
                    let test = &mut self.catalog.tests[*index];
                    test.enabled = !test.enabled;
                }
                Some(Row::Directory { path, tests }) => {
                    let enabled = !tests.iter().any(|&i| self.catalog.tests[i].enabled);
                    for &index in tests {
                        self.catalog.tests[index].enabled = enabled;
                    }
                    self.message = format!(
                        "{} tests in {}/ staged as {}. Press s to review and save.",
                        tests.len(),
                        self.directory_label(path),
                        if enabled { "enabled" } else { "disabled" }
                    );
                }
                None => {}
            },
            KeyCode::Char('e') => self.bulk(true),
            KeyCode::Char('d') => self.bulk(false),
            KeyCode::Char('o') => self.isolate(),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Esc => {
                self.query.clear();
                self.selected = 0;
            }
            KeyCode::Char('u') => {
                self.catalog.reset();
                self.message = "Pending edits undone.".into();
            }
            KeyCode::Char('r') => {
                if self.catalog.pending() > 0 {
                    self.message = "Save (s) or undo (u) pending changes before reloading.".into();
                } else {
                    match Catalog::load(&self.catalog.root) {
                        Ok(catalog) => {
                            self.catalog = catalog;
                            self.selected = 0;
                            self.message = "Rescanned Kotlin files from disk.".into();
                        }
                        Err(error) => self.message = format!("Reload failed: {error:#}"),
                    }
                }
            }
            KeyCode::Char('p') => {
                self.mode = Mode::Preview;
                self.scroll = 0;
            }
            KeyCode::Char('s') => {
                self.mode = Mode::Save;
                self.scroll = 0;
            }
            KeyCode::Char('q') => {
                if self.catalog.pending() == 0 {
                    return Ok(true);
                }
                self.mode = Mode::Quit;
                self.scroll = 0;
            }
            _ => {}
        }
        Ok(false)
    }
}

struct TerminalGuard;
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen, crossterm::cursor::Show);
    }
}

pub fn run(catalog: Catalog) -> Result<()> {
    ensure!(
        io::stdin().is_terminal() && io::stdout().is_terminal(),
        "TUI requires an interactive terminal; use --list for discovery only"
    );
    let previous_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        drop(TerminalGuard);
        previous_hook(info);
    }));
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    execute!(io::stdout(), EnterAlternateScreen)?;
    let mut terminal = Terminal::new(CrosstermBackend::new(io::stdout()))?;
    let mut app = App {
        catalog,
        query: String::new(),
        selected: 0,
        list: ListState::default(),
        mode: Mode::Browse,
        scroll: 0,
        message: "Select tests, then press s to review and save.".into(),
    };
    loop {
        terminal.draw(|frame| app.draw(frame))?;
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Release {
                continue;
            }
            if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
                app.mode = Mode::Browse;
                if app.key(KeyCode::Char('q'))? {
                    break;
                }
            } else if app.key(key.code)? {
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn directory_toggle_includes_hidden_tests_and_preserves_other_directories() {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in [
            ("Root.kt", "@Test\nfun root() {}\n"),
            ("group/First.kt", "@Test\nfun matching() {}\n"),
            ("group/Second.kt", "// @Test\nfun hidden() {}\n"),
            ("group/nested/Child.kt", "@Test\nfun child() {}\n"),
            ("other/Other.kt", "@Test\nfun other() {}\n"),
        ] {
            let path = dir.path().join(path);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(path, text).unwrap();
        }
        let mut app = App {
            catalog: Catalog::load(dir.path()).unwrap(),
            query: String::new(),
            selected: 0,
            list: ListState::default(),
            mode: Mode::Browse,
            scroll: 0,
            message: String::new(),
        };
        let directories: Vec<_> = app
            .rows()
            .iter()
            .filter_map(|row| match row {
                Row::Directory { path, .. } => Some(app.directory_label(path)),
                _ => None,
            })
            .collect();
        assert_eq!(directories, [".", "group", "group/nested", "other"]);
        app.query = "matching".into();
        assert_eq!(app.rows().len(), 2);
        let mut terminal = Terminal::new(ratatui::backend::TestBackend::new(120, 28)).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        let screen = terminal
            .backend()
            .buffer()
            .content
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(screen.contains("[-]   group/ · 1/2 enabled"));
        app.key(KeyCode::Char(' ')).unwrap();
        assert!(
            app.catalog
                .tests
                .iter()
                .all(|test| test.enabled == !matches!(test.method.as_str(), "matching" | "hidden"))
        );
        app.key(KeyCode::Char(' ')).unwrap();
        assert!(app.catalog.tests.iter().all(|test| test.enabled));
        app.key(KeyCode::Down).unwrap();
        app.key(KeyCode::Char(' ')).unwrap();
        assert!(
            !app.catalog
                .tests
                .iter()
                .find(|test| test.method == "matching")
                .unwrap()
                .enabled
        );
        assert!(
            app.catalog
                .tests
                .iter()
                .find(|test| test.method == "hidden")
                .unwrap()
                .enabled
        );
        app.key(KeyCode::Char('u')).unwrap();
        assert_eq!(app.catalog.pending(), 0);
        app.key(KeyCode::Home).unwrap();
        app.key(KeyCode::Char(' ')).unwrap();
        app.key(KeyCode::Char('s')).unwrap();
        app.key(KeyCode::Enter).unwrap();
        let saved = Catalog::load(dir.path()).unwrap();
        assert!(
            saved
                .tests
                .iter()
                .all(|test| test.enabled == !matches!(test.method.as_str(), "matching" | "hidden"))
        );
        app.query = "no matches".into();
        app.key(KeyCode::Char(' ')).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        assert_eq!(app.list.selected(), None);
    }

    #[test]
    fn filter_isolation_preview_and_save_work_together() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Sample.kt");
        std::fs::write(&path, "class Sample {\n@Test\nfun keep() {}\n@Test\nfun unrelated() {}\n// @Test\nfun alreadyDisabled() {}\n}\n").unwrap();
        let mut app = App {
            catalog: Catalog::load(dir.path()).unwrap(),
            query: "keep".into(),
            selected: 0,
            list: ListState::default(),
            mode: Mode::Browse,
            scroll: 0,
            message: String::new(),
        };
        app.key(KeyCode::Char('o')).unwrap();
        assert_eq!(app.catalog.pending(), 1);
        assert!(app.catalog.tests[0].enabled);
        assert!(!app.catalog.tests[1].enabled);
        assert!(!app.catalog.tests[2].enabled);
        app.key(KeyCode::Char('s')).unwrap();
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\n@Test\nfun unrelated")
        );
        let backend = ratatui::backend::TestBackend::new(110, 28);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal.draw(|frame| app.draw(frame)).unwrap();
        app.key(KeyCode::Enter).unwrap();
        assert_eq!(app.catalog.pending(), 0);
        assert!(
            std::fs::read_to_string(&path)
                .unwrap()
                .contains("\n// @Test\nfun unrelated")
        );
    }
}
