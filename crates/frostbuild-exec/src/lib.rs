//! Parallel build engine: dependency-counting scheduler, real process
//! execution, and constructive-trace action caching.
//!
//! Rebuild decision: an action is skipped when its action-key digest
//! (command + toolchain + content digests of declared and discovered inputs)
//! matches the journal entry from the last run AND its recorded outputs are
//! intact on disk. Because downstream keys are computed from upstream output
//! *content*, an action that re-runs but reproduces identical outputs stops
//! dirtiness from propagating (early cutoff).

use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, BinaryHeap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::OnceLock;
use std::sync::{Condvar, Mutex};
use std::time::Instant;

use anyhow::Result;
use frostbuild_core::cas::LocalCas;
use frostbuild_core::depfile;
use frostbuild_core::graph::{ActionId, ActionKind, BuildGraph};
use frostbuild_core::hashcache::HashCache;
use frostbuild_core::journal::{Journal, JournalEntry};
use rayon::prelude::*;

mod command;
mod estimates;
mod fast_noop;
mod keys;
mod options;
mod outputs;
mod process;
mod progress;
mod remote;
mod report;
mod sandbox;
mod schedule;
mod toolchain;
pub use fast_noop::{FastNoopDaemonHit, FastNoopHit, FastNoopWatchProof};
use keys::{action_key_argv, path_is_inside, streamed_action_key, StreamedActionDescriptor};
pub use options::{BuildOptions, Estimator, Scheduler, DEFAULT_TEST_TIMEOUT};
pub use process::{install_signal_handler, request_cancellation, resolve_timeout, was_cancelled};
pub use progress::{progress_channel, ProgressEvent, ProgressSender, ProgressState};
pub use report::{ActionResult, BuildReport, BuildStats, Outcome};
pub use schedule::{Schedule, Simulation};
pub use toolchain::{toolchain_closure_fingerprint_cached, toolchain_fingerprint};

static CANCELLED: AtomicBool = AtomicBool::new(false);
static RUNNING_PROCESS_GROUPS: OnceLock<Mutex<BTreeSet<u32>>> = OnceLock::new();
static SIGNAL_HANDLER: OnceLock<()> = OnceLock::new();

/// Defined in `frostbuild_core` because `frost lint` reports a `pass_env` that
/// names one of these, and that is only sound if it is reading the same list
/// the executor passes through.
use frostbuild_core::ENV_PASSTHROUGH;

/// Environment that changes what a compiler produces, so it belongs in the
/// action key. `CPATH=/a` and `CPATH=/b` select different headers with an
/// identical command line and identical declared inputs; without these in the
/// key, frost hands back the binary built against the other one.
const ENV_IN_KEY: &[&str] = &[
    "SystemRoot",
    "SDKROOT",
    "MACOSX_DEPLOYMENT_TARGET",
    "CPATH",
    "C_INCLUDE_PATH",
    "CPLUS_INCLUDE_PATH",
    "LIBRARY_PATH",
];

pub const DEFAULT_CAS_MAX_BYTES: u64 = 10 * 1024 * 1024 * 1024;

/// The action-key layout every cached entry was written under. It is reported
/// by `frost info` because a bump invalidates the whole local cache, and
/// tooling that wraps Frost needs to see that without reading the journal.
pub const ACTION_KEY_SCHEMA: &str = "frost-action-key-v4";

pub fn key_environment_snapshot() -> BTreeMap<String, String> {
    ENV_IN_KEY
        .iter()
        .filter_map(|name| {
            std::env::var_os(name)
                .map(|value| ((*name).to_string(), value.to_string_lossy().into_owned()))
        })
        .collect()
}

pub fn try_fast_noop(root: &Path, profile: &str, platform: &str) -> Result<Option<FastNoopHit>> {
    fast_noop::check(root, profile, platform, &key_environment_snapshot(), true)
}

/// Validate a certificate using key-affecting environment captured by a
/// client process. Arbitrary `pass_env` values are intentionally unavailable
/// to the daemon; certificates that depend on them return a miss and take the
/// normal child-build path.
pub fn try_fast_noop_with_key_environment(
    root: &Path,
    profile: &str,
    platform: &str,
    key_env: &BTreeMap<String, String>,
) -> Result<Option<FastNoopHit>> {
    fast_noop::check(root, profile, platform, key_env, false)
}

/// Fully validate a daemon certificate and return a watcher-cache proof only
/// when every recorded file can be covered by the workspace event stream.
pub fn try_fast_noop_for_daemon(
    root: &Path,
    profile: &str,
    platform: &str,
    key_env: &BTreeMap<String, String>,
) -> Result<Option<FastNoopDaemonHit>> {
    fast_noop::check_for_daemon(root, profile, platform, key_env, false)
}

/// Revalidate the non-workspace portion of a watcher-backed certificate.
pub fn try_fast_noop_from_watch_proof(
    root: &Path,
    profile: &str,
    platform: &str,
    key_env: &BTreeMap<String, String>,
    proof: &FastNoopWatchProof,
) -> Result<Option<FastNoopHit>> {
    fast_noop::check_watch_proof(root, profile, platform, key_env, proof)
}

struct Shared {
    ready: BinaryHeap<(u64, Reverse<usize>)>,
    /// Remaining in-closure producer count per local action.
    waiting: Vec<usize>,
    outcomes: Vec<Option<Outcome>>,
    pending: usize,
    abort: bool,
}

struct CommandBatch {
    captured: String,
    failure: Option<(Vec<String>, String)>,
}

pub struct Engine<'a> {
    root: &'a Path,
    graph: &'a BuildGraph,
    /// Closure in deterministic order; all indices below are into this.
    closure: Vec<ActionId>,
    closure_index: HashMap<ActionId, usize>,
    /// Local indices of in-closure dependents, per local action.
    dependents: Vec<Vec<usize>>,
    /// Ready-queue key per local action: estimated longest remaining chain
    /// (zero under the FIFO scheduler).
    priority: Vec<u64>,
    /// Estimated makespan lower bound and total work, for the stats report.
    critical_path_ms: u64,
    critical_path: BTreeSet<usize>,
    critical_path_labels: Vec<String>,
    /// The same chain as action ids, kept for the finished report. Only the
    /// actions on the chain, so this costs nothing on a wide graph.
    critical_path_ids: Vec<String>,
    estimated_work_ms: u64,
    toolchain_hash: String,
    /// Output-affecting environment captured once per invocation. Looking up
    /// the same handful of variables for every action is surprisingly visible
    /// in a 10k-action no-op build.
    key_env: BTreeMap<String, String>,
    command_env: Vec<(OsString, OsString)>,
    opts: BuildOptions,
    cache: HashCache,
    /// Entries recorded by the *previous* build. Immutable for the duration
    /// of this one — an action only ever consults its own entry, and never a
    /// record written by this run — so the check path reads it without a lock.
    previous: Journal,
    /// Records produced by this build, appended under a lock.
    journal: Mutex<Journal>,
    shared: Mutex<Shared>,
    cv: Condvar,
    cas: LocalCas,
}

