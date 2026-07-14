pub mod app;
pub mod confirm;
pub mod envs;
pub mod file_picker;
pub mod goto;
pub mod help;
pub mod layout;
pub mod markdown;
pub mod menu;
pub mod notification;
pub mod output;
pub mod preview;
pub mod search;
pub mod selection;
pub mod tasks;
pub mod themes;
pub mod widgets;
pub mod wrap;

use ratatui::text::Line;

/// Component-defined footer shortcuts.
pub trait Shortcut {
    fn footer_shortcuts(&self) -> Line<'static>;
    fn footer_right(&self) -> Option<Line<'static>> {
        None
    }
}
