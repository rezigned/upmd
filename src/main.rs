use color_eyre::Result;
use std::io::IsTerminal;

use crate::apps::config::{self};

mod apps;
mod args;
mod markdown_files;
mod runner;

mod pty;
mod reader;
mod utils;

/// Defines how `main` builds and runs a CLI or TUI app.
trait RunApp {
    /// Parses input text and construct from stdin or file input.
    fn from_input(input: &str, config: crate::apps::config::Config) -> Self;

    /// Constructs in file-picker mode (directory input with multiple files).
    fn from_picker(
        root: std::path::PathBuf,
        files: Vec<markdown_files::MarkdownFile>,
        config: crate::apps::config::Config,
    ) -> Self;

    /// Runs the app to completion.
    fn run(self) -> Result<()>;
}

fn main() -> Result<()> {
    color_eyre::install()?;
    init_tracing();
    let args = args::parse()?;

    // Print full default config and exit early.
    if args.dump_default_config {
        let mut full = config::UserConfig::default_full();
        full.keymap = Some(config::KeymapConfig::dump_all());
        println!("{}", toml::to_string_pretty(&full)?);
        return Ok(());
    }

    let is_cli = args.cli;
    let user_cfg = crate::apps::config::UserConfig::load();
    let mut config = args::build_config(args, user_cfg);

    // No file argument on an interactive terminal: open current directory.
    if config.file.is_none() && std::io::stdin().is_terminal() {
        config.file = Some(".".to_string());
    }

    if is_cli {
        run::<crate::apps::cli::app::App>(config)
    } else {
        run::<crate::apps::tui::app::App>(config)
    }
}

/// Resolves the input target, reads/discovers files, constructs the frontend,
/// and runs it.
fn run<App: RunApp>(config: crate::apps::config::Config) -> Result<()> {
    match crate::reader::resolve_input_target(&config.file)? {
        crate::reader::InputTarget::Stdin | crate::reader::InputTarget::File(_) => {
            let input = crate::reader::read_input(&config.file)?;
            App::from_input(&input, config).run()
        }
        crate::reader::InputTarget::Directory(path) => {
            let files = crate::markdown_files::find_markdown_files(
                &path,
                crate::markdown_files::MarkdownSearchOptions::default(),
            )
            .map_err(|err| color_eyre::eyre::eyre!("{err}"))?;

            if files.is_empty() {
                color_eyre::eyre::bail!("No Markdown files found under {}", path.display());
            }

            App::from_picker(path, files, config).run()
        }
    }
}

/// Initializes file-based tracing when `RUST_LOG` is set.
///
/// Writes to the project cache directory (e.g. `~/.cache/upmd/upmd.log` on
/// Linux). Silently skips if the log file can't be created. Logging is
/// diagnostic-only so `main` should not panic (or fail to start) just
/// because the log path is unwritable.
fn init_tracing() {
    use tracing_subscriber::{fmt, prelude::*, EnvFilter};

    if std::env::var("RUST_LOG").is_err() {
        return;
    }

    let Some(project_dirs) = directories::ProjectDirs::from("com", "rezigned", config::APP_NAME)
    else {
        return;
    };
    let log_dir = project_dirs.cache_dir();
    let Ok(()) = std::fs::create_dir_all(log_dir) else {
        return;
    };
    let log_path = log_dir.join(format!("{}.log", config::APP_NAME));
    let Ok(log_file) = std::fs::File::create(log_path) else {
        return;
    };

    tracing_subscriber::registry()
        .with(
            fmt::layer()
                .with_ansi(false)
                .with_writer(log_file)
                .with_span_events(tracing_subscriber::fmt::format::FmtSpan::CLOSE),
        )
        .with(EnvFilter::from_default_env())
        .try_init()
        .ok();

    tracing::info!(
        version = env!("CARGO_PKG_VERSION"),
        "upmd starting (tracing enabled)"
    );
    tracing::info!(
        "Command line args: {:?}",
        std::env::args().collect::<Vec<_>>()
    );
}
