//! Deciding the order, separately from running it.
//!
//! A schedule is a pure function of the graph, the journal and the estimator,
//! which is what makes `frost simulate` possible: the same code that plans a
//! real build can plan a hypothetical one without touching the filesystem.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::collections::BinaryHeap;
use std::collections::HashMap;

use frostbuild_core::graph::{ActionId, ActionKind, BuildGraph};
use frostbuild_core::journal::Journal;
use frostbuild_core::manifest::ActionResources;

use crate::estimates::Estimates;
use crate::options::{Estimator, ResourceLimits, Scheduler};
use crate::resources::ResourceAdmission;

/// The scheduling decision, separated from execution.
///
/// The engine and any measurement of the engine must agree on what the
/// scheduler would do, so both build their queue from this one type. A
/// simulator that recomputed priorities on its own would be describing a
/// different scheduler than the one that runs.
#[derive(Debug, Clone)]
pub struct Schedule {
    /// Actions in deterministic closure order; every index below is local.
    pub closure: Vec<ActionId>,
    pub closure_index: HashMap<ActionId, usize>,
    /// In-closure dependents of each action.
    pub dependents: Vec<Vec<usize>>,
    /// Unfinished producers each action waits on.
    pub waiting: Vec<usize>,
    /// Estimated duration of each action.
    pub duration_ms: Vec<u64>,
    /// Admission requirements and test classification in closure-local order.
    pub resources: Vec<ActionResources>,
    pub is_test: Vec<bool>,
    /// Ready-queue key: estimated longest remaining chain, or zero for FIFO.
    pub priority: Vec<u64>,
    /// Longest chain by estimated duration: the makespan no scheduler can beat.
    pub critical_path_ms: u64,
    /// Local indices along that chain, in execution order.
    pub critical_path: Vec<usize>,
    /// Sum of all estimated durations.
    pub work_ms: u64,
}

impl Schedule {
    pub fn plan(
        graph: &BuildGraph,
        closure: Vec<ActionId>,
        journal: &Journal,
        scheduler: Scheduler,
        estimator: Estimator,
    ) -> Self {
        let closure_index: HashMap<ActionId, usize> =
            closure.iter().enumerate().map(|(i, &a)| (a, i)).collect();

        let mut waiting = vec![0usize; closure.len()];
        let mut dependents = vec![Vec::new(); closure.len()];
        for (local, &action_id) in closure.iter().enumerate() {
            let mut producers = BTreeSet::new();
            for &input in graph.actions[action_id]
                .inputs
                .iter()
                .chain(&graph.actions[action_id].order_only_inputs)
            {
                if let Some(p) = graph.files[input].producer {
                    if let Some(&plocal) = closure_index.get(&p) {
                        producers.insert(plocal);
                    }
                }
            }
            waiting[local] = producers.len();
            for p in producers {
                dependents[p].push(local);
            }
        }

        let estimate = Estimates::new(estimator, graph, journal);
        // Longest remaining chain, computed once in reverse topological order.
        // The same vector is reused when dependents become ready, so priority
        // is consistent for the whole build rather than only the first wave.
        let mut priority = vec![0u64; closure.len()];
        let mut duration_ms = vec![0u64; closure.len()];
        for local in (0..closure.len()).rev() {
            let action = &graph.actions[closure[local]];
            duration_ms[local] = estimate.of(graph, action, journal);
            let tail = dependents[local]
                .iter()
                .map(|&dependent| priority[dependent])
                .max()
                .unwrap_or(0);
            priority[local] = duration_ms[local].saturating_add(tail);
        }
        let critical_path_ms = priority.iter().copied().max().unwrap_or(0);
        let work_ms = duration_ms.iter().sum();
        let resources = closure
            .iter()
            .map(|&action| graph.actions[action].resources)
            .collect();
        let is_test = closure
            .iter()
            .map(|&action| graph.actions[action].kind == ActionKind::Test)
            .collect();

        // Walk the chain that realizes the longest path, so a report can name
        // the actions that actually bound the build.
        let mut critical_path = Vec::new();
        if let Some(mut cur) = (0..closure.len())
            .filter(|&i| waiting[i] == 0)
            .max_by_key(|&i| priority[i])
        {
            loop {
                critical_path.push(cur);
                match dependents[cur].iter().copied().max_by_key(|&d| priority[d]) {
                    Some(next) => cur = next,
                    None => break,
                }
            }
        }

        if scheduler == Scheduler::Fifo {
            priority.fill(0);
        }
        Self {
            closure,
            closure_index,
            dependents,
            waiting,
            duration_ms,
            resources,
            is_test,
            priority,
            critical_path_ms,
            critical_path,
            work_ms,
        }
    }

