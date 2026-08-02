//! What an action is answerable for: its declared outputs and owned directories.
//!
//! Most of this module exists because a build that crashes halfway must not
//! leave a partial output that the next run mistakes for a finished one.
//! Outputs are digested, published, restored and discarded as a set.

use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::path::Path;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use frostbuild_core::journal::JournalEntry;

use crate::keys::path_is_inside;
use crate::Engine;

impl<'a> Engine<'a> {
    /// Does a recorded output set describe exactly what this action claims?
    pub(crate) fn recorded_output_set_is_this_action(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        outputs: &BTreeMap<String, String>,
    ) -> bool {
        let declared: BTreeSet<&str> = action
            .outputs
            .iter()
            .map(|&file| self.graph.files[file].path.as_str())
            .collect();
        declared.iter().all(|path| outputs.contains_key(*path))
            && outputs.keys().all(|path| {
                declared.contains(path.as_str())
                    || action
                        .output_dirs
                        .iter()
                        .any(|directory| path_is_inside(directory, path))
            })
    }

    pub(crate) fn recorded_outputs_match(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        previous: &JournalEntry,
    ) -> bool {
        if action.output_dirs.is_empty() {
            return action.outputs.len() == previous.outputs.len()
                && action
                    .outputs
                    .iter()
                    .all(|&file| previous.outputs.contains_key(&self.graph.files[file].path));
        }
        let declared: BTreeSet<&str> = action
            .outputs
            .iter()
            .map(|&file| self.graph.files[file].path.as_str())
            .collect();
        declared
            .iter()
            .all(|path| previous.outputs.contains_key(*path))
            && previous.outputs.keys().all(|path| {
                declared.contains(path.as_str())
                    || action
                        .output_dirs
                        .iter()
                        .any(|directory| path_is_inside(directory, path))
            })
    }

    pub(crate) fn digest_all(&self, paths: &[String]) -> Result<BTreeMap<String, String>> {
        self.cache.digest_many(self.root, paths)
    }

    /// Returns Ok(None) when all recorded outputs are on disk with matching
    /// digests, or Ok(Some(path)) naming the first stale output.
    pub(crate) fn outputs_intact(&self, prev: &JournalEntry) -> Result<Option<String>> {
        for (path, recorded) in &prev.outputs {
            let current = self.cache.digest(self.root, path)?;
            if &current != recorded {
                return Ok(Some(path.clone()));
            }
        }
        Ok(None)
    }

