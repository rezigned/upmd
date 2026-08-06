use std::{collections::HashMap, path::PathBuf};

use crossterm::event::Event;
use ratatui::{layout::Rect, text::Line, Frame};
use upmd_parser::CodeId;
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Cmd, Component,
};

use crate::apps::{
    task::TaskStatus,
    theme::Theme,
    tui::{confirm, dependencies, envs, file_picker, goto, help, search, themes, Shortcut},
};

pub(crate) enum Overlay {
    Confirm(confirm::Confirm),
    Help(help::Help),
    Envs,
    Search(search::Search),
    Goto(goto::Goto),
    Themes(Box<themes::ThemeSelector>),
    FilePicker {
        picker: file_picker::FilePicker,
        quit_on_cancel: bool,
    },
    Dependencies(dependencies::Dependencies),
}

#[derive(Clone, Debug)]
pub(crate) enum Message {
    Confirm(confirm::Action),
    Help(help::Action),
    Envs(envs::Action),
    Search(search::Action),
    Goto(goto::Action),
    Themes(themes::Action),
    FilePicker(file_picker::Action),
    Dependencies(dependencies::Action),
}

pub(crate) enum Effect {
    Close,
    Quit,
    Reload,
    Rerun(CodeId),
    ClearSearch,
    RefreshSearch(String),
    SelectCode(CodeId),
    OpenFile(PathBuf),
    PreviewTheme(Theme),
    SaveTheme(Theme),
    RestoreTheme(Theme),
}

pub(crate) struct Update {
    pub command: Option<Cmd<Message>>,
    pub effect: Option<Effect>,
}

impl Update {
    fn none() -> Self {
        Self {
            command: None,
            effect: None,
        }
    }

    fn effect(effect: Effect) -> Self {
        Self {
            command: None,
            effect: Some(effect),
        }
    }
}

impl Overlay {
    pub fn action(&self, event: Event, envs: &envs::EnvVars) -> Option<Message> {
        match self {
            Self::Confirm(component) => component.action(event).map(Message::Confirm),
            Self::Help(component) => component.action(event).map(Message::Help),
            Self::Envs => envs.action(event).map(Message::Envs),
            Self::Search(component) => component.action(event).map(Message::Search),
            Self::Goto(component) => component.action(event).map(Message::Goto),
            Self::Themes(component) => component.action(event).map(Message::Themes),
            Self::FilePicker { picker, .. } => picker.action(event).map(Message::FilePicker),
            Self::Dependencies(component) => component.action(event).map(Message::Dependencies),
        }
    }

