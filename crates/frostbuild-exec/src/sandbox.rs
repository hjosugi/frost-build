//! Running an action inside a sandbox, where the platform provides one.
//!
//! The sandbox is a check on the manifest, not a security boundary: it fails
//! a build that reads something it did not declare, which is the bug the
//! declaration exists to prevent.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;

use anyhow::Context;
use anyhow::Result;
use frostbuild_core::graph::BuildGraph;

pub(crate) fn sandbox_command(
    root: &Path,
    graph: &BuildGraph,
    action: &frostbuild_core::graph::ActionNode,
    inputs: &BTreeMap<String, String>,
    argv: &[String],
) -> Result<Command> {
    let bwrap = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .map(|dir| dir.join("bwrap"))
                .find(|candidate| candidate.is_file())
        })
        .context("--sandbox requires bubblewrap (bwrap) on Linux")?;
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
    let mut allowed = inputs.keys().cloned().collect::<BTreeSet<_>>();
    for &file in &action.order_only_inputs {
        allowed.insert(graph.files[file].path.clone());
    }
    let mut made_dirs = BTreeSet::new();
    for directory in readonly_dirs {
        add_sandbox_dirs(&mut command, root, directory.parent(), &mut made_dirs);
        command.arg("--ro-bind").arg(&directory).arg(&directory);
    }
    for rel in allowed {
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
    for directory in writable {
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
