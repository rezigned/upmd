use std::collections::{HashMap, HashSet, VecDeque};

use upmd_parser::{resolve_dependencies, Code, CodeId};

/// Tracks a block that failed during execution.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFailure {
    pub block: CodeId,
    pub exit_code: Option<i32>,
}

/// Result of advancing the scheduler past a completed block.
#[derive(Debug, PartialEq, Eq)]
pub enum AdvanceResult {
    /// The next batch of blocks to run concurrently.
    NextBatch(Vec<CodeId>),
    /// Blocks are still running in the current batch.
    Pending,
    /// All blocks have finished.
    Finished { failed: bool },
    /// A failure occurred and policy is Stop; no more batches.
    Stopped(BlockFailure),
    /// The given block was not being tracked by the scheduler.
    Untracked,
}

/// Whether the scheduler stops or continues after a failure.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailurePolicy {
    Continue,
    Stop,
}

/// Drives block execution in dependency order.
///
/// Produces batches of concurrent blocks.  A batch is a group of blocks whose
/// dependencies are all satisfied and that have no dependency on each other.
#[derive(Debug)]
pub struct Scheduler {
    pending: VecDeque<Vec<CodeId>>,
    running: HashSet<CodeId>,
    blocked: HashSet<CodeId>,
    graph: DependencyGraph,
    failure: Option<BlockFailure>,
    failure_policy: FailurePolicy,
    auto_run: bool,
    skip_succeeded: bool,
}

