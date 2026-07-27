use std::{
    cell::{Cell, RefCell},
    collections::VecDeque,
};

use upmd_parser::CodeId;

use crate::apps::workflow::WorkflowTransition;

/// Output lifecycle for workflow batches in the CLI frontend.
///
/// The workflow remains the source of batch ordering. This state records only
/// blocks that actually execute, queues completed batches for deterministic
/// rendering, and tracks the transient terminal dashboard height.
pub struct BatchOutput {
    terminal: bool,
    scheduled: Vec<CodeId>,
    visible: Vec<CodeId>,
    pending: RefCell<VecDeque<Vec<CodeId>>>,
    previous_lines: Cell<u16>,
}

impl BatchOutput {
    pub fn new() -> Self {
        Self {
            // `App::run` replaces this with the runtime's stdout capability.
            // Keeping terminal rendering as the construction default makes
            // component-level rendering deterministic before the runtime starts.
            terminal: true,
            scheduled: Vec::new(),
            visible: Vec::new(),
            pending: RefCell::new(VecDeque::new()),
            previous_lines: Cell::new(0),
        }
    }

    pub fn set_terminal(&mut self, terminal: bool) {
        self.terminal = terminal;
    }

    pub fn is_terminal(&self) -> bool {
        self.terminal
    }

    pub fn select(&mut self, batch: &[CodeId]) {
        self.scheduled.clear();
        self.scheduled.extend_from_slice(batch);
        self.visible.clear();
    }

    pub fn track(&mut self, id: CodeId) {
        if self.scheduled.contains(&id) && !self.visible.contains(&id) {
            self.visible.push(id);
        }
    }

    pub fn complete(&mut self, result: &WorkflowTransition) {
        if matches!(
            result,
            WorkflowTransition::Pending | WorkflowTransition::Untracked
        ) {
            return;
        }
        self.scheduled.clear();
        if !self.visible.is_empty() {
            self.pending
                .borrow_mut()
                .push_back(std::mem::take(&mut self.visible));
        }
    }

    pub fn visible(&self) -> &[CodeId] {
        &self.visible
    }

    pub fn focus_next(&self, selected: CodeId) -> Option<CodeId> {
        let index = self.visible.iter().position(|&id| id == selected)?;
        self.visible.get((index + 1) % self.visible.len()).copied()
    }

    pub fn take_pending(&self) -> VecDeque<Vec<CodeId>> {
        std::mem::take(&mut *self.pending.borrow_mut())
    }

    pub fn previous_lines(&self) -> u16 {
        self.previous_lines.get()
    }

    pub fn set_previous_lines(&self, lines: u16) {
        self.previous_lines.set(lines);
    }

    pub fn reset(&mut self) {
        self.scheduled.clear();
        self.visible.clear();
        self.pending.borrow_mut().clear();
        self.previous_lines.set(0);
    }
}

impl Default for BatchOutput {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_batch_preserves_workflow_order() {
        let mut output = BatchOutput::new();
        output.select(&[1, 2]);
        output.track(1);
        output.track(2);

        output.complete(&WorkflowTransition::Pending);
        assert!(output.take_pending().is_empty());

        output.complete(&WorkflowTransition::NextBatch(vec![3]));
        assert_eq!(output.take_pending(), VecDeque::from([vec![1, 2]]));
    }

    #[test]
    fn focus_cycles_only_through_executed_blocks() {
        let mut output = BatchOutput::new();
        output.select(&[1, 2, 3]);
        output.track(1);
        output.track(3);

        assert_eq!(output.focus_next(1), Some(3));
        assert_eq!(output.focus_next(3), Some(1));
        assert_eq!(output.focus_next(2), None);
    }
}
