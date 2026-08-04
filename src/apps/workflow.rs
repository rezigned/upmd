//! Builds and advances workflows for Markdown code blocks.
//!
//! A workflow resolves block dependencies into topologically ordered layers.
//! Blocks in the same layer have no ordering constraints and can run
//! concurrently. A later layer starts only after the current layer finishes.
//!
//! ```text
//!   Declaration        Dependency graph       Execution layers
//!
//! [deps: "B | C"]         B ──┬──→ A          ┌─────┐  ┌─────┐
//!   on block A                │               │ B C │  │  A  │
//!                         C ──┘               └─────┘  └─────┘
//!                                             layer 0  layer 1
//! ```
//!
//! Target workflows run each layer as one concurrent batch. All-block
//! workflows flatten the layers into one-block batches, preserving dependency
//! order and using source order as the tie-breaker.

use std::collections::{HashMap, HashSet, VecDeque};

use upmd_parser::{Code, CodeId, Codes};

/// A block that failed during execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFailure {
    pub block: CodeId,
    pub exit_code: Option<i32>,
}

/// Result of advancing the workflow past a completed block.
#[derive(Debug, PartialEq, Eq)]
pub enum WorkflowTransition {
    /// Next concurrent batch ready to run.
    NextBatch(Vec<CodeId>),
    /// Current batch still in progress.
    Pending,
    /// All blocks have finished.
    Finished { failed: bool },
    /// A failure occurred; remaining batches cancelled.
    Stopped(BlockFailure),
    /// The given block was not tracked by the workflow.
    Untracked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailurePolicy {
    Continue,
    Stop,
}

/// An execution plan that runs blocks in dependency order.
#[derive(Debug)]
pub struct Workflow {
    pending: VecDeque<Vec<CodeId>>,
    running: HashSet<CodeId>,
    blocked: HashSet<CodeId>,
    graph: DependencyGraph,
    failure: Option<BlockFailure>,
    failure_policy: FailurePolicy,
    auto_run: bool,
    skip_succeeded: bool,
}

impl Workflow {
    /// Runs all blocks sequentially, one at a time, preserving source order.
    /// Failed blocks skip their dependents but do not stop other blocks.
    pub fn for_all(codes: &Codes, auto_run: bool) -> Result<Self, String> {
        let graph = DependencyGraph::for_all(codes)?;
        let pending = graph
            .layers()
            .iter()
            .flatten()
            .map(|&id| vec![id])
            .collect();

        Ok(Self::new(
            pending,
            graph,
            FailurePolicy::Continue,
            auto_run,
            true,
        ))
    }

    /// Runs the dependency chain of `target`.
    /// Blocks in the same group run concurrently; groups are sequential.
    /// On failure, remaining groups are cancelled and `Stopped` is returned.
    pub fn for_target(codes: &Codes, target: CodeId) -> Result<Self, String> {
        Self::build_target(codes, target, true)
    }

    /// Runs the dependency chain of `target`, re-executing all prerequisites.
    pub fn for_target_rerun(codes: &Codes, target: CodeId) -> Result<Self, String> {
        Self::build_target(codes, target, false)
    }

    fn build_target(codes: &Codes, target: CodeId, skip_succeeded: bool) -> Result<Self, String> {
        let graph = DependencyGraph::for_target(codes, target)?;
        let pending = graph.layers().iter().cloned().collect();
        Ok(Self::new(
            pending,
            graph,
            FailurePolicy::Stop,
            true,
            skip_succeeded,
        ))
    }

    fn new(
        pending: VecDeque<Vec<CodeId>>,
        graph: DependencyGraph,
        failure_policy: FailurePolicy,
        auto_run: bool,
        skip_succeeded: bool,
    ) -> Self {
        Self {
            pending,
            running: HashSet::new(),
            blocked: HashSet::new(),
            graph,
            failure: None,
            auto_run,
            failure_policy,
            skip_succeeded,
        }
    }

    /// Returns the first batch and marks it running.
    ///
    /// Returns `None` if already started or no work remains.
    pub fn start(&mut self) -> Option<Vec<CodeId>> {
        if self.running.is_empty() {
            self.take_next_batch()
        } else {
            None
        }
    }

