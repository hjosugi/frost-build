//! Where a daemon leaf-change build spends its time, and proof that it did
//! exactly one action's worth of work (#152).
//!
//! Every frost process in the request — the client, the daemon that answers
//! it and the child build the daemon starts — appends one line of consecutive
//! phase laps to the file named by `FROST_PHASE_TIMINGS`. Nesting them gives a
//! flat attribution whose parts sum to the end-to-end time the harness
//! measured: what happens outside a process's own clock (exec, dynamic
//! loading, exit, the socket round trip) is not hidden but reported as an
//! explicit `*.outside` residual computed by difference.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use frostbuild_core::phases::PhaseLine;
use serde::Serialize;

/// Phases a leaf-change sample must report. A report missing one of these was
/// produced by a build that took a different path (for example the
/// certificate answered, so nothing was built) or by instrumentation that
/// regressed, and either way it cannot support a claim about the leaf path.
pub const REQUIRED_PHASES: &[&str] = &[
    "client.outside",
    "client.cli.startup",
    "client.prepare",
    "client.transport",
    "daemon.fast_noop",
    "daemon.child_outside",
    "build.cli.startup",
    "build.graph_load",
    "build.toolchain",
    "build.closure",
    "build.prepare",
    "build.engine_load",
    "build.engine.preflight",
    "build.engine.workers",
    "build.engine.hashcache_save",
    "build.summary",
    "daemon.post_barrier",
    "client.output",
];

#[derive(Debug, Clone, Serialize)]
pub struct Timing {
    pub name: String,
    pub ms: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct DetailTiming {
    pub name: String,
    pub ms: f64,
    pub count: u64,
}

/// One measured leaf-change build.
#[derive(Debug, Clone, Serialize)]
pub struct LeafSample {
    pub end_to_end_ms: f64,
    /// Flat, non-overlapping attribution of `end_to_end_ms`, outermost
    /// process first. Sums to `end_to_end_ms`.
    pub phases: Vec<Timing>,
    /// Nested per-action and per-check costs, prefixed by process.
    pub details: Vec<DetailTiming>,
    pub executed_actions: u64,
    pub cached_actions: u64,
    /// Declared outputs whose modification time changed during the sample.
    pub frost_outputs_rewritten: Vec<String>,
    pub ninja_outputs_rewritten: Vec<String>,
}

fn line<'a>(lines: &'a [PhaseLine], process: &str) -> Result<&'a PhaseLine> {
    let mut matching = lines.iter().filter(|line| line.process == process);
    let found = matching
        .next()
        .with_context(|| format!("no `{process}` phase line was written"))?;
    anyhow::ensure!(
        matching.next().is_none(),
        "more than one `{process}` phase line was written for one build"
    );
    Ok(found)
}

fn qualified(process: &str, name: &str) -> String {
    if name.starts_with(&format!("{process}.")) {
        name.to_string()
    } else {
        format!("{process}.{name}")
    }
}

