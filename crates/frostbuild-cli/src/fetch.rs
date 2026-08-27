//! Explicit, pinned archive acquisition.
//!
//! No build path calls this module. That boundary is the offline guarantee:
//! graph construction and action execution only read the materialized tree.

use std::path::Path;

use anyhow::{bail, Context, Result};
use frostbuild_core::cas::LocalCas;
use frostbuild_core::fetch::{
    reject_symlink_components, snapshot_tree, FetchState, STATE_FILE, STATE_SCHEMA,
};
use frostbuild_core::hashcache::hash_file;
use frostbuild_core::manifest::{closest, FetchSpec, Manifest};

use crate::acquire::{download, extract, sha256_file, Scratch};

pub(crate) fn run_fetch(
    workspace_root: &Path,
    requested: Vec<String>,
    force: bool,
    offline: bool,
) -> Result<i32> {
    let manifest = Manifest::load_for_fetch(workspace_root)?;
    if manifest.fetches.is_empty() {
        bail!("manifest declares no [fetch.*] entries");
    }
    let names = select_names(&manifest, requested)?;
    for name in names {
        fetch_one(
            workspace_root,
            &name,
            &manifest.fetches[&name],
            force,
            offline,
        )?;
    }
    Ok(0)
}

fn select_names(manifest: &Manifest, requested: Vec<String>) -> Result<Vec<String>> {
    if requested.is_empty() {
        return Ok(manifest.fetches.keys().cloned().collect());
    }
    let mut names = Vec::new();
    for name in requested {
        if !manifest.fetches.contains_key(&name) {
            let known: Vec<&str> = manifest.fetches.keys().map(String::as_str).collect();
            if let Some(hint) = closest(&name, known.iter().copied()) {
                bail!("unknown fetch {name:?}. did you mean {hint:?}?");
            }
            bail!(
                "unknown fetch {name:?}. declared fetches: {}",
                known.join(", ")
            );
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

fn fetch_one(
    workspace_root: &Path,
    name: &str,
    spec: &FetchSpec,
    force: bool,
    offline: bool,
) -> Result<()> {
    reject_symlink_components(workspace_root, &spec.vendor_dir)?;
    let vendor_root = workspace_root.join(&spec.vendor_dir);
    let state = FetchState::read(&vendor_root).ok();
    let declaration_matches = state
        .as_ref()
        .is_some_and(|state| state_matches(state, name, spec));
    let materialization_matches = declaration_matches
        && state.as_ref().is_some_and(|state| {
            snapshot_tree(&vendor_root)
                .map(|snapshot| snapshot.digest == state.tree_digest)
                .unwrap_or(false)
        });
    if materialization_matches && !force {
        println!("frost: fetch {name} is up to date at {}", spec.vendor_dir);
        return Ok(());
    }
    if offline {
        bail!(
            "fetch {name:?} is missing or stale at {:?}, and --offline forbids downloading it",
            spec.vendor_dir
        );
    }
    if vendor_root.exists() {
        let Some(state) = state.as_ref() else {
            bail!(
                "refusing to replace {:?}: it has no {} proving Frost owns it",
                spec.vendor_dir,
                STATE_FILE
            );
        };
        if state.name != name || state.vendor_dir != spec.vendor_dir {
            bail!(
                "refusing to replace {:?}: its fetch state belongs to {:?}",
                spec.vendor_dir,
                state.name
            );
        }
    }

    let parent = vendor_root.parent().unwrap_or(workspace_root);
    std::fs::create_dir_all(parent)?;
    let scratch = Scratch::create(parent, &format!("frost-fetch-{name}"))?;
    let archive_path = scratch.path().join("download");
    download(&spec.url, &archive_path).with_context(|| format!("failed to fetch {name:?}"))?;
    let actual_sha256 = sha256_file(&archive_path)?;
    if actual_sha256 != spec.sha256 {
        bail!(
            "fetch {name:?} SHA-256 mismatch: expected {}, got {} (vendor directory was not changed)",
            spec.sha256,
            actual_sha256
        );
    }

    let cas_digest = hash_file(&archive_path)?;
    LocalCas::new(workspace_root, frostbuild_exec::DEFAULT_CAS_MAX_BYTES)
        .put(&archive_path, &cas_digest)
        .with_context(|| format!("failed to store fetch {name:?} in the local CAS"))?;

    let unpacked = scratch.path().join("unpacked");
    std::fs::create_dir(&unpacked)?;
    extract(&archive_path, &unpacked)
        .with_context(|| format!("failed to extract fetch {name:?}"))?;
    let source = match &spec.strip_prefix {
        Some(prefix) => unpacked.join(prefix),
        None => unpacked,
    };
    if !source.is_dir() {
        bail!(
            "fetch {name:?} strip_prefix {:?} is not a directory in the archive",
            spec.strip_prefix
        );
    }
    if source.join(STATE_FILE).exists() {
        bail!("fetch {name:?} archive contains reserved file {STATE_FILE:?}");
    }
    let snapshot = snapshot_tree(&source)?;
    FetchState {
        schema: STATE_SCHEMA.to_string(),
        name: name.to_string(),
        url: spec.url.clone(),
        sha256: spec.sha256.clone(),
        strip_prefix: spec.strip_prefix.clone(),
        vendor_dir: spec.vendor_dir.clone(),
        tree_digest: snapshot.digest,
        cas_digest,
    }
    .write(&source)?;

    publish_tree(&source, &vendor_root, scratch.path())?;
    println!("frost: fetched {name} -> {}", spec.vendor_dir);
    Ok(())
}

fn state_matches(state: &FetchState, name: &str, spec: &FetchSpec) -> bool {
    state.schema == STATE_SCHEMA
        && state.name == name
        && state.url == spec.url
        && state.sha256 == spec.sha256
        && state.strip_prefix == spec.strip_prefix
        && state.vendor_dir == spec.vendor_dir
}

fn publish_tree(source: &Path, destination: &Path, scratch: &Path) -> Result<()> {
    let backup = scratch.join("previous");
    let had_previous = destination.exists();
    if had_previous {
        std::fs::rename(destination, &backup).with_context(|| {
            format!(
                "failed to stage existing vendor tree {}",
                destination.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(source, destination) {
        if had_previous {
            if let Err(restore) = std::fs::rename(&backup, destination) {
                bail!(
                    "failed to publish vendor tree {}: {error}; restoring the previous tree also failed: {restore}",
                    destination.display()
                );
            }
        }
        return Err(error)
            .with_context(|| format!("failed to publish vendor tree {}", destination.display()));
    }
    if had_previous {
        // Publication is already complete. Scratch's Drop retries cleanup; a
        // locked old file must not turn a valid new tree into a fetch failure.
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}
