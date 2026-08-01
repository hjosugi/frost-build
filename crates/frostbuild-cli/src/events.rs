//! One JSON object per line, so a CI job can read a build without a parser.
//!
//! This is a third consumer of the progress stream the renderers already use,
//! not a second instrumentation of the engine. That matters: an event a
//! dashboard sees is an event a human saw, and there is no way for the two to
//! drift into disagreeing about what happened.
//!
//! Deliberately not BEP. Reproducing a protobuf schema would buy compatibility
//! with tools nobody here runs, at the cost of a dependency and a shape frost
//! does not have. A line of JSON is enough to count failures, chart durations
//! and find the slow target, which is what the ledger in docs/14 actually
//! wanted.

use std::io::Write;
use std::path::Path;

use anyhow::{Context, Result};
use frostbuild_exec::{ProgressEvent, ProgressState};

/// Bumped when a field changes meaning or leaves. Adding one does not bump it,
/// which is the whole compatibility promise: a reader that knows `v1` keeps
/// working as fields appear beside the ones it reads.
pub const SCHEMA: &str = "frost-build-events-v1";

/// Writes the stream to a file as it arrives.
pub struct EventLog {
    file: std::fs::File,
    /// Monotonic per build, so a consumer can order events without trusting a
    /// clock — and so the determinism test has something stable to compare.
    sequence: u64,
}

impl EventLog {
    pub fn create(path: &Path) -> Result<Self> {
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let file = std::fs::File::create(path)
            .with_context(|| format!("failed to create {}", path.display()))?;
        Ok(Self { file, sequence: 0 })
    }

    /// Write one event. A write failure is reported once and then ignored:
    /// losing a dashboard is not a reason to fail a build that otherwise
    /// succeeded.
    pub fn write(&mut self, event: &ProgressEvent) {
        let Some(mut payload) = encode(event) else {
            return;
        };
        payload["schema"] = serde_json::json!(SCHEMA);
        payload["seq"] = serde_json::json!(self.sequence);
        self.sequence += 1;
        let line = payload.to_string();
        if let Err(error) = writeln!(self.file, "{line}") {
            eprintln!("frost: build event log: {error}");
        }
    }
}

/// One event as JSON, or `None` for events that carry no information a
/// consumer of a *log* can use.
///
/// `ActionRunning` is the only omission: it exists so a live display can move
/// a row from "queued" to "running", and in a file it is a second line saying
/// the same thing as the first.
fn encode(event: &ProgressEvent) -> Option<serde_json::Value> {
    Some(match event {
        ProgressEvent::BuildStarted {
            total,
            jobs,
            critical_path_ms,
            ..
        } => serde_json::json!({
            "event": "build_started",
            "actions": total,
            "jobs": jobs,
            "critical_path_ms": critical_path_ms,
        }),
        ProgressEvent::AllCached { total } => serde_json::json!({
            "event": "all_cached",
            "actions": total,
        }),
        ProgressEvent::ActionStarted { id, desc, .. } => serde_json::json!({
            "event": "action_started",
            "id": id,
            "desc": desc,
        }),
        ProgressEvent::ActionFinished {
            id,
            desc,
            state,
            duration_ms,
            detail,
            ..
        } => serde_json::json!({
            "event": "action_finished",
            "id": id,
            "desc": desc,
            "result": result_name(*state),
            "cached": *state == ProgressState::CacheHit,
            "duration_ms": duration_ms,
            // Present only when there is something to say, so a consumer can
            // treat its absence as "nothing went wrong" rather than comparing
            // against an empty string.
            "detail": (!detail.is_empty()).then_some(detail.as_str()),
        }),
        ProgressEvent::BuildFinished {
            success,
            elapsed_ms,
        } => serde_json::json!({
            "event": "build_finished",
            "success": success,
            "elapsed_ms": elapsed_ms,
        }),
        ProgressEvent::ActionOutput { .. } | ProgressEvent::ActionRunning { .. } => return None,
    })
}

