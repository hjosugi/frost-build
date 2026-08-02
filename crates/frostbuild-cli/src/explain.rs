//! `frost explain`: why an action ran, or why it did not.

use std::path::Path;

use anyhow::Result;
use frostbuild_core::journal::Journal;
use frostbuild_exec::{BuildOptions, Engine, Outcome};

use crate::build::toolchain_fingerprint;
use crate::graph::{load_graph, resolve_targets};

/// Write what fed every action's key, joined from the four places that hold it.
///
/// The journal has the keys and input digests, the graph has argv and
/// environment, the toolchain fingerprint is computed per run, and the profile
/// and platform come from this invocation. Only together do they explain a
/// cache miss.
pub(crate) fn run_explain(
    root: &Path,
    target: String,
    profile: &str,
    platform: &str,
) -> Result<i32> {
    let graph = load_graph(root, profile, platform)?;
    let target = resolve_targets(&graph, vec![target])?.remove(0);
    let closure = graph.action_closure(std::slice::from_ref(&target))?;
    let current = Engine::new(
        root,
        &graph,
        closure,
        toolchain_fingerprint(root, &graph)?,
        BuildOptions {
            dry_run: true,
            keep_going: true,
            ..BuildOptions::default()
        },
    )
    .run()?;
    // Name the configuration the way the output tree and journal
    // already spell it. Reporting only the profile made `explain app`
    // and `explain app --platform device` print the same sentence for
    // two different builds.
    let configuration = frostbuild_core::paths::config(platform, profile);
    if current
        .results
        .iter()
        .all(|result| matches!(result.outcome, Outcome::Cached))
    {
        println!(
            "frost: no execution required for {target} ({configuration}); \
             all actions cached"
        );
        return Ok(0);
    }
    let journal = Journal::load(root);
    let mut found = 0;
    for action in graph
        .actions
        .iter()
        .filter(|action| action.target == target)
    {
        let id = frostbuild_exec::journal_id(&graph, action);
        if let Some(entry) = journal.actions.get(&id) {
            println!(
                "{} :: {} ({} ms)",
                action.id, entry.reason, entry.duration_ms
            );
            found += 1;
        }
    }
    if found == 0 {
        println!("frost: no recorded execution for {target} ({configuration})");
    }
    Ok(0)
}