impl<'a> Engine<'a> {
    pub fn new(
        root: &'a Path,
        graph: &'a BuildGraph,
        closure: Vec<ActionId>,
        toolchain_hash: String,
        opts: BuildOptions,
    ) -> Self {
        // Neither store depends on the other. Loading them concurrently keeps
        // the warm path bounded by the larger decode instead of their sum.
        let (journal, cache) = std::thread::scope(|scope| {
            let journal = scope.spawn(|| Journal::load(root));
            let cache = HashCache::load(root);
            (
                journal.join().expect("journal loader should not panic"),
                cache,
            )
        });
        let n = closure.len();
        let cas_max_bytes = opts.cas_max_bytes;
        let key_env = key_environment_snapshot();
        let command_env = ENV_PASSTHROUGH
            .iter()
            .chain(ENV_IN_KEY)
            .filter_map(|name| std::env::var_os(name).map(|value| (OsString::from(name), value)))
            .collect();
        Self {
            root,
            graph,
            closure,
            closure_index: HashMap::new(),
            dependents: Vec::new(),
            priority: Vec::new(),
            critical_path_ms: 0,
            critical_path: BTreeSet::new(),
            critical_path_labels: Vec::new(),
            critical_path_ids: Vec::new(),
            estimated_work_ms: 0,
            toolchain_hash,
            key_env,
            command_env,
            opts,
            cache,
            previous: journal,
            journal: Mutex::new(Journal::default()),
            shared: Mutex::new(Shared {
                ready: BinaryHeap::new(),
                waiting: vec![0; n],
                outcomes: vec![None; n],
                pending: n,
                abort: false,
            }),
            cv: Condvar::new(),
            cas: LocalCas::new(root, cas_max_bytes),
        }
    }

    pub fn run(mut self) -> Result<BuildReport> {
        let workers = self.opts.jobs.max(1).min(self.closure.len().max(1));
        let progress = self.opts.progress.clone();
        let started = std::time::Instant::now();
        if self.all_cached().unwrap_or(false) {
            if let Some(progress) = &progress {
                progress.emit(ProgressEvent::BuildStarted {
                    total: self.closure.len(),
                    jobs: workers,
                    critical_path_ms: 0,
                    critical_path: Vec::new(),
                });
            }
            {
                let mut shared = self.shared.lock().unwrap();
                shared.outcomes.fill(Some(Outcome::Cached));
                shared.pending = 0;
                shared.ready.clear();
            }
            if let Some(progress) = &progress {
                progress.emit(ProgressEvent::AllCached {
                    total: self.closure.len(),
                });
            }
        } else {
            if !self.opts.dry_run {
                self.prepare_output_dirs()?;
            }
            self.prepare_schedule();
            if let Some(progress) = &progress {
                progress.emit(ProgressEvent::BuildStarted {
                    total: self.closure.len(),
                    jobs: workers,
                    critical_path_ms: self.critical_path_ms,
                    critical_path: std::mem::take(&mut self.critical_path_labels),
                });
            }
            std::thread::scope(|scope| {
                let engine = &self;
                for slot in 0..workers {
                    scope.spawn(move || engine.worker(slot));
                }
            });
        }
        let makespan_ms = started.elapsed().as_millis() as u64;

        let shared = self.shared.into_inner().unwrap();
        if !self.opts.dry_run {
            let recorded = self.journal.into_inner().unwrap();
            let journal_path = self.root.join(frostbuild_core::journal::JOURNAL_REL_PATH);
            if std::fs::metadata(journal_path).is_ok_and(|m| m.len() > 32 * 1024 * 1024) {
                // Compaction rewrites the whole file, so it must carry the
                // entries this build did not touch as well as the new ones.
                let mut compacted = self.previous;
                compacted.actions.extend(recorded.actions);
                compacted.save(self.root)?;
            }
            let _ = self.cas.gc()?;
        }
        self.cache.save(self.root)?;

        let mut results = Vec::with_capacity(self.closure.len());
        for (local, &action_id) in self.closure.iter().enumerate() {
            let action = &self.graph.actions[action_id];
            let outcome = shared.outcomes[local].clone().unwrap_or(Outcome::Skipped {
                reason: "not run (earlier failure aborted the build)".into(),
            });
            results.push(ActionResult {
                id: action.id.clone(),
                desc: action.desc.clone(),
                kind: action.kind,
                target: action.target.clone(),
                outcome,
            });
        }
        let (busy_ms, executed) =
            results
                .iter()
                .fold((0u64, 0usize), |(b, n), r| match r.outcome {
                    Outcome::Executed { duration_ms, .. } | Outcome::Flaky { duration_ms, .. } => {
                        (b + duration_ms, n + 1)
                    }
                    _ => (b, n),
                });
        let stats = BuildStats {
            scheduler: self.opts.scheduler.as_str(),
            estimator: self.opts.estimator.as_str(),
            jobs: workers,
            makespan_ms,
            busy_ms,
            critical_path_ms: self.critical_path_ms,
            estimated_work_ms: self.estimated_work_ms,
            executed,
        };
        let report = BuildReport {
            results,
            stats,
            critical_path: std::mem::take(&mut self.critical_path_ids),
        };
        if let Some(progress) = progress {
            progress.emit(ProgressEvent::BuildFinished {
                success: report.success(),
                elapsed_ms: makespan_ms,
            });
        }
        Ok(report)
    }

    /// Scheduling data is irrelevant when a whole closure is cached. Delay
    /// its O(actions + edges) allocation until the cache preflight finds work.
    fn prepare_schedule(&mut self) {
        let plan = Schedule::plan(
            self.graph,
            self.closure.clone(),
            &self.previous,
            self.opts.scheduler,
            self.opts.estimator,
        );
        // The ids are always carried: they are one string per action on the
        // chain, not per action in the closure, and the finished report needs
        // them whether or not anyone was watching the build happen.
        self.critical_path_ids = plan
            .critical_path
            .iter()
            .map(|&local| self.graph.actions[self.closure[local]].id.clone())
            .collect();
        if self.opts.progress.is_some() {
            self.critical_path = plan.critical_path.iter().copied().collect();
            self.critical_path_labels = plan
                .critical_path
                .iter()
                .map(|&local| self.graph.actions[self.closure[local]].desc.clone())
                .collect();
        } else {
            self.critical_path.clear();
            self.critical_path_labels.clear();
        }
        self.closure_index = plan.closure_index;
        self.dependents = plan.dependents;
        self.priority = plan.priority;
        self.critical_path_ms = plan.critical_path_ms;
        self.estimated_work_ms = plan.work_ms;
        let mut shared = self.shared.lock().unwrap();
        shared.waiting = plan.waiting;
        shared.ready = shared
            .waiting
            .iter()
            .enumerate()
            .filter(|(_, &waiting)| waiting == 0)
            .map(|(local, _)| (self.priority[local], Reverse(local)))
            .collect();
    }

