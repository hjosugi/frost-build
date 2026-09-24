//! `--hermetic`: running an action where only what it may read exists.
//!
//! The bubblewrap sandbox hides undeclared workspace files with a mount
//! namespace, which only Linux has. This mode reaches the same verdict on any
//! host by the other route: it materializes the sandbox's visible set
//! ([`crate::sandbox::visible_set`]) into a private tree under
//! `.frost/hermetic/`, runs the action there, and moves what the action is
//! answerable for — declared outputs, its depfile, owned and clean directories
//! — back into the workspace before the ordinary output checks run. An action
//! that reads a file nobody declared finds nothing at that path and fails,
//! exactly as it would inside bubblewrap.
//!
//! What it does not do is isolate: an action that names an absolute path into
//! the workspace still reaches it, and nothing stops network or process
//! access. It checks the manifest, which is what the sandbox is for here, and
//! it is not a security boundary, which the sandbox is not either.
//!
//! The tree's path is derived from the action's journal identity rather than
//! from a counter, so a tool that embeds its working directory (debug info
//! does) embeds the same one on every run, and `--check-determinism` compares
//! like with like.

use std::collections::BTreeMap;
use std::path::Path;
use std::path::PathBuf;

use anyhow::Context;
use anyhow::Result;

use crate::materialize::{Strategy, Tree};
use crate::sandbox::visible_set;
use crate::{journal_id, Engine};

/// Where the per-action trees live, relative to the workspace root.
pub const HERMETIC_DIR: &str = ".frost/hermetic";

impl Engine<'_> {
    /// Fill a private tree for one action.
    pub(crate) fn prepare_hermetic_tree(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
        strategy: Strategy,
    ) -> Result<Tree> {
        let visible = visible_set(self.root, self.graph, action, inputs);
        let name = blake3::hash(journal_id(self.graph, action).as_bytes()).to_hex();
        let mut tree = Tree::create(
            self.root.join(HERMETIC_DIR).join(&name.as_str()[..16]),
            strategy,
        )?;
        // Frost's own state is never an input by directory: whatever an action
        // may read from `.frost` it names file by file (an object, a generated
        // header) or by an include directory inside it. `.git` is the other
        // large tree a directory input at the root would drag in, and no
        // action has a reason to read it undeclared.
        let skip = [
            self.root.join(".frost"),
            self.root.join(".git"),
            self.root.join(HERMETIC_DIR),
        ];
        let mut placed: Vec<&PathBuf> = Vec::new();
        for directory in &visible.readonly_dirs {
            // Ordered, so a directory's ancestors come first; one already
            // placed covers everything beneath it.
            if placed.iter().any(|done| directory.starts_with(done)) {
                continue;
            }
            let Ok(relative) = directory.strip_prefix(self.root) else {
                continue;
            };
            tree.place_dir(directory, relative, &skip)?;
            placed.push(directory);
        }
        for relative in &visible.files {
            if Path::new(relative).is_absolute() {
                continue;
            }
            let source = self.root.join(relative);
            if !source.is_file() {
                continue;
            }
            tree.place_file(&source, Path::new(relative))?;
        }
        for directory in &visible.writable {
            let Ok(relative) = directory.strip_prefix(self.root) else {
                continue;
            };
            let inside = tree.root.join(relative);
            std::fs::create_dir_all(&inside)
                .with_context(|| format!("failed to create {}", inside.display()))?;
        }
        if action.preserve_outputs {
            // A tool that keeps state between runs finds it where it left it.
            // These are written, so they are copied, never linked.
            for &output in &action.outputs {
                let relative = &self.graph.files[output].path;
                let source = self.root.join(relative);
                if source.is_file() {
                    tree.place_writable(&source, Path::new(relative))?;
                }
            }
            for directory in &action.output_dirs {
                let source = self.root.join(directory);
                if source.is_dir() {
                    tree.place_writable_dir(&source, Path::new(directory))?;
                }
            }
        }
        Ok(tree)
    }

    /// Move what the action produced out of its tree.
    ///
    /// Only what it is answerable for comes back: a file it wrote that no
    /// declaration mentions is dropped with the tree, where under bubblewrap
    /// it would have been left in the workspace for nobody to account for.
    pub(crate) fn publish_hermetic_outputs(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        tree: &Tree,
    ) -> Result<()> {
        for &output in &action.outputs {
            let relative = &self.graph.files[output].path;
            move_file(&tree.root.join(relative), &self.root.join(relative))?;
        }
        if let Some(depfile) = &action.depfile {
            let produced = tree.root.join(depfile);
            if produced.is_file() {
                // The report names what the compiler opened. Anything it
                // resolved to an absolute path inside the tree is the
                // workspace file of the same relative name.
                let text = std::fs::read_to_string(&produced)
                    .with_context(|| format!("failed to read {}", produced.display()))?;
                let text = rebase_paths(&text, &tree.root, self.root);
                let destination = self.root.join(depfile);
                std::fs::write(&destination, text)
                    .with_context(|| format!("failed to write {}", destination.display()))?;
            }
        }
        for directory in action.output_dirs.iter().chain(&action.clean_dirs) {
            let produced = tree.root.join(directory);
            if !produced.is_dir() {
                continue;
            }
            let destination = self.root.join(directory);
            if destination.exists() {
                std::fs::remove_dir_all(&destination)
                    .with_context(|| format!("failed to replace {}", destination.display()))?;
            }
            if let Some(parent) = destination.parent() {
                std::fs::create_dir_all(parent)?;
            }
            if std::fs::rename(&produced, &destination).is_err() {
                copy_tree(&produced, &destination)?;
            }
        }
        Ok(())
    }
}

/// Rename, which is atomic and free inside one filesystem, else copy.
fn move_file(from: &Path, to: &Path) -> Result<()> {
    if !from.is_file() {
        return Ok(());
    }
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)?;
    }
    if std::fs::rename(from, to).is_ok() {
        return Ok(());
    }
    std::fs::copy(from, to)
        .map(|_| ())
        .with_context(|| format!("failed to publish {} to {}", from.display(), to.display()))
}

fn copy_tree(from: &Path, to: &Path) -> Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

/// Replace the tree's root with the workspace's in a dependency report, in
/// both the host's spelling and with forward slashes, which is how a
/// GCC-style tool on Windows may write it.
fn rebase_paths(text: &str, tree: &Path, root: &Path) -> String {
    let mut text = text.to_string();
    let tree_native = tree.display().to_string();
    let root_native = root.display().to_string();
    text = text.replace(&tree_native, &root_native);
    let tree_forward = tree_native.replace('\\', "/");
    if tree_forward != tree_native {
        text = text.replace(&tree_forward, &root_native.replace('\\', "/"));
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_that_names_the_tree_is_rebased_onto_the_workspace() {
        let tree = Path::new("/ws/.frost/hermetic/0123456789abcdef");
        let root = Path::new("/ws");
        let text = "out.o: src/a.c /ws/.frost/hermetic/0123456789abcdef/include/a.h \\\n /usr/include/stdio.h\n";
        assert_eq!(
            rebase_paths(text, tree, root),
            "out.o: src/a.c /ws/include/a.h \\\n /usr/include/stdio.h\n"
        );
    }
}
