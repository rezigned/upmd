use std::{io::IsTerminal, process::ExitCode};

use color_eyre::Result;

use crate::apps::config::{self};

mod apps;
mod args;
mod markdown_files;
mod runner;

mod pty;
mod reader;
mod utils;

trait RunApp: Sized {
    fn from_input(
        input: String,
        config: crate::apps::config::Config,
    ) -> std::result::Result<Self, String>;

    fn from_picker(
        root: std::path::PathBuf,
        files: Vec<markdown_files::MarkdownFile>,
        config: crate::apps::config::Config,
    ) -> Self;

    fn run(self) -> Result<ExitCode>;
}

fn main() -> Result<ExitCode> {
    color_eyre::install()?;
    init_tracing();
    let args = args::parse()?;

    // Print full default config and exit early.
    if args.dump_default_config {
        let mut full = config::UserConfig::default_full();
        full.keymap = Some(config::KeymapConfig::dump_all());
        println!("{}", toml::to_string_pretty(&full)?);
        return Ok(ExitCode::SUCCESS);
    }

    let is_cli = args.cli;
    let user_cfg = crate::apps::config::UserConfig::load();
    let mut config = args::build_config(args, user_cfg);

    // No file argument on an interactive terminal: check for up.md/UP.md,
    // otherwise open current directory for the file picker.
    if config.file.is_none() && std::io::stdin().is_terminal() {
        config.file = ["up.md", "UP.md"]
            .into_iter()
            .find(|f| std::path::Path::new(f).exists())
            .map(|f| f.to_string())
            .or_else(|| Some(".".to_string()));
    }

    if is_cli {
        run::<crate::apps::cli::app::App>(config)
    } else {
        run::<crate::apps::tui::app::App>(config)
    }
}

fn run<App: RunApp>(config: crate::apps::config::Config) -> Result<ExitCode> {
    match crate::reader::resolve_input_target(&config.file)? {
        crate::reader::InputTarget::Stdin | crate::reader::InputTarget::File(_) => {
            let input = crate::reader::read_input(&config.file)?;
            App::from_input(input, config)
                .map_err(|error| color_eyre::eyre::eyre!(error))?
                .run()
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