    /// Validate a fully cached closure in two passes instead of sending every
    /// action through the scheduler. The normal path stats the same output as
    /// one action's output and the next action's input, and takes the shared
    /// scheduler lock for every cached node. A workspace-wide pass stats each
    /// unique path once, then verifies the exact same action keys and output
    /// digests before declaring the closure cached.
    fn all_cached(&self) -> Result<bool> {
        if self.opts.dry_run {
            return Ok(false);
        }

        let mut expected_by_file = vec![None; self.graph.files.len()];
        let mut discovered_expected = HashMap::new();
        for &action_id in &self.closure {
            let action = &self.graph.actions[action_id];
            // Same reasoning as `no_cache`, and it has to be repeated here:
            // this pass declares the whole closure cached before the scheduler
            // ever sees an action, so a check that lives only in the per-action
            // path never runs.
            if action.kind == ActionKind::Test
                && (self.opts.no_cache || self.opts.runs_per_test > 1)
            {
                return Ok(false);
            }
            if !action.volatile_stamps.is_empty() {
                return Ok(false);
            }
            let Some(previous) = self.previous.actions.get(&journal_id(self.graph, action)) else {
                return Ok(false);
            };

            // Reusing `previous.inputs` for the key is valid only when the
            // current declared-input set is identical. Discovered inputs are
            // explicitly recorded, so they can be separated without changing
            // the journal format.
            if previous.discovered.is_empty() {
                if action.inputs.len() != previous.inputs.len()
                    || action
                        .inputs
                        .iter()
                        .any(|&file| !previous.inputs.contains_key(&self.graph.files[file].path))
                {
                    return Ok(false);
                }
            } else {
                let discovered: BTreeSet<&str> =
                    previous.discovered.iter().map(String::as_str).collect();
                let previous_declared: BTreeSet<&str> = previous
                    .inputs
                    .keys()
                    .map(String::as_str)
                    .filter(|path| !discovered.contains(path))
                    .collect();
                let current_declared: BTreeSet<&str> = action
                    .inputs
                    .iter()
                    .map(|&file| self.graph.files[file].path.as_str())
                    .collect();
                if current_declared != previous_declared {
                    return Ok(false);
                }
            }

            for &file in &action.inputs {
                let path = &self.graph.files[file].path;
                let Some(digest) = previous.inputs.get(path) else {
                    return Ok(false);
                };
                if expected_by_file[file]
                    .replace(digest.as_str())
                    .is_some_and(|other| other != digest)
                {
                    return Ok(false);
                }
            }
            for path in &previous.discovered {
                let Some(digest) = previous.inputs.get(path) else {
                    return Ok(false);
                };
                if discovered_expected
                    .insert(path.as_str(), digest.as_str())
                    .is_some_and(|other| other != digest)
                {
                    return Ok(false);
                }
            }
            if !self.recorded_outputs_match(action, previous) {
                return Ok(false);
            }
            for &file in &action.outputs {
                let path = &self.graph.files[file].path;
                let Some(digest) = previous.outputs.get(path) else {
                    return Ok(false);
                };
                if expected_by_file[file]
                    .replace(digest.as_str())
                    .is_some_and(|other| other != digest)
                {
                    return Ok(false);
                }
            }
            // Files inside an owned directory are recorded outputs without
            // being graph files, so they are checked alongside discovered
            // inputs rather than through the per-file slots.
            if !action.output_dirs.is_empty() {
                for (path, digest) in &previous.outputs {
                    if !action
                        .output_dirs
                        .iter()
                        .any(|directory| path_is_inside(directory, path))
                    {
                        continue;
                    }
                    if discovered_expected
                        .insert(path.as_str(), digest.as_str())
                        .is_some_and(|other| other != digest)
                    {
                        return Ok(false);
                    }
                }
            }
        }

        let mut expected = Vec::with_capacity(
            expected_by_file
                .len()
                .saturating_add(discovered_expected.len()),
        );
        expected.extend(
            self.graph
                .files
                .iter()
                .zip(expected_by_file)
                .filter_map(|(file, digest)| digest.map(|digest| (file.path.as_str(), digest))),
        );
        expected.extend(discovered_expected);
        let (files_match, keys_match) = rayon::join(
            || self.cache.matches_many(self.root, &expected),
            || {
                self.closure.par_iter().all(|&action_id| {
                    let action = &self.graph.actions[action_id];
                    let previous = &self.previous.actions[&journal_id(self.graph, action)];
                    self.action_key(action, &previous.inputs) == previous.key
                })
            },
        );
        let files_match = files_match?;
        let cached = files_match && keys_match;
        if cached && self.opts.write_fast_noop {
            let dynamic_env = self
                .closure
                .iter()
                .flat_map(|&action_id| self.graph.actions[action_id].pass_env.iter())
                .map(|name| {
                    (
                        name.clone(),
                        std::env::var_os(name).map(|value| value.to_string_lossy().into_owned()),
                    )
                })
                .collect();
            let _ = fast_noop::save(
                fast_noop::CertificateInput {
                    root: self.root,
                    profile: &self.graph.profile,
                    platform: &self.graph.platform,
                    closure_actions: self.closure.len(),
                    graph_actions: self.graph.actions.len(),
                    toolchain: &self.graph.toolchain,
                    toolchain_hash: &self.toolchain_hash,
                    key_env: &self.key_env,
                    dynamic_env: &dynamic_env,
                    paths: &expected,
                },
                || self.cache.matches_many(self.root, &expected),
            );
        }
        Ok(cached)
    }

