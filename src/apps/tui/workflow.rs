use std::{
    collections::{BTreeMap, HashMap},
    path::PathBuf,
};

use upmd_parser::CodeId;

use crate::apps::{
    config,
    task::TaskStatus,
    theme::Theme,
    tui::dependencies::Dependencies,
    workflow::{Workflow, WorkflowTransition},
};

#[derive(Default)]
pub(crate) struct State {
    active: Option<Workflow>,
    graph: Option<Dependencies>,
    graph_visible: bool,
    pending: BTreeMap<CodeId, CapturedState>,
}

#[derive(Default)]
pub(crate) struct CapturedState {
    pub envs: Option<config::Envs>,
    pub cwd: Option<PathBuf>,
}

impl State {
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn start(
        &mut self,
        mut workflow: Workflow,
        graph: Dependencies,
    ) -> Option<(Vec<CodeId>, bool)> {
        let auto_run = workflow.auto_run();
        let batch = workflow.start()?;
        self.set_graph(graph);
        self.active = Some(workflow);
        Some((batch, auto_run))
    }

    pub fn is_active(&self) -> bool {
        self.active.is_some()
    }

    pub fn is_running(&self, id: CodeId) -> bool {
        self.active
            .as_ref()
            .is_some_and(|workflow| workflow.is_running(id))
    }

    pub fn should_execute(&self, id: CodeId, succeeded: bool) -> bool {
        self.active
            .as_ref()
            .is_none_or(|workflow| workflow.should_execute(id, succeeded))
    }

    pub fn advance(&mut self, id: CodeId, exit_code: Option<i32>) -> Option<WorkflowTransition> {
        self.active
            .as_mut()
            .map(|workflow| workflow.advance(id, exit_code))
    }

    pub fn auto_run(&self) -> bool {
        self.active.as_ref().is_some_and(Workflow::auto_run)
    }

    pub fn has_target(&self) -> bool {
        self.active.as_ref().and_then(Workflow::target).is_some()
    }

    pub fn finish(&mut self) {
        self.active = None;
    }

    pub fn set_graph(&mut self, graph: Dependencies) {
        self.graph_visible = graph.has_deps();
        self.graph = Some(graph);
    }

    pub fn has_graph(&self) -> bool {
        self.graph.is_some()
    }

    pub fn toggle_graph(&mut self) {
        if self.graph.is_some() {
            self.graph_visible = !self.graph_visible;
        }
    }

    pub fn visible_graph(&self) -> Option<&Dependencies> {
        self.graph.as_ref().filter(|_| self.graph_visible)
    }

    pub fn tick(&mut self, statuses: HashMap<CodeId, TaskStatus>) {
        if let Some(graph) = &mut self.graph {
            graph.tick(statuses);
        }
    }

    pub fn set_theme(&mut self, theme: &Theme) {
        if let Some(graph) = &mut self.graph {
            graph.set_theme(theme);
        }
    }

    pub fn capture(&mut self, id: CodeId, envs: Option<config::Envs>, cwd: Option<PathBuf>) {
        if envs.is_none() && cwd.is_none() {
            return;
        }

        let state = self.pending.entry(id).or_default();
        if let Some(envs) = envs {
            state.envs = Some(envs);
        }
        if let Some(cwd) = cwd {
            state.cwd = Some(cwd);
        }
    }

    pub fn discard_capture(&mut self, id: CodeId) {
        self.pending.remove(&id);
    }

    pub fn take_captures(&mut self) -> BTreeMap<CodeId, CapturedState> {
        std::mem::take(&mut self.pending)
    }
}
