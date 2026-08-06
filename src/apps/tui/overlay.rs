use std::{collections::HashMap, path::PathBuf};

use crossterm::event::Event;
use ratatui::{layout::Rect, text::Line, Frame};
use upmd_parser::CodeId;
use upmd_runtime::{
    runtimes::tui::{Input, Output},
    Component, Effect,
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

pub(crate) enum Outcome {
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

    pub fn update(
        &mut self,
        message: Message,
        envs: &mut envs::EnvVars,
    ) -> Option<Effect<Message, Outcome>> {
        match (self, message) {
            (Self::Confirm(component), Message::Confirm(action)) => {
                let (command, outcome) = Effect::into_parts(component.update(action));
                let command = command.map(|command| command.map(Message::Confirm));
                match outcome {
                    Some(confirm::Outcome::Confirmed(confirm::ConfirmAction::Quit)) => {
                        upmd_runtime::effect!(outcome: Outcome::Quit)
                    }
                    Some(confirm::Outcome::Confirmed(confirm::ConfirmAction::ReloadFile)) => {
                        upmd_runtime::effect!(outcome: Outcome::Reload)
                    }
                    Some(confirm::Outcome::Confirmed(confirm::ConfirmAction::ReRun(id))) => {
                        upmd_runtime::effect!(outcome: Outcome::Rerun(id))
                    }
                    Some(confirm::Outcome::Cancelled) => {
                        upmd_runtime::effect!(outcome: Outcome::Close)
                    }
                    None => Effect::command(command),
                }
            }
            (Self::Help(component), Message::Help(action)) => {
                let (command, outcome) = Effect::into_parts(component.update(action));
                match outcome {
                    Some(help::Outcome::Closed) => upmd_runtime::effect!(outcome: Outcome::Close),
                    None => Effect::command(command.map(|command| command.map(Message::Help))),
                }
            }
            (Self::Envs, Message::Envs(action)) => {
                let (command, outcome) = Effect::into_parts(envs.update(action));
                let command = command.map(|command| command.map(Message::Envs));
                let outcome = outcome.map(|envs::Outcome::Closed| Outcome::Close);
                Effect::from_parts(command, outcome)
            }
            (Self::Search(component), Message::Search(action)) => {
                let (command, outcome) = Effect::into_parts(component.update(action));
                let command = command.map(|command| command.map(Message::Search));
                match outcome {
                    Some(search::Outcome::Selected) => {
                        upmd_runtime::effect!(outcome: Outcome::Close)
                    }
                    Some(search::Outcome::Cancelled) => {
                        upmd_runtime::effect!(outcome: Outcome::ClearSearch)
                    }
                    None => Effect::from_parts(
                        command,
                        Some(Outcome::RefreshSearch(component.term().to_string())),
                    ),
                }
            }
            (Self::Goto(component), Message::Goto(action)) => {
                let (command, outcome) = Effect::into_parts(component.update(action));
                let command = command.map(|command| command.map(Message::Goto));
                let outcome = outcome.map(|outcome| match outcome {
                    goto::Outcome::Selected(id) => Outcome::SelectCode(id),
                    goto::Outcome::Cancelled => Outcome::Close,
                });
                Effect::from_parts(command, outcome)
            }
            (Self::Themes(component), Message::Themes(action)) => {
                let (command, outcome) = Effect::into_parts(component.update(action));
                let command = command.map(|command| command.map(Message::Themes));
                let outcome = outcome.map(|outcome| match outcome {
                    themes::Outcome::Previewed(theme) => Outcome::PreviewTheme(theme),
                    themes::Outcome::Selected(theme) => Outcome::SaveTheme(theme),
                    themes::Outcome::Restored(theme) => Outcome::RestoreTheme(theme),
                });
                Effect::from_parts(command, outcome)
            }
            (
                Self::FilePicker {
                    picker,
                    quit_on_cancel,
                },
                Message::FilePicker(action),
            ) => {
                let (command, outcome) = Effect::into_parts(picker.update(action));
                let command = command.map(|command| command.map(Message::FilePicker));
                let outcome = outcome.map(|outcome| match outcome {
                    file_picker::Outcome::Selected(path) => Outcome::OpenFile(path),
                    file_picker::Outcome::Cancelled if *quit_on_cancel => Outcome::Quit,
                    file_picker::Outcome::Cancelled => Outcome::Close,
                });
                Effect::from_parts(command, outcome)
            }
            (Self::Dependencies(component), Message::Dependencies(action)) => {
                let (command, outcome) = Effect::into_parts(component.update(action));
                let command = command.map(|command| command.map(Message::Dependencies));
                let outcome = outcome.map(|dependencies::Outcome::Closed| Outcome::Close);
                Effect::from_parts(command, outcome)
            }
            _ => None,
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