    fn worker(&self, slot: usize) {
        let mut continuation = None;
        loop {
            let local = if let Some(local) = continuation.take() {
                local
            } else {
                let mut s = self.shared.lock().unwrap();
                loop {
                    if s.abort && s.ready.is_empty() {
                        return;
                    }
                    if let Some((_, Reverse(i))) = s.ready.pop() {
                        break i;
                    }
                    if s.pending == 0 {
                        return;
                    }
                    s = self.cv.wait(s).unwrap();
                }
            };

            let action = &self.graph.actions[self.closure[local]];
            let critical = self.opts.progress.is_some() && self.critical_path.contains(&local);
            if let Some(progress) = &self.opts.progress {
                progress.emit(ProgressEvent::ActionStarted {
                    slot,
                    id: action.id.clone(),
                    desc: action.desc.clone(),
                    command: shell_join(&action.argv),
                    critical,
                });
            }
            let action_started = self.opts.progress.as_ref().map(|_| Instant::now());
            let outcome = self.process(local);
            let elapsed_ms = action_started
                .map(|started| started.elapsed().as_millis() as u64)
                .unwrap_or(0);
            let progress_result = self.opts.progress.as_ref().map(|_| match &outcome {
                Outcome::Executed { duration_ms, .. } => {
                    (ProgressState::Executed, *duration_ms, String::new())
                }
                // Green in the progress display, because it is: the stamp
                // exists and dependents may run. The summary is where the
                // flake is named, so a passing build does not read as a
                // failing one.
                Outcome::Flaky {
                    duration_ms,
                    attempts,
                    ..
                } => (
                    ProgressState::Flaky,
                    *duration_ms,
                    format!("passed on attempt {attempts}"),
                ),
                Outcome::Cached => (ProgressState::CacheHit, elapsed_ms, String::new()),
                Outcome::Failed { detail, .. } => {
                    (ProgressState::Failed, elapsed_ms, detail.clone())
                }
                Outcome::Skipped { reason } => (ProgressState::Skipped, elapsed_ms, reason.clone()),
                Outcome::WouldRun { reason } => {
                    (ProgressState::WouldRun, elapsed_ms, reason.clone())
                }
                Outcome::MayRun { reason } => (ProgressState::MayRun, elapsed_ms, reason.clone()),
            });

            let mut s = self.shared.lock().unwrap();
            let failed = matches!(outcome, Outcome::Failed { .. });
            s.outcomes[local] = Some(outcome);
            s.pending -= 1;
            let completed = self.closure.len() - s.pending;
            if failed && !self.opts.keep_going {
                s.abort = true;
                s.ready.clear();
            }
            let mut unlocked = 0usize;
            if !s.abort {
                for &dep in &self.dependents[local] {
                    s.waiting[dep] -= 1;
                    if s.waiting[dep] == 0 {
                        let priority = self.priority(dep);
                        s.ready.push((priority, Reverse(dep)));
                        unlocked += 1;
                    }
                }
            }
            let finished = s.pending == 0 || s.abort;
            // The worker that just made an action ready is already awake.
            // Let it claim the highest-priority next action while holding the
            // scheduler lock. On a dependency chain this avoids 10k kernel
            // wakeups and ready-heap push/pop handoffs between workers.
            if !finished {
                continuation = s.ready.pop().map(|(_, Reverse(local))| local);
            }
            let claimed = usize::from(continuation.is_some());
            drop(s);
            if let (Some(progress), Some((state, duration_ms, detail))) =
                (&self.opts.progress, progress_result)
            {
                progress.emit(ProgressEvent::ActionFinished {
                    slot,
                    completed,
                    total: self.closure.len(),
                    id: action.id.clone(),
                    desc: action.desc.clone(),
                    state,
                    duration_ms,
                    detail,
                    critical,
                });
            }
            if finished {
                // Everyone must wake to observe the end and return.
                self.cv.notify_all();
            } else {
                // Wake one worker per action that became runnable. Waking all
                // of them would send every idle worker to an empty queue: a
                // dependency chain unlocks one action at a time, so on a chain
                // of N actions `notify_all` costs N * jobs wakeups to do N
                // units of work.
                for _ in 0..unlocked.saturating_sub(claimed) {
                    self.cv.notify_one();
                }
            }
        }
    }

    fn process(&self, local: usize) -> Outcome {
        let action = &self.graph.actions[self.closure[local]];

        // Upstream state: producers finished before we became ready.
        let mut upstream_dirty: Option<String> = None;
        {
            let s = self.shared.lock().unwrap();
            for &input in action.inputs.iter().chain(&action.order_only_inputs) {
                let Some(p) = self.graph.files[input].producer else {
                    continue;
                };
                let Some(&plocal) = self.closure_index.get(&p) else {
                    continue;
                };
                match &s.outcomes[plocal] {
                    Some(Outcome::Failed { .. }) | Some(Outcome::Skipped { .. }) => {
                        return Outcome::Skipped {
                            reason: format!(
                                "upstream failed: {}",
                                self.graph.actions[self.closure[plocal]].id
                            ),
                        };
                    }
                    Some(Outcome::WouldRun { .. }) | Some(Outcome::MayRun { .. }) => {
                        upstream_dirty = Some(self.graph.actions[self.closure[plocal]].id.clone());
                    }
                    _ => {}
                }
            }
        }
        if let Some(upstream) = upstream_dirty {
            // Dry run only: inputs on disk are stale, so no honest key exists.
            return Outcome::MayRun {
                reason: format!("depends on output of {upstream}, which would run"),
            };
        }

        let previous = self.previous.actions.get(&journal_id(self.graph, action));

        // Declared inputs + inputs discovered by the previous run's depfile.
        let mut input_paths: Vec<String> = action
            .inputs
            .iter()
            .map(|&f| self.graph.files[f].path.clone())
            .collect();
        if let Some(prev) = &previous {
            for d in &prev.discovered {
                if !input_paths.contains(d) {
                    input_paths.push(d.clone());
                }
            }
        }

        let inputs = match self.digest_all(&input_paths) {
            Ok(m) => m,
            Err(err) => {
                return Outcome::Failed {
                    reason: "failed to hash inputs".into(),
                    detail: format!("{err:#}"),
                }
            }
        };
        let key = self.action_key(action, &inputs);

        if self.opts.no_cache && action.kind == ActionKind::Test {
            return self.execute(local, action, inputs, "test cache disabled".into());
        }

        // A recorded result was produced with the previous build's volatile
        // values. Reusing it would ship a binary stamped with a build time
        // that is not this build's, which is the one thing the stamp was for.
        // Confined to this action: nothing downstream reruns unless the bytes
        // this produces actually differ.
        if !action.volatile_stamps.is_empty() {
            return self.execute(local, action, inputs, "volatile stamp value".into());
        }

        // A recorded pass says the test passed once. It cannot answer "does it
        // pass N times", so asking that question has to run.
        if self.opts.runs_per_test > 1 && action.kind == ActionKind::Test {
            return self.execute(
                local,
                action,
                inputs,
                format!("running {} times", self.opts.runs_per_test),
            );
        }

        if let Some(prev) = &previous {
            if prev.key == key && self.recorded_outputs_match(action, prev) {
                match self.outputs_intact(prev) {
                    Ok(None) => return Outcome::Cached,
                    Ok(Some(bad)) => {
                        if self.restore_outputs(action, prev).unwrap_or(false) {
                            return Outcome::Cached;
                        }
                        return self.execute(
                            local,
                            action,
                            inputs,
                            format!("output missing or modified: {bad}"),
                        );
                    }
                    Err(err) => {
                        return Outcome::Failed {
                            reason: "failed to hash outputs".into(),
                            detail: format!("{err:#}"),
                        }
                    }
                }
            }
            let reason = explain_key_change(prev, &inputs);
            if let Some(outcome) = self.try_remote(action, &inputs) {
                return outcome;
            }
            return self.execute(local, action, inputs, reason);
        }

        if let Some(outcome) = self.try_remote(action, &inputs) {
            return outcome;
        }
        self.execute(local, action, inputs, "not built before".into())
    }

