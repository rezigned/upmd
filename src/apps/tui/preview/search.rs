use std::cell::RefCell;

use crate::apps::tui::markdown::ViewLine;

use super::visual_lines::VisualLine;

/// Search state for the preview pane.
///
/// Owns the current search term and a cache of lower-cased searchable text for
/// each logical line so that navigating between matches does not re-allocate on
/// every keystroke.
pub struct PreviewSearch {
    term: Option<String>,
    logical_texts: RefCell<Vec<String>>,
}

impl Default for PreviewSearch {
    fn default() -> Self {
        Self::new()
    }
}

impl PreviewSearch {
    pub fn new() -> Self {
        Self {
            term: None,
            logical_texts: RefCell::new(vec![]),
        }
    }

    pub fn set_term(&mut self, term: &str) {
        self.term = if term.is_empty() {
            None
        } else {
            Some(term.to_string())
        };
    }

    pub fn rebuild_texts(&self, logical_lines: &[ViewLine]) {
        *self.logical_texts.borrow_mut() = logical_lines
            .iter()
            .map(|ll| ll.text_content().to_lowercase())
            .collect();
    }

    pub fn matches(&self, visual_lines: &[VisualLine]) -> Vec<usize> {
        let term_lower = match &self.term {
            Some(term) => term.to_lowercase(),
            None => return vec![],
        };
        let texts = self.logical_texts.borrow();
        visual_lines
            .iter()
            .enumerate()
            .filter(|(_, l)| {
                texts
                    .get(l.logical_idx)
                    .is_some_and(|t| t.contains(&term_lower))
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn term(&self) -> Option<&str> {
        self.term.as_deref()
    }
}
