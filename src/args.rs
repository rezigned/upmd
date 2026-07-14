use std::path::PathBuf;

use clap::{ArgAction, Parser};

use crate::apps::config::{Config, ConfigArgs, UserConfig};

/// Run code blocks from Markdown in the terminal.
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
pub struct Args {
    /// Markdown file or directory (default: browse the current directory).
    pub file: Option<String>,

    /// Auto-advance through all code blocks sequentially.
    #[arg(long, default_value_t = false)]
    pub all: bool,

    /// Run with CLI mode: navigate code blocks interactively without TUI.
    #[arg(long, default_value_t = false)]
    pub cli: bool,

    /// Working directory for code execution (default: current directory).
    #[arg(short = 'd', long, value_name = "DIR")]
    pub working_dir: Option<String>,

    /// Auto-run code blocks without prompting (use with --all or --block).
    #[arg(short, long, default_value_t = false)]
    pub yes: bool,

    /// Specify syntax highlight theme.
    #[arg(long)]
    pub theme: Option<String>,

    /// Enable experimental state capture for code execution (default: false).
    #[arg(long, default_value_t = false)]
    pub capture_state: bool,

    /// Run a specific code block by name or numeric ID.
    ///
    /// Matches the `name` attribute on code blocks (e.g. ```bash [name:setup]).
    /// Falls back to numeric block ID. In TUI mode, jumps to the block on
    /// startup. In CLI mode, selects the matching block on startup.
    #[arg(short = 'b', long, conflicts_with = "all")]
    pub block: Option<String>,

    /// Print the full default config (all keys & sections) to stdout and exit.
    #[arg(long, default_value_t = false)]
    pub dump_default_config: bool,

    #[command(flatten)]
    tui_args: TuiArgs,
}

#[derive(Parser, Debug)]
struct TuiArgs {
    /// Use terminal's default background color (default: true).
    #[arg(long, action = ArgAction::SetTrue)]
    pub transparent: Option<bool>,

    /// Specify tick-rate for UI (ms)
    #[arg(long)]
    pub tick_rate: Option<u64>,
}

/// Parses command line arguments and returns the result.
pub fn parse() -> Result<Args, clap::Error> {
    Ok(Args::parse())
}

impl From<Args> for Config {
    fn from(val: Args) -> Self {
        build_config(val, UserConfig::default())
    }
}

/// Merges CLI arguments with loaded user config.
/// CLI flags explicitly set by the user take precedence over the config file.
pub fn build_config(args: Args, user_cfg: UserConfig) -> Config {
    let theme = args.theme.unwrap_or_else(|| user_cfg.theme().to_string());
    let working_dir = args.working_dir.map(PathBuf::from);
    let transparent = args
        .tui_args
        .transparent
        .filter(|&v| v)
        .unwrap_or_else(|| user_cfg.transparent());
    let tick_rate = args
        .tui_args
        .tick_rate
        .unwrap_or_else(|| user_cfg.tick_rate());

    Config::new(ConfigArgs {
        file: args.file,
        theme,
        capture_state: args.capture_state,
        block: args.block,
        yes: args.yes,
        all: args.all,
        tick_rate,
        tui: user_cfg.tui.unwrap_or_default(),
        cli: user_cfg.cli.unwrap_or_default(),
        transparent,
        keymap: user_cfg.keymap.unwrap_or_default(),
        binaries: user_cfg.binaries.unwrap_or_default(),
        working_dir,
    })
}
