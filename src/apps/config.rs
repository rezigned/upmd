use crate::apps::theme::Theme;
use keymap::{DerivedConfig, KeyMapConfig};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

pub const LOGO: [&str; 4] = [
    "                   ▄",
    "█  █ █▀▀█ █▀█▀█ █▀▀█",
    "█▄▄█ █▄▄█ █ █ █ █▄▄█",
    "     ▀              ",
];
pub const APP_NAME: &str = env!("CARGO_PKG_NAME");
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");

/// Default theme name.
pub const DEFAULT_THEME: &str = "catppuccin-mocha";
pub const DEFAULT_SYNTAX: &str = "txt";

pub const SUCCESS_SYMBOL: &str = "✔";
pub const ERROR_SYMBOL: &str = "✘";

/// Tick rate in ms (default to 15 fps ~ 66).
pub const TICK_RATE_MS: u64 = 66;

pub const PTY_DEFAULT_COLS: u16 = 80;
pub const PTY_DEFAULT_ROWS: u16 = 24;

/// Height of the footer row in the full-screen output view.
pub const OUTPUT_FOOTER_HEIGHT: u16 = 1;

/// Stream channel capacity (1000 messages).
pub const STREAM_CHANNEL_SIZE: usize = 1000;

// Preview layout constants

/// Height subtracted from the terminal height to account for borders.
pub const BORDER_HEIGHT: usize = 2;

/// Fraction of the viewport height used as the inline output cap (1/2 = 50%).
pub const INLINE_MAX_LINES_FRACTION: usize = 2;

/// Minimum number of inline output lines shown in the preview.
pub const INLINE_MAX_LINES_MIN: usize = 3;

/// Maximum number of inline output lines shown in the preview.
pub const INLINE_MAX_LINES_MAX: usize = 20;

/// Initial inline output line cap before the first render sets the real value.
pub const INLINE_MAX_LINES_DEFAULT: usize = 10;

/// Horizontal offset for content inside the preview pane (left border + left padding).
pub const PREVIEW_CONTENT_X_OFFSET: u16 = 2;

/// Vertical offset for content inside the preview pane (top border).
pub const PREVIEW_CONTENT_TOP_OFFSET: u16 = 1;

/// Horizontal space consumed by the preview pane's borders and padding
/// (left border + left padding + right padding + right border).
pub const PREVIEW_FRAME_OVERHEAD: usize = 4;

/// Width of the gutter prepended to code-block lines (`"▎ "`).
pub const CODE_GUTTER_WIDTH: usize = 2;

/// Total horizontal overhead for code-block content (frame + gutter).
pub const PREVIEW_CODE_WRAP_OVERHEAD: usize = PREVIEW_FRAME_OVERHEAD + CODE_GUTTER_WIDTH;

/// Number of lines of overdraw added above and below the visible viewport
/// so ratatui's list widget can scroll smoothly without blank frames.
/// Expressed as a fraction of the viewport (1/2 = 50%).
pub const OVERDRAW_FRACTION: usize = 2;

/// Terminal height threshold below which the home layout switches from
/// horizontal (side-by-side menu + preview) to vertical stacking.
pub const VERTICAL_LAYOUT_HEIGHT_THRESHOLD: u16 = 20;

/// Height of the menu in the vertical (stacked) layout.
pub const VERTICAL_MENU_HEIGHT: u16 = 3;

/// Max menu sidebar width = total_width / MENU_MAX_WIDTH_RATIO.
pub const MENU_MAX_WIDTH_RATIO: u16 = 4;
pub const MENU_BORDER_SIZE: u16 = 2;
/// Minimum TOC panel width.
pub const MENU_TOC_MIN_WIDTH: u16 = 10;
/// Maximum TOC panel width.
pub const MENU_TOC_MAX_WIDTH: u16 = 60;

// CLI mode constants

/// Number of code lines shown per card in CLI mode.
pub const CLI_PREVIEW_LINES: usize = 8;

/// Horizontal column overhead reserved for the CLI card UI (left + right indent).
pub const CLI_PTY_COL_OVERHEAD: u16 = 4;

/// Vertical row overhead reserved for the CLI card UI (header, preview, footer, etc.).
pub const CLI_PTY_ROW_OVERHEAD: u16 = 10;

/// Minimum PTY columns in CLI mode.
pub const CLI_PTY_MIN_COLS: u16 = 40;

