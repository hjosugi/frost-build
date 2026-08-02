//! `frost journal`: exporting and diffing what past builds recorded.

use anyhow::Context;
use anyhow::Result;
use frostbuild_exec::toolchain_closure_fingerprint_cached;

use crate::graph::{attribute_missing_tool, load_graph};

pub(crate) fn run_journal_export(
    root: &std::path::Path,
    profile: &str,
    platform: &str,
    out: Option<&std::path::Path>,
) -> Result<i32> {
    use frostbuild_core::journal_export::{ActionExport, JournalExport, EXPORT_FORMAT};

    let graph = load_graph(root, profile, platform)?;
    let journal = frostbuild_core::journal::Journal::load(root);
    let toolchain = toolchain_closure_fingerprint_cached(root, &graph.toolchain)
        .map_err(|error| attribute_missing_tool(error, &graph))?;

    let mut actions = std::collections::BTreeMap::new();
    for action in &graph.actions {
        // The journal keys by action id *and configuration* — the same action
        // built for two profiles has two entries — so the lookup must use the
        // same composite the executor recorded under. Using `action.id` finds
        // nothing, silently, and exports an empty file.
        let Some(entry) = journal
            .actions
            .get(&frostbuild_exec::journal_id(&graph, action))
        else {
            continue;
        };
        actions.insert(
            action.id.clone(),
            ActionExport {
                key: entry.key.clone(),
                argv: action.argv.clone(),
                env: action.env.clone(),
                pass_env: action.pass_env.clone(),
                inputs: entry.inputs.clone(),
                outputs: entry.outputs.clone(),
            },
        );
    }

    let export = JournalExport {
        format: EXPORT_FORMAT.to_string(),
        action_key_schema: frostbuild_exec::ACTION_KEY_SCHEMA.to_string(),
        profile: profile.to_string(),
        platform: platform.to_string(),
        toolchain,
        actions,
    };
    let text = export.to_json()?;
    match out {
        Some(path) => {
            std::fs::write(path, format!("{text}\n"))
                .with_context(|| format!("failed to write {}", path.display()))?;
            println!(
                "journal: {} actions to {}",
                export.actions.len(),
                path.display()
            );
        }
        None => println!("{text}"),
    }
    Ok(0)
}

pub(crate) fn run_journal_diff(first: &std::path::Path, second: &std::path::Path) -> Result<i32> {
    use frostbuild_core::journal_export::JournalExport;

    let read = |path: &std::path::Path| -> Result<JournalExport> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        JournalExport::from_json(&text)
            .with_context(|| format!("failed to read {}", path.display()))
    };
    let differences = frostbuild_core::journal_export::diff(&read(first)?, &read(second)?);
    if differences.is_empty() {
        println!("journal: identical");
        return Ok(0);
    }
    println!(
        "journal: {} difference{}",
        differences.len(),
        if differences.len() == 1 { "" } else { "s" }
    );
    for difference in &differences {
        println!("{difference}");
    }
    // A difference is an answer, not a failure: the command succeeded in
    // explaining the build. Exiting non-zero here would make `journal diff` in
    // a script indistinguishable from one that could not read its inputs.
    Ok(0)
}
