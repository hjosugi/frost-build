//! One canonical spelling of a manifest.
//!
//! The point is not that any particular order is better. It is that two people
//! writing the same target should produce the same bytes, so a review shows
//! what changed rather than who wrote it.
//!
//! `toml_edit` rather than parse-and-reserialize, because a manifest's comments
//! are the part worth keeping: `# needs HOME for the dependency cache` explains
//! a decision that the keys around it cannot. A formatter that drops them is
//! one nobody runs twice.

use anyhow::{Context, Result};
use toml_edit::{Array, DocumentMut, Item, Value};

/// Key order inside a `[target.*]` table.
///
/// Grouped by what a reader is asking, not alphabetically: what kind of thing
/// is this, what does it read, what does it produce, how does it run. A reader
/// scanning for outputs should find them in the same place in every target.
const TARGET_KEY_ORDER: [&str; 25] = [
    "kind",
    "tool",
    "cmd",
    "args",
    "srcs",
    "inputs",
    "deps",
    "visibility",
    "includes",
    "cflags",
    "ldflags",
    "outputs",
    "output_dirs",
    "clean_dirs",
    "depfile",
    "depfile_format",
    "preserve_outputs",
    "steps",
    "env",
    "pass_env",
    "shard_count",
    "flaky_retries",
    "timeout",
    "sandbox",
    "lint_allow",
];

/// An array longer than this, rendered inline, stops being readable in a
/// review diff: one changed entry rewrites the whole line.
const INLINE_ARRAY_WIDTH: usize = 76;

/// Format a manifest's text. Idempotent by construction, and asserted so.
pub fn format_manifest(text: &str) -> Result<String> {
    let mut document: DocumentMut = text.parse().context("failed to parse manifest")?;

    // Targets in name order. A manifest that grows by appending drifts into an
    // order that reflects when things were written, which is not something a
    // reader ever wants to know.
    if let Some(Item::Table(targets)) = document.get_mut("target") {
        // Sub-tables are emitted by `position`, not by map order, so sorting
        // the values does nothing here. Collect the positions the document
        // already uses and hand them out in name order: the block stays where
        // the author put it, and only the targets within it move.
        let mut names: Vec<String> = targets.iter().map(|(name, _)| name.to_string()).collect();
        names.sort();
        let mut positions: Vec<isize> = targets
            .iter()
            .filter_map(|(_, item)| match item {
                Item::Table(table) => table.position(),
                _ => None,
            })
            .collect();
        positions.sort_unstable();

        for (name, position) in names.iter().zip(positions) {
            if let Some(Item::Table(table)) = targets.get_mut(name) {
                table.set_position(Some(position));
            }
        }
        for (_, item) in targets.iter_mut() {
            if let Item::Table(target) = item {
                sort_by_canonical_order(target);
                for (_, value) in target.iter_mut() {
                    canonicalize_arrays(value);
                }
            }
        }
    }
    for (_, item) in document.iter_mut() {
        canonicalize_arrays(item);
    }

    // `toml_edit` keeps the line endings it was given inside untouched decor,
    // while every prefix this module sets is `\n`. On a CRLF checkout -- which
    // is what git hands a Windows runner by default -- that mixes the two in
    // one file. Normalising to whichever the input used keeps `fmt` from
    // rewriting every line of a file it was asked to tidy, and keeps `--check`
    // from failing on a platform rather than on a manifest.
    let rendered = document.to_string();
    Ok(match text.contains("\r\n") {
        true => rendered.replace("\r\n", "\n").replace('\n', "\r\n"),
        false => rendered.replace("\r\n", "\n"),
    })
}

/// True when `text` is already canonical.
pub fn is_formatted(text: &str) -> Result<bool> {
    Ok(format_manifest(text)? == text)
}

/// Order a target's keys, with anything unrecognized kept after the known ones
/// in its existing relative order.
///
/// An unknown key is a manifest from a newer frost, or a typo the parser will
/// reject in a moment. Either way, moving it to the end and leaving it there is
/// better than dropping it or guessing where it belongs.
fn sort_by_canonical_order(table: &mut toml_edit::Table) {
    table.sort_values_by(|a, _, b, _| {
        let rank = |key: &str| {
            TARGET_KEY_ORDER
                .iter()
                .position(|known| *known == key)
                .unwrap_or(TARGET_KEY_ORDER.len())
        };
        // Unknown keys tie on rank, and `sort_values_by` is stable, so they
        // keep the order the author gave them.
        rank(a.get()).cmp(&rank(b.get()))
    });
}

