//! Running and debugging what was built.
//!
//! Resolving the program, picking a debugger or runtime for the target's
//! language, and building the argv — kept apart from the build because a launch
//! fails differently: a wrong path produces a process that starts and does the
//! wrong thing rather than one that fails.

use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use frostbuild_core::graph::ActionKind;
use frostbuild_core::graph::BuildGraph;

use crate::build::{run_build, BuildRequest};
use crate::cli::{EstimatorArg, SchedulerArg, TestOutputArg};
use crate::graph::{load_graph, resolve_targets};

pub(crate) fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub(crate) fn find_on_path(name: &str) -> Option<PathBuf> {
    frostbuild_core::paths::find_on_path(name, is_executable_file)
}

fn resolve_program(root: &Path, selected: PathBuf, label: &str) -> Result<PathBuf> {
    if selected.is_absolute() || selected.components().count() > 1 {
        let resolved = if selected.is_absolute() {
            selected
        } else {
            root.join(selected)
        };
        anyhow::ensure!(
            is_executable_file(&resolved),
            "{label} {} does not exist",
            resolved.display()
        );
        return Ok(resolved);
    }
    find_on_path(selected.to_string_lossy().as_ref())
        .with_context(|| format!("{label} {:?} was not found on PATH", selected))
}

fn select_debugger(root: &Path, requested: &str) -> Result<PathBuf> {
    let selected = if requested == "auto" {
        if let Some(configured) = std::env::var_os("FROST_DEBUGGER") {
            PathBuf::from(configured)
        } else {
            find_on_path("gdb")
                .or_else(|| find_on_path("lldb"))
                .context("no debugger found; install gdb/lldb or pass --debugger PATH")?
        }
    } else {
        PathBuf::from(requested)
    };
    resolve_program(root, selected, "debugger")
}

pub(crate) fn command_path(path: &Path) -> String {
    #[cfg(windows)]
    {
        use std::ffi::OsString;
        use std::os::windows::ffi::{OsStrExt as _, OsStringExt as _};

        const VERBATIM_PREFIX: &[u16] = &[b'\\' as u16, b'\\' as u16, b'?' as u16, b'\\' as u16];
        const VERBATIM_UNC_PREFIX: &[u16] = &[
            b'\\' as u16,
            b'\\' as u16,
            b'?' as u16,
            b'\\' as u16,
            b'U' as u16,
            b'N' as u16,
            b'C' as u16,
            b'\\' as u16,
        ];

        let wide = path.as_os_str().encode_wide().collect::<Vec<_>>();
        let normalized = if let Some(suffix) = wide.strip_prefix(VERBATIM_UNC_PREFIX) {
            let mut normalized = vec![b'\\' as u16, b'\\' as u16];
            normalized.extend_from_slice(suffix);
            normalized
        } else if let Some(suffix) = wide.strip_prefix(VERBATIM_PREFIX) {
            suffix.to_vec()
        } else {
            wide
        };
        return OsString::from_wide(&normalized)
            .to_string_lossy()
            .into_owned();
    }

    #[cfg(not(windows))]
    {
        path.display().to_string()
    }
}

fn debugger_argv(debugger: &Path, binary: &Path, program_args: &[String]) -> Vec<String> {
    let name = debugger
        .file_name()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut argv = vec![command_path(debugger)];
    if name.contains("lldb") {
        argv.push("--".into());
    } else {
        argv.push("--args".into());
    }
    argv.push(command_path(binary));
    argv.extend(program_args.iter().cloned());
    argv
}

pub(crate) fn target_runtime_output(graph: &BuildGraph, target: &str) -> Result<PathBuf> {
    let closure = graph.action_closure(&[target.to_string()])?;
    let link_output = closure
        .iter()
        .map(|&action| &graph.actions[action])
        .find(|action| action.kind == ActionKind::Link)
        .and_then(|action| action.outputs.first())
        .copied();
    let output = link_output
        .or_else(|| graph.targets[target].outputs.first().copied())
        .context("target has no runnable output")?;
    Ok(PathBuf::from(&graph.files[output].path))
}