    /// Makespan this schedule would reach with `jobs` workers, by list
    /// scheduling over its own estimated durations. Deterministic: no build
    /// runs, no cache is touched, and repeated calls give the same answer.
    pub fn simulate(&self, jobs: usize) -> Simulation {
        self.simulate_against(jobs, &self.duration_ms)
    }

    /// Simulate this schedule's *ordering* against reference durations.
    ///
    /// Comparing two estimators requires one clock. An estimator decides the
    /// order actions start in; it does not change how long they take. Scoring
    /// each estimator against its own guesses would rank the most optimistic
    /// guesser first — `static` calls every action 1 ms and would "win" every
    /// sweep. Pass the best available durations (the journal's recorded ones)
    /// as the reference and the comparison measures ordering quality alone.
    pub fn simulate_against(&self, jobs: usize, durations: &[u64]) -> Simulation {
        self.simulate_against_with_resources(jobs, durations, ResourceLimits::for_jobs(jobs))
    }

    /// Simulate the real scheduler's resource admission as well as its queue
    /// ordering. Oversized actions consume the whole corresponding budget so
    /// one can still run instead of deadlocking forever.
    pub fn simulate_against_with_resources(
        &self,
        jobs: usize,
        durations: &[u64],
        limits: ResourceLimits,
    ) -> Simulation {
        let jobs = jobs.max(1);
        let n = self.closure.len();
        assert_eq!(
            durations.len(),
            n,
            "reference durations must cover the closure"
        );
        let mut waiting = self.waiting.clone();
        let mut ready: BinaryHeap<(u64, Reverse<usize>)> = (0..n)
            .filter(|&i| waiting[i] == 0)
            .map(|i| (self.priority[i], Reverse(i)))
            .collect();
        // (completion time, local index), earliest first.
        let mut running: BinaryHeap<(Reverse<u64>, Reverse<usize>)> = BinaryHeap::new();
        let mut now = 0u64;
        let mut busy = 0u64;
        let mut done = 0usize;
        let mut admission = ResourceAdmission::new(limits);
        let mut resource_waits = 0usize;

        while done < n {
            while running.len() < jobs {
                let had_ready = !ready.is_empty();
                let mut deferred = Vec::new();
                let mut claimed = None;
                while let Some(entry @ (_, Reverse(local))) = ready.pop() {
                    if admission.fits(self.resources[local], self.is_test[local]) {
                        claimed = Some((entry, local));
                        break;
                    }
                    deferred.push(entry);
                }
                ready.extend(deferred);
                let Some((_, local)) = claimed else {
                    if had_ready && !running.is_empty() {
                        resource_waits += 1;
                    }
                    break;
                };
                admission.reserve(self.resources[local], self.is_test[local]);
                running.push((Reverse(now + durations[local]), Reverse(local)));
                busy += durations[local];
            }
            let Some((Reverse(finish), Reverse(local))) = running.pop() else {
                // Nothing running and nothing ready: the graph is exhausted or
                // cyclic. The graph builder rejects cycles, so this is the end.
                break;
            };
            now = finish;
            admission.release(self.resources[local], self.is_test[local]);
            done += 1;
            for &dependent in &self.dependents[local] {
                waiting[dependent] -= 1;
                if waiting[dependent] == 0 {
                    ready.push((self.priority[dependent], Reverse(dependent)));
                }
            }
        }

        Simulation {
            jobs,
            makespan_ms: now,
            busy_ms: busy,
            critical_path_ms: self.critical_path_ms,
            work_ms: self.work_ms,
            actions: n,
            peak_cpu: admission.peak_cpu(),
            peak_ram_mb: admission.peak_ram_mb(),
            peak_tests: admission.peak_tests(),
            resource_waits,
        }
    }
}

