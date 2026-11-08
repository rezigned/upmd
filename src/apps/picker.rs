use keymap::KeyMap;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};

/// Actions shared by the TUI and CLI file pickers.
///
/// Both frontends use the same key bindings so users get consistent behavior
/// whether they open the picker from the TUI (`o` key) or from the CLI
/// (`upmd --cli <directory>` with multiple Markdown files).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, KeyMap)]
pub enum PickerAction {
    /// Delete the last character from the filter query.
    #[key("backspace", help = "delete")]
    Delete,
    /// Open the currently selected file.
    #[key("enter", help = "open")]
    Select,
    /// Move selection to the next match.
    #[key("down", "ctrl-n", help = "next")]
    Next,
    /// Move selection to the previous match.
    #[key("up", "ctrl-p", help = "prev")]
    Prev,
    /// Cancel the picker without selecting a file.
    #[key("esc", "ctrl-c", "ctrl-d", help = "cancel")]
    Quit,
    /// Append a typed character to the filter query.
    #[key("@any")]
    Input(char),
}

/// Shared state for file pickers: the file list, filter query, ranked
/// matches, and current selection index.
///
/// Both the TUI [`FilePicker`](crate::apps::tui::file_picker::FilePicker) and
/// CLI [`CliPicker`](crate::apps::cli::picker::CliPicker) embed this struct and
/// delegate navigation logic to [`PickerState::handle_navigation`], ensuring
/// consistent filter and selection behavior across frontends.
pub struct PickerState {
    pub files: Vec<crate::markdown_files::MarkdownFile>,
    pub matches: Vec<usize>,
    pub query: String,
    pub selected: usize,
    matcher: Matcher,
    /// Reused UTF-32 buffer for non-ASCII display paths.
    scratch: Vec<char>,
}

impl PickerState {
    pub fn new(files: Vec<crate::markdown_files::MarkdownFile>) -> Self {
        let mut state = Self {
            files,
            matches: Vec::new(),
            query: String::new(),
            selected: 0,
            matcher: Matcher::new(Config::DEFAULT.match_paths()),
            scratch: Vec::new(),
        };
        state.rebuild();
        state
    }

    /// Recomputes `matches` from `query` and selects the best match.
    pub fn rebuild(&mut self) {
        self.matches = self.build_matches();
        self.selected = 0;
    }

    /// Returns indices into `files` whose display path fuzzy-matches `query`,
    /// best score first. Equal scores fall back to display order, then file index.
    fn build_matches(&mut self) -> Vec<usize> {
        if self.query.is_empty() {
            return (0..self.files.len()).collect();
        }

        let atom = Atom::new(
            &self.query,
            CaseMatching::Ignore,
            Normalization::Smart,
            AtomKind::Fuzzy,
            false,
        );
        let mut scored = Vec::new();
        for (idx, file) in self.files.iter().enumerate() {
            let haystack = Utf32Str::new(&file.display, &mut self.scratch);
            if let Some(score) = atom.score(haystack, &mut self.matcher) {
                scored.push((idx, score));
            }
        }

        scored.sort_by(|&(a_idx, a_score), &(b_idx, b_score)| {
            b_score
                .cmp(&a_score)
                .then_with(|| self.files[a_idx].display.cmp(&self.files[b_idx].display))
                .then_with(|| a_idx.cmp(&b_idx))
        });
        scored.into_iter().map(|(idx, _)| idx).collect()
    }

    /// Returns the index into `files` of the currently selected match, or
    /// `None` if there are no matches.
    pub fn selected_file_idx(&self) -> Option<usize> {
        self.matches.get(self.selected).copied()
    }

    /// Handles navigation actions (`Input`, `Delete`, `Next`, `Prev`).
    ///
    /// Returns `true` if the action was a navigation action (and state may have
    /// changed), `false` for `Select` and `Quit` which the caller must handle.
    pub fn handle_navigation(&mut self, action: &PickerAction) -> bool {
        match action {
            PickerAction::Input(ch) => {
                if !ch.is_control() {
                    self.query.push(*ch);
                    self.rebuild();
                }
                true
            }
            PickerAction::Delete => {
                if self.query.pop().is_some() {
                    self.rebuild();
                }
                true
            }
            PickerAction::Next => {
                if !self.matches.is_empty() {
                    self.selected = (self.selected + 1).min(self.matches.len().saturating_sub(1));
                }
                true
            }
            PickerAction::Prev => {
                self.selected = self.selected.saturating_sub(1);
                true
            }
            _ => false,
        }
    }
}
