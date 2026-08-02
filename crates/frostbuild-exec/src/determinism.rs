//! `--check-determinism`: run the action again and prove it produced the same
//! bytes.
//!
//! A build system's whole claim is that a cached result is the result you
//! would have got. This is the only place that claim is tested rather than
//! assumed, which is why the rerun rescans owned directories instead of
//! re-digesting the first set -- a rerun that names its tree files differently
//! *is* non-determinism, and comparing the first set to itself would miss it.

use std::collections::BTreeMap;

use frostbuild_core::graph::ActionKind;

use crate::report::Outcome;
use crate::{shell_join, Engine};

impl Engine<'_> {
    /// The failure, or `None` when the action reproduced itself exactly.
    pub(crate) fn verify_determinism(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
        first: &BTreeMap<String, String>,
        output_paths: &[String],
        reason: &str,
    ) -> Option<Outcome> {
        if let Some(path) = inputs.keys().find(|path| {
            std::fs::read_to_string(self.root.join(path))
                .is_ok_and(|text| text.contains("__TIME__") || text.contains("__DATE__"))
        }) {
            let detail = format!(
                "non-deterministic action {}: {} uses __DATE__/__TIME__; outputs: {}",
                action.id,
                path,
                output_paths.join(", ")
            );
            return Some(Outcome::Failed {
                reason: "determinism check failed".into(),
                detail,
            });
        }
        if let Err(err) = self.reset_clean_dirs(action) {
            return Some(Outcome::Failed {
                reason: reason.to_string(),
                detail: format!("determinism rerun setup failed: {err:#}"),
            });
        }
        if action.kind == ActionKind::Test {
            self.remove_partial_outputs(action);
        }
        let second = match self.run_action_commands(action, inputs) {
            Ok(batch) => batch,
            Err(err) => {
                return Some(Outcome::Failed {
                    reason: reason.to_string(),
                    detail: format!("determinism rerun failed: {err}"),
                })
            }
        };
        if let Some((argv, exit)) = second.failure {
            return Some(Outcome::Failed {
                reason: reason.to_string(),
                detail: format!(
                    "determinism rerun failed: {} ({exit})\n{}",
                    shell_join(&argv),
                    second.captured.trim_end()
                ),
            });
        }
        if action.kind == ActionKind::Test {
            if let Err(err) = self.write_test_success_outputs(action) {
                return Some(Outcome::Failed {
                    reason: reason.to_string(),
                    detail: format!("determinism rerun success record failed: {err:#}"),
                });
            }
        }
        for path in output_paths {
            self.cache.invalidate(path);
        }
        // A rerun may name its tree files differently, which is itself
        // non-determinism: rescan rather than re-digesting the first set,
        // so an added or renamed file is compared instead of missed.
        let mut output_paths = output_paths.to_vec();
        if !action.output_dirs.is_empty() {
            output_paths.retain(|path| {
                action
                    .output_dirs
                    .iter()
                    .all(|directory| !path.starts_with(&format!("{directory}/")))
            });
            match self.record_output_dirs(action) {
                Ok(tree) => output_paths.extend(tree),
                Err(err) => {
                    return Some(Outcome::Failed {
                        reason: reason.to_string(),
                        detail: format!("determinism rerun output scan failed: {err:#}"),
                    })
                }
            }
        }
        let second_outputs = match self.digest_all(&output_paths) {
            Ok(value) => value,
            Err(err) => {
                return Some(Outcome::Failed {
                    reason: reason.to_string(),
                    detail: format!("determinism output hash failed: {err:#}"),
                })
            }
        };
        if *first != second_outputs {
            let changed = first
                .iter()
                .filter_map(|(path, hash)| {
                    (second_outputs.get(path) != Some(hash)).then_some(path.clone())
                })
                .collect::<Vec<_>>();
            let detail = format!(
                "non-deterministic action {} produced different output: {}",
                action.id,
                changed.join(", ")
            );
            return Some(Outcome::Failed {
                reason: "determinism check failed".into(),
                detail,
            });
        }
        None
    }
}