/// Minimum PTY rows in CLI mode.
pub const CLI_PTY_MIN_ROWS: u16 = 5;

/// Per-component keymap overrides stored under `[keymap]` in config.toml.
///
/// Each field holds a partial TOML table that is deserialized as a
/// `DerivedConfig<T>`: only the variants you want to remap need to be
/// listed; everything else falls back to the compile-time `#[key(...)]`
/// defaults from the `KeyMap` derive macro.
///
/// Example config.toml:
/// ```toml
/// [keymap.home]
/// Execute = { keys = ["x"] }
/// Quit    = { keys = ["ctrl-q"] }
///
/// [keymap.output]
/// Next = { keys = ["space"] }
///
/// [keymap.cli]
/// Run  = { keys = ["space"] }
/// Quit = { keys = ["ctrl-q"] }
/// ```
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeymapConfig {
    /// Overrides for the `Home` component's `Action` enum.
    #[serde(default)]
    pub home: toml::Table,

    /// Overrides for the `Output` component's `Action` enum.
    #[serde(default)]
    pub output: toml::Table,

    /// Overrides for the CLI mode's `Action` enum.
    #[serde(default)]
    pub cli: toml::Table,

    /// Overrides for the `Menu` component's `Action` enum.
    #[serde(default)]
    pub menu: toml::Table,

    /// Overrides for the `Preview` component's `Action` enum.
    #[serde(default)]
    pub preview: toml::Table,

    /// Overrides for the `Confirm` dialog's `Action` enum.
    #[serde(default)]
    pub confirm: toml::Table,

    /// Overrides for the `Search` component's `Action` enum.
    #[serde(default)]
    pub search: toml::Table,

    /// Overrides for the `Goto` component's `Action` enum.
    #[serde(default)]
    pub goto: toml::Table,

    /// Overrides for the `FilePicker` component's `Action` enum.
    #[serde(default)]
    pub file_picker: toml::Table,

    /// Overrides for the `Help` overlay's `Action` enum.
    #[serde(default)]
    pub help: toml::Table,

    /// Overrides for the `EnvVars` component's main keymap (`MainAction`).
    #[serde(default)]
    pub envs: toml::Table,

    /// Overrides for the `EnvVars` component's edit keymap (`EditAction`).
    #[serde(default)]
    pub envs_edit: toml::Table,

    /// Overrides for the `ThemeSelector` component's keymaps.
    #[serde(default)]
    pub themes: toml::Table,
}

impl Default for KeymapConfig {
    fn default() -> Self {
        Self {
            home: toml::Table::new(),
            output: toml::Table::new(),
            cli: toml::Table::new(),
            menu: toml::Table::new(),
            preview: toml::Table::new(),
            confirm: toml::Table::new(),
            search: toml::Table::new(),
            goto: toml::Table::new(),
            file_picker: toml::Table::new(),
            help: toml::Table::new(),
            envs: toml::Table::new(),
            envs_edit: toml::Table::new(),
            themes: toml::Table::new(),
        }
    }
}

