mod catalog;
mod config;
mod ui;

use anyhow::{Result, bail};
use catalog::Catalog;
use std::path::PathBuf;

fn main() -> Result<()> {
    let mut root = std::env::var_os("RUSTROVER_UI_TESTS").map(PathBuf::from);
    let mut list = false;
    let mut positional = false;
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--help" | "-h") => {
                println!(
                    "RustRover UI test selector\n\nUsage: test_disabler [TESTS_DIRECTORY] [--list]\n\nRecursively discovers Kotlin tests from disk. No test list is embedded.\nDirectory defaults to RUSTROVER_UI_TESTS, then tests_directory in the repo root test_config.toml.\n\n--list  Print discovered tests without opening the TUI or changing files\n\nTUI: / search · Space toggle test/directory · e/d enable/disable filtered · o isolate filtered\n     u undo pending · r reload · p preview · s review and save · q quit"
                );
                return Ok(());
            }
            Some("--list") => list = true,
            Some(value) if value.starts_with('-') => bail!("unknown option: {value}; use --help"),
            _ => {
                if positional {
                    bail!("only one tests directory is allowed");
                }
                root = Some(arg.into());
                positional = true;
            }
        }
    }
    let root = match root {
        Some(root) => root,
        None => config::tests_directory(
            &PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("test_config.toml"),
        )?,
    };
    let catalog = Catalog::load(&root)?;
    if list {
        for test in &catalog.tests {
            println!(
                "[{}] {}:{}",
                if test.enabled { "x" } else { " " },
                catalog.label(test),
                test.line + 1
            );
        }
        println!("{} tests discovered", catalog.tests.len());
        return Ok(());
    }
    ui::run(catalog)
}
