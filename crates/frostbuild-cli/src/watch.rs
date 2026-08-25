//! `frost watch` and `frost dev`: rebuild when the workspace changes.
//!
//! Most of this module is about what *not* to rebuild for. A build writes into
//! the workspace it is watching, so without exclusions the first build triggers
//! the second.

use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Child;
use std::process::Command;
use std::sync::mpsc::RecvTimeoutError;
use std::sync::mpsc::{self};
use std::time::Duration;
use std::time::Instant;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use notify::RecursiveMode;
use notify::Watcher;

use crate::build::{run_build, BuildRequest};
use crate::cli::{EstimatorArg, SchedulerArg, TestOutputArg};
use crate::graph::{load_graph, resolve_targets};
use crate::launch::{runtime_argv, target_runtime_output};

pub(crate) struct WatchRequest {
    pub(crate) targets: Vec<String>,
    pub(crate) jobs: Option<usize>,
    pub(crate) profile: String,
    pub(crate) platform: String,
    pub(crate) debounce: Duration,
    pub(crate) run: Vec<String>,
    pub(crate) auto_run: Option<AutoRun>,
}

pub(crate) struct AutoRun {
    pub(crate) target: String,
    pub(crate) runner: Option<PathBuf>,
    pub(crate) program_args: Vec<String>,
}

#[derive(Default)]
pub(crate) struct WatchExclusions {
    pub(crate) outputs: BTreeSet<PathBuf>,
    pub(crate) clean_dirs: Vec<PathBuf>,
}

fn watch_build_request(request: &WatchRequest) -> BuildRequest {
    BuildRequest {
        targets: request.targets.clone(),
        jobs: request.jobs,
        keep_going: true,
        explain: false,
        verbose: false,
        profile: request.profile.clone(),
        platform: request.platform.clone(),
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
        test_mode: false,
        test_options: Default::default(),
        runs_per_test: 1,
        test_output: TestOutputArg::Errors,
        build_event_json: None,
        no_stamp: false,
        stamp_optional: false,
        daemon: false,
        affected: false,
        predictive: false,
        all: false,
        scheduler: SchedulerArg::CriticalPath,
        estimator: EstimatorArg::Journal,
        coverage: false,
    }
}

fn watch_exclusions(root: &Path, profile: &str, platform: &str) -> WatchExclusions {
    let Ok(graph) = load_graph(root, profile, platform) else {
        return WatchExclusions::default();
    };
    WatchExclusions {
        outputs: graph
            .actions
            .iter()
            .flat_map(|action| action.outputs.iter())
            .map(|&output| PathBuf::from(&graph.files[output].path))
            .collect(),
        clean_dirs: graph
            .actions
            .iter()
            .flat_map(|action| action.clean_dirs.iter())
            .map(PathBuf::from)
            .collect(),
    }
}

pub(crate) fn relevant_watch_path(
    root: &Path,
    path: &Path,
    exclusions: &WatchExclusions,
) -> Option<PathBuf> {
    let relative = path.strip_prefix(root).unwrap_or(path);
    if relative.as_os_str().is_empty()
        || relative.starts_with(".frost")
        || relative.starts_with(".git")
        || exclusions.outputs.contains(relative)
        || exclusions
            .clean_dirs
            .iter()
            .any(|directory| relative.starts_with(directory))
    {
        return None;
    }
    Some(relative.to_path_buf())
}

pub(crate) fn watch_event_changes_files(kind: &notify::EventKind) -> bool {
    matches!(
        kind,
        notify::EventKind::Any
            | notify::EventKind::Create(_)
            | notify::EventKind::Modify(_)
            | notify::EventKind::Remove(_)
    )
}

