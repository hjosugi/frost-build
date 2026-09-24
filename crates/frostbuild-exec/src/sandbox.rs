//! Running an action inside a sandbox, where the platform provides one.
//!
//! The sandbox is a check on the manifest, not a security boundary: it fails
//! a build that reads something it did not declare, which is the bug the
//! declaration exists to prevent.
//!
//! Two backends share one definition of what an action may see
//! ([`visible_set`]): bubblewrap on Linux, which hides the rest of the
//! workspace with mount namespaces, and `--hermetic` on every host, which
//! materializes the same set into a private tree and runs there
//! (`crate::hermetic`). One definition is what makes their verdicts comparable.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Result;
use frostbuild_core::graph::BuildGraph;

/// What an action may read and write, as the sandbox sees it.
pub(crate) struct Visible {
    /// Whole directories the action may read: the directory of every
    /// declared input and every `-I` directory inside the workspace, which is
    /// where a compiler finds the headers it will only report afterwards.
    pub(crate) readonly_dirs: BTreeSet<PathBuf>,
    /// Workspace-relative files the action may read: its inputs, including
    /// the ones the previous run discovered, and its order-only inputs.
    pub(crate) files: BTreeSet<String>,
    /// Directories the action writes into: output and depfile parents, owned
    /// output directories and clean directories.
    pub(crate) writable: BTreeSet<PathBuf>,
}

pub(crate) fn visible_set(
    root: &Path,
    graph: &BuildGraph,
    action: &frostbuild_core::graph::ActionNode,
    inputs: &BTreeMap<String, String>,
) -> Visible {
    let mut readonly_dirs = BTreeSet::new();
    for &file in &action.inputs {
        let relative = &graph.files[file].path;
        if !Path::new(relative).is_absolute() {
            if let Some(parent) = root.join(relative).parent() {
                readonly_dirs.insert(parent.to_path_buf());
            }
        }
    }
    for argv in std::iter::once(&action.argv).chain(&action.followup_argv) {
        let mut args = argv.iter().peekable();
        while let Some(arg) = args.next() {
            let include = if arg == "-I" {
                args.next().map(String::as_str)
            } else {
                arg.strip_prefix("-I").filter(|value| !value.is_empty())
            };
            if let Some(include) = include {
                let path = Path::new(include);
                let path = if path.is_absolute() {
                    path.to_path_buf()
                } else {
                    root.join(path)
                };
                if path.starts_with(root) && path.is_dir() {
                    readonly_dirs.insert(path);
                }
            }
        }
    }
    let mut files = inputs.keys().cloned().collect::<BTreeSet<_>>();
    for &file in &action.order_only_inputs {
        files.insert(graph.files[file].path.clone());
    }

    let mut writable = BTreeSet::new();
    for &file in &action.outputs {
        if let Some(parent) = root.join(&graph.files[file].path).parent() {
            writable.insert(parent.to_path_buf());
        }
    }
    if let Some(depfile) = &action.depfile {
        if let Some(parent) = root.join(depfile).parent() {
            writable.insert(parent.to_path_buf());
        }
    }
    for directory in &action.clean_dirs {
        writable.insert(root.join(directory));
    }
    for directory in &action.output_dirs {
        writable.insert(root.join(directory));
    }
    Visible {
        readonly_dirs,
        files,
        writable,
    }
}

/// The bubblewrap binary `--sandbox` would use, or why there is none.
///
/// Asked once, before a build starts, so a host that cannot sandbox says so in
/// one sentence naming the alternative instead of failing every action with
/// the same spawn error.
pub fn sandbox_backend() -> Result<PathBuf> {
    if !cfg!(target_os = "linux") {
        anyhow::bail!(
            "--sandbox uses bubblewrap, which only exists on Linux. On this host use \
             --hermetic, which runs each action in a private tree holding only the \
             files it may read"
        );
    }
    std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("bwrap"))
                .find(|candidate| candidate.is_file())
        })
        .ok_or_else(|| {
            anyhow::anyhow!(
                "--sandbox requires bubblewrap (bwrap) on PATH, and none was found. \
                 Install it (the package is usually named `bubblewrap`), or use \
                 --hermetic, which needs no extra tool: it runs each action in a \
                 private tree holding only the files it may read"
            )
        })
}

pub(crate) fn sandbox_command(
    root: &Path,
    graph: &BuildGraph,
    action: &frostbuild_core::graph::ActionNode,
    inputs: &BTreeMap<String, String>,
    argv: &[String],
) -> Result<Command> {
    let bwrap = sandbox_backend()?;
    let mut command = Command::new(bwrap);
    command.args([
        "--die-with-parent",
        "--unshare-pid",
        "--unshare-ipc",
        "--unshare-uts",
        "--ro-bind",
        "/",
        "/",
        "--tmpfs",
    ]);
    command.arg(root);

    let visible = visible_set(root, graph, action, inputs);
    let mut made_dirs = BTreeSet::new();
    for directory in visible.readonly_dirs {
        add_sandbox_dirs(&mut command, root, directory.parent(), &mut made_dirs);
        command.arg("--ro-bind").arg(&directory).arg(&directory);
    }
    for rel in visible.files {
        let source = Path::new(&rel);
        if source.is_absolute() {
            continue;
        }
        let source = root.join(&rel);
        if !source.exists() {
            continue;
        }
        let destination = root.join(&rel);
        add_sandbox_dirs(&mut command, root, destination.parent(), &mut made_dirs);
        command.arg("--ro-bind").arg(&source).arg(&destination);
    }
    for directory in visible.writable {
        std::fs::create_dir_all(&directory)?;
        add_sandbox_dirs(&mut command, root, directory.parent(), &mut made_dirs);
        command.arg("--bind").arg(&directory).arg(&directory);
    }
    command.arg("--chdir").arg(root).arg("--").args(argv);
    Ok(command)
}

fn add_sandbox_dirs(
    command: &mut Command,
    root: &Path,
    parent: Option<&Path>,
    made: &mut BTreeSet<PathBuf>,
) {
    let Some(parent) = parent else { return };
    let Ok(relative) = parent.strip_prefix(root) else {
        return;
    };
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component);
        if made.insert(current.clone()) {
            command.arg("--dir").arg(&current);
        }
    }
}
