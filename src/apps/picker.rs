use keymap::KeyMap;
use nucleo_matcher::pattern::{Atom, AtomKind, CaseMatching, Normalization};
use nucleo_matcher::{Config, Matcher, Utf32Str};
use unicode_segmentation::UnicodeSegmentation;

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

/// Shared file-picker state: files, query, ranked matches, highlight ranges,
/// and selection.
pub struct PickerState {
    pub files: Vec<crate::markdown_files::MarkdownFile>,
    pub matches: Vec<usize>,
    pub query: String,
    pub selected: usize,
    matcher: Matcher,
    /// Reused UTF-32 buffer for matching display paths.
    scratch: Vec<char>,
    /// Reused match index buffer, cleared per file.
    indices: Vec<u32>,
    /// Matched byte ranges in each file's display path, keyed by file index.
    highlights: Vec<Vec<(usize, usize)>>,
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
            indices: Vec::new(),
            highlights: Vec::new(),
        };
        state.rebuild();
        state
    }

    /// Rebuilds ranked matches and highlight ranges from `query`, resetting the
    /// selection. An empty query keeps discovery order; no matches leave nothing
    /// selected.
    pub fn rebuild(&mut self) {
        self.selected = 0;
        self.highlights.clear();
        self.highlights.resize(self.files.len(), Vec::new());

        if self.query.is_empty() {
            self.matches = (0..self.files.len()).collect();
            return;
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
            self.scratch.clear();
            self.scratch
                .extend(nucleo_matcher::chars::graphemes(&file.display));
            let haystack = Utf32Str::Unicode(&self.scratch);

            self.indices.clear();
            if let Some(score) = atom.indices(haystack, &mut self.matcher, &mut self.indices) {
                self.highlights[idx] = matched_ranges(&file.display, &self.indices);
                scored.push((idx, score));
            }
        }

        scored.sort_by(|&(a_idx, a_score), &(b_idx, b_score)| {
            b_score
                .cmp(&a_score)
                .then_with(|| self.files[a_idx].display.cmp(&self.files[b_idx].display))
                .then_with(|| a_idx.cmp(&b_idx))
        });
        self.matches = scored.into_iter().map(|(idx, _)| idx).collect();
    }

    /// Splits a file's display path into unmatched and matcher-selected parts.
    pub fn highlight_parts(&self, file_idx: usize) -> Vec<(&str, bool)> {
        let file = &self.files[file_idx];
        let ranges = &self.highlights[file_idx];

        // Pre-allocate capacity: up to 2 parts per range + 1 for trailing text
        let mut parts = Vec::with_capacity(ranges.len() * 2 + 1);
        let mut start = 0;

        for &(range_start, range_end) in ranges {
            if start < range_start {
                parts.push((&file.display[start..range_start], false));
            }
            parts.push((&file.display[range_start..range_end], true));
            start = range_end;
        }

        if start < file.display.len() {
            parts.push((&file.display[start..], false));
        }

        parts
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

/// Converts increasing matcher grapheme indices to byte ranges in `display`,
/// merging adjacent matches.
fn matched_ranges(display: &str, indices: &[u32]) -> Vec<(usize, usize)> {
    let graphemes: Vec<(usize, usize)> = display
        .grapheme_indices(true)
        .map(|(start, grapheme)| (start, start + grapheme.len()))
        .collect();

    let mut matched: Vec<(usize, usize)> = Vec::with_capacity(indices.len());
    for &idx in indices {
        let range = graphemes[idx as usize];
        match matched.last_mut() {
            Some(last) if last.1 == range.0 => last.1 = range.1, // Merge contiguous ranges
            _ => matched.push(range),
        }
    }
    matched
}