impl KeymapConfig {
    /// Deserialise the `home` table into a `DerivedConfig<T>`, merging user
    /// overrides on top of the compile-time defaults.
    ///
    /// Returns the default config when the table is absent or malformed.
    pub fn home<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.home)
    }

    /// Same as [`home_keymap`] but for the `output` table.
    pub fn output<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.output)
    }

    /// Same as [`home_keymap`] but for the CLI mode `Action` table.
    pub fn cli<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.cli)
    }

    /// Same as [`home_keymap`] but for the `Menu` component.
    pub fn menu<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.menu)
    }

    /// Same as [`home_keymap`] but for the `Preview` component.
    pub fn preview<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.preview)
    }

    /// Same as [`home_keymap`] but for the `Confirm` dialog.
    pub fn confirm<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.confirm)
    }

    /// Same as [`home_keymap`] but for the `Search` component.
    pub fn search<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.search)
    }

    /// Same as [`home_keymap`] but for the `Goto` component.
    pub fn goto<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.goto)
    }

    /// Same as [`home_keymap`] but for the `FilePicker` component.
    pub fn file_picker<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.file_picker)
    }

    /// Same as [`home_keymap`] but for the `Help` overlay.
    pub fn help<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.help)
    }

    /// Same as [`home_keymap`] but for the `EnvVars` component's main actions.
    pub fn envs<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.envs)
    }

    /// Same as [`home_keymap`] but for the `EnvVars` component's edit mode actions.
    pub fn envs_edit<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.envs_edit)
    }

    /// Same as [`home_keymap`] but for the `ThemeSelector` component.
    pub fn themes<T>(&self) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        Self::parse_derived(&self.themes)
    }

    /// Serializes the default keybindings for a [`KeyMapConfig<T>`] type into a
    /// `toml::Table`, using the actual `#[key(...)]` attributes from the code.
    ///
    /// Each entry is a variant name → `{ keys = [...], description = "...",
    /// symbol = "...", help = "..." }` map. Used by `--dump-default-config` to show
    /// every binding that the app ships by default.
    ///
    /// The `KeyMap` derive macro (from `keymap_derive`) automatically generates
    /// `serde::Serialize` for `T`, serialising each variant to its name string.
    pub fn dump_keymap_table<T>() -> toml::Table
    where
        T: KeyMapConfig<T> + serde::Serialize + PartialEq + Eq + std::hash::Hash,
    {
        let config = T::keymap_config();
        let mut table = toml::Table::new();
        for (action, item) in &config.items {
            let name = toml::Value::try_from(action)
                .ok()
                .and_then(|v| v.as_str().map(String::from))
                .unwrap_or_default();

            if name.is_empty() {
                continue;
            }

            let mut entry = toml::Table::new();
            entry.insert(
                "keys".to_string(),
                toml::Value::Array(
                    item.keys
                        .iter()
                        .map(|k| toml::Value::String(k.clone()))
                        .collect(),
                ),
            );
            if !item.description.is_empty() {
                entry.insert(
                    "description".to_string(),
                    toml::Value::String(item.description.clone()),
                );
            }
            if let Some(symbol) = &item.symbol {
                entry.insert("symbol".to_string(), toml::Value::String(symbol.clone()));
            }
            if let Some(help) = &item.help {
                entry.insert("help".to_string(), toml::Value::String(help.clone()));
            }
            table.insert(name, toml::Value::Table(entry));
        }
        table
    }

    /// Returns a `KeymapConfig` with every component's default keybindings
    /// populated via [`dump_keymap_table`]. Used by `--dump-default-config`.
    ///
    /// When adding a new component with keybindings, register its `Action` type
    /// here so `--dump-default-config` includes it.
    pub fn dump_all() -> Self {
        Self {
            home: Self::dump_keymap_table::<crate::apps::tui::app::Action>(),
            output: Self::dump_keymap_table::<crate::apps::tui::output::Action>(),
            cli: Self::dump_keymap_table::<crate::apps::cli::app::Action>(),
            menu: Self::dump_keymap_table::<crate::apps::navigation::Navigation>(),
            preview: Self::dump_keymap_table::<crate::apps::tui::preview::Action>(),
            confirm: Self::dump_keymap_table::<crate::apps::tui::confirm::Action>(),
            search: Self::dump_keymap_table::<crate::apps::tui::search::Action>(),
            goto: Self::dump_keymap_table::<crate::apps::tui::goto::Action>(),
            help: Self::dump_keymap_table::<crate::apps::tui::help::Action>(),
            file_picker: Self::dump_keymap_table::<crate::apps::tui::file_picker::Action>(),
            envs: Self::dump_keymap_table::<crate::apps::tui::envs::MainAction>(),
            envs_edit: Self::dump_keymap_table::<crate::apps::tui::envs::EditAction>(),
            themes: Self::dump_keymap_table::<crate::apps::tui::themes::MainAction>(),
        }
    }

    pub(crate) fn parse_derived<T>(table: &toml::Table) -> DerivedConfig<T>
    where
        T: KeyMapConfig<T> + for<'de> Deserialize<'de> + Eq + std::hash::Hash,
    {
        let raw = toml::to_string(table).unwrap_or_default();
        tracing::debug!("Parsing keymap config: {}", raw);
        toml::from_str(&raw).unwrap_or_else(|e| {
            tracing::warn!("Failed to parse keymap config: {e}");
            // Empty table → DerivedConfig falls back entirely to defaults
            toml::from_str("").expect("empty keymap deserializes to defaults")
        })
    }
}

