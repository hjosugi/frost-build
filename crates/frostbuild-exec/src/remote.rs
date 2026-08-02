//! The remote cache, as seen from an action.
//!
//! Every path here is best-effort by construction. A remote that is slow,
//! unreachable or wrong must cost time and nothing else, so a miss, a timeout
//! and a malformed response all fall through to running the action.

use std::collections::BTreeMap;

use frostbuild_core::journal::JournalEntry;

use crate::report::Outcome;
use crate::{journal_id, Engine};

impl<'a> Engine<'a> {
    /// Every declared output was recorded, and nothing was recorded that this
    /// action no longer claims. With owned directories the recorded set is
    /// discovered rather than declared, so membership is checked against the
    /// declared directories instead of by count.
    /// Record and report a shared-cache hit, or nothing when the action has to
    /// run. A journal write failure is not fatal here: the outputs are already
    /// correct, and the next build simply checks again.
    pub(crate) fn try_remote(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
    ) -> Option<Outcome> {
        if self.opts.dry_run || self.opts.no_cache {
            return None;
        }
        let entry = self.remote_hit(action, inputs)?;
        let _ =
            self.journal
                .lock()
                .unwrap()
                .record(self.root, journal_id(self.graph, action), entry);
        Some(Outcome::Cached)
    }

    /// The action key over *declared* inputs only.
    ///
    /// This is what a shared cache can be asked for on a cold workspace, where
    /// the inputs a previous run discovered are not known yet. The entry it
    /// addresses records those discovered inputs with their digests, so
    /// accepting it still requires every one of them to match what is on disk.
    fn trace_key(&self, action: &frostbuild_core::graph::ActionNode) -> Option<String> {
        let declared: Vec<String> = action
            .inputs
            .iter()
            .map(|&file| self.graph.files[file].path.clone())
            .collect();
        let digests = self.digest_all(&declared).ok()?;
        if digests
            .values()
            .any(|digest| digest == frostbuild_core::hashcache::MISSING)
        {
            return None;
        }
        Some(self.action_key(action, &digests))
    }

    /// Try to satisfy an action from the shared cache.
    ///
    /// Returns the journal entry to record on success. Every failure — a miss,
    /// a discovered input that no longer matches, a blob that does not verify,
    /// a transport error — returns `None`, and the caller executes the action.
    fn remote_hit(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
    ) -> Option<JournalEntry> {
        let remote = self.opts.remote.as_ref()?;
        let trace_key = self.trace_key(action)?;
        let recorded = remote.action(&trace_key)?;
        // The producing run read these; this workspace must have the same
        // bytes at those paths or the result does not apply here.
        let discovered: Vec<String> = recorded.discovered.keys().cloned().collect();
        let current = self.digest_all(&discovered).ok()?;
        if discovered
            .iter()
            .any(|path| current.get(path) != recorded.discovered.get(path))
        {
            return None;
        }
        if !self.recorded_output_set_is_this_action(action, &recorded.outputs) {
            return None;
        }
        // Stage every blob into the local CAS first, so a partially available
        // remote leaves the workspace untouched.
        let staging = self.root.join(".frost/remote");
        if std::fs::create_dir_all(&staging).is_err() {
            return None;
        }
        for digest in recorded.outputs.values() {
            if self.cas.has(digest) {
                continue;
            }
            let staged = staging.join(digest);
            if !remote.stage_blob(digest, &staged) {
                let _ = std::fs::remove_file(&staged);
                return None;
            }
            let published = self.cas.put(&staged, digest).is_ok();
            let _ = std::fs::remove_file(&staged);
            if !published {
                return None;
            }
        }
        let mut entry = JournalEntry {
            key: String::new(),
            inputs: inputs.clone(),
            discovered: discovered.clone(),
            outputs: recorded.outputs.clone(),
            duration_ms: recorded.duration_ms,
            reason: "remote cache hit".into(),
        };
        entry.inputs.retain(|path, _| {
            action
                .inputs
                .iter()
                .any(|&file| self.graph.files[file].path == *path)
        });
        entry.inputs.extend(current);
        entry.key = self.action_key(action, &entry.inputs);
        if !self.restore_outputs(action, &entry).unwrap_or(false) {
            return None;
        }
        Some(entry)
    }

    /// Publish what an action produced for other workspaces.
    pub(crate) fn remote_publish(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        entry: &JournalEntry,
    ) {
        let Some(remote) = self.opts.remote.as_ref() else {
            return;
        };
        if !remote.uploads() {
            return;
        }
        let Some(trace_key) = self.trace_key(action) else {
            return;
        };
        let mut discovered = BTreeMap::new();
        for path in &entry.discovered {
            match entry.inputs.get(path) {
                Some(digest) => {
                    discovered.insert(path.clone(), digest.clone());
                }
                // Without its digest the trace cannot be checked by a
                // consumer, so the entry is not published at all.
                None => return,
            }
        }
        for (path, digest) in &entry.outputs {
            remote.put_blob(digest, &self.root.join(path));
        }
        remote.put_action(
            &trace_key,
            &frostbuild_core::remote::RemoteAction {
                discovered,
                outputs: entry.outputs.clone(),
                duration_ms: entry.duration_ms,
            },
        );
    }
}
