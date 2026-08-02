//! Loading the configured graph and naming targets in it.
//!
//! Nearly every command starts here, which is why it is its own module: the
//! warm path, the target-name errors and the missing-tool attribution are one
//! behaviour shared by all of them rather than something `build` happens to own.

use std::path::Path;

use anyhow::bail;
use anyhow::Result;
use frostbuild_core::graph::BuildGraph;
use frostbuild_core::graph_store::GraphStore;
use frostbuild_core::manifest::Manifest;

/// Load the configured graph, taking the manifest-free warm path when the
/// sources stamp proves the workspace inputs are unchanged; otherwise fall
/// back to a full manifest load and (re)compile.
pub(crate) fn load_graph(
    root: &std::path::Path,
    profile: &str,
    platform: &str,
) -> Result<BuildGraph> {
    if let Some(graph) = GraphStore::load_cached(root, profile, platform) {
        return Ok(graph);
    }
    let manifest = Manifest::load(root)?;
    GraphStore::load_or_compile_configured(root, &manifest, profile, platform)
}

/// Name the targets that need a tool frost could not find.
///
/// The executor resolves the whole toolchain up front, so it knows the tool
/// and the manifest line but not who asked for it. The graph is here, and
/// "which of my targets breaks" is the next question after "where do I install
/// it" — answering both in one message saves a round trip through `frost
/// query`.
pub(crate) fn attribute_missing_tool(error: anyhow::Error, graph: &BuildGraph) -> anyhow::Error {
    let text = format!("{error:#}");
    let Some(tool) = text
        .split_once("tool \"")
        .and_then(|(_, rest)| rest.split_once('"'))
        .map(|(name, _)| name.to_string())
    else {
        return error;
    };
    let mut targets: Vec<&str> = graph
        .actions
        .iter()
        .filter(|action| action.argv.first().is_some_and(|driver| *driver == tool))
        .map(|action| action.target.as_str())
        .collect();
    targets.sort_unstable();
    targets.dedup();
    if targets.is_empty() {
        return error;
    }
    let shown = targets.len().min(3);
    let attribution = format!(
        "  required by {}{}",
        targets[..shown].join(", "),
        if targets.len() > shown {
            format!(" and {} more", targets.len() - shown)
        } else {
            String::new()
        }
    );
    // Inserted above the closing advice rather than wrapped around the whole
    // message: `anyhow`'s context prefixes, which would put "required by X"
    // before "tool X not found" and read backwards.
    let advice = "  run `frost doctor`";
    let rebuilt = match text.split_once(advice) {
        Some((head, tail)) => format!("{head}{attribution}\n{advice}{tail}"),
        None => format!("{text}\n{attribution}"),
    };
    anyhow::anyhow!(rebuilt)
}

pub(crate) fn resolve_targets(graph: &BuildGraph, requested: Vec<String>) -> Result<Vec<String>> {
    if requested.is_empty() {
        return Ok(graph.default_targets.clone());
    }
    for name in &requested {
        if !graph.targets.contains_key(name) {
            let known: Vec<&str> = graph.targets.keys().map(String::as_str).collect();
            let hints = frostbuild_core::manifest::suggestions(name, known.iter().copied(), 3);
            if !hints.is_empty() {
                bail!(
                    "unknown target {name:?}. did you mean {}?",
                    hints
                        .iter()
                        .map(|hint| format!("{hint:?}"))
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
            // Listing every target is only useful while there are few. Past
            // that it is a wall of text hiding the one line that helps, so
            // point at the query that answers the question properly.
            if known.len() > 12 {
                bail!(
                    "unknown target {name:?}, and nothing similar. \
                     {} targets are defined; run `frost query deps //...` to list them",
                    known.len()
                );
            }
            bail!(
                "unknown target {name:?}. known targets: {}",
                known.join(", ")
            );
        }
    }
    Ok(requested)
}

/// Print the target graph, as text or as Graphviz `dot`.
pub(crate) fn run_graph(root: &Path, dot: bool, profile: &str, platform: &str) -> Result<i32> {
    let graph = load_graph(root, profile, platform)?;
    if dot {
        print!("{}", graph.to_dot());
    } else {
        for target in graph.targets.values() {
            let deps = if target.deps.is_empty() {
                String::new()
            } else {
                format!(" <- {}", target.deps.join(", "))
            };
            println!("{} [{}]{}", target.name, target.kind.as_str(), deps);
        }
    }
    Ok(0)
}
