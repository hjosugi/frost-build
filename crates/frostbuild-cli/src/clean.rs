//! `frost clean`: remove build outputs for a configuration.

use std::path::Path;

use anyhow::{Context, Result};
use frostbuild_core::graph::{BIN_DIR, LIB_DIR, OBJ_DIR};
use frostbuild_core::manifest::{Manifest, TargetKind};

use crate::graph::load_graph;

/// Remove build outputs, narrowed to the requested configuration.
///
/// The names are validated against the graph before anything is deleted, so a
/// typo in `--platform` cannot become a wider removal than was asked for.
pub(crate) fn run_clean(
    root: &Path,
    cache: bool,
    profile: Option<String>,
    platform: Option<String>,
) -> Result<i32> {
    let active_profile = profile.as_deref().unwrap_or("debug");
    let active_platform = platform
        .as_deref()
        .unwrap_or(frostbuild_core::manifest::HOST_PLATFORM);
    // Validate explicitly selected names before touching anything.
    let graph = load_graph(root, active_profile, active_platform)?;

    // Narrow the removal to the requested platform/profile subtree;
    // with neither given, the whole output trees go.
    let subtree = match (&platform, &profile) {
        (None, None) => None,
        (None, Some(profile)) => Some(profile.clone()),
        (Some(platform), None) => Some(platform.clone()),
        (Some(platform), Some(profile)) => Some(format!("{platform}/{profile}")),
    };
    let mut removed = 0usize;
    for dir in [OBJ_DIR, LIB_DIR, BIN_DIR] {
        let path = subtree
            .as_ref()
            .map_or_else(|| root.join(dir), |sub| root.join(dir).join(sub));
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
            removed += 1;
        }
    }
    // Genrule/command outputs may live outside the native .frost
    // trees. With no selector, or a platform-only selector, expand
    // every configuration whose outputs the native tree removal above
    // covers as well.
    let mut configured_graphs = vec![graph];
    if profile.is_none() {
        let manifest = Manifest::load(root)?;
        let mut profiles = vec![frostbuild_core::manifest::DEFAULT_PROFILE.to_string()];
        profiles.extend(manifest.profiles.keys().cloned());
        profiles.sort();
        profiles.dedup();
        let mut platforms = if let Some(platform) = &platform {
            vec![platform.clone()]
        } else {
            let mut values = vec![frostbuild_core::manifest::HOST_PLATFORM.to_string()];
            values.extend(manifest.platforms.keys().cloned());
            values
        };
        platforms.sort();
        platforms.dedup();
        for configured_platform in platforms {
            for configured_profile in &profiles {
                if configured_platform == active_platform && configured_profile == active_profile {
                    continue;
                }
                configured_graphs.push(load_graph(root, configured_profile, &configured_platform)?);
            }
        }
    }
    let mut external_outputs = std::collections::BTreeSet::new();
    let mut intermediate_dirs = std::collections::BTreeSet::new();
    for graph in &configured_graphs {
        for target in graph.targets.values() {
            if matches!(target.kind, TargetKind::Genrule | TargetKind::Command) {
                external_outputs.extend(
                    target
                        .outputs
                        .iter()
                        .map(|&out| graph.files[out].path.clone()),
                );
                for &action in &target.actions {
                    intermediate_dirs.extend(graph.actions[action].clean_dirs.iter().cloned());
                    // Frost owns these outright, so cleaning has to
                    // take the whole directory rather than the files it
                    // happened to record last time.
                    intermediate_dirs.extend(graph.actions[action].output_dirs.iter().cloned());
                }
            }
        }
    }
    for output in external_outputs {
        let path = root.join(output);
        if path.exists() {
            std::fs::remove_file(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
            removed += 1;
        }
    }
    for directory in intermediate_dirs {
        let path = root.join(directory);
        if path.exists() {
            std::fs::remove_dir_all(&path)
                .with_context(|| format!("failed to remove {}", path.display()))?;
            removed += 1;
        }
    }
    if cache {
        for rel in [
            frostbuild_core::journal::JOURNAL_REL_PATH,
            ".frost/journal.json",
            ".frost/hashcache.bin",
            ".frost/hashcache.json",
        ] {
            let path = root.join(rel);
            if path.exists() {
                std::fs::remove_file(&path)?;
                removed += 1;
            }
        }
        if let Ok(entries) = std::fs::read_dir(root.join(".frost")) {
            for entry in entries.flatten() {
                let name = entry.file_name();
                let name = name.to_string_lossy();
                if name.starts_with("noop-") && name.ends_with(".bin") {
                    std::fs::remove_file(entry.path())?;
                    removed += 1;
                }
            }
        }
    }
    println!("frost: cleaned ({removed} entries removed)");
    Ok(0)
}