/// Persistent user preferences stored in `~/.config/upmd/config.toml`.
///
/// CLI flags always take precedence over values in this file.
/// Fields use `Option` + `skip_serializing_if` so only user-set values are
/// persisted. Everything else falls back to compile-time defaults on load.
///
/// Example config.toml:
/// ```toml
/// theme = "tokyo-night"
/// transparent = true
///
/// [tui]
/// inline_max_lines = 15
///
/// [cli]
/// preview_lines = 12
/// ```
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UserConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub transparent: Option<bool>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub tick_rate: Option<u64>,

    /// TUI-specific display preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tui: Option<TuiConfig>,

    /// CLI-specific display preferences.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cli: Option<CliConfig>,

    /// Optional per-component keymap overrides.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub keymap: Option<KeymapConfig>,

    /// Per-language binary overrides (e.g. `rust.bin = "/usr/local/bin/rustc-ng"`).
    ///
    /// Each key is a language name matching the runner (e.g. `rust`, `python`).
    /// Values override the binary, extra args, and environment.
    ///
    /// ```toml
    /// [binaries.rust]
    /// bin = "/usr/local/bin/rustc-ng"
    /// extra_args = ["--edition", "2021"]
    /// ```
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binaries: Option<HashMap<String, upmd_runner::RunnerOptions>>,
}

/// TUI display preferences (`[tui]` in config.toml).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TuiConfig {
    /// Maximum number of inline output lines shown beneath a code block in the
    /// preview. Clamped to [`INLINE_MAX_LINES_MIN`]..=[`INLINE_MAX_LINES_MAX`]
    /// at render time. Defaults to [`INLINE_MAX_LINES_MAX`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inline_max_lines: Option<usize>,
}

impl TuiConfig {
    pub fn inline_max_lines(&self) -> usize {
        self.inline_max_lines.unwrap_or(INLINE_MAX_LINES_MAX)
    }
}

/// CLI display preferences (`[cli]` in config.toml).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct CliConfig {
    /// Number of code lines shown per card in CLI mode.
    /// Defaults to [`CLI_PREVIEW_LINES`].
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_lines: Option<usize>,
}

impl UserConfig {
    /// Returns the platform-appropriate config directory path.
    /// e.g. `~/.config/upmd/config.toml` on Linux,
    ///      `~/Library/Application Support/com.rezigned.upmd/config.toml` on macOS.
    pub fn path() -> Option<std::path::PathBuf> {
        directories::ProjectDirs::from("com", "rezigned", "upmd")
            .map(|dirs| dirs.config_dir().join("config.toml"))
    }

    /// Loads from disk; returns `Default` if missing or malformed.
    pub fn load() -> Self {
        let Some(path) = Self::path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(content) => toml::from_str(&content).unwrap_or_else(|e| {
                tracing::warn!("Failed to parse config file {:?}: {e}", path);
                Self::default()
            }),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => {
                tracing::warn!("Failed to read config file {:?}: {e}", path);
                Self::default()
            }
        }
    }

    /// Persists to disk, creating the config directory if needed.
    ///
    /// Only fields set to `Some(...)` are serialized (via
    /// `#[serde(skip_serializing_if = "Option::is_none")]`), so the saved file
    /// stays minimal. All other fields fall back to defaults on the next
    /// `load()`.
    pub fn save(&self) -> anyhow::Result<()> {
        let path =
            Self::path().ok_or_else(|| anyhow::anyhow!("Cannot determine config directory"))?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let content = toml::to_string_pretty(self)?;
        std::fs::write(&path, content)?;
        tracing::debug!("Saved user config to {:?}", path);
        Ok(())
    }

    /// Loads, applies a mutation, and saves the config in one shot.
    pub fn update<F>(f: F) -> anyhow::Result<()>
    where
        F: FnOnce(&mut Self),
    {
        let mut cfg = Self::load();
        f(&mut cfg);
        cfg.save()
    }

    pub fn theme(&self) -> &str {
        self.theme.as_deref().unwrap_or(DEFAULT_THEME)
    }

    pub fn transparent(&self) -> bool {
        self.transparent.unwrap_or(false)
    }

    pub fn tick_rate(&self) -> u64 {
        self.tick_rate.unwrap_or(TICK_RATE_MS)
    }

    /// Returns a `UserConfig` with every field populated to its runtime default.
    ///
    /// This is the inverse of the normal `Default` (all-`None`): it produces a
    /// config where every `Option` is `Some(...)` so that serialising it shows
    /// users every key and section available in the config file.
    ///
    /// Used by the `--dump-default-config` CLI flag.
    pub fn default_full() -> Self {
        Self {
            theme: Some(DEFAULT_THEME.to_string()),
            transparent: Some(false),
            tick_rate: Some(TICK_RATE_MS),
            tui: Some(TuiConfig {
                inline_max_lines: Some(INLINE_MAX_LINES_MAX),
            }),
            cli: Some(CliConfig {
                preview_lines: Some(CLI_PREVIEW_LINES),
            }),
            keymap: None,
            binaries: Some(HashMap::new()),
        }
    }
}

