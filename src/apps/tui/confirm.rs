use crate::apps::tui::layout::centered_rect;
use keymap::{DerivedConfig, KeyMap};

use crate::apps::theme::Theme;
use crate::runner::CodeId;

use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ConfirmAction {
    #[default]
    Quit,
    ReloadFile,
    ReRun(CodeId),
}

/// Prompts the user to confirm or cancel an action.
pub struct Confirm {
    title: String,
    message: String,
    selected: bool,
    theme: Theme,
    action: ConfirmAction,
    keymap: DerivedConfig<Action>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, KeyMap)]
pub enum Action {
    /// Switches between confirm dialog options.
    #[key("left", "right", "tab")]
    Switch,
    /// Confirms the currently highlighted option.
    #[key("enter")]
    Select,
    /// Cancels the dialog without taking action.
    #[key("esc")]
    Cancel,
    Confirmed(ConfirmAction),
    Cancelled,
}
impl Confirm {
    pub fn new(
        title: &str,
        message: &str,
        theme: Theme,
        action: ConfirmAction,
        keymap: DerivedConfig<Action>,
    ) -> Self {
        Self {
            title: title.to_string(),
            message: message.to_string(),
            selected: false,
            theme,
            action,
            keymap,
        }
    }

    pub fn quit(theme: Theme, keymap: DerivedConfig<Action>) -> Self {
        Self::new(
            "Quit",
            "Are you sure you want to quit?",
            theme,
            ConfirmAction::Quit,
            keymap,
        )
    }

    pub fn reload(theme: Theme, keymap: DerivedConfig<Action>) -> Self {
        Self::new(
            "Reload File",
            "This will clear all output and state. Are you sure you want to reload?",
            theme,
            ConfirmAction::ReloadFile,
            keymap,
        )
    }

    pub fn rerun(id: CodeId, theme: Theme, keymap: DerivedConfig<Action>) -> Self {
        Self::new(
            "Re-run?",
            "This will override existing output.",
            theme,
            ConfirmAction::ReRun(id),
            keymap,
        )
    }
}

impl Component for Confirm {
    type Msg = Action;

    fn update(&mut self, msg: Self::Msg) -> Option<Cmd<Self::Msg>> {
        match msg {
            Action::Switch => {
                self.selected = !self.selected;
                None
            }
            Action::Select => {
                if self.selected {
                    Some(Cmd::msg(Action::Confirmed(self.action)))
                } else {
                    Some(Cmd::msg(Action::Cancelled))
                }
            }
            Action::Cancel => Some(Cmd::msg(Action::Cancelled)),
            Action::Confirmed(_) | Action::Cancelled => Some(Cmd::msg(msg)),
        }
    }
}

impl Input for Confirm {
    fn action(&self, event: crossterm::event::Event) -> Option<Self::Msg> {
        match event {
            crossterm::event::Event::Key(key) => self.keymap.get(&key).copied(),
            _ => None,
        }
    }
}

impl Output for Confirm {
    fn render(&self, frame: &mut ratatui::Frame, area: ratatui::layout::Rect) {
        use ratatui::{
            layout::{Alignment, Constraint, Direction, Layout},
            style::{Modifier, Style},
            text::Line,
            widgets::{Clear, Paragraph},
        };

        let popup_area = centered_rect(60, 20, area);
        frame.render_widget(Clear, popup_area);

        let block = self.theme.popup_block(&self.title);
        let inner = block.inner(popup_area);
        frame.render_widget(block, popup_area);

        let margin = if inner.height > 4 { 1 } else { 0 };
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(1),
                Constraint::Length(1), // buttons
            ])
            .margin(margin)
            .split(inner);

        let msg = Paragraph::new(self.message.as_str()).alignment(Alignment::Center);
        frame.render_widget(msg, chunks[0]);

        let (yes, no) = if self.selected {
            (
                self.theme.active_style().add_modifier(Modifier::BOLD),
                Style::default(),
            )
        } else {
            (
                Style::default(),
                self.theme.active_style().add_modifier(Modifier::BOLD),
            )
        };

        let buttons = Line::from(vec![
            ratatui::text::Span::styled("  Yes  ", yes),
            ratatui::text::Span::raw("    "),
            ratatui::text::Span::styled("  No  ", no),
        ]);

        frame.render_widget(
            Paragraph::new(buttons).alignment(Alignment::Center),
            chunks[1],
        );
    }
}