    /// Records completion of `block` and returns the next action.
    ///
    /// Call once per block when it finishes.  The caller provides `exit_code`:
    /// `Some(0)` for success, `Some(n)` for failure, or `None` if the process
    /// was externally killed.
    pub fn advance(&mut self, block: CodeId, exit_code: Option<i32>) -> WorkflowTransition {
        if !self.running.remove(&block) {
            return WorkflowTransition::Untracked;
        }

        if exit_code != Some(0) {
            self.failure
                .get_or_insert(BlockFailure { block, exit_code });
            if self.failure_policy == FailurePolicy::Continue {
                self.block_dependents(block);
            }
        }

        if !self.running.is_empty() {
            return WorkflowTransition::Pending;
        }

        if self.failure_policy == FailurePolicy::Stop {
            if let Some(failure) = self.failure {
                self.pending.clear();
                return WorkflowTransition::Stopped(failure);
            }
        }

        self.take_next_batch().map_or_else(
            || WorkflowTransition::Finished {
                failed: self.failure.is_some(),
            },
            WorkflowTransition::NextBatch,
        )
    }

    /// The target block this workflow is working toward, if any.
    pub fn target(&self) -> Option<CodeId> {
        self.graph.target()
    }

    #[allow(dead_code)]
    pub fn graph(&self) -> &DependencyGraph {
        &self.graph
    }

    pub fn auto_run(&self) -> bool {
        self.auto_run
    }

    /// Whether `block` should execute given its previous result.
    ///
    /// Standard execution skips successful non-target prerequisites.
    /// Rerun execution runs the entire dependency chain.
    pub fn should_execute(&self, block: CodeId, succeeded: bool) -> bool {
        !self.skip_succeeded || !succeeded || self.graph.target() == Some(block)
    }

    pub fn is_running(&self, block: CodeId) -> bool {
        self.running.contains(&block)
    }

    /// Pops the next non-empty batch that hasn't been blocked.
    ///
    /// Skips batches where all blocks were blocked by a failure.
    fn take_next_batch(&mut self) -> Option<Vec<CodeId>> {
        while let Some(mut batch) = self.pending.pop_front() {
            batch.retain(|id| !self.blocked.contains(id));
            if !batch.is_empty() {
                self.running.extend(batch.iter().copied());
                return Some(batch);
            }
        }
        None
    }

    /// Marks all transitive dependents of `failed` as blocked.
    ///
    /// A blocked block will be skipped when its batch reaches the front of the
    /// queue.  If it is the last non-blocked block in its batch the entire batch
    /// is skipped.
    fn block_dependents(&mut self, failed: CodeId) {
        let mut stack = vec![failed];
        while let Some(block) = stack.pop() {
            if let Some(dependents) = self.graph.dependents.get(&block) {
                for &dependent in dependents {
                    if self.blocked.insert(dependent) {
                        stack.push(dependent);
                    }
                }
            }
        }
    }
}

/// Topologically sorted dependency layers and their edge map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyGraph {
    layers: Vec<Vec<CodeId>>,
    dependents: HashMap<CodeId, Vec<CodeId>>,
    target: Option<CodeId>,
}

impl DependencyGraph {
    pub fn for_all(codes: &Codes) -> Result<Self, String> {
        let ids = codes.iter().map(|code| code.id).collect::<HashSet<_>>();
        build_dependency_graph(codes, &ids, None)
    }

    pub fn for_target(codes: &Codes, target: CodeId) -> Result<Self, String> {
        let ids = collect_dependencies(codes, target)?;
        build_dependency_graph(codes, &ids, Some(target))
    }

    pub fn layers(&self) -> &[Vec<CodeId>] {
        &self.layers
    }

    pub fn has_deps(&self) -> bool {
        !self.dependents.is_empty()
    }

    pub fn target(&self) -> Option<CodeId> {
        self.target
    }
}

