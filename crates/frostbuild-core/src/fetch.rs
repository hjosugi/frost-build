//! Pinned fetch state and deterministic snapshots of vendored trees.
//!
//! This module deliberately contains no network client. Builds and manifest
//! loading may inspect already-vendored bytes, but only the CLI's explicit
//! `frost fetch` command is allowed to obtain them.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};

use crate::hashcache::hash_file;

pub const STATE_FILE: &str = ".frost-fetch.json";
pub const STATE_SCHEMA: &str = "frost-fetch-state-v1";

/// Proof that a vendor directory was published by Frost from one declaration.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FetchState {
    pub schema: String,
    pub name: String,
    pub url: String,
    pub sha256: String,
    pub strip_prefix: Option<String>,
    pub vendor_dir: String,
    pub tree_digest: String,
    pub cas_digest: String,
}

impl FetchState {
    pub fn read(vendor_root: &Path) -> Result<Self> {
        let path = vendor_root.join(STATE_FILE);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("missing fetch state {}", path.display()))?;
        serde_json::from_slice(&bytes)
            .with_context(|| format!("invalid fetch state {}", path.display()))
    }

    pub fn write(&self, vendor_root: &Path) -> Result<()> {
        let path = vendor_root.join(STATE_FILE);
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        std::fs::write(&path, bytes)
            .with_context(|| format!("failed to write fetch state {}", path.display()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeSnapshot {
    /// BLAKE3 over entry kinds, normalized relative paths and file digests.
    pub digest: String,
    /// Regular files relative to the tree root, excluding [`STATE_FILE`].
    pub files: Vec<String>,
}

/// Inspect a materialized tree without following links.
///
/// Symlinks and special files are rejected so an archive cannot smuggle an
/// input outside the pinned tree. Empty directories participate in the tree
/// digest even though actions consume only regular files.
pub fn snapshot_tree(root: &Path) -> Result<TreeSnapshot> {
    let metadata = std::fs::symlink_metadata(root)
        .with_context(|| format!("missing fetch tree {}", root.display()))?;
    if metadata.file_type().is_symlink() {
        bail!("fetch tree {} is a symlink", root.display());
    }
    if !metadata.is_dir() {
        bail!("fetch tree {} is not a directory", root.display());
    }
    let mut entries = Vec::new();
    walk(root, root, &mut entries)?;
    entries.sort_by(|left, right| left.1.cmp(&right.1));

    let mut hasher = blake3::Hasher::new();
    let mut files = Vec::new();
    for (kind, rel, digest) in entries {
        hasher.update(&[kind]);
        hasher.update(&(rel.len() as u64).to_le_bytes());
        hasher.update(rel.as_bytes());
        if let Some(digest) = digest {
            hasher.update(&(digest.len() as u64).to_le_bytes());
            hasher.update(digest.as_bytes());
            files.push(rel);
        }
    }
    Ok(TreeSnapshot {
        digest: hasher.finalize().to_hex().to_string(),
        files,
    })
}

/// Refuse an existing symlink anywhere between a workspace and a vendor path.
///
/// Missing suffix components are fine: `frost fetch` is about to create them.
/// Existing non-directory parents are left to the caller's ordinary I/O error,
/// while links are diagnosed deliberately because following one could publish
/// fetched bytes outside the workspace.
pub fn reject_symlink_components(workspace_root: &Path, relative: &str) -> Result<()> {
    let mut current = workspace_root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            bail!("fetch vendor path {relative:?} is not normalized");
        };
        current.push(component);
        match std::fs::symlink_metadata(&current) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                bail!(
                    "fetch vendor path {relative:?} traverses symlink {}",
                    current.display()
                )
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => break,
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<(u8, String, Option<String>)>) -> Result<()> {
    let mut children: Vec<_> = std::fs::read_dir(dir)
        .with_context(|| format!("failed to read fetch tree {}", dir.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for entry in children {
        let path = entry.path();
        let rel = relative_utf8(root, &path)?;
        if rel == STATE_FILE {
            continue;
        }
        let ty = entry.file_type()?;
        if ty.is_symlink() {
            bail!("fetch tree contains symlink {rel:?}");
        }
        if ty.is_dir() {
            out.push((b'd', rel, None));
            walk(root, &path, out)?;
        } else if ty.is_file() {
            let digest =
                hash_file(&path).with_context(|| format!("failed to hash fetched file {rel:?}"))?;
            out.push((b'f', rel, Some(digest)));
        } else {
            bail!("fetch tree contains special file {rel:?}");
        }
    }
    Ok(())
}

fn relative_utf8(root: &Path, path: &Path) -> Result<String> {
    let rel: PathBuf = path
        .strip_prefix(root)
        .context("fetch tree walk escaped its root")?
        .to_path_buf();
    rel.to_str()
        .context("non-UTF-8 fetched paths are not supported")
        .map(|path| path.replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("frost-fetch-{name}-{}", std::process::id()))
    }

    #[test]
    fn snapshots_are_sorted_and_ignore_the_state_file() {
        let root = scratch("snapshot");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("z")).unwrap();
        std::fs::write(root.join("z/b"), b"b").unwrap();
        std::fs::write(root.join("a"), b"a").unwrap();
        std::fs::write(root.join(STATE_FILE), b"state").unwrap();
        let first = snapshot_tree(&root).unwrap();
        assert_eq!(first.files, ["a", "z/b"]);
        std::fs::write(root.join(STATE_FILE), b"changed state").unwrap();
        assert_eq!(snapshot_tree(&root).unwrap(), first);
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_never_followed() {
        use std::os::unix::fs::symlink;

        let root = scratch("symlink");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        symlink("outside", root.join("link")).unwrap();
        let error = snapshot_tree(&root).unwrap_err().to_string();
        assert!(error.contains("symlink \"link\""), "{error}");
        let error = reject_symlink_components(&root, "link/child")
            .unwrap_err()
            .to_string();
        assert!(error.contains("traverses symlink"), "{error}");
        let _ = std::fs::remove_dir_all(root);
    }
}
