//! `frost ide`: generate editor configuration from the graph.

use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use frostbuild_core::graph::BuildGraph;

use crate::build::{run_build, BuildRequest};
use crate::cli::{EstimatorArg, SchedulerArg, TestOutputArg};
use crate::graph::{load_graph, resolve_targets};
use crate::launch::{jar_main_class, target_runtime_output};

fn vscode_files(
    root: &Path,
    graph: &BuildGraph,
    target: &str,
    profile: &str,
    platform: &str,
    artifact: &Path,
) -> Result<(serde_json::Value, serde_json::Value, &'static str)> {
    let relative = artifact
        .strip_prefix(root)
        .with_context(|| format!("artifact {} is outside the workspace", artifact.display()))?;
    let artifact_variable = format!(
        "${{workspaceFolder}}/{}",
        relative.to_string_lossy().replace('\\', "/")
    );
    let task_label = format!("frost: build {target}");
    let extension = artifact
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let launch = match extension.as_str() {
        "jar" => serde_json::json!({
            "name": format!("Frost: debug {target}"),
            "type": "java",
            "request": "launch",
            "mainClass": jar_main_class(artifact)?,
            "classPaths": [artifact_variable],
            "cwd": "${workspaceFolder}",
            "preLaunchTask": task_label,
            "args": []
        }),
        "js" | "mjs" | "cjs" => {
            let closure = graph.action_closure(&[target.to_string()])?;
            let source_maps = closure.iter().any(|&action| {
                graph.actions[action].outputs.iter().any(|&output| {
                    graph.files[output]
                        .path
                        .to_ascii_lowercase()
                        .ends_with(".map")
                })
            });
            serde_json::json!({
                "name": format!("Frost: debug {target}"),
                "type": "node",
                "request": "launch",
                "program": artifact_variable,
                "cwd": "${workspaceFolder}",
                "preLaunchTask": task_label,
                "sourceMaps": source_maps,
                "args": []
            })
        }
        "py" | "pyw" => serde_json::json!({
            "name": format!("Frost: debug {target}"),
            "type": "debugpy",
            "request": "launch",
            "program": artifact_variable,
            "cwd": "${workspaceFolder}",
            "preLaunchTask": task_label,
            "args": []
        }),
        "whl" => bail!("a wheel has no direct IDE launch configuration; choose a runnable target"),
        _ => serde_json::json!({
            "name": format!("Frost: debug {target}"),
            "type": "cppdbg",
            "request": "launch",
            "program": artifact_variable,
            "cwd": "${workspaceFolder}",
            "preLaunchTask": task_label,
            "MIMode": if cfg!(target_os = "macos") { "lldb" } else { "gdb" },
            "args": [],
            "stopAtEntry": false
        }),
    };
    let problem_matcher = if matches!(
        extension.as_str(),
        "jar" | "js" | "mjs" | "cjs" | "py" | "pyw"
    ) {
        serde_json::json!([])
    } else {
        serde_json::json!(["$gcc"])
    };
    let tasks = serde_json::json!({
        "version": "2.0.0",
        "tasks": [{
            "label": task_label,
            "type": "process",
            "command": "frost",
            "args": [
                "-C", "${workspaceFolder}", "build", target,
                "--profile", profile, "--platform", platform, "--no-tui"
            ],
            "options": { "cwd": "${workspaceFolder}" },
            "problemMatcher": problem_matcher,
            "group": { "kind": "build", "isDefault": true }
        }]
    });
    let launches = serde_json::json!({
        "version": "0.2.0",
        "configurations": [launch]
    });
    let flavor = match extension.as_str() {
        "jar" => "Java",
        "js" | "mjs" | "cjs" => "JavaScript",
        "py" | "pyw" => "Python",
        _ => "native",
    };
    Ok((tasks, launches, flavor))
}

pub(crate) fn run_ide(
    root: &Path,
    target: Option<String>,
    jobs: Option<usize>,
    profile: String,
    platform: String,
    output: PathBuf,
    dry_run: bool,
) -> Result<i32> {
    let graph = load_graph(root, &profile, &platform)?;
    let targets = resolve_targets(&graph, target.into_iter().collect())?;
    anyhow::ensure!(
        targets.len() == 1,
        "ide requires exactly one target; choose one of: {}",
        targets.join(", ")
    );
    let target = targets[0].clone();
    let build_code = run_build(
        root,
        BuildRequest {
            targets: vec![target.clone()],
            jobs,
            keep_going: false,
            explain: false,
            verbose: false,
            profile: profile.clone(),
            platform: platform.clone(),
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
        },
    )?;
    if build_code != 0 {
        return Ok(build_code);
    }
    let graph = load_graph(root, &profile, &platform)?;
    let artifact = root.join(target_runtime_output(&graph, &target)?);
    anyhow::ensure!(
        artifact.is_file(),
        "IDE artifact {} was not produced",
        artifact.display()
    );
    let (tasks, launch, flavor) =
        vscode_files(root, &graph, &target, &profile, &platform, &artifact)?;
    if dry_run {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "tasks.json": tasks,
                "launch.json": launch,
            }))?
        );
        return Ok(0);
    }
    let output_text = output
        .to_str()
        .context("non-UTF-8 IDE output path is not supported")?;
    let relative = frostbuild_core::paths::validate_rel_path(output_text)
        .context("IDE output must be a workspace-relative directory")?;
    let directory = root.join(relative);
    let tasks_path = directory.join("tasks.json");
    let launch_path = directory.join("launch.json");
    for path in [&tasks_path, &launch_path] {
        anyhow::ensure!(
            !path.exists(),
            "{} already exists; use --dry-run and merge the Frost entry instead of overwriting it",
            path.display()
        );
    }
    std::fs::create_dir_all(&directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    std::fs::write(&tasks_path, serde_json::to_vec_pretty(&tasks)?)?;
    std::fs::write(&launch_path, serde_json::to_vec_pretty(&launch)?)?;
    println!("frost: IDE configuration");
    println!("|-- target   {target} ({flavor})");
    println!("|-- task     {}", tasks_path.display());
    println!("`-- launch   {}", launch_path.display());
    Ok(0)
}