impl Scheduler {
    /// Creates a scheduler that runs _all_ blocks in order.
    ///
    /// Blocks with no dependencies run first, one at a time, preserving source
    /// order.  Failed blocks skip their dependents but do not stop other blocks.
    /// Set `auto_run` to start execution immediately after creation.
    pub fn for_all(codes: &[Code], auto_run: bool) -> Result<Self, String> {
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

    /// Creates a scheduler that runs only the dependency chain of `target`.
    ///
    /// Blocks are grouped into batches: blocks in the same group run
    /// concurrently, batches are sequential.  On failure, remaining batches are
    /// cancelled and `Stopped` is returned.
    pub fn for_target(codes: &[Code], target: CodeId) -> Result<Self, String> {
        Self::build_target(codes, target, true)
    }

    /// Creates a target scheduler that re-executes previously successful dependencies.
    pub fn for_target_rerun(codes: &[Code], target: CodeId) -> Result<Self, String> {
        Self::build_target(codes, target, false)
    }

    fn build_target(codes: &[Code], target: CodeId, skip_succeeded: bool) -> Result<Self, String> {
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
    pub fn advance(&mut self, block: CodeId, exit_code: Option<i32>) -> AdvanceResult {
        if !self.running.remove(&block) {
            return AdvanceResult::Untracked;
        }

        if exit_code != Some(0) {
            self.failure
                .get_or_insert(BlockFailure { block, exit_code });
            if self.failure_policy == FailurePolicy::Continue {
                self.block_dependents(block);
            }
        }

        if !self.running.is_empty() {
            return AdvanceResult::Pending;
        }

        if self.failure_policy == FailurePolicy::Stop {
            if let Some(failure) = self.failure {
                self.pending.clear();
                return AdvanceResult::Stopped(failure);
            }
        }

        self.take_next_batch().map_or_else(
            || AdvanceResult::Finished {
                failed: self.failure.is_some(),
            },
            AdvanceResult::NextBatch,
        )
    }

    /// The target block this schedule is working toward, if any.
    pub fn target(&self) -> Option<CodeId> {
        self.graph.target()
    }

    pub fn graph(&self) -> &DependencyGraph {
        &self.graph
    }

    /// Whether the scheduler should start running immediately.
    pub fn auto_run(&self) -> bool {
        self.auto_run
    }

    /// Whether `block` should execute given its previous result.
    ///
    /// Normal plans reuse successful prerequisites while always executing the
    /// target. Explicit rerun plans execute the entire dependency chain.
    pub fn should_execute(&self, block: CodeId, succeeded: bool) -> bool {
        !self.skip_succeeded || !succeeded || self.graph.target() == Some(block)
    }

    /// Whether `block` is currently executing.
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

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DependencyGraph {
    layers: Vec<Vec<CodeId>>,
    dependents: HashMap<CodeId, Vec<CodeId>>,
    target: Option<CodeId>,
}

impl DependencyGraph {
    pub fn for_all(codes: &[Code]) -> Result<Self, String> {
        let ids = codes.iter().map(|code| code.id).collect::<HashSet<_>>();
        build_plan(codes, &ids, None)
    }

    pub fn for_target(codes: &[Code], target: CodeId) -> Result<Self, String> {
        let ids = collect_dependencies(codes, target)?;
        build_plan(codes, &ids, Some(target))
    }

    pub fn layers(&self) -> &[Vec<CodeId>] {
        &self.layers
    }

    pub fn target(&self) -> Option<CodeId> {
        self.target
    }
}

/// Walks the dependency tree of `target` and returns all reachable block IDs.
fn collect_dependencies(codes: &[Code], target: CodeId) -> Result<HashSet<CodeId>, String> {
    if !codes.iter().any(|code| code.id == target) {
        return Err(format!("target block {target} not found"));
    }

    let mut selected = HashSet::from([target]);
    let mut stack = vec![target];
    while let Some(id) = stack.pop() {
        let code = codes
            .iter()
            .find(|code| code.id == id)
            .ok_or_else(|| format!("block {id} not found"))?;
        for dependency in dependencies_for(codes, code)?.into_iter().flatten() {
            if selected.insert(dependency) {
                stack.push(dependency);
            }
        }
    }
    Ok(selected)
}

/// Builds a topological layer plan from a selected subset of blocks.
///
/// Returns layers (batches of concurrent blocks) and the dependency edges for
/// dependent-blocking on failure.  Errors on cycles.
fn build_plan(
    codes: &[Code],
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
        let groups = dependencies_for(codes, code)?;
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

/// Resolves the dependency names of `code` into block IDs.
fn dependencies_for(codes: &[Code], code: &Code) -> Result<Vec<Vec<CodeId>>, String> {
    resolve_dependencies(codes, &code.dependencies)
        .map_err(|error| format!("block {}: {error}", code.id))
}

/// Adds a directed edge `from -> to` in the dependency graph.
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
    use std::collections::HashMap;

    use upmd_parser::nodes::Options;

    use super::*;

    fn code(id: CodeId, name: &str, dependencies: Option<&str>) -> Code {
        let mut attrs = HashMap::from([("name".to_string(), name.to_string())]);
        if let Some(dependencies) = dependencies {
            attrs.insert("deps".to_string(), dependencies.to_string());
        }
        Code::new(
            id,
            String::new(),
            Options {
                language: "sh".to_string(),
                attrs,
            },
        )
    }

    #[test]
    fn target_batches_preserve_sequential_and_parallel_groups() {
        let codes = vec![
            code(1, "setup", None),
            code(2, "build", None),
            code(3, "lint", None),
            code(4, "test", None),
            code(5, "target", Some("setup, build | lint, test")),
        ];

        let mut scheduler = Scheduler::for_target(&codes, 5).unwrap();
        assert_eq!(scheduler.start(), Some(vec![1]));
        assert_eq!(
            scheduler.advance(1, Some(0)),
            AdvanceResult::NextBatch(vec![2, 3])
        );
        assert_eq!(scheduler.advance(2, Some(0)), AdvanceResult::Pending);
        assert_eq!(
            scheduler.advance(3, Some(0)),
            AdvanceResult::NextBatch(vec![4])
        );
        assert_eq!(
            scheduler.advance(4, Some(0)),
            AdvanceResult::NextBatch(vec![5])
        );
        assert_eq!(
            scheduler.advance(5, Some(0)),
            AdvanceResult::Finished { failed: false }
        );
    }

    #[test]
    fn target_rerun_executes_successful_dependencies_again() {
        let codes = vec![code(1, "setup", None), code(2, "target", Some("setup"))];

        let normal = Scheduler::for_target(&codes, 2).unwrap();
        assert!(!normal.should_execute(1, true));
        assert!(normal.should_execute(2, true));

        let rerun = Scheduler::for_target_rerun(&codes, 2).unwrap();
        assert!(rerun.should_execute(1, true));
        assert!(rerun.should_execute(2, true));
    }

    #[test]
    fn nested_dependencies_are_ordered_before_the_target() {
        let codes = vec![
            code(1, "target", Some("build")),
            code(2, "build", Some("setup")),
            code(3, "setup", None),
        ];

        let mut scheduler = Scheduler::for_target(&codes, 1).unwrap();
        assert_eq!(scheduler.start(), Some(vec![3]));
        assert_eq!(
            scheduler.advance(3, Some(0)),
            AdvanceResult::NextBatch(vec![2])
        );
        assert_eq!(
            scheduler.advance(2, Some(0)),
            AdvanceResult::NextBatch(vec![1])
        );
        assert_eq!(
            scheduler.advance(1, Some(0)),
            AdvanceResult::Finished { failed: false }
        );
    }

    #[test]
    fn all_mode_is_sequential_and_stably_sorted() {
        let codes = vec![
            code(1, "target", Some("setup")),
            code(2, "unrelated", None),
            code(3, "setup", None),
        ];

        let mut scheduler = Scheduler::for_all(&codes, false).unwrap();
        assert_eq!(scheduler.start(), Some(vec![2]));
        assert_eq!(
            scheduler.advance(2, Some(0)),
            AdvanceResult::NextBatch(vec![3])
        );
        assert_eq!(
            scheduler.advance(3, Some(0)),
            AdvanceResult::NextBatch(vec![1])
        );
        assert_eq!(
            scheduler.advance(1, Some(0)),
            AdvanceResult::Finished { failed: false }
        );
    }

    #[test]
    fn graph_uses_scheduler_validation() {
        let codes = vec![
            code(1, "same", None),
            code(2, "same", None),
            code(3, "target", Some("same")),
        ];
        assert!(DependencyGraph::for_target(&codes, 3)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn graph_layers_contain_each_block_once() {
        let codes = vec![
            code(1, "a", None),
            code(2, "b", Some("a")),
            code(3, "target", Some("a, b")),
        ];
        let graph = DependencyGraph::for_target(&codes, 3).unwrap();
        assert_eq!(graph.layers(), &[vec![1], vec![2], vec![3]]);
    }

    #[test]
    fn all_mode_retains_its_complete_graph() {
        let codes = vec![
            code(1, "a", None),
            code(2, "b", Some("a")),
            code(3, "independent", None),
        ];
        let scheduler = Scheduler::for_all(&codes, false).unwrap();
        assert_eq!(scheduler.graph().layers(), &[vec![1, 3], vec![2]]);
        assert_eq!(scheduler.graph().target(), None);
    }

    #[test]
    fn cycles_and_ambiguous_names_are_rejected() {
        let cycle = vec![code(1, "a", Some("b")), code(2, "b", Some("a"))];
        assert!(Scheduler::for_all(&cycle, false)
            .unwrap_err()
            .contains("cycle"));

        let ambiguous = vec![
            code(1, "same", None),
            code(2, "same", None),
            code(3, "target", Some("same")),
        ];
        assert!(Scheduler::for_target(&ambiguous, 3)
            .unwrap_err()
            .contains("ambiguous"));
    }

    #[test]
    fn target_validation_ignores_unrelated_blocks() {
        let codes = vec![code(1, "target", None), code(2, "broken", Some("missing"))];
        assert!(Scheduler::for_target(&codes, 1).is_ok());
        assert!(Scheduler::for_all(&codes, false).is_err());
    }

    #[test]
    fn target_failure_waits_for_its_parallel_batch() {
        let codes = vec![
            code(1, "a", None),
            code(2, "b", None),
            code(3, "target", Some("a | b")),
        ];
        let mut scheduler = Scheduler::for_target(&codes, 3).unwrap();

        assert_eq!(scheduler.start(), Some(vec![1, 2]));
        assert_eq!(scheduler.advance(1, Some(7)), AdvanceResult::Pending);
        assert_eq!(
            scheduler.advance(2, Some(0)),
            AdvanceResult::Stopped(BlockFailure {
                block: 1,
                exit_code: Some(7)
            })
        );
    }

    #[test]
    fn all_mode_skips_failed_dependents_but_runs_other_blocks() {
        let codes = vec![
            code(1, "fail", None),
            code(2, "dependent", Some("fail")),
            code(3, "independent", None),
        ];
        let mut scheduler = Scheduler::for_all(&codes, true).unwrap();

        assert_eq!(scheduler.start(), Some(vec![1]));
        assert_eq!(
            scheduler.advance(1, Some(1)),
            AdvanceResult::NextBatch(vec![3])
        );
        assert_eq!(
            scheduler.advance(3, Some(0)),
            AdvanceResult::Finished { failed: true }
        );
    }
}
