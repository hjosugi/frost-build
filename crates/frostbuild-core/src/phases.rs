//! Opt-in phase timing for performance work (#152).
//!
//! `FROST_PHASE_TIMINGS=<file>` makes every frost process that takes part in
//! a build append one JSON object per line to `<file>`: the client, the daemon
//! that answered it, and the child build the daemon started. Each line names
//! its process and carries wall-clock phases in the order they first ran, plus
//! counters. The daemon forwards the client's environment to the child build
//! verbatim, so one variable set on the client reaches all three.
//!
//! Two kinds of timing are kept apart. `phases` are consecutive laps of one
//! clock ([`lap`]): they do not overlap, so their sum accounts for the
//! process's wall time and anything missing is visibly unattributed.
//! `details` are nested measurements ([`add`], [`time`], [`Stopwatch`]) —
//! per-action costs, possibly summed across worker threads — that explain a
//! phase without being added to it.
//!
//! This is a measurement surface, not a contract: phase names follow the code
//! and change with it. Unset — the normal case — every call below is a load of
//! one `OnceLock` and a branch, so a build that is not being measured does not
//! pay for being measurable.
//!
//! A line is written with a single `write` on a file opened for append, so
//! the three processes can share one file without interleaving inside a line.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// The environment variable naming the file phase lines are appended to.
pub const ENV: &str = "FROST_PHASE_TIMINGS";

