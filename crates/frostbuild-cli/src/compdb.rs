//! `frost compdb`: `compile_commands.json` for clangd and the editors on it.

use std::path::{Path, PathBuf};

use anyhow::Result;
use frostbuild_core::graph::ActionKind;

use crate::graph::load_graph;

/// Write `compile_commands.json` for the configured graph, so clangd and the
/// editors built on it see the same flags the build uses.
pub(crate) fn run_compdb(
    root: &Path,
    output: PathBuf,
    profile: &str,
    platform: &str,
) -> Result<i32> {
    let graph = load_graph(root, profile, platform)?;
    let entries = graph
        .actions
        .iter()
        .filter(|action| action.kind == ActionKind::Compile)
        .map(|action| {
            let file = action
                .inputs
                .first()
                .map(|&id| graph.files[id].path.clone())
                .unwrap_or_default();
            serde_json::json!({
                "directory": root,
                "file": file,
                "arguments": action.argv,
                "output": action.outputs.first().map(|&id| graph.files[id].path.clone()),
            })
        })
        .collect::<Vec<_>>();
    let destination = if output.is_absolute() {
        output
    } else {
        root.join(output)
    };
    std::fs::write(&destination, serde_json::to_vec_pretty(&entries)?)?;
    println!(
        "frost: wrote {} entries to {}",
        entries.len(),
        destination.display()
    );
    Ok(0)
}
