//! Deciding the order, separately from running it.
//!
//! A schedule is a pure function of the graph, the journal and the estimator,
//! which is what makes `frost simulate` possible: the same code that plans a
//! real build can plan a hypothetical one without touching the filesystem.

use std::cmp::Reverse;
use std::collections::BTreeSet;
use std::collections::BinaryHeap;
use std::collections::HashMap;

use frostbuild_core::graph::ActionId;
use frostbuild_core::graph::BuildGraph;
use frostbuild_core::journal::Journal;

use crate::estimates::Estimates;
use crate::options::{Estimator, Scheduler};

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

        while done < n {
            while running.len() < jobs {
                let Some((_, Reverse(local))) = ready.pop() else {
                    break;
                };
                running.push((Reverse(now + durations[local]), Reverse(local)));
                busy += durations[local];
            }
            let Some((Reverse(finish), Reverse(local))) = running.pop() else {
                // Nothing running and nothing ready: the graph is exhausted or
                // cyclic. The graph builder rejects cycles, so this is the end.
                break;
            };
            now = finish;
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