/// One process's measurements, as written to the phase file.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseLine {
    /// `client`, `daemon` or `build`.
    pub process: String,
    pub pid: u32,
    /// From the moment the recorder was created (the start of `main` for a
    /// CLI process, the arrival of the request for the daemon) to the write.
    pub wall_ms: f64,
    /// Consecutive, non-overlapping laps; they sum to `wall_ms`.
    pub phases: Vec<PhaseTiming>,
    /// Nested measurements inside the phases, in first-seen order.
    #[serde(default)]
    pub details: Vec<PhaseTiming>,
    pub counters: Vec<Counter>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PhaseTiming {
    pub name: String,
    pub ms: f64,
    /// How many times the phase ran. Per-action phases run once per action.
    pub count: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Counter {
    pub name: String,
    pub value: u64,
}

struct Totals {
    last_lap: Instant,
    phases: Vec<(&'static str, Duration, u64)>,
    details: Vec<(&'static str, Duration, u64)>,
    counters: Vec<(&'static str, u64)>,
}

fn accumulate(
    list: &mut Vec<(&'static str, Duration, u64)>,
    name: &'static str,
    elapsed: Duration,
    count: u64,
) {
    match list.iter_mut().find(|(known, ..)| *known == name) {
        Some((_, total, occurrences)) => {
            *total += elapsed;
            *occurrences += count;
        }
        None => list.push((name, elapsed, count)),
    }
}

/// Accumulates named phases and counters for one process or one request.
pub struct PhaseLog {
    path: PathBuf,
    started: Instant,
    totals: Mutex<Totals>,
}

impl PhaseLog {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let started = Instant::now();
        Self {
            path: path.into(),
            started,
            totals: Mutex::new(Totals {
                last_lap: started,
                phases: Vec::new(),
                details: Vec::new(),
                counters: Vec::new(),
            }),
        }
    }

    /// A log for the file named by `ENV` in an explicit environment, such as
    /// the one a daemon request carries.
    pub fn from_env_pairs<'a>(
        mut environment: impl Iterator<Item = (&'a str, &'a str)>,
    ) -> Option<Self> {
        environment
            .find(|(name, value)| *name == ENV && !value.is_empty())
            .map(|(_, value)| Self::new(value))
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// End the current lap: everything since the previous lap (or since the
    /// log was created) is phase `name`.
    pub fn lap(&self, name: &'static str) {
        let now = Instant::now();
        let mut totals = self.totals.lock().unwrap();
        let elapsed = now - totals.last_lap;
        totals.last_lap = now;
        accumulate(&mut totals.phases, name, elapsed, 1);
    }

    /// Add `elapsed` to detail `name`, counting `count` occurrences.
    pub fn add(&self, name: &'static str, elapsed: Duration, count: u64) {
        let mut totals = self.totals.lock().unwrap();
        accumulate(&mut totals.details, name, elapsed, count);
    }

    /// Add `value` to counter `name`.
    pub fn count(&self, name: &'static str, value: u64) {
        let mut totals = self.totals.lock().unwrap();
        match totals.counters.iter_mut().find(|(known, _)| *known == name) {
            Some((_, total)) => *total += value,
            None => totals.counters.push((name, value)),
        }
    }

    /// Time `work` as one occurrence of detail `name`.
    pub fn time<T>(&self, name: &'static str, work: impl FnOnce() -> T) -> T {
        let started = Instant::now();
        let value = work();
        self.add(name, started.elapsed(), 1);
        value
    }

    pub fn snapshot(&self, process: &str) -> PhaseLine {
        let totals = self.totals.lock().unwrap();
        PhaseLine {
            process: process.to_string(),
            pid: std::process::id(),
            wall_ms: millis(totals.last_lap - self.started),
            phases: timings(&totals.phases),
            details: timings(&totals.details),
            counters: totals
                .counters
                .iter()
                .map(|(name, value)| Counter {
                    name: (*name).to_string(),
                    value: *value,
                })
                .collect(),
        }
    }

    /// Append this log as one line. Measurement must never fail a build, so
    /// callers ignore the result; it is returned for tests.
    pub fn write(&self, process: &str) -> std::io::Result<()> {
        let mut line = serde_json::to_vec(&self.snapshot(process))?;
        line.push(b'\n');
        std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?
            .write_all(&line)
    }
}

fn timings(list: &[(&'static str, Duration, u64)]) -> Vec<PhaseTiming> {
    list.iter()
        .map(|(name, elapsed, count)| PhaseTiming {
            name: (*name).to_string(),
            ms: millis(*elapsed),
            count: *count,
        })
        .collect()
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1000.0
}

static GLOBAL: OnceLock<Option<PhaseLog>> = OnceLock::new();

/// The process-wide log, if `ENV` is set. The first call starts its clock, so
/// a CLI calls this at the top of `main`.
pub fn global() -> Option<&'static PhaseLog> {
    GLOBAL
        .get_or_init(|| {
            std::env::var_os(ENV)
                .filter(|value| !value.is_empty())
                .map(PhaseLog::new)
        })
        .as_ref()
}

/// Whether this process is recording phases.
pub fn enabled() -> bool {
    global().is_some()
}

/// End the current process-wide lap; a no-op when not recording.
pub fn lap(name: &'static str) {
    if let Some(log) = global() {
        log.lap(name);
    }
}

/// Add to a process-wide detail; a no-op when not recording.
pub fn add(name: &'static str, elapsed: Duration, count: u64) {
    if let Some(log) = global() {
        log.add(name, elapsed, count);
    }
}

/// Add to a process-wide counter; a no-op when not recording.
pub fn count(name: &'static str, value: u64) {
    if let Some(log) = global() {
        log.count(name, value);
    }
}

/// Time `work` as one occurrence of a process-wide detail. When not recording
/// this is `work()` and a branch.
pub fn time<T>(name: &'static str, work: impl FnOnce() -> T) -> T {
    match global() {
        Some(log) => log.time(name, work),
        None => work(),
    }
}

/// A clock that only reads the time when this process is recording, for
/// per-action phases on paths that run ten thousand times per build.
#[derive(Debug, Clone, Copy)]
pub struct Stopwatch(Option<Instant>);

impl Stopwatch {
    pub fn start() -> Self {
        Self(enabled().then(Instant::now))
    }

    /// Record the time since `start` (or the previous lap) under `name` and
    /// restart the clock.
    pub fn lap(&mut self, name: &'static str) {
        if let Some(started) = self.0 {
            let now = Instant::now();
            add(name, now - started, 1);
            self.0 = Some(now);
        }
    }
}

/// Close the final lap as `last_phase` and append the process-wide log,
/// labelled `process`, if recording.
pub fn flush(process: &str, last_phase: &'static str) {
    if let Some(log) = global() {
        log.lap(last_phase);
        let _ = log.write(process);
    }
}

/// Read every line of a phase file, skipping lines that do not parse.
pub fn read_lines(path: &Path) -> std::io::Result<Vec<PhaseLine>> {
    Ok(std::fs::read_to_string(path)?
        .lines()
        .filter_map(|line| serde_json::from_str(line).ok())
        .collect())
}

impl PhaseLine {
    pub fn phase(&self, name: &str) -> Option<&PhaseTiming> {
        self.phases.iter().find(|phase| phase.name == name)
    }

    pub fn detail(&self, name: &str) -> Option<&PhaseTiming> {
        self.details.iter().find(|detail| detail.name == name)
    }

    pub fn counter(&self, name: &str) -> Option<u64> {
        self.counters
            .iter()
            .find(|counter| counter.name == name)
            .map(|counter| counter.value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phases_accumulate_in_first_seen_order_and_lines_append() {
        let dir = std::env::temp_dir().join(format!(
            "frost-phases-test-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("phases.ndjson");
        let _ = std::fs::remove_file(&path);

        let log = PhaseLog::new(&path);
        log.add("load", Duration::from_millis(3), 1);
        log.add("check", Duration::from_millis(1), 1);
        log.add("load", Duration::from_millis(2), 1);
        log.lap("startup");
        log.lap("work");
        log.lap("startup");
        log.count("executed", 1);
        log.count("executed", 2);
        log.write("build").unwrap();
        PhaseLog::new(&path).write("client").unwrap();

        let lines = read_lines(&path).unwrap();
        assert_eq!(lines.len(), 2);
        let build = &lines[0];
        assert_eq!(build.process, "build");
        let names: Vec<_> = build.details.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["load", "check"]);
        let load = build.detail("load").unwrap();
        assert_eq!(load.count, 2);
        assert!((load.ms - 5.0).abs() < 1e-9);
        let laps: Vec<_> = build.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(laps, ["startup", "work"]);
        assert_eq!(build.phase("startup").unwrap().count, 2);
        // Laps partition the wall time exactly.
        let summed: f64 = build.phases.iter().map(|p| p.ms).sum();
        assert!((summed - build.wall_ms).abs() < 1e-6);
        assert_eq!(build.counter("executed"), Some(3));
        assert_eq!(lines[1].process, "client");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_explicit_environment_selects_the_file_and_ignores_an_empty_value() {
        let pairs = [("PATH", "/bin"), (ENV, "/tmp/x.ndjson")];
        let log = PhaseLog::from_env_pairs(pairs.iter().map(|(k, v)| (*k, *v))).unwrap();
        assert_eq!(log.path(), Path::new("/tmp/x.ndjson"));
        let empty = [(ENV, "")];
        assert!(PhaseLog::from_env_pairs(empty.iter().map(|(k, v)| (*k, *v))).is_none());
    }
}