    fn execute(
        &self,
        local: usize,
        action: &frostbuild_core::graph::ActionNode,
        mut inputs: BTreeMap<String, String>,
        reason: String,
    ) -> Outcome {
        let _ = local;
        if self.opts.dry_run {
            return Outcome::WouldRun { reason };
        }
        // Raw terminal mode turns Ctrl-C into an input event instead of a
        // signal. That event can arrive after scheduling but before this
        // action has spawned; do not delete outputs or start new work once
        // cancellation has already been requested.
        if was_cancelled() {
            return Outcome::Failed {
                reason: "build cancelled".into(),
                detail: "cancelled before action start".into(),
            };
        }
        if let Some(progress) = &self.opts.progress {
            progress.emit(ProgressEvent::ActionRunning {
                id: action.id.clone(),
            });
        }

        for &out in &action.outputs {
            let path = &self.graph.files[out].path;
            self.cache.invalidate(path);
            if !action.preserve_outputs {
                let _ = std::fs::remove_file(self.root.join(path));
            }
        }
        if !action.preserve_outputs {
            // The recorded tree must be exactly what this run produced, so a
            // file the previous run left behind cannot linger into it.
            self.discard_output_dirs(action);
        }
        if let Err(err) = self.reset_clean_dirs(action) {
            return Outcome::Failed {
                reason,
                detail: format!("failed to reset command intermediates: {err:#}"),
            };
        }

        let runs = if action.kind == ActionKind::Test {
            self.opts.runs_per_test.max(1)
        } else {
            1
        };
        // Hunting for a flake and hiding one are opposite tools. Asking for N
        // runs turns retries off, or each run would paper over its own failure
        // and the repetition would prove nothing.
        let retries = if runs > 1 { 0 } else { action.flaky_retries };

        let started = Instant::now();
        let mut batch = match self.run_action_commands(action, &inputs) {
            Ok(batch) => batch,
            Err(err) => {
                return Outcome::Failed {
                    reason,
                    detail: err,
                }
            }
        };

        // A test that declared `flaky_retries` gets that many more attempts
        // before the failure is the verdict. Each retry starts from the same
        // state a first attempt would: a partial stamp from the failed run
        // must not be mistaken for success, and a clean directory the test
        // dirtied must be reset, or attempt two runs in a world attempt one
        // left behind.
        let mut attempts = 1;
        while batch.failure.is_some()
            && action.kind == ActionKind::Test
            && attempts <= retries
            && !was_cancelled()
        {
            attempts += 1;
            self.remove_partial_outputs(action);
            if let Err(err) = self.reset_clean_dirs(action) {
                return Outcome::Failed {
                    reason,
                    detail: format!("failed to reset intermediates before retry: {err:#}"),
                };
            }
            batch = match self.run_action_commands(action, &inputs) {
                Ok(next) => next,
                Err(err) => {
                    return Outcome::Failed {
                        reason,
                        detail: err,
                    }
                }
            };
        }
        let flaky = attempts > 1 && batch.failure.is_none();

        // The remaining runs. Every one must pass, and each starts from the
        // state the first did, or run two would inherit whatever run one left.
        let mut completed = 1;
        while batch.failure.is_none() && completed < runs && !was_cancelled() {
            completed += 1;
            self.remove_partial_outputs(action);
            if let Err(err) = self.reset_clean_dirs(action) {
                return Outcome::Failed {
                    reason,
                    detail: format!("failed to reset intermediates before rerun: {err:#}"),
                };
            }
            batch = match self.run_action_commands(action, &inputs) {
                Ok(next) => next,
                Err(err) => {
                    return Outcome::Failed {
                        reason,
                        detail: err,
                    }
                }
            };
        }

        if let Some((argv, exit)) = batch.failure {
            self.remove_partial_outputs(action);
            let detail = format!(
                "command: {}\nexit: {}\n{}{}",
                shell_join(&argv),
                exit,
                if runs > 1 {
                    // Which run failed is the whole result of asking for N of
                    // them: failing on run 7 of 10 is a flake, failing on run
                    // 1 is a broken test.
                    format!("failed on run {completed} of {runs}\n")
                } else if attempts > 1 {
                    // Without this the log shows one failure for what was
                    // several runs, and the retries look like they never
                    // happened.
                    format!("failed all {attempts} attempts\n")
                } else {
                    String::new()
                },
                batch.captured.trim_end()
            );
            return Outcome::Failed { reason, detail };
        }
        if action.kind == ActionKind::Test {
            if let Err(err) = self.write_test_success_outputs(action) {
                self.remove_partial_outputs(action);
                return Outcome::Failed {
                    reason,
                    detail: format!("failed to record test success: {err:#}"),
                };
            }
        }
        let duration_ms = started.elapsed().as_millis() as u64;
        let mut captured = batch.captured;

        // Ingest the dependency report: replace previous discovered deps with
        // fresh ones and fold their digests into the recorded key. Most tools
        // write a file; MSVC writes its includes to stdout, so that report is
        // taken from the captured output and removed from the build log, which
        // would otherwise carry the whole include tree on every rebuild.
        let mut discovered = Vec::new();
        let report = if action.depfile_format.reads_captured_output() {
            let text = captured.clone();
            captured = depfile::strip_showincludes(&captured);
            Some((action.depfile_format.as_str().to_string(), Some(text)))
        } else {
            action.depfile.as_ref().map(|dep_rel| {
                (
                    dep_rel.clone(),
                    std::fs::read_to_string(self.root.join(dep_rel)).ok(),
                )
            })
        };
        if let Some((source, text)) = report {
            if let Some(text) = text {
                match depfile::parse_format(action.depfile_format, &text, self.root) {
                    Ok(deps) => discovered = deps,
                    Err(err) => {
                        let detail = format!("failed to parse depfile {source}: {err:#}");
                        return Outcome::Failed { reason, detail };
                    }
                }
            }
            let declared: BTreeSet<String> = action
                .inputs
                .iter()
                .map(|&f| self.graph.files[f].path.clone())
                .collect();
            discovered.retain(|d| !declared.contains(d));
            inputs.retain(|path, _| declared.contains(path));
            match self.digest_all(&discovered) {
                Ok(extra) => inputs.extend(extra),
                Err(err) => {
                    return Outcome::Failed {
                        reason,
                        detail: format!("failed to hash discovered deps: {err:#}"),
                    }
                }
            }
        };

        let mut output_paths: Vec<String> = action
            .outputs
            .iter()
            .map(|&f| self.graph.files[f].path.clone())
            .collect();
        // Owned directories are scanned after the command ran, so their file
        // names never had to be predicted. From here they are ordinary
        // recorded outputs: digested, published to the CAS, and restored.
        match self.record_output_dirs(action) {
            Ok(tree) => output_paths.extend(tree),
            Err(err) => {
                return Outcome::Failed {
                    reason,
                    detail: format!("{err:#}"),
                }
            }
        }
        let outputs = match self.digest_all(&output_paths) {
            Ok(m) => m,
            Err(err) => {
                return Outcome::Failed {
                    reason,
                    detail: format!("failed to hash outputs: {err:#}"),
                }
            }
        };
        if let Some(missing) = outputs
            .iter()
            .find(|(_, h)| h.as_str() == frostbuild_core::hashcache::MISSING)
        {
            let detail = format!(
                "command succeeded but declared output {} was not created",
                missing.0
            );
            return Outcome::Failed { reason, detail };
        }

        // A compiler/code generator may publish hundreds of small outputs.
        // CAS objects are independent, so serial copy+rename publication makes
        // post-processing dominate the action itself. Deduplicate by digest
        // first (parallel writers for identical bytes would share a temp name),
        // then publish distinct immutable objects concurrently.
        let unique_outputs: BTreeMap<&str, &str> = outputs
            .iter()
            .map(|(path, digest)| (digest.as_str(), path.as_str()))
            .collect();
        if let Err(err) = unique_outputs
            .par_iter()
            .try_for_each(|(digest, path)| self.cas.put(&self.root.join(path), digest))
        {
            return Outcome::Failed {
                reason,
                detail: format!("failed to store output in CAS: {err:#}"),
            };
        }

        if self.opts.check_determinism {
            if let Some(path) = inputs.keys().find(|path| {
                std::fs::read_to_string(self.root.join(path))
                    .is_ok_and(|text| text.contains("__TIME__") || text.contains("__DATE__"))
            }) {
                let detail = format!(
                    "non-deterministic action {}: {} uses __DATE__/__TIME__; outputs: {}",
                    action.id,
                    path,
                    output_paths.join(", ")
                );
                return Outcome::Failed {
                    reason: "determinism check failed".into(),
                    detail,
                };
            }
            let first = outputs.clone();
            if let Err(err) = self.reset_clean_dirs(action) {
                return Outcome::Failed {
                    reason,
                    detail: format!("determinism rerun setup failed: {err:#}"),
                };
            }
            if action.kind == ActionKind::Test {
                self.remove_partial_outputs(action);
            }
            let second = match self.run_action_commands(action, &inputs) {
                Ok(batch) => batch,
                Err(err) => {
                    return Outcome::Failed {
                        reason,
                        detail: format!("determinism rerun failed: {err}"),
                    }
                }
            };
            if let Some((argv, exit)) = second.failure {
                return Outcome::Failed {
                    reason,
                    detail: format!(
                        "determinism rerun failed: {} ({exit})\n{}",
                        shell_join(&argv),
                        second.captured.trim_end()
                    ),
                };
            }
            if action.kind == ActionKind::Test {
                if let Err(err) = self.write_test_success_outputs(action) {
                    return Outcome::Failed {
                        reason,
                        detail: format!("determinism rerun success record failed: {err:#}"),
                    };
                }
            }
            for path in &output_paths {
                self.cache.invalidate(path);
            }
            // A rerun may name its tree files differently, which is itself
            // non-determinism: rescan rather than re-digesting the first set,
            // so an added or renamed file is compared instead of missed.
            let mut output_paths = output_paths.clone();
            if !action.output_dirs.is_empty() {
                output_paths.retain(|path| {
                    action
                        .output_dirs
                        .iter()
                        .all(|directory| !path.starts_with(&format!("{directory}/")))
                });
                match self.record_output_dirs(action) {
                    Ok(tree) => output_paths.extend(tree),
                    Err(err) => {
                        return Outcome::Failed {
                            reason,
                            detail: format!("determinism rerun output scan failed: {err:#}"),
                        }
                    }
                }
            }
            let second_outputs = match self.digest_all(&output_paths) {
                Ok(value) => value,
                Err(err) => {
                    return Outcome::Failed {
                        reason,
                        detail: format!("determinism output hash failed: {err:#}"),
                    }
                }
            };
            if first != second_outputs {
                let changed = first
                    .iter()
                    .filter_map(|(path, hash)| {
                        (second_outputs.get(path) != Some(hash)).then_some(path.clone())
                    })
                    .collect::<Vec<_>>();
                let detail = format!(
                    "non-deterministic action {} produced different output: {}",
                    action.id,
                    changed.join(", ")
                );
                return Outcome::Failed {
                    reason: "determinism check failed".into(),
                    detail,
                };
            }
        }

        if flaky {
            // The stamp is written, so the build is green and dependents may
            // proceed — but nothing is recorded, locally or remotely. A result
            // the test only reached on a later attempt is not evidence that it
            // passes, and caching it would hide the flake from every build
            // after this one, including the run that would have caught it.
            if let Some(progress) = &self.opts.progress {
                progress.emit(ProgressEvent::ActionOutput {
                    id: action.id.clone(),
                    output: captured,
                });
            }
            return Outcome::Flaky {
                reason,
                duration_ms,
                attempts,
            };
        }

        let key = self.action_key(action, &inputs);
        let entry = JournalEntry {
            key,
            inputs,
            discovered,
            outputs,
            duration_ms,
            reason: reason.clone(),
        };
        // Other workspaces can reuse this only once it is recorded here, and
        // only if publication cannot affect this build: every upload failure is
        // counted and ignored.
        self.remote_publish(action, &entry);
        {
            let mut journal = self.journal.lock().unwrap();
            if let Err(err) = journal.record(self.root, journal_id(self.graph, action), entry) {
                return Outcome::Failed {
                    reason,
                    detail: format!("failed to flush journal: {err:#}"),
                };
            }
        }

        if let Some(progress) = &self.opts.progress {
            progress.emit(ProgressEvent::ActionOutput {
                id: action.id.clone(),
                output: captured,
            });
        }
        Outcome::Executed {
            reason,
            duration_ms,
        }
    }

