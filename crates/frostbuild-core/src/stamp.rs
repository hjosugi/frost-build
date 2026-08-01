//! Values from outside the build — a git SHA, a version, a build time — split
//! by how often they change.
//!
//! The naive way to embed a git SHA is to put it in a compile flag. Then every
//! commit changes every action key, and an incremental build tool stops being
//! one. The split every build system that solved this converged on is by *rate
//! of change*, not by kind:
//!
//! * **stable** values change rarely and *are* action-key material. A new
//!   commit should rebuild the binary that embeds its SHA. That is not cache
//!   thrash, it is the correct answer.
//! * **volatile** values change on every invocation — a wall clock, a build
//!   counter — and are *not* action-key material. Feeding them to the key
//!   would rebuild the world every second, which is why an action that reads
//!   one is instead re-executed unconditionally: cheap, and confined to the
//!   one target that asked.
//!
//! Which half a key falls in is decided by its **name**, not by its value:
//! `stable_prefix` (default `STABLE_`) splits them. That matters more than it
//! looks. It means the graph can be built, and a manifest validated, without
//! running the stamp command — so classification is a property of the manifest,
//! knowable at load, rather than something that arrives with the output of a
//! subprocess.
//!
//! Frost does not run the stamp command per action. It runs once per build and
//! hands the values to the engine; see `docs/16_action_key_audit.md` for why
//! each half is or is not key material.

use std::collections::BTreeMap;

use anyhow::{bail, Result};

/// Keys beginning with this are stable unless the manifest says otherwise.
pub const DEFAULT_STABLE_PREFIX: &str = "STABLE_";

/// The reference syntax, as it appears in a manifest.
const OPEN: &str = "${stamp.";

/// Read `KEY=VALUE` lines from the stamp command's stdout.
///
/// Strict on purpose. This output reaches action keys, and a line frost
/// silently ignored would be a value the author believes is being stamped in
/// and is not — which surfaces as a mystery months later, in a binary that
/// reports the wrong version.
pub fn parse(stdout: &str) -> Result<BTreeMap<String, String>> {
    let mut values = BTreeMap::new();
    for (index, line) in stdout.lines().enumerate() {
        let number = index + 1;
        let line = line.strip_suffix('\r').unwrap_or(line);
        if line.trim().is_empty() {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            bail!("stamp line {number} is not KEY=VALUE: {line:?}");
        };
        if !valid_key(key) {
            bail!(
                "stamp line {number} has an invalid key {key:?} \
                 (letters, digits and underscore, not starting with a digit)"
            );
        }
        if values.insert(key.to_string(), value.to_string()).is_some() {
            // Last-wins would make the build depend on the order of lines in a
            // script's output, which nobody reviews.
            bail!("stamp key {key:?} is set twice (line {number})");
        }
    }
    Ok(values)
}

fn valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(first) if first.is_ascii_alphabetic() || first == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Does this key belong to the half that participates in action keys?
pub fn is_stable(key: &str, stable_prefix: &str) -> bool {
    // An empty prefix would make every key stable, which is a legitimate thing
    // for a workspace to ask for: "I have no volatile values, and I would like
    // to be told if I ever add one."
    key.starts_with(stable_prefix)
}

/// Every `${stamp.KEY}` in `text`, in order of appearance, with duplicates.
///
/// A malformed reference is an error rather than literal text: `${stamp.FOO`
/// is a typo every time, and expanding it to itself would put a `$` into a
/// command line and let the build continue.
pub fn references(text: &str, context: &str) -> Result<Vec<String>> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(OPEN) {
        let after = &rest[start + OPEN.len()..];
        let Some(end) = after.find('}') else {
            bail!("unterminated ${{stamp.…}} reference in {context}: {text:?}");
        };
        let key = &after[..end];
        if !valid_key(key) {
            bail!(
                "invalid stamp key {key:?} in {context} \
                 (letters, digits and underscore, not starting with a digit)"
            );
        }
        found.push(key.to_string());
        rest = &after[end + 1..];
    }
    Ok(found)
}

/// Replace every `${stamp.KEY}` with its value.
///
/// A key the stamp command did not produce is an error: expanding it to
/// nothing would ship a binary reporting an empty version, and the manifest
/// asked for a value.
pub fn expand(text: &str, values: &BTreeMap<String, String>, context: &str) -> Result<String> {
    let keys = references(text, context)?;
    if keys.is_empty() {
        return Ok(text.to_string());
    }
    let mut expanded = text.to_string();
    for key in keys {
        let Some(value) = values.get(&key) else {
            let known: Vec<&str> = values.keys().map(String::as_str).collect();
            if known.is_empty() {
                bail!(
                    "{context} references ${{stamp.{key}}} but no stamp values are available \
                     (no [stamp] section, or --no-stamp)"
                );
            }
            let hint = crate::manifest::closest(&key, known.iter().copied())
                .map(|name| format!(". did you mean {name:?}?"))
                .unwrap_or_default();
            bail!(
                "{context} references ${{stamp.{key}}}, which the stamp command did not \
                 print{hint} (it printed: {})",
                known.join(", ")
            );
        };
        expanded = expanded.replace(&format!("{OPEN}{key}}}"), value);
    }
    Ok(expanded)
}