pub(crate) fn stop_dev_process(child: &mut Option<Child>) {
    let Some(mut running) = child.take() else {
        return;
    };
    if running.try_wait().ok().flatten().is_some() {
        return;
    }
    let pid = running.id();
    #[cfg(unix)]
    unsafe {
        // The process was placed in its own group immediately before spawn.
        // Terminating the group also stops language servers, web servers and
        // Bazel-run children that would otherwise survive a hot restart.
        libc::kill(-(pid as i32), libc::SIGTERM);
    }
    #[cfg(windows)]
    {
        let _ = Command::new("taskkill")
            .args(["/PID", &pid.to_string(), "/T", "/F"])
            .status();
    }
    for _ in 0..20 {
        if running.try_wait().ok().flatten().is_some() {
            return;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    #[cfg(unix)]
    unsafe {
        libc::kill(-(pid as i32), libc::SIGKILL);
    }
    let _ = running.kill();
    let _ = running.wait();
}

pub(crate) fn configure_dev_command(_command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        _command.process_group(0);
    }
}

fn restart_dev_process(root: &Path, argv: &[String], child: &mut Option<Child>) -> Result<()> {
    if argv.is_empty() {
        return Ok(());
    }
    stop_dev_process(child);
    let mut command = Command::new(&argv[0]);
    command.args(&argv[1..]).current_dir(root);
    configure_dev_command(&mut command);
    let running = command
        .spawn()
        .with_context(|| format!("failed to start watch process {:?}", argv))?;
    println!("`-- dev process restarted · pid {}", running.id());
    *child = Some(running);
    Ok(())
}

fn watch_run_argv(root: &Path, request: &WatchRequest) -> Result<Vec<String>> {
    if !request.run.is_empty() {
        return Ok(request.run.clone());
    }
    let Some(auto) = &request.auto_run else {
        return Ok(Vec::new());
    };
    let graph = load_graph(root, &request.profile, &request.platform)?;
    let output = root.join(target_runtime_output(&graph, &auto.target)?);
    anyhow::ensure!(
        output.is_file(),
        "dev target output {} was not produced",
        output.display()
    );
    runtime_argv(root, &output, auto.runner.as_deref(), &auto.program_args).map(|(argv, _)| argv)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_dev(
    root: &Path,
    target: Option<String>,
    jobs: Option<usize>,
    profile: String,
    platform: String,
    debounce: Duration,
    runner: Option<PathBuf>,
    program_args: Vec<String>,
) -> Result<i32> {
    let graph = load_graph(root, &profile, &platform)?;
    let targets = resolve_targets(&graph, target.into_iter().collect())?;
    anyhow::ensure!(
        targets.len() == 1,
        "dev requires exactly one target; choose one of: {}",
        targets.join(", ")
    );
    if platform != frostbuild_core::manifest::HOST_PLATFORM && runner.is_none() {
        bail!(
            "cannot execute platform {platform:?} on the host directly; pass --runner for an emulator"
        );
    }
    run_watch(
        root,
        WatchRequest {
            targets: targets.clone(),
            jobs,
            profile,
            platform,
            debounce,
            run: Vec::new(),
            auto_run: Some(AutoRun {
                target: targets[0].clone(),
                runner,
                program_args,
            }),
        },
    )
}

pub(crate) fn run_watch(root: &Path, request: WatchRequest) -> Result<i32> {
    let (sender, receiver) = mpsc::channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = sender.send(event);
    })?;
    watcher.watch(root, RecursiveMode::Recursive)?;

    println!(
        "frost: watch · profile {} · platform {} · debounce {} ms",
        request.profile,
        request.platform,
        request.debounce.as_millis()
    );
    println!("|-- initial build");
    let mut child = None;
    match run_build(root, watch_build_request(&request)) {
        Ok(0) => {
            let argv = watch_run_argv(root, &request);
            if let Err(error) = argv.and_then(|argv| restart_dev_process(root, &argv, &mut child)) {
                eprintln!("|   dev process: {error:#}");
            }
        }
        Ok(code) => eprintln!("|   initial build failed (exit {code}); watching for a fix"),
        Err(error) => eprintln!("|   initial build failed: {error:#}; watching for a fix"),
    }
    println!("`-- ready · Ctrl-C stops");

    let mut exclusions = watch_exclusions(root, &request.profile, &request.platform);
    let mut change_set = 0usize;
    while !frostbuild_exec::was_cancelled() {
        if let Some(running) = child.as_mut() {
            if let Some(status) = running.try_wait()? {
                println!("frost: dev process exited · {status}");
                child = None;
            }
        }

        let first = match receiver.recv_timeout(Duration::from_millis(200)) {
            Ok(Ok(event)) => event,
            Ok(Err(error)) => {
                eprintln!("frost: watch error: {error}");
                continue;
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => bail!("filesystem watcher stopped"),
        };
        let mut changed = if watch_event_changes_files(&first.kind) {
            first
                .paths
                .iter()
                .filter_map(|path| relevant_watch_path(root, path, &exclusions))
                .collect::<BTreeSet<_>>()
        } else {
            BTreeSet::new()
        };
        let mut deadline = Instant::now() + request.debounce;
        while let Some(remaining) = deadline.checked_duration_since(Instant::now()) {
            match receiver.recv_timeout(remaining) {
                Ok(Ok(event)) => {
                    let before = changed.len();
                    if watch_event_changes_files(&event.kind) {
                        changed.extend(
                            event
                                .paths
                                .iter()
                                .filter_map(|path| relevant_watch_path(root, path, &exclusions)),
                        );
                    }
                    if changed.len() > before {
                        deadline = Instant::now() + request.debounce;
                    }
                }
                Ok(Err(error)) => eprintln!("frost: watch error: {error}"),
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => bail!("filesystem watcher stopped"),
            }
        }
        if changed.is_empty() {
            continue;
        }

        change_set += 1;
        println!(
            "frost: change #{change_set} · {} path{}",
            changed.len(),
            if changed.len() == 1 { "" } else { "s" }
        );
        for (index, path) in changed.iter().take(4).enumerate() {
            let last = index + 1 == changed.len().min(4);
            println!("{} {}", if last { "`--" } else { "|--" }, path.display());
        }
        if changed.len() > 4 {
            println!("    … and {} more", changed.len() - 4);
        }

        match run_build(root, watch_build_request(&request)) {
            Ok(0) => {
                exclusions = watch_exclusions(root, &request.profile, &request.platform);
                let argv = watch_run_argv(root, &request);
                if let Err(error) =
                    argv.and_then(|argv| restart_dev_process(root, &argv, &mut child))
                {
                    eprintln!("frost: dev process: {error:#}");
                }
            }
            Ok(code) => eprintln!(
                "frost: build failed (exit {code}); keeping the last successful dev process"
            ),
            Err(error) => {
                eprintln!("frost: build failed: {error:#}; keeping the last successful dev process")
            }
        }
    }
    stop_dev_process(&mut child);
    println!("frost: watch stopped");
    Ok(130)
}

/// What the watcher decides to ignore, and how it reads `--run`.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli::{Cli, Cmd};
    use clap::Parser;

    #[test]
    fn watch_ignores_self_writes_but_keeps_sources_and_manifests() {
        let root = Path::new("/workspace");
        let exclusions = WatchExclusions {
            outputs: BTreeSet::from([PathBuf::from("dist/app.js")]),
            clean_dirs: vec![PathBuf::from("tmp/generated")],
        };
        for ignored in [
            ".frost/out/debug/app",
            ".git/index",
            "dist/app.js",
            "tmp/generated/member.js",
        ] {
            assert!(
                relevant_watch_path(root, &root.join(ignored), &exclusions).is_none(),
                "{ignored}"
            );
        }
        for watched in ["src/app.ts", "frost.toml", "dist/source.ts"] {
            assert_eq!(
                relevant_watch_path(root, &root.join(watched), &exclusions),
                Some(PathBuf::from(watched))
            );
        }
    }

    #[test]
    fn watch_ignores_read_access_but_keeps_content_events() {
        use notify::event::{AccessKind, CreateKind, ModifyKind, RemoveKind};
        use notify::EventKind;

        assert!(!watch_event_changes_files(&EventKind::Access(
            AccessKind::Any
        )));
        assert!(watch_event_changes_files(&EventKind::Create(
            CreateKind::Any
        )));
        assert!(watch_event_changes_files(&EventKind::Modify(
            ModifyKind::Any
        )));
        assert!(watch_event_changes_files(&EventKind::Remove(
            RemoveKind::Any
        )));
    }

    #[test]
    fn watch_parses_a_direct_dev_process_argv() {
        let cli = Cli::try_parse_from([
            "frost",
            "watch",
            "app",
            "--debounce-ms",
            "25",
            "--run",
            "node",
            "dist/app.js",
        ])
        .unwrap();
        let Cmd::Watch {
            targets,
            debounce_ms,
            run,
            ..
        } = cli.command
        else {
            panic!("watch command was not parsed")
        };
        assert_eq!(targets, vec!["app"]);
        assert_eq!(debounce_ms, 25);
        assert_eq!(run, vec!["node", "dist/app.js"]);
    }
}