    fn action_key(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
    ) -> String {
        let mut action_environment = None;
        if !action.env.is_empty() || !action.pass_env.is_empty() {
            let mut environment = self.key_env.clone();
            environment.extend(action.env.clone());
            for name in &action.pass_env {
                if let Some(value) = std::env::var_os(name) {
                    environment.insert(name.clone(), value.to_string_lossy().into_owned());
                } else {
                    environment.remove(name);
                }
            }
            action_environment = Some(environment);
        }
        let environment = action_environment.as_ref().unwrap_or(&self.key_env);
        let argv = action_key_argv(action, self.opts.stamps.as_ref());
        streamed_action_key(
            StreamedActionDescriptor {
                builder: "frost-engine-v1",
                target: &action.id,
                argv: argv.as_ref(),
                cwd: ".",
                toolchain_hash: &self.toolchain_hash,
                output_dirs: &action.output_dirs,
            },
            environment,
            inputs,
            action
                .outputs
                .iter()
                .map(|&file| self.graph.files[file].path.as_str()),
        )
    }

    fn priority(&self, local: usize) -> u64 {
        self.priority[local]
    }
}

/// Windows resolves a relative program name before applying `current_dir`.
/// Make workspace-relative paths explicit while leaving bare tool names for
/// PATH lookup on every host.
///
/// Public because actions are not the only thing frost spawns from a workspace
/// path: the `[stamp]` command is spawned by the CLI and hits the same trap.
/// One copy, so a host that needs a different rule needs one change.
pub fn resolve_action_program(root: &Path, program: &str) -> PathBuf {
    let path = Path::new(program);
    if path.is_relative() && path.components().count() > 1 {
        root.join(path)
    } else {
        path.to_path_buf()
    }
}

