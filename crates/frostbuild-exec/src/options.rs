//! What the caller asked for: [`BuildOptions`] and the scheduler and estimator
//! it selects.

use std::collections::BTreeMap;
use std::time::Duration;

use crate::{ProgressSender, DEFAULT_CAS_MAX_BYTES};

#[derive(Debug, Clone)]
pub struct BuildOptions {
    pub jobs: usize,
    /// Admission budgets applied in addition to the worker-count ceiling.
    pub resources: ResourceLimits,
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

/// Host capacity the scheduler may reserve concurrently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ResourceLimits {
    pub cpu: usize,
    pub ram_mb: u64,
    pub test_jobs: usize,
}

impl ResourceLimits {
    /// Host-derived production defaults. `jobs` remains a separate hard cap.
    pub fn host(jobs: usize) -> Self {
        Self {
            cpu: std::thread::available_parallelism().map_or(1, |n| n.get()),
            ram_mb: physical_ram_mb(),
            test_jobs: jobs.max(1),
        }
    }

    /// Preserve ordinary list-scheduling semantics for a simulated `-j N`.
    pub fn for_jobs(jobs: usize) -> Self {
        let jobs = jobs.max(1);
        Self {
            cpu: jobs,
            ram_mb: u64::MAX,
            test_jobs: jobs,
        }
    }
}

#[cfg(unix)]
fn physical_ram_mb() -> u64 {
    // SAFETY: sysconf reads process-independent host constants and has no
    // pointer arguments. A negative return means the host cannot answer.
    let pages = unsafe { libc::sysconf(libc::_SC_PHYS_PAGES) };
    let page_size = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
    if pages <= 0 || page_size <= 0 {
        return u64::MAX;
    }
    (pages as u64)
        .saturating_mul(page_size as u64)
        .saturating_div(1024 * 1024)
        .max(1)
}

#[cfg(windows)]
fn physical_ram_mb() -> u64 {
    use windows_sys::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};

    // SAFETY: the structure is initialized to its documented length and the
    // API writes only within it for the duration of the call.
    let mut status: MEMORYSTATUSEX = unsafe { std::mem::zeroed() };
    status.dwLength = std::mem::size_of::<MEMORYSTATUSEX>() as u32;
    if unsafe { GlobalMemoryStatusEx(&mut status) } == 0 {
        return u64::MAX;
    }
    status.ullTotalPhys.saturating_div(1024 * 1024).max(1)
}

#[cfg(not(any(unix, windows)))]
fn physical_ram_mb() -> u64 {
    u64::MAX
}

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
        let jobs = std::thread::available_parallelism().map_or(1, |n| n.get());
        Self {
            jobs,
            resources: ResourceLimits::host(jobs),
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