/// Result of scheduling without executing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Simulation {
    pub jobs: usize,
    pub makespan_ms: u64,
    pub busy_ms: u64,
    pub critical_path_ms: u64,
    pub work_ms: u64,
    pub actions: usize,
    pub peak_cpu: usize,
    pub peak_ram_mb: u64,
    pub peak_tests: usize,
    pub resource_waits: usize,
}

impl Simulation {
    pub fn utilization_pct(&self) -> f64 {
        let capacity = self.makespan_ms.saturating_mul(self.jobs as u64);
        if capacity == 0 {
            return 0.0;
        }
        100.0 * self.busy_ms as f64 / capacity as f64
    }

    /// How far above the unbeatable lower bound this schedule lands.
    pub fn over_critical_path_pct(&self) -> Option<f64> {
        (self.critical_path_ms > 0).then(|| {
            100.0 * (self.makespan_ms as f64 - self.critical_path_ms as f64)
                / self.critical_path_ms as f64
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use frostbuild_core::manifest::Manifest;

    fn plan(text: &str, targets: &[&str]) -> Schedule {
        let manifest = Manifest::parse_str(text).unwrap();
        let graph = BuildGraph::from_manifest(&manifest).unwrap();
        let closure = graph
            .action_closure(
                &targets
                    .iter()
                    .map(|target| target.to_string())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        Schedule::plan(
            &graph,
            closure,
            &Journal::default(),
            Scheduler::CriticalPath,
            Estimator::Heuristic,
        )
    }

    #[test]
    fn ram_admission_serializes_heavy_actions_deterministically() {
        let schedule = plan(
            r#"
            [target.a]
            kind = "genrule"
            cmd = "true"
            outputs = ["a"]
            resources = { ram_mb = 600 }

            [target.b]
            kind = "genrule"
            cmd = "true"
            outputs = ["b"]
            resources = { ram_mb = 600 }
            "#,
            &["a", "b"],
        );
        let durations = vec![10; schedule.closure.len()];
        let constrained = schedule.simulate_against_with_resources(
            2,
            &durations,
            ResourceLimits {
                cpu: 2,
                ram_mb: 1_000,
                test_jobs: 2,
            },
        );
        assert_eq!(constrained.makespan_ms, 20);
        assert_eq!(constrained.peak_ram_mb, 600);
        assert!(constrained.resource_waits > 0);

        let roomy = schedule.simulate_against_with_resources(
            2,
            &durations,
            ResourceLimits {
                cpu: 2,
                ram_mb: 1_200,
                test_jobs: 2,
            },
        );
        assert_eq!(roomy.makespan_ms, 10);
        assert_eq!(roomy.peak_ram_mb, 1_200);

        let oversized = schedule.simulate_against_with_resources(
            2,
            &durations,
            ResourceLimits {
                cpu: 2,
                ram_mb: 512,
                test_jobs: 1,
            },
        );
        assert_eq!(oversized.makespan_ms, 20);
        assert_eq!(
            oversized.peak_ram_mb, 512,
            "an oversized action consumes the pool instead of deadlocking"
        );
    }

    #[test]
    fn exclusive_and_test_limits_use_the_same_admission_model() {
        let exclusive = plan(
            r#"
            [target.a]
            kind = "genrule"
            cmd = "true"
            outputs = ["a"]
            resources = { exclusive = true }

            [target.b]
            kind = "genrule"
            cmd = "true"
            outputs = ["b"]
            "#,
            &["a", "b"],
        );
        let durations = vec![10; exclusive.closure.len()];
        let result =
            exclusive.simulate_against_with_resources(2, &durations, ResourceLimits::for_jobs(2));
        assert_eq!(result.makespan_ms, 20, "exclusive work must run alone");

        let tests = plan(
            r#"
            [target.a]
            kind = "test"
            cmd = "true"

            [target.b]
            kind = "test"
            cmd = "true"
            "#,
            &["a", "b"],
        );
        let durations = vec![10; tests.closure.len()];
        let result = tests.simulate_against_with_resources(
            4,
            &durations,
            ResourceLimits {
                cpu: 4,
                ram_mb: u64::MAX,
                test_jobs: 1,
            },
        );
        assert_eq!(result.makespan_ms, 20);
        assert_eq!(result.peak_tests, 1);
    }
}