fn select_language_debugger(
    root: &Path,
    requested: &str,
    environment: &str,
    candidates: &[&str],
) -> Result<PathBuf> {
    if requested != "auto" {
        return select_debugger(root, requested);
    }
    if let Some(configured) = std::env::var_os(environment) {
        return select_debugger(root, Path::new(&configured).to_string_lossy().as_ref());
    }
    candidates
        .iter()
        .find_map(|candidate| find_on_path(candidate))
        .with_context(|| {
            format!(
                "no {} debugger found; install {} or pass --debugger PATH",
                candidates.join("/"),
                candidates.join(" or ")
            )
        })
}

pub(crate) fn jar_main_class(path: &Path) -> Result<String> {
    use std::io::Read as _;

    let file = std::fs::File::open(path)
        .with_context(|| format!("failed to open JAR {}", path.display()))?;
    let mut archive =
        zip::ZipArchive::new(file).with_context(|| format!("invalid JAR {}", path.display()))?;
    let mut raw = String::new();
    archive
        .by_name("META-INF/MANIFEST.MF")
        .context("JAR has no META-INF/MANIFEST.MF")?
        .read_to_string(&mut raw)
        .context("JAR manifest is not UTF-8")?;
    let mut unfolded = Vec::<String>::new();
    for line in raw.lines() {
        if let Some(continuation) = line.strip_prefix(' ') {
            let previous = unfolded
                .last_mut()
                .context("JAR manifest starts with a continuation line")?;
            previous.push_str(continuation);
        } else {
            unfolded.push(line.trim_end_matches('\r').to_string());
        }
    }
    unfolded
        .into_iter()
        .find_map(|line| line.strip_prefix("Main-Class: ").map(str::to_string))
        .context("JAR has no Main-Class; add pack-jar --main-class or use a direct command")
}

pub(crate) fn language_debug_argv(
    root: &Path,
    requested: &str,
    output: &Path,
    program_args: &[String],
) -> Result<(PathBuf, Vec<String>, &'static str)> {
    let extension = output
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    match extension.as_str() {
        "jar" => {
            let debugger = select_language_debugger(root, requested, "JDB_BIN", &["jdb"])?;
            let main_class = jar_main_class(output)?;
            let mut argv = vec![
                command_path(&debugger),
                "-classpath".into(),
                command_path(output),
                main_class,
            ];
            argv.extend(program_args.iter().cloned());
            Ok((debugger, argv, "Java/jdb"))
        }
        "js" | "mjs" | "cjs" => {
            let debugger = select_language_debugger(root, requested, "NODE_BIN", &["node"])?;
            let mut argv = vec![
                command_path(&debugger),
                "inspect".into(),
                command_path(output),
            ];
            argv.extend(program_args.iter().cloned());
            Ok((debugger, argv, "JavaScript/Node inspector"))
        }
        "py" | "pyw" => {
            let debugger =
                select_language_debugger(root, requested, "PYTHON_BIN", &["python3", "python"])?;
            let mut argv = vec![
                command_path(&debugger),
                "-m".into(),
                "pdb".into(),
                command_path(output),
            ];
            argv.extend(program_args.iter().cloned());
            Ok((debugger, argv, "Python/pdb"))
        }
        _ => {
            let debugger = select_debugger(root, requested)?;
            let argv = debugger_argv(&debugger, output, program_args);
            Ok((debugger, argv, "native"))
        }
    }
}

fn select_runtime(root: &Path, environment: &str, candidates: &[&str]) -> Result<PathBuf> {
    if let Some(configured) = std::env::var_os(environment) {
        return resolve_program(root, PathBuf::from(configured), "runtime");
    }
    candidates
        .iter()
        .find_map(|candidate| find_on_path(candidate))
        .with_context(|| format!("runtime {} was not found on PATH", candidates.join("/")))
}

