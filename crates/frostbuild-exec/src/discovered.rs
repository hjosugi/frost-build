//! What the action turned out to read, on top of what it declared.
//!
//! A compiler reports the headers it opened, and those files have to reach the
//! action key or the next build reuses a result whose real inputs changed.
//!
//! Declared inputs are re-filtered here rather than merged into: a header
//! dropped from the source has to stop being part of the key, and a report
//! that only ever added would pin it there forever.

use std::collections::{BTreeMap, BTreeSet};

use frostbuild_core::depfile;

use crate::Engine;

impl Engine<'_> {
    /// Fold the command's dependency report into `inputs`, returning the paths
    /// it discovered.
    ///
    /// `captured` loses the report when the tool wrote it to stdout rather
    /// than to a file -- MSVC's `/showIncludes` does -- so the build log does
    /// not carry the whole include tree on every rebuild.
    pub(crate) fn ingest_dependency_report(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        captured: &mut String,
        inputs: &mut BTreeMap<String, String>,
    ) -> Result<Vec<String>, String> {
        // Ingest the dependency report: replace previous discovered deps with
        // fresh ones and fold their digests into the recorded key. Most tools
        // write a file; MSVC writes its includes to stdout, so that report is
        // taken from the captured output and removed from the build log, which
        // would otherwise carry the whole include tree on every rebuild.
        let mut discovered = Vec::new();
        let report = if action.depfile_format.reads_captured_output() {
            let text = captured.clone();
            *captured = depfile::strip_showincludes(captured);
            Some((action.depfile_format.as_str().to_string(), Some(text)))
        } else {
            action.depfile.as_ref().map(|dep_rel| {
                (
                    dep_rel.clone(),
                    std::fs::read_to_string(self.root.join(dep_rel)).ok(),
                )
            })
        };
        if let Some((source, text)) = report {
            if let Some(text) = text {
                match depfile::parse_format(action.depfile_format, &text, self.root) {
                    Ok(deps) => discovered = deps,
                    Err(err) => {
                        return Err(format!("failed to parse depfile {source}: {err:#}"));
                    }
                }
            }
            let declared: BTreeSet<String> = action
                .inputs
                .iter()
                .map(|&f| self.graph.files[f].path.clone())
                .collect();
            discovered.retain(|d| !declared.contains(d));
            inputs.retain(|path, _| declared.contains(path));
            match self.digest_all(&discovered) {
                Ok(extra) => inputs.extend(extra),
                Err(err) => return Err(format!("failed to hash discovered deps: {err:#}")),
            }
        };
        Ok(discovered)
    }
}