/// Merged application configuration from CLI arguments, user config, and defaults.
#[derive(Debug, Default, Clone)]
pub struct Config {
    pub theme: Theme,
    pub transparent: bool,
    pub file: Option<String>,
    pub capture_state: bool,
    pub block: Option<String>,
    pub yes: bool,
    pub all: bool,
    pub tick_rate: u64,
    pub tui: TuiConfig,
    /// CLI-specific display preferences loaded from the user config file.
    pub cli: CliConfig,
    /// Per-component keymap overrides loaded from the user config file.
    pub keymap: KeymapConfig,
    /// Per-language binary overrides.
    pub binaries: HashMap<String, upmd_runner::RunnerOptions>,
    /// Working directory for code execution.
    pub working_dir: Option<PathBuf>,
}

pub type Envs = BTreeMap<String, String>;

/// Arguments for constructing a [`Config`].
///
/// Using a struct avoids the 13 positional parameters that `Config::new`
/// previously required and prevents argument-order mistakes at call sites.
#[derive(Debug, Clone)]
pub struct ConfigArgs {
    pub file: Option<String>,
    pub theme: String,
    pub capture_state: bool,
    pub block: Option<String>,
    pub yes: bool,
    pub all: bool,
    pub tick_rate: u64,
    pub tui: TuiConfig,
    pub cli: CliConfig,
    pub transparent: bool,
    pub keymap: KeymapConfig,
    pub binaries: HashMap<String, upmd_runner::RunnerOptions>,
    pub working_dir: Option<PathBuf>,
}

impl Config {
    pub fn new(args: ConfigArgs) -> Self {
        Self {
            theme: Theme::new(&args.theme, args.transparent),
            file: args.file,
            capture_state: args.capture_state,
            block: args.block,
            yes: args.yes,
            all: args.all,
            tick_rate: args.tick_rate,
            tui: args.tui,
            cli: args.cli,
            transparent: args.transparent,
            keymap: args.keymap,
            binaries: args.binaries,
            working_dir: args.working_dir,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_user_config_defaults() {
        let cfg = UserConfig::default();
        assert_eq!(cfg.theme(), "catppuccin-mocha");
        assert!(!cfg.transparent());
        assert_eq!(cfg.tick_rate(), 66);
        assert_eq!(cfg.tui.as_ref().map(|t| t.inline_max_lines()), None);
        assert!(cfg.cli.is_none());
    }

    #[test]
    fn test_user_config_deserialize_empty() {
        let cfg: UserConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.theme(), "catppuccin-mocha");
        assert!(!cfg.transparent());
    }

    #[test]
    fn test_user_config_deserialize_partial() {
        let toml_str = r#"
theme = "tokyo-night"
transparent = true
"#;
        let cfg: UserConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.theme(), "tokyo-night");
        assert!(cfg.transparent());
        assert_eq!(cfg.tick_rate(), 66); // default
    }

