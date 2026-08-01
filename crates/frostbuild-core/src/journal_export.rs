//! A build's action-key material, written down so two builds can be compared.
//!
//! The question this answers is "why did CI rebuild what my machine had
//! cached". The journal already knows each action's key and the digests that
//! produced it, but the key is a hash: it says two builds disagree, never
//! about what. The graph holds the argv and environment, the toolchain
//! fingerprint is computed per run, and the profile and platform are chosen by
//! the invocation — so the answer is spread across four places and nothing
//! collects it.
//!
//! An export collects it. A diff then reports, per action, the *first* field
//! that differs rather than every field that does: a changed compiler makes
//! every input digest differ too, and a list of four thousand differences
//! buries the one line that explains them.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

/// Bumped when the meaning of a field changes, not when one is added.
/// Comparing exports across a change in meaning would produce confident
/// nonsense, so a mismatch refuses rather than guessing.
pub const EXPORT_FORMAT: &str = "frost-journal-export-v1";

/// Everything that fed one action's key, in a stable order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActionExport {
    pub key: String,
    pub argv: Vec<String>,
    pub env: BTreeMap<String, String>,
    pub pass_env: Vec<String>,
    /// path -> content digest, for every input that fed the key.
    pub inputs: BTreeMap<String, String>,
    pub outputs: BTreeMap<String, String>,
}

/// One build's key material.
///
/// `BTreeMap` throughout is the whole reason two identical builds produce
/// identical bytes: a `HashMap` would order by hash seed and the byte-equality
/// test below would fail at random.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JournalExport {
    pub format: String,
    pub action_key_schema: String,
    pub profile: String,
    pub platform: String,
    /// Digest of every toolchain binary frost would invoke.
    pub toolchain: String,
    pub actions: BTreeMap<String, ActionExport>,
}

impl JournalExport {
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        serde_json::to_string_pretty(self)
    }

    pub fn from_json(text: &str) -> anyhow::Result<Self> {
        let export: Self = serde_json::from_str(text)?;
        if export.format != EXPORT_FORMAT {
            anyhow::bail!(
                "export format {:?} cannot be compared with {EXPORT_FORMAT:?}; \
                 the fields mean different things, so frost refuses rather than \
                 producing a confident wrong answer",
                export.format
            );
        }
        Ok(export)
    }
}

/// What differed, and where. Ordered so the first entry is the one to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Difference {
    /// A whole-build property. Everything downstream differs because of it, so
    /// reporting per-action noise underneath would be misleading.
    Build {
        field: &'static str,
        left: String,
        right: String,
    },
    /// The first differing field of one action.
    Action {
        action: String,
        field: &'static str,
        detail: String,
    },
    /// Present in one build and not the other.
    OnlyIn { action: String, side: &'static str },
}

impl std::fmt::Display for Difference {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Difference::Build { field, left, right } => {
                write!(f, "{field}: {left} != {right}")
            }
            Difference::Action {
                action,
                field,
                detail,
            } => write!(f, "{action}\n  {field}: {detail}"),
            Difference::OnlyIn { action, side } => {
                write!(f, "{action}\n  only in {side}")
            }
        }
    }
}

/// Compare two exports, root cause first.
///
/// Build-wide differences short-circuit. If the toolchain moved, every action
/// in the build has a different key *because* of that, and listing them would
/// bury the sentence that explains the run.
pub fn diff(left: &JournalExport, right: &JournalExport) -> Vec<Difference> {
    for (field, a, b) in [
        (
            "action_key_schema",
            &left.action_key_schema,
            &right.action_key_schema,
        ),
        ("profile", &left.profile, &right.profile),
        ("platform", &left.platform, &right.platform),
        ("toolchain", &left.toolchain, &right.toolchain),
    ] {
        if a != b {
            return vec![Difference::Build {
                field,
                left: a.clone(),
                right: b.clone(),
            }];
        }
    }

    let mut differences = Vec::new();
    let names: BTreeSet<&String> = left.actions.keys().chain(right.actions.keys()).collect();
    for name in names {
        match (left.actions.get(name), right.actions.get(name)) {
            (Some(a), Some(b)) => {
                if let Some(difference) = first_field_difference(name, a, b) {
                    differences.push(difference);
                }
            }
            (Some(_), None) => differences.push(Difference::OnlyIn {
                action: name.clone(),
                side: "the first build",
            }),
            (None, Some(_)) => differences.push(Difference::OnlyIn {
                action: name.clone(),
                side: "the second build",
            }),
            (None, None) => unreachable!("name came from one of the two maps"),
        }
    }
    differences
}

/// The first field of one action that differs, in cause-before-effect order.
///
/// argv and env are chosen by the manifest and the invocation; inputs are
/// chosen by the tree. A different argv is a decision someone made, and it is
/// the more useful thing to be told first even when input digests also differ.
fn first_field_difference(name: &str, a: &ActionExport, b: &ActionExport) -> Option<Difference> {
    let action = name.to_string();
    if a.argv != b.argv {
        return Some(Difference::Action {
            action,
            field: "argv",
            detail: format!("{:?} != {:?}", a.argv, b.argv),
        });
    }
    if a.env != b.env {
        return Some(Difference::Action {
            action,
            field: "env",
            detail: describe_map_difference(&a.env, &b.env),
        });
    }
    if a.pass_env != b.pass_env {
        return Some(Difference::Action {
            action,
            field: "pass_env",
            detail: format!("{:?} != {:?}", a.pass_env, b.pass_env),
        });
    }
    if a.inputs != b.inputs {
        return Some(Difference::Action {
            action,
            field: "inputs",
            detail: describe_map_difference(&a.inputs, &b.inputs),
        });
    }
    if a.outputs != b.outputs {
        return Some(Difference::Action {
            action,
            field: "outputs",
            detail: describe_map_difference(&a.outputs, &b.outputs),
        });
    }
    // Equal in every field frost compares but not in key: the key covers
    // something this export does not, which is a frost bug rather than a
    // difference between the two builds. Say so instead of reporting nothing.
    if a.key != b.key {
        return Some(Difference::Action {
            action,
            field: "key",
            detail: format!(
                "{} != {} with every exported field equal; \
                 the key covers something this export does not",
                a.key, b.key
            ),
        });
    }
    None
}