/// Journal namespace for an action: host builds keep the historical
/// `id@profile` form; platform builds add the platform segment so each
/// (platform, profile) pair has an independent cache identity.
pub fn journal_id(graph: &BuildGraph, action: &frostbuild_core::graph::ActionNode) -> String {
    if graph.platform == frostbuild_core::manifest::HOST_PLATFORM {
        format!("{}@{}", action.id, graph.profile)
    } else {
        format!("{}@{}@{}", action.id, graph.platform, graph.profile)
    }
}

fn explain_key_change(prev: &JournalEntry, inputs: &BTreeMap<String, String>) -> String {
    for (path, digest) in inputs {
        match prev.inputs.get(path) {
            Some(old) if old != digest => return format!("input changed: {path}"),
            None => return format!("new input: {path}"),
            _ => {}
        }
    }
    for path in prev.inputs.keys() {
        if !inputs.contains_key(path) {
            return format!("input removed: {path}");
        }
    }
    "command or toolchain changed".into()
}

fn describe_exit(status: &std::process::ExitStatus) -> String {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return format!("signal {sig}");
        }
    }
    match status.code() {
        Some(code) => format!("code {code}"),
        None => "unknown".into(),
    }
}

fn shell_join(argv: &[String]) -> String {
    argv.iter()
        .map(|a| {
            if a.is_empty()
                || a.contains(|c: char| c.is_whitespace() || "'\"$&|;<>()`\\".contains(c))
            {
                format!("'{}'", a.replace('\'', "'\\''"))
            } else {
                a.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use std::borrow::Cow;
    use std::time::Duration;

    use super::*;
    use crate::toolchain::{ToolchainStamp, TOOLCHAIN_STAMP_PATH};

    #[test]
    fn shell_join_quotes_specials() {
        let argv = vec!["cc".to_string(), "a b".to_string(), "plain".to_string()];
        assert_eq!(shell_join(&argv), "cc 'a b' plain");
    }

    #[test]
    fn action_program_paths_are_resolved_but_bare_tools_still_use_path() {
        let root = Path::new("/workspace");
        assert_eq!(
            resolve_action_program(root, ".frost/bin/debug/test"),
            root.join(".frost/bin/debug/test")
        );
        assert_eq!(resolve_action_program(root, "cc"), PathBuf::from("cc"));
    }

    #[test]
    fn a_limit_comes_from_the_most_specific_statement_about_the_work() {
        use frostbuild_core::graph::ActionKind;

        // The target speaks for the work, so it wins over the invocation.
        assert_eq!(
            resolve_timeout(Some(7), Some(Duration::from_secs(60)), ActionKind::Compile),
            Some(Duration::from_secs(7))
        );
        // The invocation speaks for the environment, and covers every kind.
        assert_eq!(
            resolve_timeout(None, Some(Duration::from_secs(60)), ActionKind::Compile),
            Some(Duration::from_secs(60))
        );
        assert_eq!(
            resolve_timeout(None, Some(Duration::from_secs(60)), ActionKind::Test),
            Some(Duration::from_secs(60))
        );
        // A hanging test would otherwise hold a CI job open on its own, so it
        // is the one kind that carries a limit nobody asked for.
        assert_eq!(
            resolve_timeout(None, None, ActionKind::Test),
            Some(DEFAULT_TEST_TIMEOUT)
        );
        // Build actions stay unbounded by default: the watchdog costs a thread
        // per action, and a long link is not a hang.
        assert_eq!(resolve_timeout(None, None, ActionKind::Compile), None);
        assert_eq!(resolve_timeout(None, None, ActionKind::Link), None);
    }

    #[test]
    fn streamed_action_key_matches_the_canonical_core_key() {
        let root = Path::new("/workspace");
        let argv = vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "cp $in $out".to_string(),
        ];
        let env = BTreeMap::from([
            ("CPATH".to_string(), "/headers".to_string()),
            ("SDKROOT".to_string(), "/sdk".to_string()),
        ]);
        let inputs = BTreeMap::from([
            ("include/a.h".to_string(), "abc".to_string()),
            ("src/main.c".to_string(), "def".to_string()),
        ]);
        let outputs = vec!["out/main".to_string(), "out/main.map".to_string()];
        let output_dirs = vec!["out/tree".to_string()];
        let mut canonical = frostbuild_core::ActionKey::new(
            "frost-engine-v1",
            "compile:main",
            argv.clone(),
            root,
            "toolchain",
        );
        for (key, value) in &env {
            canonical = canonical.with_env(key, value);
        }
        for (path, digest) in &inputs {
            canonical = canonical.with_input(path, digest);
        }
        for path in &outputs {
            canonical = canonical.with_output(path);
        }
        for path in &output_dirs {
            canonical = canonical.with_output_dir(path);
        }
        assert_eq!(
            streamed_action_key(
                StreamedActionDescriptor {
                    builder: "frost-engine-v1",
                    target: "compile:main",
                    argv: &argv,
                    cwd: ".",
                    toolchain_hash: "toolchain",
                    output_dirs: &output_dirs,
                },
                &env,
                &inputs,
                outputs.iter().map(String::as_str),
            ),
            canonical.digest(root)
        );
    }

    /// The property the whole stable/volatile split rests on, pinned where it
    /// lives rather than through a build.
    ///
    /// End to end it is invisible: an action reading a volatile value is
    /// re-executed unconditionally, so whether the value is also in its key
    /// changes nothing you can observe. That is exactly why it needs a test.
    /// The day someone replaces the unconditional re-execution with ordinary
    /// key-based invalidation — a reasonable-looking simplification — a
    /// volatile value in the key would rebuild everything downstream on every
    /// build, and no other test in this repository would notice.
    #[test]
    fn a_volatile_stamp_value_stays_out_of_the_action_key_and_a_stable_one_does_not() {
        fn action(stable: &[&str], volatile: &[&str]) -> frostbuild_core::graph::ActionNode {
            frostbuild_core::graph::ActionNode {
                id: "command:release".into(),
                desc: "RUN release [sh]".into(),
                kind: ActionKind::Command,
                target: "release".into(),
                sandbox: false,
                argv: vec![
                    "sh".into(),
                    "-c".into(),
                    "echo ${stamp.STABLE_V} ${stamp.BUILD_TIME}".into(),
                ],
                followup_argv: Vec::new(),
                clean_dirs: Vec::new(),
                preserve_outputs: false,
                env: BTreeMap::new(),
                pass_env: Vec::new(),
                inputs: Vec::new(),
                order_only_inputs: Vec::new(),
                outputs: Vec::new(),
                output_dirs: Vec::new(),
                depfile: None,
                depfile_format: frostbuild_core::depfile::Format::Make,
                flaky_retries: 0,
                stable_stamps: stable.iter().map(|key| key.to_string()).collect(),
                volatile_stamps: volatile.iter().map(|key| key.to_string()).collect(),
            }
        }
        let key = |action: &frostbuild_core::graph::ActionNode,
                   stamps: &BTreeMap<String, String>| {
            action_key_argv(action, Some(stamps)).into_owned()
        };
        let node = action(&["STABLE_V"], &["BUILD_TIME"]);
        let first = BTreeMap::from([
            ("STABLE_V".to_string(), "1.0.0".to_string()),
            ("BUILD_TIME".to_string(), "100".to_string()),
        ]);
        let later_clock = BTreeMap::from([
            ("STABLE_V".to_string(), "1.0.0".to_string()),
            ("BUILD_TIME".to_string(), "999".to_string()),
        ]);
        let new_version = BTreeMap::from([
            ("STABLE_V".to_string(), "2.0.0".to_string()),
            ("BUILD_TIME".to_string(), "100".to_string()),
        ]);

        assert_eq!(
            key(&node, &first),
            key(&node, &later_clock),
            "a clock that moved must not change an action key"
        );
        assert_ne!(
            key(&node, &first),
            key(&node, &new_version),
            "a stable value that changed must change it"
        );
        // And the value that is key material is the value, not the reference:
        // keeping only the key name would make every version look alike.
        assert!(key(&node, &first).contains(&"1.0.0".to_string()));
        assert!(!key(&node, &first).contains(&"100".to_string()));
    }

    #[test]
    fn multi_step_commands_and_clean_dirs_are_unambiguous_key_material() {
        fn action(
            followup_argv: Vec<Vec<String>>,
            clean_dirs: Vec<String>,
            preserve_outputs: bool,
        ) -> frostbuild_core::graph::ActionNode {
            frostbuild_core::graph::ActionNode {
                id: "command:java".into(),
                desc: "RUN java [javac]".into(),
                kind: ActionKind::Command,
                target: "java".into(),
                sandbox: false,
                argv: vec!["javac".into(), "Hello.java".into()],
                followup_argv,
                clean_dirs,
                preserve_outputs,
                env: BTreeMap::new(),
                pass_env: Vec::new(),
                inputs: Vec::new(),
                order_only_inputs: Vec::new(),
                outputs: Vec::new(),
                output_dirs: Vec::new(),
                depfile: None,
                depfile_format: frostbuild_core::depfile::Format::Make,
                flaky_retries: 0,
                stable_stamps: Vec::new(),
                volatile_stamps: Vec::new(),
            }
        }
        let digest = |action: &frostbuild_core::graph::ActionNode| {
            streamed_action_key(
                StreamedActionDescriptor {
                    builder: "frost-engine-v1",
                    target: &action.id,
                    argv: action_key_argv(action, None).as_ref(),
                    cwd: ".",
                    toolchain_hash: "toolchain",
                    output_dirs: &action.output_dirs,
                },
                &BTreeMap::new(),
                &BTreeMap::new(),
                std::iter::empty(),
            )
        };

        let primary_only = action(Vec::new(), Vec::new(), false);
        assert!(matches!(
            action_key_argv(&primary_only, None),
            Cow::Borrowed(_)
        ));
        let jar = action(
            vec![vec!["jar".into(), "classes".into()]],
            vec![".frost/tmp/debug/java".into()],
            false,
        );
        let differently_segmented = action(
            vec![vec!["jar".into()], vec!["classes".into()]],
            vec![".frost/tmp/debug/java".into()],
            false,
        );
        let different_clean_dir = action(
            vec![vec!["jar".into(), "classes".into()]],
            vec![".frost/tmp/debug/java-v2".into()],
            false,
        );
        let preserving = action(Vec::new(), Vec::new(), true);

        assert_ne!(digest(&primary_only), digest(&jar));
        assert_ne!(digest(&jar), digest(&differently_segmented));
        assert_ne!(digest(&jar), digest(&different_clean_dir));
        assert_ne!(digest(&primary_only), digest(&preserving));
    }

    #[test]
    fn the_fingerprint_covers_the_shell_frost_chooses() {
        // Every genrule and shell test runs through this interpreter, and the
        // manifest has no way to name it, so leaving it out would make it the
        // one tool frost picks and does not account for.
        let dir = std::env::temp_dir().join(format!("frost-tc-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("tools")).unwrap();
        std::fs::write(dir.join("tools/kofun"), b"kofun compiler v1\n").unwrap();
        std::fs::write(dir.join("tools/language"), b"language adapter v1\n").unwrap();
        let mut named_tools = BTreeMap::new();
        named_tools.insert("language".into(), "tools/language".into());
        let toolchain = frostbuild_core::manifest::Toolchain {
            cc: frostbuild_core::graph::SHELL.into(),
            cxx: frostbuild_core::graph::SHELL.into(),
            ar: frostbuild_core::graph::SHELL.into(),
            kofunc: Some("tools/kofun".into()),
            tools: named_tools,
            arflags: vec!["rcsD".into()],
            cflags: Vec::new(),
            cxxflags: Vec::new(),
            ldflags: Vec::new(),
        };
        let first = toolchain_closure_fingerprint_cached(&dir, &toolchain).unwrap();
        assert_eq!(
            toolchain_closure_fingerprint_cached(&dir, &toolchain).unwrap(),
            first,
            "an unchanged toolchain keeps its fingerprint"
        );
        let stamp = std::fs::read(dir.join(TOOLCHAIN_STAMP_PATH)).unwrap();
        let stamp: ToolchainStamp = postcard::from_bytes(&stamp).unwrap();
        assert!(
            stamp
                .tools
                .iter()
                .any(|(path, ..)| path.ends_with(frostbuild_core::graph::SHELL)),
            "the shell must be one of the hashed tools: {:?}",
            stamp.tools
        );
        assert!(
            stamp
                .tools
                .iter()
                .any(|(path, ..)| path.ends_with("tools/language")),
            "named command tools must be hashed: {:?}",
            stamp.tools
        );
        assert!(
            stamp
                .tools
                .iter()
                .any(|(path, ..)| path.ends_with("tools/kofun")),
            "the configured Kofun compiler must be hashed: {:?}",
            stamp.tools
        );
        std::fs::write(dir.join("tools/kofun"), b"kofun compiler v2 changed\n").unwrap();
        assert_ne!(
            toolchain_closure_fingerprint_cached(&dir, &toolchain).unwrap(),
            first,
            "changing kofunc must invalidate the toolchain fingerprint"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn toolchain_fingerprint_is_stable_and_errors_on_missing() {
        let a = toolchain_fingerprint(frostbuild_core::graph::SHELL).unwrap();
        let b = toolchain_fingerprint(frostbuild_core::graph::SHELL).unwrap();
        assert_eq!(a, b);
        assert!(toolchain_fingerprint("definitely-not-a-compiler-xyz").is_err());
    }
}
