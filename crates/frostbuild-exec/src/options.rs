//! What the caller asked for: [`BuildOptions`] and the scheduler and estimator
//! it selects.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::{ProgressSender, DEFAULT_CAS_MAX_BYTES};

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub jobs: usize,
    pub keep_going: bool,
    pub dry_run: bool,
    pub verbose: bool,
    pub no_cache: bool,
    pub sandbox: bool,
    pub check_determinism: bool,
    pub cas_max_bytes: u64,
    /// Persist a whole-closure certificate after the normal path proves that
    /// a plain default-target build is entirely cached.
    pub write_fast_noop: bool,
    pub scheduler: Scheduler,
    pub estimator: Estimator,
    /// Optional structured progress sink. The execution engine never renders
    /// terminal output itself; callers choose a TTY or plain-text renderer.
    pub progress: Option<ProgressSender>,
    /// Optional shared cache consulted when the local journal misses. It can
    /// only make a build faster: every response is verified and any failure
    /// falls back to executing the action.
    pub remote: Option<std::sync::Arc<frostbuild_core::remote::RemoteCache>>,
    /// Seconds any action may run when its target declares no limit of its
    /// own. `None` leaves build actions unbounded, which is the default: a
    /// watchdog costs a thread per action, and the common hang is a test.
    pub timeout: Option<Duration>,
    /// Values the workspace's `[stamp]` command printed for this build, keyed
    /// by name.
    ///
    /// `None` means stamping is off — `--no-stamp`, or `--stamp-optional`
    /// after the command failed — and every reference expands to nothing.
    /// That is deliberately different from `Some(empty)`, which would mean the
    /// command ran and printed nothing, and where a reference is a mistake
    /// worth reporting. Collapsing the two would make `--no-stamp` fail every
    /// workspace that actually uses a stamp, which is all of them.
    ///
    /// One map for the whole build on purpose: a stamp names a property of the
    /// invocation, so an action that saw a different value than its neighbour
    /// would make "which build produced this binary" unanswerable.
    pub stamps: Option<BTreeMap<String, String>>,
    /// Run every test this many times, requiring all of them to pass. 1 is the
    /// default and means the ordinary single run.
    ///
    /// A value above 1 does not consult the cache: a recorded single-run pass
    /// cannot answer "does this pass ten times in a row", which is the only
    /// question worth asking N runs.
    pub runs_per_test: u32,
}

/// A test that hangs blocks the whole CI job until its runner's own limit, if
/// it has one. Tests are few and long-lived, so a default limit here costs
/// nothing measurable and removes the most common way a build never returns.
pub const DEFAULT_TEST_TIMEOUT: Duration = Duration::from_secs(300);

/// Ready-queue ordering. Both schedulers run the same actions and produce the
/// same outputs; they differ only in the order independent work is started,
/// which shows up as makespan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scheduler {
    /// Start the action with the longest remaining dependency chain first.
    CriticalPath,
    /// Start whichever became ready first.
    Fifo,
}

/// How the scheduler guesses an action's duration. Only affects ordering, so a
/// bad estimate costs makespan, never correctness.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Estimator {
    /// Fixed cost per action kind. No history needed.
    Heuristic,
    /// This action's own last recorded duration; heuristic when unseen.
    Journal,
    /// Every action costs the same, so priority is pure graph depth.
    Static,
    /// This action's own history when present, otherwise the median duration
    /// of the same kind across this workspace's journal. The difference from
    /// `Journal` is entirely in the unseen case — new and changed actions get
    /// a workspace-calibrated estimate instead of a hardcoded constant.
    Learned,
}

impl Scheduler {
    pub fn as_str(self) -> &'static str {
        match self {
            Scheduler::CriticalPath => "critical-path",
            Scheduler::Fifo => "fifo",
        }
    }
}

impl Estimator {
    pub fn as_str(self) -> &'static str {
        match self {
            Estimator::Heuristic => "heuristic",
            Estimator::Journal => "journal",
            Estimator::Static => "static",
            Estimator::Learned => "learned",
        }
    }
}

impl Default for BuildOptions {
    fn default() -> Self {
        Self {
            jobs: std::thread::available_parallelism().map_or(1, |n| n.get()),
            keep_going: false,
            dry_run: false,
            verbose: false,
            no_cache: false,
            sandbox: false,
            check_determinism: false,
            cas_max_bytes: DEFAULT_CAS_MAX_BYTES,
            write_fast_noop: false,
            scheduler: Scheduler::CriticalPath,
            estimator: Estimator::Journal,
            progress: None,
            timeout: None,
            remote: None,
            stamps: None,
            runs_per_test: 1,
        }
    }
}
