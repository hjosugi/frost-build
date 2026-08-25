//! `frost build`, `frost test`, `frost plan` and `frost pick` — everything that
//! runs the executor.
//!
//! The three entry points differ in what they put in a [`BuildRequest`] and in
//! what they print afterwards; the run itself is one path, so the daemon
//! handoff, the event stream, the trace and the summary line all live here.

use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use frostbuild_core::graph::BuildGraph;
use frostbuild_core::manifest::Manifest;
use frostbuild_core::manifest::TargetKind;
use frostbuild_exec::toolchain_closure_fingerprint_cached_instrumented;
use frostbuild_exec::try_fast_noop;
use frostbuild_exec::BuildOptions;
use frostbuild_exec::Engine;
use frostbuild_exec::Outcome;

use crate::cli::{DaemonCmd, EstimatorArg, SchedulerArg, TestOutputArg};
use crate::daemon::daemon_command;
use crate::graph::{attribute_missing_tool, load_graph, load_graph_instrumented, resolve_targets};
use crate::human_bytes;
use crate::{events, progress, report};

#[derive(Clone)]
pub(crate) struct BuildRequest {
    pub(crate) targets: Vec<String>,
    pub(crate) jobs: Option<usize>,
    pub(crate) keep_going: bool,
    pub(crate) explain: bool,
    pub(crate) verbose: bool,
    pub(crate) profile: String,
    pub(crate) platform: String,
    pub(crate) no_cache: bool,
    pub(crate) sandbox: bool,
    pub(crate) check_determinism: bool,
    pub(crate) trace: Option<PathBuf>,
    /// `None` writes no report; `Some` writes one, at the default path when
    /// the inner option is empty.
    pub(crate) report: Option<Option<PathBuf>>,
    pub(crate) stats: bool,
    pub(crate) remote_cache: Option<String>,
    pub(crate) remote_upload: bool,
    pub(crate) remote_timeout: u64,
    pub(crate) no_tui: bool,
    /// Seconds an action may run when its target declares no limit.
    pub(crate) timeout: Option<u64>,
    pub(crate) test_mode: bool,
    /// Command-line test options, folded into test actions after the graph
    /// loads. Empty for every non-test build.
    pub(crate) test_options: frostbuild_core::graph::TestOptions,
    /// Run each test this many times, all of which must pass. 1 is ordinary.
    pub(crate) runs_per_test: u32,
    /// How much of what the tests wrote to show.
    pub(crate) test_output: TestOutputArg,
    /// Where to write the ndjson build event stream, if anywhere.
    pub(crate) build_event_json: Option<PathBuf>,
    /// Skip the workspace's `[stamp]` command; every `${stamp.KEY}` expands to
    /// nothing.
    pub(crate) no_stamp: bool,
    /// A stamp command that fails leaves the values empty instead of failing
    /// the build.
    pub(crate) stamp_optional: bool,
    pub(crate) daemon: bool,
    pub(crate) affected: bool,
    pub(crate) predictive: bool,
    pub(crate) all: bool,
    pub(crate) scheduler: SchedulerArg,
    pub(crate) estimator: EstimatorArg,
    /// Build instrumented for coverage and merge a tracefile per test target.
    /// Part of the configuration rather than an option applied to one: it
    /// selects a different output tree, journal identity and cache.
    pub(crate) coverage: bool,
}
pub(crate) fn run_build_selected(
    root: &std::path::Path,
    request: BuildRequest,
    all_platforms: bool,
) -> Result<i32> {
    if !all_platforms {
        return run_build(root, request);
    }

    let manifest = Manifest::load(root)?;
    let mut platforms = vec![frostbuild_core::manifest::HOST_PLATFORM.to_string()];
    platforms.extend(manifest.platforms.into_keys());
    println!(
        "frost: multi-platform build ({} platforms, profile {})",
        platforms.len(),
        request.profile
    );

    let mut results = Vec::with_capacity(platforms.len());
    for platform in platforms {
        println!("+-- {platform}");
        let mut configured = request.clone();
        configured.platform = platform.clone();
        match run_build(root, configured) {
            Ok(code) => results.push((platform, code)),
            Err(error) => {
                eprintln!("|   error: {error:#}");
                results.push((platform, 2));
            }
        }
    }

    println!("frost: platform summary");
    for (index, (platform, code)) in results.iter().enumerate() {
        let branch = if index + 1 == results.len() {
            "`--"
        } else {
            "|--"
        };
        println!(
            "{branch} {platform:<16} {}",
            if *code == 0 { "ok" } else { "failed" }
        );
    }
    Ok(results.iter().map(|(_, code)| *code).max().unwrap_or(0))
}

