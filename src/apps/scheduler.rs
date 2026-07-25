use std::collections::{HashMap, HashSet, VecDeque};

use upmd_parser::{resolve_dependencies, Code, CodeId};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BlockFailure {
    pub block: CodeId,
    pub exit_code: Option<i32>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum AdvanceResult {
    NextBatch(Vec<CodeId>),
    Pending,
    Done,
    Stopped(BlockFailure),
    Untracked,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FailurePolicy {
    Continue,
    Stop,
}

#[derive(Debug)]
pub struct Scheduler {
    pending: VecDeque<Vec<CodeId>>,
    running: HashSet<CodeId>,
    blocked: HashSet<CodeId>,
    dependents: HashMap<CodeId, Vec<CodeId>>,
    failure: Option<BlockFailure>,
    failure_policy: FailurePolicy,
}

impl Scheduler {
    pub fn for_all(codes: &[Code]) -> Result<Self, String> {
        let ids = codes.iter().map(|code| code.id).collect::<HashSet<_>>();
        let plan = build_plan(codes, &ids)?;
        let pending = plan
            .layers
            .into_iter()
            .flatten()
            .map(|id| vec![id])
            .collect();
        Ok(Self::new(pending, plan.dependents, FailurePolicy::Continue))
    }

    pub fn for_target(codes: &[Code], target: CodeId) -> Result<Self, String> {
        let ids = collect_dependencies(codes, target)?;
        let plan = build_plan(codes, &ids)?;
        Ok(Self::new(
            plan.layers.into(),
            plan.dependents,
            FailurePolicy::Stop,
        ))
    }

    fn new(
        pending: VecDeque<Vec<CodeId>>,
        dependents: HashMap<CodeId, Vec<CodeId>>,
        failure_policy: FailurePolicy,
    ) -> Self {
        Self {
            pending,
            running: HashSet::new(),
            blocked: HashSet::new(),
            dependents,
            failure: None,
            failure_policy,
        }
    }

    pub fn start(&mut self) -> Option<Vec<CodeId>> {
        if self.running.is_empty() {
            self.take_next_batch()
        } else {
            None
        }
    }

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

        self.take_next_batch()
            .map_or(AdvanceResult::Done, AdvanceResult::NextBatch)
    }

    pub fn has_failures(&self) -> bool {
        self.failure.is_some()
    }

    pub fn is_running(&self, block: CodeId) -> bool {
        self.running.contains(&block)
    }

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

    fn block_dependents(&mut self, failed: CodeId) {
        let mut stack = vec![failed];
        while let Some(block) = stack.pop() {
            if let Some(dependents) = self.dependents.get(&block) {
                for &dependent in dependents {
                    if self.blocked.insert(dependent) {
                        stack.push(dependent);
                    }
                }
            }
        }
    }

    #[cfg(test)]
    fn batches(&self) -> Vec<Vec<CodeId>> {
        self.pending.iter().cloned().collect()
    }
}

struct Plan {
    layers: Vec<Vec<CodeId>>,
    dependents: HashMap<CodeId, Vec<CodeId>>,
}

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

fn build_plan(codes: &[Code], selected: &HashSet<CodeId>) -> Result<Plan, String> {
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

    Ok(Plan { layers, dependents })
}

fn dependencies_for(codes: &[Code], code: &Code) -> Result<Vec<Vec<CodeId>>, String> {
    resolve_dependencies(codes, &code.dependencies)
        .map_err(|error| format!("block {}: {error}", code.id))
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

        let scheduler = Scheduler::for_target(&codes, 5).unwrap();
        assert_eq!(
            scheduler.batches(),
            vec![vec![1], vec![2, 3], vec![4], vec![5]]
        );
    }

    #[test]
    fn nested_dependencies_are_ordered_before_the_target() {
        let codes = vec![
            code(1, "target", Some("build")),
            code(2, "build", Some("setup")),
            code(3, "setup", None),
        ];

        let scheduler = Scheduler::for_target(&codes, 1).unwrap();
        assert_eq!(scheduler.batches(), vec![vec![3], vec![2], vec![1]]);
    }

    #[test]
    fn all_mode_is_sequential_and_stably_sorted() {
        let codes = vec![
            code(1, "target", Some("setup")),
            code(2, "unrelated", None),
            code(3, "setup", None),
        ];

        let scheduler = Scheduler::for_all(&codes).unwrap();
        assert_eq!(scheduler.batches(), vec![vec![2], vec![3], vec![1]]);
    }

    #[test]
    fn cycles_and_ambiguous_names_are_rejected() {
        let cycle = vec![code(1, "a", Some("b")), code(2, "b", Some("a"))];
        assert!(Scheduler::for_all(&cycle).unwrap_err().contains("cycle"));

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
        assert!(Scheduler::for_all(&codes).is_err());
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
        let mut scheduler = Scheduler::for_all(&codes).unwrap();

        assert_eq!(scheduler.start(), Some(vec![1]));
        assert_eq!(
            scheduler.advance(1, Some(1)),
            AdvanceResult::NextBatch(vec![3])
        );
        assert_eq!(scheduler.advance(3, Some(0)), AdvanceResult::Done);
        assert!(scheduler.has_failures());
    }
}