pub(crate) fn runtime_argv(
    root: &Path,
    output: &Path,
    runner: Option<&Path>,
    program_args: &[String],
) -> Result<(Vec<String>, &'static str)> {
    if let Some(runner) = runner {
        let runner = resolve_program(root, runner.to_path_buf(), "runner")?;
        let mut argv = vec![command_path(&runner), command_path(output)];
        argv.extend(program_args.iter().cloned());
        return Ok((argv, "explicit runner"));
    }
    let extension = output
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase();
    let (mut argv, flavor) = match extension.as_str() {
        "jar" => {
            let java = select_runtime(root, "JAVA_BIN", &["java"])?;
            (
                vec![
                    command_path(&java),
                    "-jar".into(),
                    command_path(output),
                ],
                "Java",
            )
        }
        "js" | "mjs" | "cjs" => {
            let node = select_runtime(root, "NODE_BIN", &["node"])?;
            (
                vec![command_path(&node), command_path(output)],
                "JavaScript",
            )
        }
        "py" | "pyw" => {
            let python = select_runtime(root, "PYTHON_BIN", &["python3", "python"])?;
            (
                vec![command_path(&python), command_path(output)],
                "Python",
            )
        }
        "whl" => bail!(
            "a wheel is installable, not directly runnable; select a runnable target or pass --runner"
        ),
        _ => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let mode = std::fs::metadata(output)?.permissions().mode();
                anyhow::ensure!(
                    mode & 0o111 != 0,
                    "output {} is not executable; use --runner for a custom artifact",
                    output.display()
                );
            }
            (vec![command_path(output)], "native")
        }
    };
    argv.extend(program_args.iter().cloned());
    Ok((argv, flavor))
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_target(
    root: &Path,
    target: Option<String>,
    jobs: Option<usize>,
    profile: String,
    platform: String,
    runner: Option<PathBuf>,
    print: bool,
    program_args: Vec<String>,
) -> Result<i32> {
    let graph = load_graph(root, &profile, &platform)?;
    let targets = resolve_targets(&graph, target.into_iter().collect())?;
    anyhow::ensure!(
        targets.len() == 1,
        "run requires exactly one target; choose one of: {}",
        targets.join(", ")
    );
    let target = targets[0].clone();
    if platform != frostbuild_core::manifest::HOST_PLATFORM && runner.is_none() {
        bail!(
            "cannot execute platform {platform:?} on the host directly; pass --runner for an emulator"
        );
    }
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
            coverage: false,
        },
    )?;
    if build_code != 0 {
        return Ok(build_code);
    }
    let graph = load_graph(root, &profile, &platform)?;
    let output = root.join(target_runtime_output(&graph, &target)?);
    anyhow::ensure!(
        output.is_file(),
        "run target output {} was not produced",
        output.display()
    );
    let (argv, flavor) = runtime_argv(root, &output, runner.as_deref(), &program_args)?;
    println!("frost: run");
    println!("|-- target    {target}");
    println!("|-- artifact  {}", output.display());
    println!("|-- profile   {profile} / {platform}");
    println!("`-- runtime   {flavor}");
    if print {
        println!("{}", serde_json::to_string(&argv)?);
        return Ok(0);
    }
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(root)
        .status()
        .context("failed to run built artifact")?;
    Ok(status.code().unwrap_or(1))
}
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_debug(
    root: &Path,
    target: Option<String>,
    jobs: Option<usize>,
    profile: String,
    platform: String,
    debugger: String,
    print: bool,
    program_args: Vec<String>,
) -> Result<i32> {
    let graph = load_graph(root, &profile, &platform)?;
    let targets = resolve_targets(&graph, target.into_iter().collect())?;
    anyhow::ensure!(
        targets.len() == 1,
        "debug requires exactly one target; choose one of: {}",
        targets.join(", ")
    );
    let target = targets[0].clone();
    let closure = graph.action_closure(std::slice::from_ref(&target))?;
    let compile_actions = closure
        .iter()
        .map(|&action| &graph.actions[action])
        .filter(|action| action.kind == ActionKind::Compile)
        .collect::<Vec<_>>();
    if !compile_actions.is_empty()
        && compile_actions.iter().any(|action| {
            !action
                .argv
                .iter()
                .any(|arg| arg == "/Zi" || arg == "/Z7" || arg == "-ggdb" || arg.starts_with("-g"))
        })
    {
        bail!(
            "target {target:?} is not compiled with debug symbols in profile {profile:?}; \
             add [profile.{profile}] cflags = [\"-O0\", \"-g\"]"
        );
    }

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
            coverage: false,
        },
    )?;
    if build_code != 0 {
        return Ok(build_code);
    }

    let graph = load_graph(root, &profile, &platform)?;
    let binary = root.join(target_runtime_output(&graph, &target)?);
    anyhow::ensure!(
        binary.is_file(),
        "debug target output {} was not produced",
        binary.display()
    );
    let (debugger, argv, flavor) = language_debug_argv(root, &debugger, &binary, &program_args)?;
    println!("frost: debug");
    println!("|-- target    {target}");
    println!("|-- binary    {}", binary.display());
    println!("|-- profile   {profile} / {platform}");
    println!("|-- mode      {flavor}");
    println!("`-- debugger  {}", debugger.display());
    if print {
        println!("{}", serde_json::to_string(&argv)?);
        return Ok(0);
    }
    let status = Command::new(&argv[0])
        .args(&argv[1..])
        .current_dir(root)
        .status()
        .context("failed to launch debugger")?;
    Ok(status.code().unwrap_or(1))
}