pub(crate) fn run_pick(
    root: &std::path::Path,
    tests: bool,
    print: bool,
    profile: String,
    platform: String,
) -> Result<i32> {
    use std::io::Write;
    use std::process::{Command, Stdio};

    let manifest = Manifest::load(root)?;
    let rows: Vec<String> = manifest
        .targets
        .values()
        .filter(|target| !tests || matches!(target.kind, TargetKind::CcTest | TargetKind::Test))
        .map(|target| {
            let deps = if target.deps.is_empty() {
                "-".to_string()
            } else {
                target.deps.join(",")
            };
            format!("{}\t{}\t{}", target.name, target.kind.as_str(), deps)
        })
        .collect();
    if rows.is_empty() {
        bail!(
            "this workspace has no {}targets to select",
            if tests { "test " } else { "" }
        );
    }

    let prompt = if tests {
        "frost test > "
    } else {
        "frost build > "
    };
    let mut child = Command::new("fzf")
        .args([
            "--multi",
            "--height=70%",
            "--layout=reverse",
            "--border=rounded",
            "--delimiter=\t",
            "--with-nth=1,2,3",
            "--header=TAB: multi-select  ENTER: confirm  ESC: cancel",
            "--prompt",
            prompt,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .context(
            "fzf was not found. install fzf, or use shell completion and pass target names directly",
        )?;
    {
        let stdin = child.stdin.as_mut().context("failed to open fzf input")?;
        for row in &rows {
            if let Err(error) = writeln!(stdin, "{row}") {
                if error.kind() == std::io::ErrorKind::BrokenPipe {
                    break;
                }
                return Err(error.into());
            }
        }
    }
    let output = child.wait_with_output()?;
    if !output.status.success() {
        // fzf uses 1 for no match and 130 for an interactive cancel. Neither
        // should turn an intentional escape into a scary Frost error.
        return Ok(0);
    }
    let selected: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| line.split('\t').next())
        .filter(|target| !target.is_empty())
        .map(str::to_string)
        .collect();
    if selected.is_empty() {
        return Ok(0);
    }
    if print {
        for target in selected {
            println!("{target}");
        }
        return Ok(0);
    }

    run_build(
        root,
        BuildRequest {
            targets: selected,
            test_options: Default::default(),
            runs_per_test: 1,
            test_output: TestOutputArg::Errors,
            build_event_json: None,
            no_stamp: false,
            stamp_optional: false,
            jobs: None,
            keep_going: false,
            explain: false,
            verbose: false,
            profile,
            platform,
            no_cache: false,
            sandbox: false,
            check_determinism: false,
            trace: None,
            report: None,
            stats: false,
            remote_cache: None,
            remote_upload: false,
            remote_timeout: 10,
            no_tui: false,
            timeout: None,
            test_mode: tests,
            daemon: false,
            affected: false,
            predictive: false,
            all: false,
            scheduler: SchedulerArg::CriticalPath,
            estimator: EstimatorArg::Journal,
            coverage: false,
        },
    )
}

/// Run this build through the workspace daemon, or return `Ok(None)` when the
/// daemon cannot be asked to produce exactly the build this process would.
fn run_build_via_daemon(
    root: &std::path::Path,
    request: &BuildRequest,
    enable_fast_noop: bool,
) -> Result<Option<i32>> {
    use frostbuild_daemon::{FastNoopRequest, Request, PROTOCOL_VERSION};
    let mut args = vec![
        "-C".to_string(),
        root.to_string_lossy().into_owned(),
        if request.test_mode { "test" } else { "build" }.to_string(),
    ];
    args.extend(request.targets.iter().cloned());
    if let Some(jobs) = request.jobs {
        args.extend(["--jobs".into(), jobs.to_string()]);
    }
    if request.keep_going {
        args.push("--keep-going".into());
    }
    if request.explain {
        args.push("--explain".into());
    }
    if request.verbose {
        args.push("--verbose".into());
    }
    if request.no_cache {
        args.push("--no-cache".into());
    }
    // A flag dropped here is a build the daemon runs differently from the one
    // that was asked for, silently: `--no-stamp` would come back stamped, and
    // `--coverage` would come back measuring nothing.
    if request.coverage {
        args.push("--coverage".into());
    }
    if request.no_stamp {
        args.push("--no-stamp".into());
    }
    if request.stamp_optional {
        args.push("--stamp-optional".into());
    }
    if let Some(endpoint) = &request.remote_cache {
        args.extend(["--remote-cache".into(), endpoint.clone()]);
        if request.remote_upload {
            args.push("--remote-upload".into());
        }
        args.extend([
            "--remote-timeout".into(),
            request.remote_timeout.to_string(),
        ]);
    }
    if request.no_tui {
        args.push("--no-tui".into());
    }
    if request.affected {
        args.push("--affected".into());
    }
    if request.predictive {
        args.push("--predictive".into());
    }
    if request.all {
        args.push("--all".into());
    }
    if request.sandbox {
        args.push("--sandbox".into());
    }
    if request.check_determinism {
        args.push("--check-determinism".into());
    }
    args.extend([
        "--scheduler".into(),
        match request.scheduler {
            SchedulerArg::CriticalPath => "critical-path",
            SchedulerArg::Fifo => "fifo",
        }
        .into(),
    ]);
    args.extend([
        "--estimator".into(),
        match request.estimator {
            EstimatorArg::Heuristic => "heuristic",
            EstimatorArg::Journal => "journal",
            EstimatorArg::Static => "static",
            EstimatorArg::Learned => "learned",
        }
        .into(),
    ]);
    args.extend(["--profile".into(), request.profile.clone()]);
    args.extend(["--platform".into(), request.platform.clone()]);
    if request.stats {
        args.push("--stats".into());
    }
    if let Some(trace) = &request.trace {
        args.extend(["--trace".into(), trace.to_string_lossy().into_owned()]);
    }
    // The daemon applies this to the child build verbatim. An environment
    // frost cannot represent as text is not forwarded rather than silently
    // altered, so such a build runs in this process instead.
    let mut environment = Vec::new();
    for (key, value) in std::env::vars_os() {
        match (key.into_string(), value.into_string()) {
            (Ok(key), Ok(value)) => environment.push((key, value)),
            _ => return Ok(None),
        }
    }
    let request_message = Request::Run {
        version: PROTOCOL_VERSION,
        program: std::env::current_exe()?,
        args,
        env: environment,
        fast_noop: enable_fast_noop.then(|| FastNoopRequest {
            profile: request.profile.clone(),
            platform: request.platform.clone(),
            key_env: frostbuild_exec::key_environment_snapshot(),
        }),
    };
    let response = match frostbuild_daemon::request(root, &request_message) {
        Ok(response) => response,
        Err(_) => {
            daemon_command(root, DaemonCmd::Start)?;
            frostbuild_daemon::request(root, &request_message)?
        }
    };
    let response = if is_protocol_mismatch(&response) {
        // A daemon from a different frost version is resident. Replace it and
        // retry once; one too old to honour a shutdown request has to be
        // stopped by hand, and reporting a build failure for that is worse
        // than building here.
        let _ = frostbuild_daemon::request(
            root,
            &Request::Shutdown {
                version: PROTOCOL_VERSION,
            },
        );
        match replace_daemon_and_retry(root, &request_message) {
            Some(response) => response,
            None => {
                eprintln!(
                    "frost: warning: a frostd from another frost version is running for this \
                     workspace and would not stop; building without the daemon"
                );
                return Ok(None);
            }
        }
    } else {
        response
    };
    print!("{}", response.stdout);
    eprint!("{}", response.stderr);
    Ok(Some(response.code))
}

pub(crate) fn is_protocol_mismatch(response: &frostbuild_daemon::Response) -> bool {
    response.code == 2
        && response
            .stderr
            .contains(frostbuild_daemon::PROTOCOL_MISMATCH)
}

fn replace_daemon_and_retry(
    root: &std::path::Path,
    request_message: &frostbuild_daemon::Request,
) -> Option<frostbuild_daemon::Response> {
    daemon_command(root, DaemonCmd::Start).ok()?;
    frostbuild_daemon::request(root, request_message)
        .ok()
        .filter(|response| !is_protocol_mismatch(response))
}

/// Run the workspace's `[stamp]` command and read its `KEY=VALUE` output.
///
/// Once per build, never per action: a stamp names a property of the
/// invocation, and two actions that disagreed about the build time would make
/// "which build produced this binary" unanswerable.
///
/// Skipped entirely when nothing in this closure reads a stamp. A workspace
/// that stamps its release binary should not pay for a `git describe` — or be
/// broken by a status script that stopped working — when it builds a library.
fn build_stamps(
    root: &Path,
    graph: &frostbuild_core::graph::BuildGraph,
    closure: &[usize],
    no_stamp: bool,
    stamp_optional: bool,
) -> Result<Option<std::collections::BTreeMap<String, String>>> {
    let Some(stamp) = graph.stamp.as_ref() else {
        return Ok(None);
    };
    if no_stamp {
        return Ok(None);
    }
    let referenced = closure.iter().any(|&action| {
        let action = &graph.actions[action];
        !action.stable_stamps.is_empty() || !action.volatile_stamps.is_empty()
    });
    if !referenced {
        return Ok(None);
    }

    // Inherits the environment rather than being handed frost's action
    // baseline. This is not an action: its output is not cached, it is not
    // sandboxed, and a status script needs the PATH and credentials of the
    // person or the CI job invoking frost to ask git or a registry anything.
    // The same rule actions are spawned under: Windows resolves a relative
    // program name against the process working directory, before `current_dir`
    // applies, and a bare name stays bare so it is still found on PATH.
    let program = frostbuild_exec::resolve_action_program(root, &stamp.command[0]);
    let output = std::process::Command::new(program)
        .args(&stamp.command[1..])
        .current_dir(root)
        .stdin(std::process::Stdio::null())
        .output();
    let failure = match output {
        Err(error) => format!("failed to run {:?}: {error}", stamp.command[0]),
        Ok(output) if !output.status.success() => format!(
            "{} exited with {}{}",
            stamp.command.join(" "),
            output.status,
            match String::from_utf8_lossy(&output.stderr).trim() {
                "" => String::new(),
                stderr => format!("\n{stderr}"),
            }
        ),
        Ok(output) => {
            return frostbuild_core::stamp::parse(&String::from_utf8_lossy(&output.stdout))
                .map(Some)
                .with_context(|| format!("[stamp] command {:?}", stamp.command.join(" ")))
        }
    };
    if stamp_optional {
        eprintln!("frost: [stamp] command failed, continuing with no values: {failure}");
        return Ok(None);
    }
    // Failing by default: a status script that stopped working is how a
    // release binary ends up reporting no version at all, and the build that
    // shipped it looked green.
    anyhow::bail!("[stamp] {failure}\n(--stamp-optional treats this as no values)")
}

pub(crate) fn run_build(root: &std::path::Path, request: BuildRequest) -> Result<i32> {
    let enable_fast_noop = !request.test_mode
        && request.targets.is_empty()
        && !request.keep_going
        && !request.explain
        && !request.verbose
        && !request.no_cache
        && !request.sandbox
        && !request.check_determinism
        && request.trace.is_none()
        // The certificate answers without producing a BuildReport, and a
        // report of a build that was never planned would have nothing in it.
        && request.report.is_none()
        && !request.stats
        && !request.affected
        && !request.predictive
        && !request.all
        // The certificate answers "nothing to do here". A build that would also
        // publish to a shared cache still has something to do.
        && request.remote_cache.is_none();
    if request.daemon {
        // A daemon that cannot serve this request correctly declines it; the
        // build then runs in this process, which is always the same build.
        if let Some(code) = run_build_via_daemon(root, &request, enable_fast_noop)? {
            return Ok(code);
        }
    }
    if enable_fast_noop {
        let started = Instant::now();
        if let Some(hit) = try_fast_noop(root, &request.profile, &request.platform)? {
            println!(
                "{}",
                summarize(
                    0,
                    hit.closure_actions,
                    0,
                    0,
                    hit.closure_actions,
                    hit.graph_actions,
                    started.elapsed().as_millis(),
                )
            );
            return Ok(0);
        }
    }
    let mut graph =
        load_graph_instrumented(root, &request.profile, &request.platform, request.coverage)?;
    // In memory only. The stored graph stays the manifest's, so a run with
    // `--test-filter parse` cannot leave a filtered graph behind for the next
    // one; and because argv and env are already action-key material, the
    // filtered actions key differently and cannot be served a cached result
    // from an unfiltered run.
    graph.apply_test_options(&request.test_options);
    let graph = graph;
    let toolchain = toolchain_fingerprint(root, &graph)?;
    let mut requested = if request.test_mode && (request.all || request.targets.is_empty()) {
        graph
            .targets
            .values()
            .filter(|target| matches!(target.kind, TargetKind::CcTest | TargetKind::Test))
            .map(|target| target.name.clone())
            .collect::<Vec<_>>()
    } else {
        resolve_targets(&graph, request.targets)?
    };
    if request.test_mode && requested.is_empty() {
        bail!("workspace declares no cc_test or test targets");
    }
    for name in &requested {
        if request.test_mode
            && !matches!(
                graph.targets[name].kind,
                TargetKind::CcTest | TargetKind::Test
            )
        {
            bail!("{name:?} is not a test target");
        }
    }
    if request.test_mode && (request.affected || request.predictive) {
        let all_closure = graph.action_closure(&requested)?;
        let plan = Engine::new(
            root,
            &graph,
            all_closure,
            toolchain.clone(),
            BuildOptions {
                jobs: request.jobs.unwrap_or_else(default_jobs),
                keep_going: true,
                dry_run: true,
                ..BuildOptions::default()
            },
        )
        .run()?;
        let affected = plan
            .results
            .iter()
            .filter(|result| {
                result.id.starts_with("test:")
                    && matches!(
                        result.outcome,
                        Outcome::WouldRun { .. } | Outcome::MayRun { .. }
                    )
            })
            .map(|result| result.id.trim_start_matches("test:").to_string())
            .collect::<std::collections::BTreeSet<_>>();
        requested.retain(|target| affected.contains(target));
        if requested.is_empty() {
            println!("tests: 0 passed, 0 failed, 0 cached (no affected tests)");
            return Ok(0);
        }
        if request.explain {
            println!("affected tests: {}", requested.join(", "));
        }
    }
    let closure = graph.action_closure(&requested)?;
    // A misspelled endpoint is a configuration error worth reporting before the
    // build; an endpoint that is merely unreachable is not, and is handled per
    // request by falling back to local execution.
    let remote = match &request.remote_cache {
        Some(spec) => Some(std::sync::Arc::new(
            frostbuild_core::remote::RemoteCache::parse(
                spec,
                std::time::Duration::from_secs(request.remote_timeout),
                request.remote_upload,
            )?,
        )),
        None => None,
    };
    // Only `--test-output=all` echoes what a passing test wrote. A green
    // suite's output is the noise that buries the one failure worth reading.
    let echo_success = !request.test_mode || request.test_output == TestOutputArg::All;
    let events = request
        .build_event_json
        .as_deref()
        .map(events::EventLog::create)
        .transpose()?;
    let stamps = build_stamps(
        root,
        &graph,
        &closure,
        request.no_stamp,
        request.stamp_optional,
    )?;
    let (progress, renderer) =
        progress::start(request.no_tui, request.verbose, echo_success, events);
    let opts = BuildOptions {
        jobs: request.jobs.unwrap_or_else(default_jobs),
        keep_going: request.keep_going,
        dry_run: false,
        verbose: request.verbose,
        no_cache: request.no_cache,
        sandbox: request.sandbox,
        check_determinism: request.check_determinism,
        write_fast_noop: enable_fast_noop,
        scheduler: match request.scheduler {
            SchedulerArg::CriticalPath => frostbuild_exec::Scheduler::CriticalPath,
            SchedulerArg::Fifo => frostbuild_exec::Scheduler::Fifo,
        },
        estimator: match request.estimator {
            EstimatorArg::Heuristic => frostbuild_exec::Estimator::Heuristic,
            EstimatorArg::Journal => frostbuild_exec::Estimator::Journal,
            EstimatorArg::Static => frostbuild_exec::Estimator::Static,
            EstimatorArg::Learned => frostbuild_exec::Estimator::Learned,
        },
        progress: Some(progress),
        remote: remote.clone(),
        timeout: request.timeout.map(std::time::Duration::from_secs),
        runs_per_test: request.runs_per_test,
        stamps,
        ..BuildOptions::default()
    };

    let started = Instant::now();
    let total = closure.len();
    let report = Engine::new(root, &graph, closure, toolchain, opts).run();
    renderer.finish();
    let report = report?;
    let elapsed = started.elapsed().as_millis();

    if request.explain {
        println!("explain:");
        // `explain` says an action reran; it cannot say why two *machines*
        // disagree, because that needs the other machine's key material. This
        // is the pointer from the one question to the other.
        println!("  (compare two builds with `frost journal export` and `frost journal diff`)");
        for result in &report.results {
            match &result.outcome {
                Outcome::Executed { reason, .. } => println!("  ran {} :: {reason}", result.id),
                Outcome::Flaky {
                    reason, attempts, ..
                } => println!(
                    "  flaky {} :: {reason} (passed on attempt {attempts}, not cached)",
                    result.id
                ),
                Outcome::Cached => println!("  cached {}", result.id),
                Outcome::Failed { reason, .. } => println!("  failed {} :: {reason}", result.id),
                Outcome::Skipped { reason } => println!("  skipped {} :: {reason}", result.id),
                Outcome::WouldRun { .. } | Outcome::MayRun { .. } => {}
            }
        }
    }

    let trace = match request.trace {
        Some(trace) => Some(write_trace(root, trace, &report)?),
        None => None,
    };

    if let Some(remote) = &remote {
        let summary = remote.summary();
        // Printed unconditionally when a remote cache was configured: a shared
        // cache that is silently failing looks exactly like one that is simply
        // cold, and the difference is worth a line.
        println!(
            "remote: {} hit, {} miss, {} down ({}), {} up ({}), {} rejected, {} error",
            summary.action_hits,
            summary.action_misses,
            summary.blobs_downloaded,
            human_bytes(summary.bytes_downloaded),
            summary.blobs_uploaded,
            human_bytes(summary.bytes_uploaded),
            summary.rejected,
            summary.errors,
        );
    }

    let failed = report.failed();
    let skipped = report.count(|outcome| matches!(outcome, Outcome::Skipped { .. }));
    println!(
        "{}",
        summarize(
            report.executed(),
            report.cached(),
            failed,
            skipped,
            total,
            graph.actions.len(),
            elapsed,
        )
    );
    if request.stats {
        let st = &report.stats;
        println!(
            "  strategy    {} / {}  (-j {})",
            st.scheduler, st.estimator, st.jobs
        );
        // Scheduling statistics describe how work was spread across workers.
        // A run that executed nothing has none to describe, and printing
        // "0 ms, 0.0%, 0.00x" reads like something went wrong.
        if st.executed == 0 {
            println!("  scheduling  nothing ran, so there was nothing to schedule");
        } else {
            println!(
                "  makespan    {} ms, {} ms of work across {} actions",
                st.makespan_ms, st.busy_ms, st.executed
            );
            println!(
                "  utilization {:.0}% of worker capacity",
                st.utilization_pct()
            );
            // makespan / critical path. Near 1 the graph is the limit; above
            // it there is ordering to win back; below it the estimate simply
            // over-predicted, and saying anything about scheduling on that
            // basis would be a claim the numbers do not support.
            match st.critical_path_ratio() {
                Some(ratio) if ratio < 0.95 => println!(
                    "  critical    {} ms estimated, longer than the run itself — \
                     the recorded durations are stale, so run again to compare",
                    st.critical_path_ms
                ),
                Some(ratio) if ratio <= 1.05 => println!(
                    "  critical    {} ms estimated — the dependency graph bounds \
                     this build, so no scheduler can improve it",
                    st.critical_path_ms
                ),
                Some(ratio) => println!(
                    "  critical    {} ms estimated, {:.1}x under the run — that \
                     gap is what a better schedule could win",
                    st.critical_path_ms, ratio
                ),
                None => {}
            }
        }
    }
    if failed > 0 {
        println!("failure summary (first 10):");
        for result in report
            .results
            .iter()
            .filter(|result| matches!(result.outcome, Outcome::Failed { .. }))
            .take(10)
        {
            if let Outcome::Failed { detail, .. } = &result.outcome {
                println!(
                    "  {}: {}",
                    result.id,
                    detail.lines().next().unwrap_or("failed")
                );
            }
        }
    }
    if request.test_mode {
        let tests = report
            .results
            .iter()
            .filter(|result| result.id.starts_with("test:"));
        let (mut passed, mut test_failed, mut cached, mut flaky, mut skipped) = (0, 0, 0, 0, 0);
        for test in tests {
            match test.outcome {
                Outcome::Executed { .. } => passed += 1,
                // A flake passed, so it is not a failure and the build is
                // green; but counting it under `passed` would erase the only
                // signal that this test cannot be trusted.
                Outcome::Flaky { .. } => flaky += 1,
                Outcome::Cached => cached += 1,
                Outcome::Failed { .. } => test_failed += 1,
                // Not run because something upstream failed. Reporting it as
                // a failure blames a test that never executed, which sends
                // the reader to the wrong file.
                Outcome::Skipped { .. } => skipped += 1,
                Outcome::WouldRun { .. } | Outcome::MayRun { .. } => {}
            }
        }
        // Replayed at the end, in full. During the run a failure scrolls away
        // behind the tests that were still going; the point of a summary is to
        // put the thing you have to read last.
        if request.test_output == TestOutputArg::Errors && test_failed > 0 {
            println!();
            for result in report
                .results
                .iter()
                .filter(|result| result.id.starts_with("test:"))
            {
                if let Outcome::Failed { detail, .. } = &result.outcome {
                    println!("--- {} ---", result.id);
                    let detail = detail.trim_end();
                    if !detail.is_empty() {
                        println!("{detail}");
                    }
                }
            }
            println!();
        }
        let mut summary = format!("tests: {passed} passed, {test_failed} failed, {cached} cached");
        // Only when non-zero: a line that always ends "0 flaky, 0 skipped"
        // trains the reader to stop reading it.
        if flaky > 0 {
            summary.push_str(&format!(", {flaky} flaky"));
        }
        if skipped > 0 {
            summary.push_str(&format!(", {skipped} skipped"));
        }
        println!("{summary}");
    }

    // Written last, and deliberately: the build has already been timed,
    // summarized and had its failures printed, so rendering cannot move any
    // number the report goes on to show. A failed build gets one too — that is
    // the build whose report someone actually wants.
    if let Some(destination) = request.report {
        let destination = destination
            .unwrap_or_else(|| report::default_path(&request.profile, &request.platform));
        let destination = if destination.is_absolute() {
            destination
        } else {
            root.join(destination)
        };
        report::write(
            &destination,
            &report::Build {
                workspace: root
                    .file_name()
                    .map_or_else(
                        || root.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    )
                    .as_str(),
                profile: &request.profile,
                platform: &request.platform,
                targets: &requested,
                report: &report,
                graph_actions: graph.actions.len(),
                elapsed_ms: elapsed,
                trace: trace.as_deref(),
                test_mode: request.test_mode,
            },
        )?;
        println!("frost: report {}", destination.display());
    }

    Ok(if frostbuild_exec::was_cancelled() {
        130
    } else if report.success() {
        0
    } else {
        1
    })
}

/// Returns where the trace landed, so a report written alongside it can link
/// to it rather than describing where to look.
fn write_trace(
    root: &std::path::Path,
    destination: PathBuf,
    report: &frostbuild_exec::BuildReport,
) -> Result<PathBuf> {
    let mut timestamp = 0u64;
    let mut events = Vec::new();
    for result in &report.results {
        if let Outcome::Executed { duration_ms, .. } | Outcome::Flaky { duration_ms, .. } =
            result.outcome
        {
            events.push(serde_json::json!({
                "name": result.desc,
                "cat": "action",
                "ph": "X",
                "pid": 1,
                "tid": 1,
                "ts": timestamp,
                "dur": duration_ms * 1000,
                "args": { "id": result.id },
            }));
            timestamp += duration_ms * 1000;
        }
    }
    let path = if destination.is_absolute() {
        destination
    } else {
        root.join(destination)
    };
    std::fs::write(
        &path,
        serde_json::to_vec(&serde_json::json!({ "traceEvents": events }))?,
    )?;
    Ok(path)
}

/// The toolchain fingerprint, with a missing tool attributed to the targets
/// that need it.
///
/// A wrapper rather than the `map_err` at each call site, because every path
/// that needs the fingerprint wants the same attribution: the reader who ran
/// `frost explain` deserves it as much as the one who ran `frost build`.
pub(crate) fn toolchain_fingerprint(root: &std::path::Path, graph: &BuildGraph) -> Result<String> {
    let needs_gcov = graph
        .actions
        .iter()
        .any(|action| action.kind == frostbuild_core::graph::ActionKind::Coverage);
    toolchain_closure_fingerprint_cached_instrumented(root, &graph.toolchain, needs_gcov)
        .map_err(|error| attribute_missing_tool(error, graph))
}

/// Turn the `--test-*` flags into the options the graph understands.
///
/// `KEY=VALUE` is split on the first `=` only, so a value may contain them.
/// An empty key or a missing `=` is rejected here rather than becoming a
/// variable nothing can read.
pub(crate) fn parse_test_options(
    filter: Option<String>,
    env: Vec<String>,
    args: Vec<String>,
) -> Result<frostbuild_core::graph::TestOptions> {
    let mut parsed = Vec::new();
    for entry in env {
        let Some((key, value)) = entry.split_once('=') else {
            bail!("--test-env expects KEY=VALUE, got {entry:?}");
        };
        if key.is_empty() {
            bail!("--test-env has an empty name in {entry:?}");
        }
        parsed.push((key.to_string(), value.to_string()));
    }
    Ok(frostbuild_core::graph::TestOptions {
        filter,
        env: parsed,
        args,
    })
}
pub(crate) fn default_jobs() -> usize {
    std::thread::available_parallelism().map_or(1, |n| n.get())
}
/// The line every build ends with, and the one people actually read.
///
/// It leads with what happened rather than a fixed set of counters, and drops
/// every term that is zero: a build where nothing needed doing says so in
/// three words instead of reporting four zeroes. The action count and the
/// share of the graph left out of this build appear only when they say
/// something — a full build of everything does not need to be told it built
/// everything.
pub(crate) fn summarize(
    executed: usize,
    cached: usize,
    failed: usize,
    skipped: usize,
    selected: usize,
    total_in_graph: usize,
    elapsed_ms: u128,
) -> String {
    let mut parts: Vec<String> = Vec::new();
    if failed > 0 {
        parts.push(format!("{failed} failed"));
    }
    if executed > 0 {
        parts.push(format!("{executed} built"));
    }
    if skipped > 0 {
        parts.push(format!("{skipped} skipped"));
    }
    // A run where everything was already current is the common case and
    // deserves words, not a row of counters.
    let headline = if parts.is_empty() && cached > 0 {
        "up to date".to_string()
    } else {
        if cached > 0 {
            parts.push(format!("{cached} cached"));
        }
        if parts.is_empty() {
            "nothing to do".to_string()
        } else {
            parts.join(", ")
        }
    };

    let pruned = total_in_graph.saturating_sub(selected);
    let scope = if pruned > 0 {
        format!("{selected} of {total_in_graph} actions")
    } else {
        format!("{selected} actions")
    };
    format!("frost: {headline} · {scope} · {elapsed_ms} ms")
}

/// Show what a build would do without doing it: the schedule, its critical
/// path and what the estimator expects each action to cost.
pub(crate) fn run_plan(
    root: &Path,
    targets: Vec<String>,
    profile: &str,
    platform: &str,
) -> Result<i32> {
    let graph = load_graph(root, profile, platform)?;
    let requested = resolve_targets(&graph, targets)?;
    let closure = graph.action_closure(&requested)?;
    let toolchain = toolchain_fingerprint(root, &graph)?;
    let opts = BuildOptions {
        jobs: default_jobs(),
        keep_going: true,
        dry_run: true,
        verbose: false,
        ..BuildOptions::default()
    };

    let total = closure.len();
    // Named before the actions, because "why is this compiling a
    // different file than I expected" is a question about the manifest
    // rather than about the plan.
    let shaped: Vec<&str> = graph
        .targets
        .values()
        .filter(|target| target.applied_platform.is_some())
        .map(|target| target.name.as_str())
        .collect();
    if !shaped.is_empty() {
        println!(
            "platform {platform}: section applied to {}",
            shaped.join(", ")
        );
    }
    let report = Engine::new(root, &graph, closure, toolchain, opts).run()?;

    for r in &report.results {
        match &r.outcome {
            Outcome::WouldRun { reason } => {
                println!("would run {} :: {reason}", r.id)
            }
            Outcome::MayRun { reason } => {
                println!("may run   {} :: {reason}", r.id)
            }
            _ => {}
        }
    }
    let would = report.count(|o| matches!(o, Outcome::WouldRun { .. }));
    let may = report.count(|o| matches!(o, Outcome::MayRun { .. }));
    println!(
        "plan: {} would run, {} may run, {} cached ({} actions)",
        would,
        may,
        report.cached(),
        total
    );
    Ok(0)
}

/// The summary line, which is the only build output most runs produce.
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn says_what_happened_and_omits_what_did_not() {
        assert_eq!(
            summarize(0, 5, 0, 0, 5, 5, 12),
            "frost: up to date · 5 actions · 12 ms"
        );
        assert_eq!(
            summarize(5, 0, 0, 0, 5, 5, 70),
            "frost: 5 built · 5 actions · 70 ms"
        );
        assert_eq!(
            summarize(2, 3, 0, 0, 5, 5, 40),
            "frost: 2 built, 3 cached · 5 actions · 40 ms"
        );
        // Failures lead, because that is what the reader needs first.
        assert_eq!(
            summarize(0, 3, 1, 1, 5, 5, 20),
            "frost: 1 failed, 1 skipped, 3 cached · 5 actions · 20 ms"
        );
        // Building a subset is worth saying; building everything is not.
        assert_eq!(
            summarize(0, 2, 0, 0, 2, 9, 5),
            "frost: up to date · 2 of 9 actions · 5 ms"
        );
        assert_eq!(
            summarize(0, 0, 0, 0, 0, 0, 1),
            "frost: nothing to do · 0 actions · 1 ms"
        );
    }
}
