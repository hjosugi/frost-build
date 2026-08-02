//! Fingerprinting the toolchain, so an upgraded compiler invalidates.
//!
//! The fingerprint covers the resolved path of every configured tool and its
//! stat identity, not just its name: a `cc` that now points somewhere else is
//! a different toolchain even though the manifest did not change.

use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;
use serde::Deserialize;
use serde::Serialize;

/// Hash identifying the compiler binary so a toolchain swap invalidates the
/// cache (a lightweight stand-in for the closure hashing planned in #28).
pub fn toolchain_fingerprint(cc: &str) -> Result<String> {
    let resolved: PathBuf = if cc.contains('/') {
        PathBuf::from(cc)
    } else {
        frostbuild_core::paths::find_on_path(cc, |candidate| candidate.is_file())
            .with_context(|| format!("compiler {cc:?} not found in PATH"))?
    };
    frostbuild_core::hashcache::hash_file(&resolved)
        .with_context(|| format!("compiler {} not accessible", resolved.display()))
}

/// Stat identity of the toolchain binaries that produced a fingerprint.
#[derive(Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct ToolchainStamp {
    pub(crate) tools: Vec<(String, i128, u64, u64)>,
    pub(crate) fingerprint: String,
}

pub(crate) const TOOLCHAIN_STAMP_PATH: &str = ".frost/toolchain.bin";

/// Fingerprint of the compiler closure, cached by the stat identity of the
/// configured driver binaries.
///
/// A second function used to exist that also mixed in `cc --print-sysroot`,
/// but nothing called it, so the fingerprint frost actually used was the
/// weaker of the two. Rather than pay a process spawn per build to reconcile
/// them, note why the sysroot needs no separate treatment: an explicit
/// `--sysroot=` reaches the action key through argv, a default sysroot is a
/// property of the driver binary whose contents are hashed here, and the
/// headers actually read from it arrive as depfile-discovered inputs.
///
/// This used to load the workspace-wide content cache — megabytes covering
/// every source file — to digest a handful of executables. It now keeps its
/// own stamp: a few stats on the warm path, and the binaries are re-hashed
/// only when one of them actually changed.
pub fn toolchain_closure_fingerprint_cached(
    root: &Path,
    toolchain: &frostbuild_core::manifest::Toolchain,
) -> Result<String> {
    let shell = frostbuild_core::graph::SHELL.to_string();
    // The shell is in here because frost picks it, the same reason the C
    // drivers are: every genrule and shell test runs through it, and a
    // different /bin/sh can produce different bytes from the same command.
    // Paired with where the manifest declared each one, so a tool that cannot
    // be found says which line to go and look at.
    let mut all: Vec<(String, &String)> = vec![
        ("[toolchain].cc".into(), &toolchain.cc),
        ("[toolchain].cxx".into(), &toolchain.cxx),
        ("[toolchain].ar".into(), &toolchain.ar),
    ];
    if let Some(kofunc) = &toolchain.kofunc {
        all.push(("[toolchain].kofunc".into(), kofunc));
    }
    for (name, tool) in &toolchain.tools {
        all.push((format!("[toolchain.tools].{name}"), tool));
    }
    all.push(("frost's own shell".into(), &shell));
    let mut tools = Vec::with_capacity(all.len());
    let mut resolved_paths = Vec::with_capacity(all.len());
    for (declared_at, tool) in all {
        // A manifest may name a driver by a workspace-relative path (a
        // wrapper script for a cross toolchain, say), which only resolves
        // against the workspace root, not the process working directory.
        let resolved = resolve_executable(tool, &declared_at)?;
        let resolved = if resolved.is_absolute() {
            resolved
        } else {
            root.join(resolved)
        };
        let stat = std::fs::metadata(&resolved)
            .map(|m| stat_identity(&m))
            .unwrap_or((0, 0, 0));
        tools.push((
            resolved.to_string_lossy().into_owned(),
            stat.0,
            stat.1,
            stat.2,
        ));
        resolved_paths.push((tool.clone(), resolved));
    }

    let stamp_path = root.join(TOOLCHAIN_STAMP_PATH);
    if let Some(stamp) = std::fs::read(&stamp_path)
        .ok()
        .and_then(|b| postcard::from_bytes::<ToolchainStamp>(&b).ok())
    {
        if stamp.tools == tools {
            return Ok(stamp.fingerprint);
        }
    }

    let mut hasher = blake3::Hasher::new();
    for (tool, resolved) in &resolved_paths {
        hasher.update(tool.as_bytes());
        hasher.update(b"\0");
        hasher.update(
            frostbuild_core::hashcache::hash_file(resolved)
                .with_context(|| format!("compiler {} not accessible", resolved.display()))?
                .as_bytes(),
        );
        hasher.update(b"\0");
    }
    let fingerprint = hasher.finalize().to_hex().to_string();
    if let Some(parent) = stamp_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stamp = ToolchainStamp {
        tools,
        fingerprint: fingerprint.clone(),
    };
    let tmp = stamp_path.with_extension("bin.tmp");
    std::fs::write(&tmp, postcard::to_allocvec(&stamp)?)?;
    std::fs::rename(&tmp, &stamp_path)?;
    Ok(fingerprint)
}

#[cfg(unix)]
fn stat_identity(meta: &std::fs::Metadata) -> (i128, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (
        i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        meta.size(),
        meta.ino(),
    )
}

#[cfg(not(unix))]
fn stat_identity(meta: &std::fs::Metadata) -> (i128, u64, u64) {
    (0, meta.len(), 0)
}

fn resolve_executable(tool: &str, declared_at: &str) -> Result<PathBuf> {
    if tool.contains('/') {
        return Ok(PathBuf::from(tool));
    }
    frostbuild_core::paths::find_on_path(tool, |candidate| candidate.is_file()).with_context(|| {
        // Three questions, in the order someone hits them: what was I looking
        // for, where did I look, and what do I do now. Without the second one
        // a wrong PATH looks exactly like a missing package.
        let path = std::env::var("PATH").unwrap_or_default();
        let entries = path.split(PATH_SEPARATOR).filter(|p| !p.is_empty()).count();
        format!(
            "tool {tool:?} not found\n  \
             declared as {declared_at}\n  \
             searched {entries} PATH {}\n  \
             run `frost doctor` to see every tool this workspace needs and where frost looked",
            if entries == 1 { "entry" } else { "entries" },
        )
    })
}

#[cfg(unix)]
const PATH_SEPARATOR: char = ':';
#[cfg(not(unix))]
const PATH_SEPARATOR: char = ';';