/// Nest client ⊃ daemon ⊃ build into one attribution of `end_to_end_ms`.
///
/// The client's `client.daemon_request` lap is replaced by the transport
/// residual (the round trip outside the daemon's clock) followed by the
/// daemon's laps; the daemon's `daemon.child_build` lap is replaced by the
/// child's residual (exec, loading, exit, pipe drain) followed by the build's
/// laps. Each process's laps sum to its wall time, so the result sums to the
/// harness's end-to-end measurement exactly.
pub fn attribute(end_to_end_ms: f64, lines: &[PhaseLine]) -> Result<LeafSample> {
    let client = line(lines, "client")?;
    let daemon = line(lines, "daemon")?;
    let build = line(lines, "build")?;
    let request = client
        .phase("client.daemon_request")
        .context("the client did not record `client.daemon_request`")?;
    let child = daemon.phase("daemon.child_build").context(
        "the daemon did not start a child build (was the leaf change answered as a no-op?)",
    )?;

    let mut phases = vec![Timing {
        name: "client.outside".into(),
        ms: end_to_end_ms - client.wall_ms,
    }];
    for lap in &client.phases {
        if lap.name != request.name {
            phases.push(Timing {
                name: qualified("client", &lap.name),
                ms: lap.ms,
            });
            continue;
        }
        phases.push(Timing {
            name: "client.transport".into(),
            ms: lap.ms - daemon.wall_ms,
        });
        for lap in &daemon.phases {
            if lap.name != child.name {
                phases.push(Timing {
                    name: qualified("daemon", &lap.name),
                    ms: lap.ms,
                });
                continue;
            }
            phases.push(Timing {
                name: "daemon.child_outside".into(),
                ms: lap.ms - build.wall_ms,
            });
            phases.extend(build.phases.iter().map(|lap| Timing {
                name: qualified("build", &lap.name),
                ms: lap.ms,
            }));
        }
    }

    let mut details = Vec::new();
    for source in [client, daemon, build] {
        for detail in &source.details {
            details.push(DetailTiming {
                name: format!("{}:{}", source.process, detail.name),
                ms: detail.ms,
                count: detail.count,
            });
        }
    }
    Ok(LeafSample {
        end_to_end_ms,
        phases,
        details,
        executed_actions: build.counter("actions.executed").unwrap_or(0),
        cached_actions: build.counter("actions.cached").unwrap_or(0),
        frost_outputs_rewritten: Vec::new(),
        ninja_outputs_rewritten: Vec::new(),
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct PhaseSummary {
    pub name: String,
    pub median_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
    pub share_of_median_end_to_end: f64,
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

/// Per-phase medians over all samples, in attribution order.
pub fn summarize(samples: &[LeafSample]) -> Vec<PhaseSummary> {
    let mut order: Vec<String> = Vec::new();
    let mut values: BTreeMap<String, Vec<f64>> = BTreeMap::new();
    for sample in samples {
        for phase in &sample.phases {
            if !values.contains_key(&phase.name) {
                order.push(phase.name.clone());
            }
            values.entry(phase.name.clone()).or_default().push(phase.ms);
        }
    }
    let mut end_to_end: Vec<f64> = samples.iter().map(|s| s.end_to_end_ms).collect();
    let end_to_end = if end_to_end.is_empty() {
        0.0
    } else {
        median(&mut end_to_end)
    };
    order
        .into_iter()
        .map(|name| {
            let mut samples = values.remove(&name).unwrap_or_default();
            let median_ms = median(&mut samples);
            PhaseSummary {
                share_of_median_end_to_end: if end_to_end > 0.0 {
                    median_ms / end_to_end
                } else {
                    0.0
                },
                min_ms: samples[0],
                max_ms: *samples.last().unwrap(),
                median_ms,
                name,
            }
        })
        .collect()
}

/// Per-detail medians over all samples, by name.
pub fn summarize_details(samples: &[LeafSample]) -> Vec<DetailTiming> {
    let mut order: Vec<String> = Vec::new();
    let mut values: BTreeMap<String, (Vec<f64>, Vec<u64>)> = BTreeMap::new();
    for sample in samples {
        for detail in &sample.details {
            if !values.contains_key(&detail.name) {
                order.push(detail.name.clone());
            }
            let entry = values.entry(detail.name.clone()).or_default();
            entry.0.push(detail.ms);
            entry.1.push(detail.count);
        }
    }
    order
        .into_iter()
        .map(|name| {
            let (mut ms, mut counts) = values.remove(&name).unwrap_or_default();
            counts.sort_unstable();
            DetailTiming {
                ms: median(&mut ms),
                count: counts[counts.len() / 2],
                name,
            }
        })
        .collect()
}

/// Modification times of every file below `directory`, to prove which outputs
/// a sample rewrote.
pub fn snapshot_mtimes(directory: &Path) -> Result<BTreeMap<PathBuf, std::time::SystemTime>> {
    let mut times = BTreeMap::new();
    for entry in std::fs::read_dir(directory)
        .with_context(|| format!("failed to list {}", directory.display()))?
    {
        let entry = entry?;
        let metadata = entry.metadata()?;
        if metadata.is_file() {
            times.insert(entry.path(), metadata.modified()?);
        }
    }
    Ok(times)
}

/// Files whose modification time changed, or that appeared or disappeared.
pub fn rewritten(
    before: &BTreeMap<PathBuf, std::time::SystemTime>,
    after: &BTreeMap<PathBuf, std::time::SystemTime>,
    root: &Path,
) -> Vec<String> {
    let mut changed: Vec<String> = after
        .iter()
        .filter(|(path, time)| before.get(*path) != Some(*time))
        .map(|(path, _)| path.clone())
        .chain(
            before
                .keys()
                .filter(|path| !after.contains_key(*path))
                .cloned(),
        )
        .map(|path| {
            path.strip_prefix(root)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    changed.sort();
    changed.dedup();
    changed
}

/// Thresholds a daemon-graph report is held to.
#[derive(Debug, Clone, Copy)]
pub struct Gates {
    /// The #25 no-op gate: daemon CLI no-op median must stay under this.
    pub noop_max_ms: f64,
    /// ...and at least this many times faster than Ninja's no-op.
    pub noop_min_speedup_vs_ninja: f64,
    /// Optional ceiling on Frost leaf-change / Ninja leaf-change.
    pub leaf_max_ratio_vs_ninja: Option<f64>,
}

impl Default for Gates {
    fn default() -> Self {
        Self {
            noop_max_ms: 5.0,
            noop_min_speedup_vs_ninja: 2.0,
            leaf_max_ratio_vs_ninja: None,
        }
    }
}

fn number(value: &serde_json::Value, pointer: &str) -> Option<f64> {
    value.pointer(pointer).and_then(serde_json::Value::as_f64)
}

/// Check a `frost-daemon-graph-v2` report. Returns every problem found, so a
/// failing CI run lists them all rather than the first.
pub fn validate_report(report: &serde_json::Value, gates: Gates) -> Vec<String> {
    let mut problems = Vec::new();
    if report.get("schema").and_then(|s| s.as_str()) != Some("frost-daemon-graph-v2") {
        problems.push("schema is not frost-daemon-graph-v2".to_string());
    }
    let targets = report
        .get("targets")
        .and_then(serde_json::Value::as_u64)
        .unwrap_or(0);

    match (
        number(report, "/daemon_cli_noop/median_ms"),
        number(report, "/ninja_noop/median_ms"),
    ) {
        (Some(frost), Some(ninja)) => {
            if frost >= gates.noop_max_ms {
                problems.push(format!(
                    "no-op gate regressed: daemon CLI no-op median {frost:.3} ms is not below {:.3} ms",
                    gates.noop_max_ms
                ));
            }
            if ninja / frost <= gates.noop_min_speedup_vs_ninja {
                problems.push(format!(
                    "no-op gate regressed: daemon CLI no-op is {:.2}x Ninja, needs more than {:.2}x",
                    ninja / frost,
                    gates.noop_min_speedup_vs_ninja
                ));
            }
        }
        _ => problems.push("no-op medians are missing".to_string()),
    }

    let samples = report
        .pointer("/leaf_change/samples")
        .and_then(serde_json::Value::as_array);
    let Some(samples) = samples.filter(|samples| !samples.is_empty()) else {
        problems.push("leaf_change.samples is missing or empty".to_string());
        return problems;
    };
    for (index, sample) in samples.iter().enumerate() {
        let phases: BTreeMap<&str, f64> = sample
            .get("phases")
            .and_then(serde_json::Value::as_array)
            .map(|phases| {
                phases
                    .iter()
                    .filter_map(|phase| {
                        Some((phase.get("name")?.as_str()?, phase.get("ms")?.as_f64()?))
                    })
                    .collect()
            })
            .unwrap_or_default();
        for required in REQUIRED_PHASES {
            if !phases.contains_key(required) {
                problems.push(format!("sample {index}: phase `{required}` is missing"));
            }
        }
        if let Some(end_to_end) = sample.get("end_to_end_ms").and_then(|v| v.as_f64()) {
            let sum: f64 = phases.values().sum();
            if (sum - end_to_end).abs() > 0.01 {
                problems.push(format!(
                    "sample {index}: phases sum to {sum:.3} ms, end-to-end was {end_to_end:.3} ms"
                ));
            }
            for (name, ms) in &phases {
                // Residuals are differences of two clocks; a clearly negative
                // one means a phase was counted twice.
                if *ms < -0.5 {
                    problems.push(format!(
                        "sample {index}: phase `{name}` is negative ({ms:.3} ms)"
                    ));
                }
            }
        } else {
            problems.push(format!("sample {index}: end_to_end_ms is missing"));
        }
        let executed = sample.get("executed_actions").and_then(|v| v.as_u64());
        if executed != Some(1) {
            problems.push(format!(
                "sample {index}: expected exactly 1 executed action, found {executed:?}"
            ));
        }
        let cached = sample.get("cached_actions").and_then(|v| v.as_u64());
        if targets > 0 && cached != Some(targets - 1) {
            problems.push(format!(
                "sample {index}: expected {} cached actions, found {cached:?}",
                targets - 1
            ));
        }
        for tool in ["frost", "ninja"] {
            let rewritten = sample
                .get(format!("{tool}_outputs_rewritten"))
                .and_then(serde_json::Value::as_array)
                .map(Vec::len);
            if rewritten != Some(1) {
                problems.push(format!(
                    "sample {index}: {tool} rewrote {rewritten:?} outputs, expected exactly 1"
                ));
            }
        }
    }
    if report.pointer("/leaf_change/artifact/content_verified") != Some(&true.into()) {
        problems.push("the changed artifact's content was not verified".to_string());
    }
    if let Some(limit) = gates.leaf_max_ratio_vs_ninja {
        match number(report, "/leaf_change_vs_ninja") {
            Some(ratio) if ratio <= limit => {}
            Some(ratio) => problems.push(format!(
                "leaf change is {ratio:.2}x Ninja, above the {limit:.2}x ceiling"
            )),
            None => problems.push("leaf_change_vs_ninja is missing".to_string()),
        }
    }
    problems
}

#[cfg(test)]
mod tests {
    use super::*;
    use frostbuild_core::phases::{Counter, PhaseTiming};

    fn timing(name: &str, ms: f64) -> PhaseTiming {
        PhaseTiming {
            name: name.into(),
            ms,
            count: 1,
        }
    }

    fn phase_line(process: &str, phases: &[(&str, f64)], counters: &[(&str, u64)]) -> PhaseLine {
        PhaseLine {
            process: process.into(),
            pid: 1,
            wall_ms: phases.iter().map(|(_, ms)| ms).sum(),
            phases: phases.iter().map(|(name, ms)| timing(name, *ms)).collect(),
            details: vec![timing("action.process", 0.5)],
            counters: counters
                .iter()
                .map(|(name, value)| Counter {
                    name: (*name).into(),
                    value: *value,
                })
                .collect(),
        }
    }

    fn lines() -> Vec<PhaseLine> {
        vec![
            phase_line(
                "build",
                &[
                    ("cli.startup", 1.0),
                    ("build.certificate_check", 2.0),
                    ("build.graph_load", 10.0),
                    ("build.toolchain", 0.5),
                    ("build.closure", 0.5),
                    ("build.prepare", 1.0),
                    ("build.engine_load", 8.0),
                    ("engine.preflight", 5.0),
                    ("engine.workers", 20.0),
                    ("engine.hashcache_save", 3.0),
                    ("build.summary", 0.5),
                ],
                &[("actions.executed", 1), ("actions.cached", 2)],
            ),
            phase_line(
                "daemon",
                &[
                    ("daemon.fast_noop", 4.0),
                    ("daemon.child_build", 60.0),
                    ("daemon.post_barrier", 1.0),
                ],
                &[],
            ),
            phase_line(
                "client",
                &[
                    ("cli.startup", 1.0),
                    ("client.prepare", 0.2),
                    ("client.daemon_request", 66.0),
                    ("client.output", 0.1),
                ],
                &[],
            ),
        ]
    }

    #[test]
    fn attribution_nests_processes_and_sums_to_end_to_end() {
        let sample = attribute(70.0, &lines()).unwrap();
        let names: Vec<&str> = sample.phases.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names.first(), Some(&"client.outside"));
        assert_eq!(names.last(), Some(&"client.output"));
        for required in REQUIRED_PHASES {
            assert!(
                names.contains(required),
                "{required} missing from {names:?}"
            );
        }
        let sum: f64 = sample.phases.iter().map(|p| p.ms).sum();
        assert!((sum - 70.0).abs() < 1e-9, "{sum}");
        let outside = |name: &str| sample.phases.iter().find(|p| p.name == name).unwrap().ms;
        // 70 - 67.3 client wall; 66 - 65 daemon wall; 60 - 51.5 build wall.
        assert!((outside("client.outside") - 2.7).abs() < 1e-9);
        assert!((outside("client.transport") - 1.0).abs() < 1e-9);
        assert!((outside("daemon.child_outside") - 8.5).abs() < 1e-9);
        assert_eq!(sample.executed_actions, 1);
        assert_eq!(sample.cached_actions, 2);
        assert!(sample
            .details
            .iter()
            .any(|detail| detail.name == "build:action.process"));
    }

    #[test]
    fn a_certificate_answer_is_not_a_leaf_sample() {
        let mut lines = lines();
        lines.retain(|line| line.process != "build");
        lines[0] = phase_line("daemon", &[("daemon.fast_noop", 1.0)], &[]);
        let error = attribute(3.0, &lines).unwrap_err().to_string();
        assert!(error.contains("build"), "{error}");
    }

    fn report() -> serde_json::Value {
        let mut sample =
            serde_json::to_value(attribute(70.0, &lines()).unwrap()).expect("serializable");
        sample["frost_outputs_rewritten"] = serde_json::json!([".frost/out/node00002.out"]);
        sample["ninja_outputs_rewritten"] = serde_json::json!(["out/node00002.out"]);
        serde_json::json!({
            "schema": "frost-daemon-graph-v2",
            "targets": 3,
            "daemon_cli_noop": { "median_ms": 2.0 },
            "ninja_noop": { "median_ms": 60.0 },
            "leaf_change_vs_ninja": 1.5,
            "leaf_change": {
                "artifact": { "content_verified": true },
                "samples": [sample],
            },
        })
    }

    #[test]
    fn a_complete_report_passes_every_gate() {
        let gates = Gates {
            leaf_max_ratio_vs_ninja: Some(2.0),
            ..Gates::default()
        };
        assert_eq!(validate_report(&report(), gates), Vec::<String>::new());
    }

    #[test]
    fn the_validator_catches_a_missing_phase_field() {
        let mut report = report();
        let phases = report["leaf_change"]["samples"][0]["phases"]
            .as_array_mut()
            .unwrap();
        phases.retain(|phase| phase["name"] != "build.engine.preflight");
        let problems = validate_report(&report, Gates::default());
        assert!(
            problems
                .iter()
                .any(|p| p.contains("`build.engine.preflight` is missing")),
            "{problems:?}"
        );
        // Dropping a phase also breaks the sum, which is reported too.
        assert!(
            problems.iter().any(|p| p.contains("phases sum")),
            "{problems:?}"
        );
    }

    #[test]
    fn the_validator_catches_wrong_action_counts() {
        let mut report = report();
        report["leaf_change"]["samples"][0]["executed_actions"] = 2.into();
        report["leaf_change"]["samples"][0]["cached_actions"] = 1.into();
        report["leaf_change"]["samples"][0]["frost_outputs_rewritten"] =
            serde_json::json!(["a", "b"]);
        let problems = validate_report(&report, Gates::default());
        assert!(problems.iter().any(|p| p.contains("exactly 1 executed")));
        assert!(problems.iter().any(|p| p.contains("2 cached")));
        assert!(problems.iter().any(|p| p.contains("frost rewrote Some(2)")));
    }

    #[test]
    fn the_validator_catches_a_noop_gate_regression() {
        let mut report = report();
        report["daemon_cli_noop"]["median_ms"] = 6.0.into();
        let problems = validate_report(&report, Gates::default());
        assert!(problems.iter().any(|p| p.contains("not below 5.000 ms")));

        let mut report = self::tests::report();
        report["daemon_cli_noop"]["median_ms"] = 40.0.into();
        let problems = validate_report(&report, Gates::default());
        assert!(problems.iter().any(|p| p.contains("1.50x Ninja")));
    }

    #[test]
    fn the_validator_enforces_an_optional_leaf_ratio() {
        let mut report = report();
        report["leaf_change_vs_ninja"] = 2.5.into();
        let strict = Gates {
            leaf_max_ratio_vs_ninja: Some(2.0),
            ..Gates::default()
        };
        assert!(validate_report(&report, strict)
            .iter()
            .any(|p| p.contains("2.50x Ninja")));
        assert!(validate_report(&report, Gates::default()).is_empty());
    }

    #[test]
    fn rewritten_outputs_are_reported_relative_to_the_workspace() {
        let root = Path::new("/w");
        let t0 = std::time::SystemTime::UNIX_EPOCH;
        let t1 = t0 + std::time::Duration::from_secs(1);
        let before: BTreeMap<PathBuf, _> = [
            (PathBuf::from("/w/out/a.out"), t0),
            (PathBuf::from("/w/out/b.out"), t0),
        ]
        .into();
        let after: BTreeMap<PathBuf, _> = [
            (PathBuf::from("/w/out/a.out"), t0),
            (PathBuf::from("/w/out/b.out"), t1),
        ]
        .into();
        assert_eq!(rewritten(&before, &after, root), ["out/b.out"]);
    }
}