    pub(crate) fn prepare_output_dirs(&self) -> Result<()> {
        let mut directories = BTreeSet::new();
        for &action_id in &self.closure {
            let action = &self.graph.actions[action_id];
            for &out in &action.outputs {
                let path = self.root.join(&self.graph.files[out].path);
                if let Some(parent) = path.parent() {
                    directories.insert(parent.to_path_buf());
                }
            }
            if let Some(dep) = &action.depfile {
                let path = self.root.join(dep);
                if let Some(parent) = path.parent() {
                    directories.insert(parent.to_path_buf());
                }
            }
            // A tool told to write into a directory frost owns should find it
            // present, exactly as it finds the parent of a declared output.
            for owned in &action.output_dirs {
                directories.insert(self.root.join(owned));
            }
        }
        for parent in directories {
            std::fs::create_dir_all(&parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        Ok(())
    }

    pub(crate) fn restore_outputs(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        prev: &JournalEntry,
    ) -> Result<bool> {
        // An owned directory is restored as a whole: remove it first so a file
        // that is not in the recorded tree cannot survive the restore. Single
        // declared outputs are overwritten in place, as before. Restoration is
        // only attempted for a tree that was already found stale, and a failed
        // restore falls through to re-execution, so removing first cannot lose
        // a tree that was intact.
        self.discard_output_dirs(action);
        for (path, digest) in &prev.outputs {
            if !self.cas.materialize(digest, &self.root.join(path))? {
                return Ok(false);
            }
            self.cache.invalidate(path);
        }
        Ok(true)
    }

    pub(crate) fn remove_partial_outputs(&self, action: &frostbuild_core::graph::ActionNode) {
        for &output in &action.outputs {
            let _ = std::fs::remove_file(self.root.join(&self.graph.files[output].path));
        }
        self.discard_output_dirs(action);
    }

    /// Remove the directories this action owns. Frost publishes an owned
    /// directory wholesale, so a half-written tree from a failed or superseded
    /// run must not survive to be mistaken for output.
    pub(crate) fn discard_output_dirs(&self, action: &frostbuild_core::graph::ActionNode) {
        for directory in &action.output_dirs {
            for path in self.scan_output_dir(directory).unwrap_or_default() {
                self.cache.invalidate(&path);
            }
            let _ = std::fs::remove_dir_all(self.root.join(directory));
        }
    }

    /// Workspace-relative paths of every file under one owned directory, in a
    /// deterministic order. Symlinks are reported so the caller can reject
    /// them: a tree republished through the CAS would silently become regular
    /// files, which is a different tree.
    fn scan_output_dir(&self, directory: &str) -> Result<Vec<String>> {
        fn walk(root: &Path, relative: &Path, out: &mut Vec<String>) -> Result<()> {
            let mut entries = std::fs::read_dir(root.join(relative))
                .with_context(|| format!("failed to read {}", relative.display()))?
                .collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            for entry in entries {
                let child = relative.join(entry.file_name());
                let kind = entry.file_type()?;
                if kind.is_dir() && !kind.is_symlink() {
                    walk(root, &child, out)?;
                } else {
                    let path = child
                        .to_str()
                        .with_context(|| format!("non-UTF-8 output path {}", child.display()))?;
                    if kind.is_symlink() {
                        bail!("output_dir entry {path} is a symlink, which Frost cannot republish");
                    }
                    out.push(path.replace('\\', "/"));
                }
            }
            Ok(())
        }
        let mut out = Vec::new();
        walk(self.root, Path::new(directory), &mut out)
            .with_context(|| format!("failed to scan output_dir {directory}"))?;
        Ok(out)
    }

    /// Scan every owned directory and write the stamp that represents the tree
    /// in the graph. Returns the recorded file paths.
    pub(crate) fn record_output_dirs(
        &self,
        action: &frostbuild_core::graph::ActionNode,
    ) -> Result<Vec<String>> {
        if action.output_dirs.is_empty() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for directory in &action.output_dirs {
            if !self.root.join(directory).is_dir() {
                bail!("command succeeded but declared output_dir {directory} was not created");
            }
            files.extend(self.scan_output_dir(directory)?);
        }
        for path in &files {
            self.cache.invalidate(path);
        }
        let digests = self.digest_all(&files)?;
        let mut stamp = String::from("frost-tree-v1\n");
        for (path, digest) in &digests {
            stamp.push_str(digest);
            stamp.push(' ');
            stamp.push_str(path);
            stamp.push('\n');
        }
        for &output in &action.outputs {
            let relative = &self.graph.files[output].path;
            if !relative.starts_with(frostbuild_core::graph::TREE_STAMP_DIR) {
                continue;
            }
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&path, stamp.as_bytes())
                .with_context(|| format!("failed to write {}", path.display()))?;
            self.cache.invalidate(relative);
        }
        Ok(files)
    }

    /// Test outputs are Frost-owned success stamps, not files the test
    /// process is expected to manufacture. Keeping this outside the command
    /// removes a POSIX-shell dependency and guarantees a stamp exists only
    /// after every command in the test action has succeeded.
    pub(crate) fn write_test_success_outputs(
        &self,
        action: &frostbuild_core::graph::ActionNode,
    ) -> Result<()> {
        for &output in &action.outputs {
            let relative = &self.graph.files[output].path;
            let path = self.root.join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .with_context(|| format!("failed to create {}", parent.display()))?;
            }
            std::fs::write(&path, b"")
                .with_context(|| format!("failed to write {}", path.display()))?;
            self.cache.invalidate(relative);
        }
        Ok(())
    }

    pub(crate) fn reset_clean_dirs(
        &self,
        action: &frostbuild_core::graph::ActionNode,
    ) -> Result<()> {
        for directory in &action.clean_dirs {
            let path = self.root.join(directory);
            if path.exists() {
                std::fs::remove_dir_all(&path)
                    .with_context(|| format!("failed to remove {}", path.display()))?;
            }
            std::fs::create_dir_all(&path)
                .with_context(|| format!("failed to create {}", path.display()))?;
        }
        Ok(())
    }
}
