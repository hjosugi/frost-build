//! How long an action is expected to take.
//!
//! Estimates only affect *order*, never correctness -- a bad estimate costs
//! wall-clock, not a wrong build -- which is why a missing history falls back
//! to a per-kind constant rather than refusing to schedule.

use std::collections::BTreeMap;
use std::collections::HashMap;

use frostbuild_core::graph::ActionKind;
use frostbuild_core::graph::BuildGraph;
use frostbuild_core::journal::Journal;

use crate::journal_id;
use crate::options::Estimator;

fn default_duration(kind: ActionKind) -> u64 {
    match kind {
        ActionKind::Link => 100,
        ActionKind::Archive => 30,
        ActionKind::Compile => 20,
        ActionKind::Genrule => 10,
        ActionKind::Test => 50,
        ActionKind::KofunCompile => 30,
        ActionKind::Command => 40,
        // One `gcov` per object plus a merge, and it runs after the test it
        // reports on, so it sits at the end of a chain where a low guess would
        // schedule it late.
        ActionKind::Coverage => 60,
    }
}

/// Duration estimates for scheduling. Estimates only order work, so an
/// inaccurate model costs makespan and never correctness.
pub(crate) struct Estimates {
    kind: Estimator,
    /// Median observed duration per action kind, learned from this
    /// workspace's journal. Empty unless the learned estimator is selected.
    learned: BTreeMap<u8, u64>,
}

impl Estimates {
    pub(crate) fn new(kind: Estimator, graph: &BuildGraph, journal: &Journal) -> Self {
        let mut learned = BTreeMap::new();
        if kind == Estimator::Learned {
            let mut by_kind: BTreeMap<u8, Vec<u64>> = BTreeMap::new();
            let mut kind_of: HashMap<&str, ActionKind> = HashMap::new();
            for action in &graph.actions {
                kind_of.insert(action.id.as_str(), action.kind);
            }
            for (id, entry) in &journal.actions {
                // Journal ids are `action@profile[@platform]`; the action id
                // is the prefix before the first '@'.
                let action_id = id.split('@').next().unwrap_or(id);
                if let Some(&k) = kind_of.get(action_id) {
                    if entry.duration_ms > 0 {
                        by_kind
                            .entry(kind_code(k))
                            .or_default()
                            .push(entry.duration_ms);
                    }
                }
            }
            for (k, mut samples) in by_kind {
                samples.sort_unstable();
                learned.insert(k, samples[samples.len() / 2].max(1));
            }
        }
        Self { kind, learned }
    }

    pub(crate) fn of(
        &self,
        graph: &BuildGraph,
        action: &frostbuild_core::graph::ActionNode,
        journal: &Journal,
    ) -> u64 {
        let recorded = || {
            journal
                .actions
                .get(&journal_id(graph, action))
                .map(|e| e.duration_ms)
                .filter(|&d| d > 0)
        };
        match self.kind {
            Estimator::Static => 1,
            Estimator::Heuristic => default_duration(action.kind),
            Estimator::Journal => recorded().unwrap_or_else(|| default_duration(action.kind)),
            Estimator::Learned => recorded().unwrap_or_else(|| {
                self.learned
                    .get(&kind_code(action.kind))
                    .copied()
                    .unwrap_or_else(|| default_duration(action.kind))
            }),
        }
    }
}

fn kind_code(kind: ActionKind) -> u8 {
    match kind {
        ActionKind::Compile => 0,
        ActionKind::Archive => 1,
        ActionKind::Link => 2,
        ActionKind::Genrule => 3,
        ActionKind::Test => 4,
        ActionKind::KofunCompile => 5,
        ActionKind::Command => 6,
        // Appended, never renumbered: these codes key learned durations in a
        // journal written by earlier versions.
        ActionKind::Coverage => 7,
    }
}