/// Inline short arrays, one entry per line for long ones.
///
/// Both spellings are canonical for their width, so the rule survives a round
/// trip: a long array is not re-inlined on the second run, and a short one is
/// not re-expanded.
fn canonicalize_arrays(item: &mut Item) {
    match item {
        Item::Value(Value::Array(array)) => rewrap(array),
        Item::Value(Value::InlineTable(table)) => {
            for (_, value) in table.iter_mut() {
                if let Value::Array(array) = value {
                    rewrap(array);
                }
            }
        }
        Item::Table(table) => {
            for (_, value) in table.iter_mut() {
                canonicalize_arrays(value);
            }
        }
        _ => {}
    }
}

fn rewrap(array: &mut Array) {
    // Measured on the entries alone: the key and indentation vary with nesting,
    // and a rule that depended on them would rewrap an array for being moved.
    let width: usize = array
        .iter()
        .map(|value| value.to_string().trim().len() + 2)
        .sum();

    if width <= INLINE_ARRAY_WIDTH {
        for value in array.iter_mut() {
            let decor = value.decor_mut();
            decor.set_prefix(" ");
            decor.set_suffix("");
        }
        array.set_trailing("");
        array.set_trailing_comma(false);
        // The first entry carries no leading space: `[a, b]`, not `[ a, b]`.
        if let Some(first) = array.iter_mut().next() {
            first.decor_mut().set_prefix("");
        }
        return;
    }

    for value in array.iter_mut() {
        let decor = value.decor_mut();
        decor.set_prefix("\n  ");
        decor.set_suffix("");
    }
    array.set_trailing("\n");
    // A trailing comma keeps the next addition to a one-line diff.
    array.set_trailing_comma(true);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formatting_is_idempotent() {
        // The property that makes `--check` meaningful: if a second run could
        // change something, `--check` would fail on its own output.
        let inputs = [
            "[target.b]\nkind = \"cc_binary\"\nsrcs = [\"b.c\"]\n",
            "[target.a]\nsrcs = [\"a.c\"]\nkind = \"cc_library\"\ndeps = [\"b\"]\n",
            "[workspace]\ndefault_targets = [\"a\"]\n\n[target.a]\nkind = \"cc_binary\"\nsrcs = [\"a.c\", \"b.c\", \"c.c\", \"d.c\", \"e.c\", \"f.c\", \"g.c\", \"h.c\", \"i.c\", \"j.c\", \"k.c\"]\n",
            "[target.t]\nkind = \"command\"\ntool = \"x\"\nenv = { A = \"1\", B = \"2\" }\n",
        ];
        for input in inputs {
            let once = format_manifest(input).unwrap();
            let twice = format_manifest(&once).unwrap();
            assert_eq!(once, twice, "not idempotent for {input:?}");
            assert!(is_formatted(&once).unwrap());
        }
    }

    #[test]
    fn a_files_line_endings_are_its_own_business() {
        // Found by Windows CI, not here: git checks out CRLF on Windows by
        // default, `toml_edit` preserves those endings in decor it did not
        // touch, and every prefix this module sets is "\n". The result was a
        // file mixing both, so `--check` failed on every manifest for being on
        // Windows rather than for being wrong.
        let unix = "[target.a]\nkind = \"cc_library\"\nsrcs = [\"a.c\"]\n";
        let windows = unix.replace('\n', "\r\n");

        let formatted_unix = format_manifest(unix).unwrap();
        let formatted_windows = format_manifest(&windows).unwrap();

        assert!(!formatted_unix.contains('\r'), "{formatted_unix:?}");
        assert!(
            !formatted_windows.replace("\r\n", "").contains('\n'),
            "every newline must be CRLF, not just the untouched ones: {formatted_windows:?}"
        );
        // Same content either way, so the choice really is only about endings.
        assert_eq!(formatted_windows.replace("\r\n", "\n"), formatted_unix);

        // And both are already canonical, which is the property CI checks.
        assert!(is_formatted(&formatted_unix).unwrap());
        assert!(is_formatted(&formatted_windows).unwrap());
    }

    #[test]
    fn formatting_never_changes_what_the_manifest_means() {
        // The property that actually matters. Idempotence only says the second
        // run is a no-op; this says the first one did not change the build.
        // Reordering keys and tables is exactly the kind of edit that could,
        // and a formatter that alters a build is worse than no formatter.
        let input = r#"
            [workspace]
            default_targets = ["app"]

            [target.zebra]
            srcs = ["z.c"]
            kind = "cc_library"
            includes = ["include"]

            [target.app]
            deps = ["zebra"]
            srcs = ["a.c", "b.c", "c.c", "d.c", "e.c", "f.c", "g.c", "h.c", "i.c"]
            kind = "cc_binary"
            cflags = ["-O2", "-Wall"]
            "#;
        let before = crate::manifest::Manifest::parse_str(input).unwrap();
        let after = crate::manifest::Manifest::parse_str(&format_manifest(input).unwrap()).unwrap();

        assert_eq!(before.default_targets, after.default_targets);
        assert_eq!(before.targets.len(), after.targets.len());
        for (name, target) in &before.targets {
            let other = after.targets.get(name).expect("target survived");
            assert_eq!(target.kind, other.kind, "{name} kind");
            assert_eq!(target.srcs, other.srcs, "{name} srcs");
            assert_eq!(target.deps, other.deps, "{name} deps");
            assert_eq!(target.includes, other.includes, "{name} includes");
            assert_eq!(target.cflags, other.cflags, "{name} cflags");
        }
    }

    #[test]
    fn comments_and_string_contents_survive() {
        // The reason this uses toml_edit at all. A comment explains a decision
        // the keys cannot, and a formatter that drops them is one nobody runs
        // a second time.
        let input = r#"# The gate, in five stages.
[target.app]
# Needs HOME for the dependency cache; see docs/16.
kind = "cc_binary"
srcs = ["main.c"] # the only source
cmd = "printf 'a  b' > ${out} && echo done"
"#;
        let formatted = format_manifest(input).unwrap();
        assert!(
            formatted.contains("# The gate, in five stages."),
            "{formatted}"
        );
        assert!(
            formatted.contains("# Needs HOME for the dependency cache; see docs/16."),
            "{formatted}"
        );
        assert!(formatted.contains("# the only source"), "{formatted}");
        // Whitespace inside a string is content, not formatting.
        assert!(formatted.contains("printf 'a  b'"), "{formatted}");
    }

    #[test]
    fn keys_reach_a_canonical_order_from_either_spelling() {
        let one = "[target.a]\nsrcs = [\"a.c\"]\nkind = \"cc_library\"\nsandbox = false\n";
        let other = "[target.a]\nsandbox = false\nkind = \"cc_library\"\nsrcs = [\"a.c\"]\n";
        // The whole point: two people writing the same target produce the same
        // bytes, so a review shows what changed rather than who wrote it.
        assert_eq!(
            format_manifest(one).unwrap(),
            format_manifest(other).unwrap()
        );
        let formatted = format_manifest(one).unwrap();
        let kind = formatted.find("kind").unwrap();
        let srcs = formatted.find("srcs").unwrap();
        let sandbox = formatted.find("sandbox").unwrap();
        assert!(kind < srcs && srcs < sandbox, "{formatted}");
    }

    #[test]
    fn targets_reach_name_order() {
        let input = "[target.zebra]\nkind = \"cc_library\"\nsrcs = [\"z.c\"]\n\n[target.alpha]\nkind = \"cc_library\"\nsrcs = [\"a.c\"]\n";
        let formatted = format_manifest(input).unwrap();
        assert!(
            formatted.find("[target.alpha]") < formatted.find("[target.zebra]"),
            "{formatted}"
        );
    }

    #[test]
    fn an_unknown_key_is_kept_rather_than_dropped_or_guessed_at() {
        // A manifest from a newer frost, or a typo the parser rejects in a
        // moment. Losing it would be the worst possible response to either.
        let input = "[target.a]\nfuture_key = \"x\"\nkind = \"cc_library\"\nsrcs = [\"a.c\"]\n";
        let formatted = format_manifest(input).unwrap();
        assert!(formatted.contains("future_key"), "{formatted}");
        // After the keys it knows, so the known ones still read in order.
        assert!(
            formatted.find("kind") < formatted.find("future_key"),
            "{formatted}"
        );
    }

    #[test]
    fn long_arrays_wrap_and_short_ones_do_not() {
        let short = "[target.a]\nkind = \"cc_library\"\nsrcs = [\"a.c\"]\n";
        assert!(
            format_manifest(short).unwrap().contains("srcs = [\"a.c\"]"),
            "a short array stays on one line"
        );

        let long = format!(
            "[target.a]\nkind = \"cc_library\"\nsrcs = [{}]\n",
            (0..12)
                .map(|n| format!("\"src/file_number_{n}.c\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let formatted = format_manifest(&long).unwrap();
        assert!(
            formatted.contains("\n  \"src/file_number_0.c\","),
            "a long array goes one per line:\n{formatted}"
        );
        // And stays that way, rather than being re-inlined next run.
        assert_eq!(formatted, format_manifest(&formatted).unwrap());
    }

    #[test]
    fn a_manifest_that_does_not_parse_is_refused_rather_than_rewritten() {
        let error = format_manifest("[target.a\nkind = \"x\"\n").unwrap_err();
        assert!(format!("{error:#}").contains("failed to parse"));
    }
}