    pub fn update(&mut self, message: Message, envs: &mut envs::EnvVars) -> Update {
        match (self, message) {
            (Self::Confirm(component), Message::Confirm(action)) => {
                let command = component.update(action);
                match action {
                    confirm::Action::Confirmed(confirm::ConfirmAction::Quit) => {
                        Update::effect(Effect::Quit)
                    }
                    confirm::Action::Confirmed(confirm::ConfirmAction::ReloadFile) => {
                        Update::effect(Effect::Reload)
                    }
                    confirm::Action::Confirmed(confirm::ConfirmAction::ReRun(id)) => {
                        Update::effect(Effect::Rerun(id))
                    }
                    confirm::Action::Cancelled => Update::effect(Effect::Close),
                    _ => Update {
                        command: command.map(|cmd| cmd.map(Message::Confirm)),
                        effect: None,
                    },
                }
            }
            (Self::Help(component), Message::Help(action)) => {
                if component.update(action).is_some() {
                    Update::effect(Effect::Close)
                } else {
                    Update::none()
                }
            }
            (Self::Envs, Message::Envs(action)) => {
                let command = envs.update(action.clone());
                if matches!(action, envs::Action::Quit) {
                    Update::effect(Effect::Close)
                } else {
                    Update {
                        command: command.map(|cmd| cmd.map(Message::Envs)),
                        effect: None,
                    }
                }
            }
            (Self::Search(component), Message::Search(action)) => {
                let completed = component.update(action).is_some();
                if completed {
                    match action {
                        search::Action::Quit => Update::effect(Effect::ClearSearch),
                        search::Action::Select => Update::effect(Effect::Close),
                        _ => Update::none(),
                    }
                } else {
                    Update::effect(Effect::RefreshSearch(component.term().to_string()))
                }
            }
            (Self::Goto(component), Message::Goto(action)) => {
                let completed = component.update(action).is_some();
                if !completed {
                    return Update::none();
                }
                match action {
                    goto::Action::Select => component
                        .selected_code_id()
                        .map(Effect::SelectCode)
                        .map(Update::effect)
                        .unwrap_or_else(Update::none),
                    goto::Action::Quit => Update::effect(Effect::Close),
                    _ => Update::none(),
                }
            }
            (Self::Themes(component), Message::Themes(action)) => {
                let command = component.update(action.clone());
                match action {
                    themes::Action::Preview(theme) => Update::effect(Effect::PreviewTheme(theme)),
                    themes::Action::Select(theme) => Update::effect(Effect::SaveTheme(theme)),
                    themes::Action::Restore(theme) => Update::effect(Effect::RestoreTheme(theme)),
                    _ => Update {
                        command: command.map(|cmd| cmd.map(Message::Themes)),
                        effect: None,
                    },
                }
            }
            (
                Self::FilePicker {
                    picker,
                    quit_on_cancel,
                },
                Message::FilePicker(action),
            ) => {
                let completed = picker.update(action).is_some();
                if !completed {
                    return Update::none();
                }
                match action {
                    file_picker::Action::Select => picker
                        .selected_path()
                        .map(PathBuf::from)
                        .map(Effect::OpenFile)
                        .map(Update::effect)
                        .unwrap_or_else(Update::none),
                    file_picker::Action::Quit if *quit_on_cancel => Update::effect(Effect::Quit),
                    file_picker::Action::Quit => Update::effect(Effect::Close),
                    _ => Update::none(),
                }
            }
            (Self::Dependencies(component), Message::Dependencies(action)) => {
                let command = component.update(action);
                if action == dependencies::Action::Quit {
                    Update::effect(Effect::Close)
                } else {
                    Update {
                        command: command.map(|cmd| cmd.map(Message::Dependencies)),
                        effect: None,
                    }
                }
            }
            _ => Update::none(),
        }
    }

    pub fn apply_search_results(&mut self, results: &[usize]) -> Option<usize> {
        match self {
            Self::Search(component) => component.search(results).copied(),
            _ => None,
        }
    }

    pub fn tick(&mut self, statuses: &HashMap<CodeId, TaskStatus>) {
        match self {
            Self::Goto(component) => component.tick(),
            Self::Dependencies(component) => component.tick(statuses.clone()),
            _ => {}
        }
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        if let Self::Dependencies(component) = self {
            component.set_theme(theme);
        }
    }

    pub fn render(&self, frame: &mut Frame, area: Rect, envs: &envs::EnvVars) {
        match self {
            Self::Confirm(component) => component.render(frame, area),
            Self::Help(component) => component.render(frame, area),
            Self::Envs => envs.render(frame, area),
            Self::Search(_) => {}
            Self::Goto(component) => component.render(frame, area),
            Self::Themes(component) => component.render(frame, area),
            Self::FilePicker { picker, .. } => picker.render(frame, area),
            Self::Dependencies(component) => component.render(frame, area),
        }
    }

    pub fn footer_content(
        &self,
        theme: &Theme,
    ) -> Option<(Line<'static>, Line<'static>, Option<Line<'static>>)> {
        match self {
            Self::Search(component) => Some((
                theme.mode_badge("SEARCH", theme.active),
                component.footer_shortcuts(),
                component.footer_right(),
            )),
            Self::Goto(_) => Some((
                theme.mode_badge("GOTO", theme.active),
                Line::default(),
                None,
            )),
            Self::FilePicker { picker, .. } => Some((
                theme.mode_badge("OPEN", theme.active),
                picker.footer_shortcuts(),
                None,
            )),
            _ => None,
        }
    }
}
