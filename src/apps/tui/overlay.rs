use std::{collections::HashMap, path::PathBuf};

use crossterm::event::Event;
use ratatui::{layout::Rect, text::Line, Frame};
use upmd_parser::CodeId;
use upmd_runtime::{
    effect,
    runtimes::tui::{Input, Output},
    Component, Effect, EffectExt,
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
pub(crate) enum Action {
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
    pub fn action(&self, event: Event, envs: &envs::EnvVars) -> Option<Action> {
        match self {
            Self::Confirm(component) => component.action(event).map(Action::Confirm),
            Self::Help(component) => component.action(event).map(Action::Help),
            Self::Envs => envs.action(event).map(Action::Envs),
            Self::Search(component) => component.action(event).map(Action::Search),
            Self::Goto(component) => component.action(event).map(Action::Goto),
            Self::Themes(component) => component.action(event).map(Action::Themes),
            Self::FilePicker { picker, .. } => picker.action(event).map(Action::FilePicker),
            Self::Dependencies(component) => component.action(event).map(Action::Dependencies),
        }
    }

    pub fn update(
        &mut self,
        message: Action,
        envs: &mut envs::EnvVars,
    ) -> Option<Effect<Action, Outcome>> {
        match (self, message) {
            (Self::Confirm(component), Action::Confirm(action)) => {
                let (command, outcome) = component.update(action).into_parts();
                let command = command.map(|command| command.map(Action::Confirm));
                match outcome {
                    Some(confirm::Outcome::Confirmed(confirm::ConfirmAction::Quit)) => {
                        effect!(outcome: Outcome::Quit)
                    }
                    Some(confirm::Outcome::Confirmed(confirm::ConfirmAction::ReloadFile)) => {
                        effect!(outcome: Outcome::Reload)
                    }
                    Some(confirm::Outcome::Confirmed(confirm::ConfirmAction::ReRun(id))) => {
                        effect!(outcome: Outcome::Rerun(id))
                    }
                    Some(confirm::Outcome::Cancelled) => {
                        effect!(outcome: Outcome::Close)
                    }
                    None => command.map(Effect::Command),
                }
            }
            (Self::Help(component), Action::Help(action)) => {
                let (command, outcome) = component.update(action).into_parts();
                match outcome {
                    Some(help::Outcome::Closed) => effect!(outcome: Outcome::Close),
                    None => command
                        .map(|command| command.map(Action::Help))
                        .map(Effect::Command),
                }
            }
            (Self::Envs, Action::Envs(action)) => {
                let (command, outcome) = envs.update(action).into_parts();
                let command = command.map(|command| command.map(Action::Envs));
                let outcome = outcome.map(|envs::Outcome::Closed| Outcome::Close);
                Effect::from_parts(command, outcome)
            }
            (Self::Search(component), Action::Search(action)) => {
                let (command, outcome) = component.update(action).into_parts();
                let command = command.map(|command| command.map(Action::Search));
                match outcome {
                    Some(search::Outcome::Selected) => {
                        effect!(outcome: Outcome::Close)
                    }
                    Some(search::Outcome::Cancelled) => {
                        effect!(outcome: Outcome::ClearSearch)
                    }
                    None => Effect::from_parts(
                        command,
                        Some(Outcome::RefreshSearch(component.term().to_string())),
                    ),
                }
            }
            (Self::Goto(component), Action::Goto(action)) => {
                let (command, outcome) = component.update(action).into_parts();
                let command = command.map(|command| command.map(Action::Goto));
                let outcome = outcome.map(|outcome| match outcome {
                    goto::Outcome::Selected(id) => Outcome::SelectCode(id),
                    goto::Outcome::Cancelled => Outcome::Close,
                });
                Effect::from_parts(command, outcome)
            }
            (Self::Themes(component), Action::Themes(action)) => {
                let (command, outcome) = component.update(action).into_parts();
                let command = command.map(|command| command.map(Action::Themes));
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
                Action::FilePicker(action),
            ) => {
                let (command, outcome) = picker.update(action).into_parts();
                let command = command.map(|command| command.map(Action::FilePicker));
                let outcome = outcome.map(|outcome| match outcome {
                    file_picker::Outcome::Selected(path) => Outcome::OpenFile(path),
                    file_picker::Outcome::Cancelled if *quit_on_cancel => Outcome::Quit,
                    file_picker::Outcome::Cancelled => Outcome::Close,
                });
                Effect::from_parts(command, outcome)
            }
            (Self::Dependencies(component), Action::Dependencies(action)) => {
                let (command, outcome) = component.update(action).into_parts();
                let command = command.map(|command| command.map(Action::Dependencies));
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
