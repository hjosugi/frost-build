//! What a build produced: per-action [`Outcome`], and the [`BuildReport`] and
//! [`BuildStats`] over them.
//!
//! Kept apart from the engine because these are the crate's output types --
//! the CLI, the daemon and the JSON event stream all read them, and none of
//! them needs to know how the engine reached them.

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Ran the command successfully.
    Executed { reason: String, duration_ms: u64 },
    /// Action key and outputs matched the journal; nothing to do.
    Cached,
    /// Dry run: this action would definitely run.
    WouldRun { reason: String },
    /// Dry run: upstream would run, so this action's inputs are unknowable.
    MayRun { reason: String },
    /// The command ran and failed.
    Failed { reason: String, detail: String },
    /// A test failed and then passed on a retry it declared with
    /// `flaky_retries`.
    ///
    /// Separate from `Executed` because the build is green but the result is
    /// not trustworthy, and folding the two would lose exactly the signal the
    /// feature exists to surface. The success is deliberately not journalled:
    /// caching a verdict the test only reached on the second try would hide
    /// the flake from every later build.
    Flaky {
        reason: String,
        duration_ms: u64,
        attempts: u32,
    },
    /// Not run because an upstream action failed or the build aborted.
    Skipped { reason: String },
}

#[derive(Debug, Clone)]
pub struct ActionResult {
    pub id: String,
    pub desc: String,
    /// What sort of work this was, so a report can group by it rather than
    /// re-deriving categories from the shape of an id.
    pub kind: frostbuild_core::graph::ActionKind,
    /// The target this action belongs to.
    pub target: String,
    pub outcome: Outcome,
}

#[derive(Debug, Default)]
pub struct BuildReport {
    /// One entry per closure action, in deterministic graph order.
    pub results: Vec<ActionResult>,
    /// Scheduling measurements, so two strategies can be compared from a
    /// single run rather than by wall-clock feel.
    pub stats: BuildStats,
    /// Action ids along the estimated longest chain, in execution order.
    ///
    /// The scheduler computes this to order the ready queue; carrying it out
    /// means a report can name the chain that bounded the build instead of
    /// recomputing one that might not be the chain the scheduler used. Empty
    /// when nothing ran, because then nothing bounded anything.
    pub critical_path: Vec<String>,
}

/// What the chosen scheduler and estimator actually bought.
///
/// `busy_ms / (makespan_ms * jobs)` is the fraction of the available worker
/// time that was spent executing; the gap is idle workers waiting on the
/// dependency graph, which is exactly what a scheduler can improve.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BuildStats {
    pub scheduler: &'static str,
    pub estimator: &'static str,
    pub jobs: usize,
    /// Wall time of the execution phase.
    pub makespan_ms: u64,
    /// Sum of executed action durations.
    pub busy_ms: u64,
    /// Estimated longest dependency chain, before execution.
    pub critical_path_ms: u64,
    /// Estimated total work, before execution.
    pub estimated_work_ms: u64,
    pub executed: usize,
    pub local_cpu_resources: usize,
    pub local_ram_resources_mb: u64,
    pub local_test_jobs: usize,
    pub peak_cpu: usize,
    pub peak_ram_mb: u64,
    pub peak_tests: usize,
    pub resource_constrained: bool,
}

impl BuildStats {
    /// Executed work over available worker time, in percent.
    pub fn utilization_pct(&self) -> f64 {
        let capacity = self.makespan_ms.saturating_mul(self.jobs as u64);
        if capacity == 0 {
            return 0.0;
        }
        100.0 * self.busy_ms as f64 / capacity as f64
    }

    /// How close the run came to the estimated critical path. A ratio near 1
    /// means the schedule is bounded by the graph, not by the ordering, so a
    /// better scheduler cannot help.
    pub fn critical_path_ratio(&self) -> Option<f64> {
        (self.critical_path_ms > 0).then(|| self.makespan_ms as f64 / self.critical_path_ms as f64)
    }
}

impl BuildReport {
    pub fn count(&self, pred: impl Fn(&Outcome) -> bool) -> usize {
        self.results.iter().filter(|r| pred(&r.outcome)).count()
    }

    pub fn executed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Executed { .. } | Outcome::Flaky { .. }))
    }

    /// Tests that only passed on a retry. Green, and worth knowing about.
    pub fn flaky(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Flaky { .. }))
    }

    pub fn cached(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Cached))
    }

    pub fn failed(&self) -> usize {
        self.count(|o| matches!(o, Outcome::Failed { .. }))
    }

    pub fn success(&self) -> bool {
        self.results.iter().all(|r| {
            matches!(
                r.outcome,
                Outcome::Executed { .. }
                    // A flake reached a passing verdict, so the build is
                    // green and the exit code says so. It is reported, not
                    // cached, which is where the cost lands instead.
                    | Outcome::Flaky { .. }
                    | Outcome::Cached
                    | Outcome::WouldRun { .. }
                    | Outcome::MayRun { .. }
            )
        })
    }
}