/// Argv construction, which is where a launch goes wrong silently:
/// a wrong path or a missing `-classpath` produces a process that
/// starts and does the wrong thing rather than one that fails.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::jar;

    #[cfg(windows)]
    #[test]
    fn child_process_paths_drop_windows_verbatim_prefixes() {
        assert_eq!(
            command_path(Path::new(r"\\?\C:\workspace\app.jar")),
            r"C:\workspace\app.jar"
        );
        assert_eq!(
            command_path(Path::new(r"\\?\UNC\server\share\app.jar")),
            r"\\server\share\app.jar"
        );
    }

    #[test]
    fn debug_selects_language_native_argv_without_a_shell() {
        let root = Path::new("/");
        let executable = std::env::current_exe().unwrap();
        let debugger = executable.to_string_lossy().into_owned();
        let (debugger, javascript, flavor) = language_debug_argv(
            root,
            &debugger,
            Path::new("/workspace/app.js"),
            &["--port".into(), "3000".into()],
        )
        .unwrap();
        assert_eq!(debugger, executable);
        assert_eq!(
            javascript,
            [
                debugger.to_string_lossy().as_ref(),
                "inspect",
                "/workspace/app.js",
                "--port",
                "3000"
            ]
        );
        assert_eq!(flavor, "JavaScript/Node inspector");

        let (_, python, flavor) = language_debug_argv(
            root,
            debugger.to_string_lossy().as_ref(),
            Path::new("/workspace/app.py"),
            &[],
        )
        .unwrap();
        assert_eq!(
            python,
            [
                debugger.to_string_lossy().as_ref(),
                "-m",
                "pdb",
                "/workspace/app.py"
            ]
        );
        assert_eq!(flavor, "Python/pdb");
    }

    #[test]
    fn debug_reads_an_executable_jars_main_class() {
        let root = std::env::temp_dir().join(format!("frost-debug-jar-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("classes/pkg")).unwrap();
        std::fs::write(root.join("classes/pkg/Main.class"), b"class").unwrap();
        jar::pack(
            &root,
            Path::new("classes"),
            Path::new("out/app.jar"),
            Some("pkg.Main"),
        )
        .unwrap();
        let executable = std::env::current_exe().unwrap();
        let debugger = executable.to_string_lossy();
        let (_, argv, flavor) = language_debug_argv(
            &root,
            &debugger,
            &root.join("out/app.jar"),
            &["argument".into()],
        )
        .unwrap();
        assert_eq!(
            argv,
            [
                debugger.as_ref(),
                "-classpath",
                root.join("out/app.jar").to_str().unwrap(),
                "pkg.Main",
                "argument"
            ]
        );
        assert_eq!(flavor, "Java/jdb");
        std::fs::remove_dir_all(root).ok();
    }
}