/// Stable names, independent of the human-facing strings.
///
/// `ProgressState::as_str` says "cache miss" for an action that ran, which is
/// the right thing on a terminal and the wrong thing in a field a machine
/// switches on. Keeping them separate means the display can be reworded
/// without breaking every consumer.
fn result_name(state: ProgressState) -> &'static str {
    match state {
        ProgressState::CacheHit => "cached",
        ProgressState::Executed => "executed",
        ProgressState::Flaky => "flaky",
        ProgressState::Failed => "failed",
        ProgressState::Skipped => "skipped",
        ProgressState::WouldRun => "would_run",
        ProgressState::MayRun => "may_run",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finished(state: ProgressState, detail: &str) -> ProgressEvent {
        ProgressEvent::ActionFinished {
            slot: 0,
            completed: 1,
            total: 1,
            id: "test:t".into(),
            desc: "TEST t".into(),
            state,
            duration_ms: 12,
            detail: detail.into(),
            critical: false,
        }
    }

    #[test]
    fn every_result_has_a_stable_name_of_its_own() {
        // Two states sharing a name would make them indistinguishable to a
        // consumer, which is exactly the bug this feature had to fix in
        // `ProgressState` before it could report a flake at all.
        let states = [
            ProgressState::CacheHit,
            ProgressState::Executed,
            ProgressState::Flaky,
            ProgressState::Failed,
            ProgressState::Skipped,
            ProgressState::WouldRun,
            ProgressState::MayRun,
        ];
        let mut names: Vec<&str> = states.iter().map(|state| result_name(*state)).collect();
        let total = names.len();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), total, "result names must be distinct");

        // And they are not the display strings, so rewording the terminal
        // cannot break a consumer.
        assert_eq!(result_name(ProgressState::Executed), "executed");
        assert_eq!(ProgressState::Executed.as_str(), "cache miss");
    }

    #[test]
    fn a_flake_is_reported_as_one_rather_than_as_a_plain_pass() {
        let payload = encode(&finished(ProgressState::Flaky, "passed on attempt 2")).unwrap();
        assert_eq!(payload["result"], "flaky");
        assert_eq!(payload["cached"], false);
        assert_eq!(payload["detail"], "passed on attempt 2");
    }

    #[test]
    fn a_clean_result_carries_no_detail_field() {
        // Absence is the signal. A consumer comparing against "" would have to
        // know that an empty string means the same as nothing.
        let payload = encode(&finished(ProgressState::Executed, "")).unwrap();
        assert!(payload["detail"].is_null(), "{payload}");
        assert_eq!(payload["cached"], false);

        let payload = encode(&finished(ProgressState::CacheHit, "")).unwrap();
        assert_eq!(payload["cached"], true);
    }

    #[test]
    fn display_only_events_are_left_out() {
        // `ActionRunning` moves a row in a live display; in a file it repeats
        // what `action_started` already said. `ActionOutput` is the build log,
        // which has its own destination.
        assert!(encode(&ProgressEvent::ActionRunning { id: "a".into() }).is_none());
        assert!(encode(&ProgressEvent::ActionOutput {
            id: "a".into(),
            output: "x".into()
        })
        .is_none());
    }

    #[test]
    fn every_line_carries_the_schema_and_its_position() {
        let dir = std::env::temp_dir().join(format!("frost-events-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("events.ndjson");
        let mut log = EventLog::create(&path).unwrap();
        log.write(&ProgressEvent::BuildStarted {
            total: 1,
            jobs: 1,
            critical_path_ms: 0,
            critical_path: Vec::new(),
        });
        log.write(&finished(ProgressState::Executed, ""));
        log.write(&ProgressEvent::BuildFinished {
            success: true,
            elapsed_ms: 5,
        });
        drop(log);

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 3);
        for (index, line) in lines.iter().enumerate() {
            let payload: serde_json::Value = serde_json::from_str(line).expect("one object a line");
            assert_eq!(payload["schema"], SCHEMA);
            // A sequence number rather than only a timestamp: a consumer can
            // order events without trusting a clock, and two runs of the same
            // build produce the same sequence.
            assert_eq!(payload["seq"], index);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