/// Every `${stamp.KEY}` removed, for a build that asked not to stamp.
///
/// Not the same as expanding against an empty map: there, a reference is a
/// mistake worth reporting. Here the caller has said it does not want the
/// values, and refusing to build would make the flag useless to exactly the
/// workspaces that have a `[stamp]` section.
pub fn blank(text: &str, context: &str) -> Result<String> {
    let mut blanked = text.to_string();
    for key in references(text, context)? {
        blanked = blanked.replace(&format!("{OPEN}{key}}}"), "");
    }
    Ok(blanked)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_key_value_line_per_value() {
        let values = parse("STABLE_GIT_SHA=abc123\nBUILD_TIME=1699999999\n").unwrap();
        assert_eq!(values["STABLE_GIT_SHA"], "abc123");
        assert_eq!(values["BUILD_TIME"], "1699999999");
    }

    #[test]
    fn a_value_may_contain_the_separator_and_be_empty() {
        // `git describe` output and `KEY=` both occur in real status scripts.
        let values = parse("STABLE_URL=https://x/y?a=b\nEMPTY=\n").unwrap();
        assert_eq!(values["STABLE_URL"], "https://x/y?a=b");
        assert_eq!(values["EMPTY"], "");
    }

    #[test]
    fn blank_lines_and_windows_endings_are_not_content() {
        let values = parse("\r\nSTABLE_V=1\r\n\r\n  \nB=2\r\n").unwrap();
        assert_eq!(values["STABLE_V"], "1");
        assert_eq!(values["B"], "2");
        assert_eq!(values.len(), 2);
    }

    #[test]
    fn a_line_frost_cannot_read_is_an_error_rather_than_a_skip() {
        // Silently skipping is how a value the author believes is stamped in
        // turns out not to be, months later, in a shipped binary.
        let error = parse("STABLE_V=1\ngit rev-parse HEAD\n")
            .unwrap_err()
            .to_string();
        assert!(error.contains("line 2"), "{error}");
        assert!(error.contains("KEY=VALUE"), "{error}");

        let error = parse("2BAD=1\n").unwrap_err().to_string();
        assert!(error.contains("invalid key"), "{error}");

        let error = parse("A=1\nA=2\n").unwrap_err().to_string();
        assert!(error.contains("set twice"), "{error}");
    }

    #[test]
    fn the_prefix_decides_the_half_and_the_name_is_all_it_needs() {
        // The whole design rests on this: classification without running the
        // command, so a manifest can be validated at load.
        assert!(is_stable("STABLE_GIT_SHA", DEFAULT_STABLE_PREFIX));
        assert!(!is_stable("BUILD_TIME", DEFAULT_STABLE_PREFIX));
        assert!(!is_stable("stable_lowercase", DEFAULT_STABLE_PREFIX));
        // An empty prefix is a workspace declaring it has no volatile values.
        assert!(is_stable("ANYTHING", ""));
    }

    #[test]
    fn references_are_found_in_order_and_typos_are_refused() {
        assert_eq!(
            references("-DV=${stamp.STABLE_V} -DT=${stamp.T}", "arg").unwrap(),
            ["STABLE_V", "T"]
        );
        assert!(references("plain", "arg").unwrap().is_empty());

        let error = references("${stamp.V", "command arg")
            .unwrap_err()
            .to_string();
        assert!(error.contains("unterminated"), "{error}");
        let error = references("${stamp.1V}", "command arg")
            .unwrap_err()
            .to_string();
        assert!(error.contains("invalid stamp key"), "{error}");
    }

    #[test]
    fn expansion_substitutes_every_occurrence() {
        let values = parse("STABLE_V=9\n").unwrap();
        assert_eq!(
            expand("${stamp.STABLE_V}.${stamp.STABLE_V}", &values, "arg").unwrap(),
            "9.9"
        );
    }

    #[test]
    fn a_key_the_command_did_not_print_names_the_ones_it_did() {
        let values = parse("STABLE_GIT_SHA=abc\n").unwrap();
        let error = expand("${stamp.STABLE_GIT_SH}", &values, "command arg")
            .unwrap_err()
            .to_string();
        assert!(error.contains("did you mean"), "{error}");
        assert!(error.contains("STABLE_GIT_SHA"), "{error}");

        let error = expand("${stamp.X}", &BTreeMap::new(), "command arg")
            .unwrap_err()
            .to_string();
        assert!(error.contains("--no-stamp"), "{error}");
    }
}