    #[test]
    fn test_user_config_deserialize_full() {
        let toml_str = r#"
theme = "dracula"
transparent = true
tick_rate = 100

[tui]
inline_max_lines = 15

[cli]
preview_lines = 6
"#;
        let cfg: UserConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.theme(), "dracula");
        assert!(cfg.transparent());
        assert_eq!(cfg.tick_rate(), 100);
        assert_eq!(cfg.tui.as_ref().unwrap().inline_max_lines(), 15);
        assert_eq!(cfg.cli.as_ref().unwrap().preview_lines.unwrap(), 6);
    }

    #[test]
    fn test_user_config_serialize_roundtrip() {
        let original = UserConfig {
            theme: Some("catppuccin-mocha".into()),
            transparent: Some(true),
            tick_rate: Some(50),
            tui: Some(TuiConfig {
                inline_max_lines: Some(12),
            }),
            cli: Some(CliConfig {
                preview_lines: Some(10),
            }),
            keymap: Some(KeymapConfig::default()),
            binaries: None,
        };
        let serialized = toml::to_string_pretty(&original).unwrap();
        let deserialized: UserConfig = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.theme(), "catppuccin-mocha");
        assert!(deserialized.transparent());
        assert_eq!(deserialized.tick_rate(), 50);
        assert_eq!(deserialized.tui.as_ref().unwrap().inline_max_lines(), 12);
        assert_eq!(
            deserialized.cli.as_ref().unwrap().preview_lines.unwrap(),
            10
        );
    }

    #[test]
    fn test_keymap_config_default() {
        let kc = KeymapConfig::default();
        assert!(kc.home.is_empty());
        assert!(kc.output.is_empty());
        assert!(kc.cli.is_empty());
        assert!(kc.menu.is_empty());
        assert!(kc.preview.is_empty());
    }

    #[test]
    fn test_tui_config_default() {
        let tui = TuiConfig::default();
        assert_eq!(tui.inline_max_lines(), INLINE_MAX_LINES_MAX);
    }

    #[test]
    fn test_cli_config_default() {
        let cli = CliConfig::default();
        assert!(cli.preview_lines.is_none());
    }

    #[test]
    fn test_user_config_serialize_only_non_none() {
        let cfg = UserConfig {
            theme: Some("tokyo-night".into()),
            ..Default::default()
        };
        let raw = toml::to_string_pretty(&cfg).unwrap();
        assert!(!raw.contains("transparent"));
        assert!(!raw.contains("tick_rate"));
        assert!(!raw.contains("[tui]"));
        assert!(!raw.contains("[cli]"));
        assert!(!raw.contains("[keymap]"));
        assert!(raw.contains("tokyo-night"));

        // Roundtrip: load should fill defaults
        let loaded: UserConfig = toml::from_str(&raw).unwrap();
        assert_eq!(loaded.theme(), "tokyo-night");
        assert!(!loaded.transparent());
        assert_eq!(loaded.tick_rate(), 66);
    }

    #[test]
    fn test_config_new_theme_non_transparent() {
        let config = Config::new(ConfigArgs {
            file: None,
            theme: "base16-ocean.dark".into(),
            capture_state: false,
            block: None,
            yes: false,
            all: false,
            tick_rate: 66,
            tui: TuiConfig::default(),
            cli: CliConfig::default(),
            transparent: false,
            keymap: KeymapConfig::default(),
            binaries: HashMap::new(),
            working_dir: None,
        });
        assert!(!config.transparent);
        assert_ne!(config.theme.background, ratatui::style::Color::Reset);
        assert_ne!(config.theme.foreground, ratatui::style::Color::Reset);
    }

    #[test]
    fn test_config_new_theme_transparent() {
        let config = Config::new(ConfigArgs {
            file: None,
            theme: "base16-ocean.dark".into(),
            capture_state: false,
            block: None,
            yes: false,
            all: false,
            tick_rate: 66,
            tui: TuiConfig::default(),
            cli: CliConfig::default(),
            transparent: true,
            keymap: KeymapConfig::default(),
            binaries: HashMap::new(),
            working_dir: None,
        });
        assert!(config.transparent);
        assert_eq!(config.theme.background, ratatui::style::Color::Reset);
        assert_eq!(config.theme.foreground, ratatui::style::Color::Reset);
    }

    #[test]
    fn test_config_new_transparent_field_matches_argument() {
        let non_transparent = Config::new(ConfigArgs {
            file: None,
            theme: "base16-ocean.dark".into(),
            capture_state: false,
            block: None,
            yes: false,
            all: false,
            tick_rate: 66,
            tui: TuiConfig::default(),
            cli: CliConfig::default(),
            transparent: false,
            keymap: KeymapConfig::default(),
            binaries: HashMap::new(),
            working_dir: None,
        });
        let transparent = Config::new(ConfigArgs {
            file: None,
            theme: "base16-ocean.dark".into(),
            capture_state: false,
            block: None,
            yes: false,
            all: false,
            tick_rate: 66,
            tui: TuiConfig::default(),
            cli: CliConfig::default(),
            transparent: true,
            keymap: KeymapConfig::default(),
            binaries: HashMap::new(),
            working_dir: None,
        });
        assert!(!non_transparent.transparent);
        assert!(transparent.transparent);
    }
}