/// Name the first entry that differs rather than dumping both maps.
fn describe_map_difference(a: &BTreeMap<String, String>, b: &BTreeMap<String, String>) -> String {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    let mut changed = Vec::new();
    for key in keys {
        match (a.get(key), b.get(key)) {
            (Some(x), Some(y)) if x != y => changed.push(format!("{key} {x} != {y}")),
            (Some(_), None) => changed.push(format!("{key} only in the first build")),
            (None, Some(_)) => changed.push(format!("{key} only in the second build")),
            _ => {}
        }
    }
    let total = changed.len();
    changed.truncate(3);
    if total > 3 {
        changed.push(format!("and {} more", total - 3));
    }
    changed.join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn export() -> JournalExport {
        JournalExport {
            format: EXPORT_FORMAT.to_string(),
            action_key_schema: "frost-action-key-v4".into(),
            profile: "debug".into(),
            platform: "host".into(),
            toolchain: "abc".into(),
            actions: BTreeMap::from([(
                "compile:app".to_string(),
                ActionExport {
                    key: "k1".into(),
                    argv: vec!["cc".into(), "-c".into()],
                    env: BTreeMap::from([("LANG".to_string(), "C".to_string())]),
                    pass_env: vec!["PATH".into()],
                    inputs: BTreeMap::from([("a.c".to_string(), "d1".to_string())]),
                    outputs: BTreeMap::from([("a.o".to_string(), "d2".to_string())]),
                },
            )]),
        }
    }

    #[test]
    fn an_unchanged_build_exports_the_same_bytes() {
        assert_eq!(export().to_json().unwrap(), export().to_json().unwrap());
        assert!(diff(&export(), &export()).is_empty());
    }

    #[test]
    fn a_build_wide_difference_is_reported_alone() {
        // A different toolchain changes every action's key. Listing them would
        // bury the one sentence that explains the whole run.
        let mut right = export();
        right.toolchain = "different".into();
        right.actions.get_mut("compile:app").unwrap().key = "k2".into();
        right
            .actions
            .get_mut("compile:app")
            .unwrap()
            .inputs
            .insert("a.c".into(), "changed".into());

        let differences = diff(&export(), &right);
        assert_eq!(differences.len(), 1, "{differences:?}");
        assert!(matches!(
            differences[0],
            Difference::Build {
                field: "toolchain",
                ..
            }
        ));
    }

    #[test]
    fn an_action_reports_its_first_differing_field_only() {
        // env and inputs both differ; env is the cause-side field and is what
        // gets reported.
        let mut right = export();
        let action = right.actions.get_mut("compile:app").unwrap();
        action.env.insert("LANG".into(), "en_US".into());
        action.inputs.insert("a.c".into(), "d9".into());

        let differences = diff(&export(), &right);
        assert_eq!(differences.len(), 1);
        match &differences[0] {
            Difference::Action { field, detail, .. } => {
                assert_eq!(*field, "env");
                assert!(detail.contains("LANG"), "{detail}");
            }
            other => panic!("expected an action difference, got {other:?}"),
        }
    }

    #[test]
    fn an_input_difference_names_the_file() {
        let mut right = export();
        right
            .actions
            .get_mut("compile:app")
            .unwrap()
            .inputs
            .insert("a.c".into(), "d9".into());
        let differences = diff(&export(), &right);
        match &differences[0] {
            Difference::Action { field, detail, .. } => {
                assert_eq!(*field, "inputs");
                assert!(detail.contains("a.c"), "{detail}");
            }
            other => panic!("expected an input difference, got {other:?}"),
        }
    }

    #[test]
    fn an_action_present_in_one_build_is_named() {
        let mut right = export();
        right.actions.clear();
        let differences = diff(&export(), &right);
        assert!(matches!(differences[0], Difference::OnlyIn { .. }));
    }

    #[test]
    fn a_foreign_format_is_refused_rather_than_guessed_at() {
        let mut text = export();
        text.format = "frost-journal-export-v0".into();
        let error = JournalExport::from_json(&text.to_json().unwrap()).unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("cannot be compared"), "{message}");
        assert!(message.contains("v0"), "{message}");
    }

    #[test]
    fn a_key_difference_with_equal_fields_is_reported_as_a_frost_bug() {
        // Not reachable by editing a workspace: it means the key covers
        // something the export does not, which is worth saying out loud
        // rather than reporting "no differences" on two builds that disagree.
        let mut right = export();
        right.actions.get_mut("compile:app").unwrap().key = "k2".into();
        let differences = diff(&export(), &right);
        match &differences[0] {
            Difference::Action { field, detail, .. } => {
                assert_eq!(*field, "key");
                assert!(detail.contains("does not"), "{detail}");
            }
            other => panic!("expected a key difference, got {other:?}"),
        }
    }
}