/// Walks the dependency tree of `target` and returns all reachable block IDs.
fn collect_dependencies(codes: &Codes, target: CodeId) -> Result<HashSet<CodeId>, String> {
    if codes.by_id(target).is_none() {
        return Err(format!("target block {target} not found"));
    }

    let mut selected = HashSet::from([target]);
    let mut stack = vec![target];
    while let Some(id) = stack.pop() {
        let code = codes
            .by_id(id)
            .ok_or_else(|| format!("block {id} not found"))?;
        for dependency in resolve_dependencies_for(codes, code)?.into_iter().flatten() {
            if selected.insert(dependency) {
                stack.push(dependency);
            }
        }
    }
    Ok(selected)
}

/// Builds a `DependencyGraph` via topological sort from a selected subset of blocks.
fn build_dependency_graph(
    codes: &Codes,
    selected: &HashSet<CodeId>,
    target: Option<CodeId>,
) -> Result<DependencyGraph, String> {
    let positions = codes
        .iter()
        .enumerate()
        .map(|(position, code)| (code.id, position))
        .collect::<HashMap<_, _>>();
    let mut in_degree = selected
        .iter()
        .map(|&id| (id, 0usize))
        .collect::<HashMap<_, _>>();
    let mut edges = HashMap::<CodeId, HashSet<CodeId>>::new();

    for code in codes.iter().filter(|code| selected.contains(&code.id)) {
        let groups = resolve_dependencies_for(codes, code)?;
        for &dependency in groups.iter().flatten() {
            add_edge(dependency, code.id, &mut edges, &mut in_degree);
        }
        for adjacent in groups.windows(2) {
            for &before in &adjacent[0] {
                for &after in &adjacent[1] {
                    add_edge(before, after, &mut edges, &mut in_degree);
                }
            }
        }
    }

    let mut layers = Vec::new();
    while !in_degree.is_empty() {
        let mut ready = in_degree
            .iter()
            .filter_map(|(&id, &degree)| (degree == 0).then_some(id))
            .collect::<Vec<_>>();
        ready.sort_by_key(|id| positions.get(id).copied().unwrap_or(usize::MAX));
        if ready.is_empty() {
            let mut cycle = in_degree.keys().copied().collect::<Vec<_>>();
            cycle.sort_by_key(|id| positions.get(id).copied().unwrap_or(usize::MAX));
            return Err(format!(
                "dependency cycle involving blocks {}",
                cycle
                    .iter()
                    .map(CodeId::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }

        for id in &ready {
            in_degree.remove(id);
            if let Some(dependents) = edges.get(id) {
                for dependent in dependents {
                    if let Some(degree) = in_degree.get_mut(dependent) {
                        *degree -= 1;
                    }
                }
            }
        }
        layers.push(ready);
    }

    let dependents = edges
        .into_iter()
        .map(|(id, dependents)| {
            let mut dependents = dependents.into_iter().collect::<Vec<_>>();
            dependents.sort_by_key(|id| positions.get(id).copied().unwrap_or(usize::MAX));
            (id, dependents)
        })
        .collect();

    Ok(DependencyGraph {
        layers,
        dependents,
        target,
    })
}

/// Resolves a block's dependency names and numeric IDs while preserving groups.
fn resolve_dependencies_for(codes: &Codes, code: &Code) -> Result<Vec<Vec<CodeId>>, String> {
    let groups = code
        .deps
        .groups()
        .map_err(|error| format!("block {}: {error}", code.id))?;
    let mut seen = HashSet::new();

    groups
        .iter()
        .map(|group| {
            group
                .iter()
                .map(|dependency| {
                    let matches = codes.resolve(dependency);
                    let id = match matches.as_slice() {
                        [] => {
                            return Err(format!(
                                "block {}: dependency {dependency:?} not found in document",
                                code.id
                            ))
                        }
                        [id] => *id,
                        _ => {
                            return Err(format!(
                                "block {}: dependency {dependency:?} is ambiguous ({} matches)",
                                code.id,
                                matches.len()
                            ))
                        }
                    };

                    if !seen.insert(id) {
                        return Err(format!(
                            "block {}: dependency {dependency:?} refers to block {id}, which is already listed",
                            code.id
                        ));
                    }
                    Ok(id)
                })
                .collect()
        })
        .collect()
}

fn add_edge(
    from: CodeId,
    to: CodeId,
    edges: &mut HashMap<CodeId, HashSet<CodeId>>,
    in_degree: &mut HashMap<CodeId, usize>,
) {
    if edges.entry(from).or_default().insert(to) {
        *in_degree.entry(to).or_default() += 1;
    }
}

#[cfg(test)]
mod tests {
    use upmd_parser::options;

    use super::*;

    fn code(id: CodeId, name: &str, dependencies: Option<&str>) -> Code {
        let mut attrs = Vec::new();
        if !name.is_empty() {
            attrs.push(format!("name:{name}"));
        }
        if let Some(dependencies) = dependencies {
            attrs.push(format!(r#"deps:"{dependencies}""#));
        }
        let info = if attrs.is_empty() {
            "sh".to_string()
        } else {
            format!("sh [{}]", attrs.join(", "))
        };
        Code::new(id, String::new(), options::parse(&info))
    }

    fn codes(items: Vec<Code>) -> Codes {
        Codes::try_from(items).unwrap()
    }

    #[test]
    fn dependency_groups_resolve_names_and_numeric_ids() {
        let codes = codes(vec![
            code(1, "setup", None),
            code(2, "build", None),
            code(3, "test", None),
            code(4, "", None),
            code(5, "", None),
        ]);

        for (dependencies, expected) in [
            ("setup", vec![vec![1]]),
            ("test", vec![vec![3]]),
            ("4, 5", vec![vec![4], vec![5]]),
            ("build", vec![vec![2]]),
        ] {
            let target = code(6, "target", Some(dependencies));
            assert_eq!(resolve_dependencies_for(&codes, &target).unwrap(), expected);
        }

        let target = code(6, "target", Some("missing"));
        assert!(resolve_dependencies_for(&codes, &target)
            .unwrap_err()
            .starts_with("block 6:"));
    }

    #[test]
    fn target_batches_preserve_sequential_and_parallel_groups() {
        let codes = codes(vec![
            code(1, "setup", None),
            code(2, "build", None),
            code(3, "lint", None),
            code(4, "test", None),
            code(5, "target", Some("setup, build | lint, test")),
        ]);

        let mut workflow = Workflow::for_target(&codes, 5).unwrap();
        assert_eq!(workflow.start(), Some(vec![1]));
        assert_eq!(
            workflow.advance(1, Some(0)),
            WorkflowTransition::NextBatch(vec![2, 3])
        );
        assert_eq!(workflow.advance(2, Some(0)), WorkflowTransition::Pending);
        assert_eq!(
            workflow.advance(3, Some(0)),
            WorkflowTransition::NextBatch(vec![4])
        );
        assert_eq!(
            workflow.advance(4, Some(0)),
            WorkflowTransition::NextBatch(vec![5])
        );
        assert_eq!(
            workflow.advance(5, Some(0)),
            WorkflowTransition::Finished { failed: false }
        );
    }

    #[test]
    fn target_rerun_executes_successful_dependencies_again() {
        let codes = codes(vec![
            code(1, "setup", None),
            code(2, "target", Some("setup")),
        ]);

        let normal = Workflow::for_target(&codes, 2).unwrap();
        assert!(!normal.should_execute(1, true));
        assert!(normal.should_execute(2, true));

        let rerun = Workflow::for_target_rerun(&codes, 2).unwrap();
        assert!(rerun.should_execute(1, true));
        assert!(rerun.should_execute(2, true));
    }

    #[test]
    fn nested_dependencies_are_ordered_before_the_target() {
        let codes = codes(vec![
            code(1, "target", Some("build")),
            code(2, "build", Some("setup")),
            code(3, "setup", None),
        ]);

        let mut workflow = Workflow::for_target(&codes, 1).unwrap();
        assert_eq!(workflow.start(), Some(vec![3]));
        assert_eq!(
            workflow.advance(3, Some(0)),
            WorkflowTransition::NextBatch(vec![2])
        );
        assert_eq!(
            workflow.advance(2, Some(0)),
            WorkflowTransition::NextBatch(vec![1])
        );
        assert_eq!(
            workflow.advance(1, Some(0)),
            WorkflowTransition::Finished { failed: false }
        );
    }

    #[test]
    fn all_mode_is_sequential_and_stably_sorted() {
        let codes = codes(vec![
            code(1, "target", Some("setup")),
            code(2, "unrelated", None),
            code(3, "setup", None),
        ]);

        let mut workflow = Workflow::for_all(&codes, false).unwrap();
        assert_eq!(workflow.start(), Some(vec![2]));
        assert_eq!(
            workflow.advance(2, Some(0)),
            WorkflowTransition::NextBatch(vec![3])
        );
        assert_eq!(
            workflow.advance(3, Some(0)),
            WorkflowTransition::NextBatch(vec![1])
        );
        assert_eq!(
            workflow.advance(1, Some(0)),
            WorkflowTransition::Finished { failed: false }
        );
    }

    #[test]
    fn graph_uses_workflow_validation() {
        let codes = codes(vec![
            code(1, "same", None),
            code(2, "same", None),
            code(3, "target", Some("same")),
        ]);
        assert!(DependencyGraph::for_target(&codes, 3)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn graph_layers_contain_each_block_once() {
        let codes = codes(vec![
            code(1, "a", None),
            code(2, "b", Some("a")),
            code(3, "target", Some("a, b")),
        ]);
        let graph = DependencyGraph::for_target(&codes, 3).unwrap();
        assert_eq!(graph.layers(), &[vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn all_mode_retains_its_complete_graph() {
        let codes = codes(vec![
            code(1, "a", None),
            code(2, "b", Some("a")),
            code(3, "independent", None),
        ]);
        let workflow = Workflow::for_all(&codes, false).unwrap();
        assert_eq!(workflow.graph().layers(), &[vec![1, 3], vec![2]]);
        assert_eq!(workflow.graph().target(), None);
    }

    #[test]
    fn cycles_and_ambiguous_names_are_rejected() {
        let cycle = codes(vec![code(1, "a", Some("b")), code(2, "b", Some("a"))]);
        assert!(Workflow::for_all(&cycle, false)
            .unwrap_err()
            .contains("cycle"));

        let ambiguous = codes(vec![
            code(1, "same", None),
            code(2, "same", None),
            code(3, "target", Some("same")),
        ]);
        assert!(Workflow::for_target(&ambiguous, 3)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn target_validation_ignores_unrelated_blocks() {
        let codes = codes(vec![
            code(1, "target", None),
            code(2, "broken", Some("missing")),
        ]);
        assert!(Workflow::for_target(&codes, 1).is_ok());
        assert!(Workflow::for_all(&codes, false).is_err());
    }

    #[test]
    fn target_failure_waits_for_its_parallel_batch() {
        let codes = codes(vec![
            code(1, "a", None),
            code(2, "b", None),
            code(3, "target", Some("a | b")),
        ]);
        let mut workflow = Workflow::for_target(&codes, 3).unwrap();

        assert_eq!(workflow.start(), Some(vec![1, 2]));
        assert_eq!(workflow.advance(1, Some(7)), WorkflowTransition::Pending);
        assert_eq!(
            workflow.advance(2, Some(0)),
            WorkflowTransition::Stopped(BlockFailure {
                block: 1,
                exit_code: Some(7)
            })
        );
    }

    #[test]
    fn all_mode_skips_failed_dependents_but_runs_other_blocks() {
        let codes = codes(vec![
            code(1, "fail", None),
            code(2, "dependent", Some("fail")),
            code(3, "independent", None),
        ]);
        let mut workflow = Workflow::for_all(&codes, true).unwrap();

        assert_eq!(workflow.start(), Some(vec![1]));
        assert_eq!(
            workflow.advance(1, Some(1)),
            WorkflowTransition::NextBatch(vec![3])
        );
        assert_eq!(
            workflow.advance(3, Some(0)),
            WorkflowTransition::Finished { failed: true }
        );
    }
}
