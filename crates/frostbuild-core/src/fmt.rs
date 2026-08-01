//! One canonical rendering of a `frost.toml`.
//!
//! A declarative manifest can be read by a machine, which also means it can be
//! written by one. `docs/14` turned Starlark down; the cost of that choice is
//! that "how a manifest is laid out" becomes a matter of taste unless something
//! settles it, and diffs fill with reordered keys nobody meant to change.
//!
//! The rules are deliberately few. Order the tables the way the specification
//! introduces them, order a target's keys the way one is read — what it is,
//! what it consumes, what it produces, how it runs — and wrap an array when it
//! does not fit. Everything else, including every comment and every string
//! exactly as written, is left alone: a formatter that rewrote a `cmd` would be
//! changing the build.

use anyhow::{Context, Result};
use toml_edit::{DocumentMut, Item, Table, Value};

/// Columns a line may reach before an array is broken across lines.
///
/// Manifests are read next to code, in the same window, and this is the width
/// the rest of the tree is written to.
const WIDTH: usize = 88;

/// Top-level tables, in the order `docs/06_manifest_spec.md` introduces them:
/// what the workspace is, what builds it, where it builds to, and then the
/// work itself.
const TABLE_ORDER: &[&str] = &["workspace", "toolchain", "platform", "profile", "target"];

/// Keys of a `[target.*]` table, in the order a reader asks about them: what
/// kind of thing this is, what it consumes, what it produces, and how it runs.
const TARGET_KEY_ORDER: &[&str] = &[
    "kind",
    "srcs",
    "inputs",
    "cmd",
    "tool",
    "args",
    "steps",
    "deps",
    "includes",
    "cflags",
    "ldflags",
    "outputs",
    "output_dirs",
    "clean_dirs",
    "depfile",
    "depfile_format",
    "preserve_outputs",
    "env",
    "pass_env",
    "sandbox",
    "shard_count",
    "timeout",
];

/// Keys of `[workspace]` and `[toolchain]`, likewise.
const OTHER_KEY_ORDER: &[&str] = &[
    "name",
    "default_targets",
    "cc",
    "cxx",
    "ar",
    "kofunc",
    "sysroot",
    "arflags",
    "cflags",
    "cxxflags",
    "ldflags",
];

/// Rewrite `text` into the canonical form.
///
/// Pure, so the same rules serve `frost fmt`, `frost fmt --check` and an
/// editor asking for a formatting edit, and so idempotence is a property of a
/// function rather than of a command.
pub fn format(text: &str) -> Result<String> {
    let mut document: DocumentMut = text.parse().context("parsing the manifest to format")?;
    sort_tables(document.as_table_mut(), TABLE_ORDER);
    for (_, item) in document.as_table_mut().iter_mut() {
        format_item(item);
    }
    // Sorting the map is not enough: a table remembers where it was in the
    // original document and is rendered in that order, so the order has to be
    // restated rather than merely rearranged.
    let mut position = 0usize;
    reposition(document.as_table_mut(), &mut position);
    Ok(document.to_string())
}

/// Renumber every table so it renders in the order the map now holds.
fn reposition(table: &mut Table, next: &mut usize) {
    for (_, item) in table.iter_mut() {
        let Some(child) = item.as_table_mut() else {
            continue;
        };
        // An implicit table — the `target` in `[target.app]`, which has no
        // header of its own — is not rendered, so numbering it would leave a
        // gap that means nothing.
        if !child.is_implicit() {
            child.set_position(Some(*next as isize));
            *next += 1;
        }
        reposition(child, next);
    }
}

/// Whether `text` is already canonical.
pub fn is_formatted(text: &str) -> Result<bool> {
    Ok(format(text)? == text)
}

fn format_item(item: &mut Item) {
    let Some(table) = item.as_table_mut() else {
        return;
    };
    // A `[target.*]` table holds the targets; the targets themselves hold the
    // keys. Which order applies is decided by what the table contains, not by
    // its name, so `[platform.aarch64.tools]` needs no special case.
    if table.iter().all(|(_, item)| item.is_table()) {
        sort_tables(table, &[]);
    } else {
        sort_keys(table);
    }
    for (_, child) in table.iter_mut() {
        format_item(child);
    }
    for (key, value) in table.iter_mut() {
        if let Some(value) = value.as_value_mut() {
            // `key = ` is on the line too, so the decision about whether the
            // value fits has to count it.
            wrap_value(value, key.get().chars().count() + 3, 0);
        }
    }
}

/// Order a table's sub-tables: the ones `order` names first, in that order,
/// then everything else alphabetically.
fn sort_tables(table: &mut Table, order: &[&str]) {
    let rank = |key: &str| order.iter().position(|name| *name == key);
    let mut keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    keys.sort_by(|a, b| match (rank(a), rank(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        (None, None) => a.cmp(b),
    });
    reinsert(table, &keys);
}

fn sort_keys(table: &mut Table) {
    // A target's keys and a toolchain's keys are different lists, and a table
    // is one or the other; asking both and taking whichever recognizes the key
    // avoids having to know which table this is.
    let rank = |key: &str| {
        TARGET_KEY_ORDER
            .iter()
            .position(|name| *name == key)
            .or_else(|| {
                OTHER_KEY_ORDER
                    .iter()
                    .position(|name| *name == key)
                    .map(|at| at + TARGET_KEY_ORDER.len())
            })
    };
    let mut keys: Vec<String> = table.iter().map(|(key, _)| key.to_string()).collect();
    keys.sort_by(|a, b| match (rank(a), rank(b)) {
        (Some(a), Some(b)) => a.cmp(&b),
        (Some(_), None) => std::cmp::Ordering::Less,
        (None, Some(_)) => std::cmp::Ordering::Greater,
        // An unrecognized key is a manifest error, but formatting runs on
        // manifests that do not load yet — that is when it is wanted. Ordering
        // them alphabetically keeps the output stable rather than guessing.
        (None, None) => a.cmp(b),
    });
    reinsert(table, &keys);
}

/// Rebuild a table in `keys` order.
///
/// toml_edit has no reorder, and removing and re-inserting is what carries the
/// decoration — the blank lines and comments written above an entry — along
/// with it. Losing those would make a formatter something nobody runs twice.
fn reinsert(table: &mut Table, keys: &[String]) {
    // The key itself, not its name: a comment written above an entry is decor
    // on the key, and re-inserting by name would build a fresh key and drop it.
    let mut taken = Vec::with_capacity(keys.len());
    for key in keys {
        if let Some(entry) = table.remove_entry(key) {
            taken.push(entry);
        }
    }
    for (key, item) in taken {
        table.insert_formatted(&key, item);
    }
}

/// Break an array across lines when it does not fit, and put it back on one
/// line when it does.
///
/// `column` is where the value starts on its line, so the fit is measured
/// where it will actually be printed; `indent` is the block it belongs to, and
/// the two differ whenever a `key = ` precedes the value.
fn wrap_value(value: &mut Value, column: usize, indent: usize) {
    let Some(array) = value.as_array_mut() else {
        return;
    };
    for element in array.iter_mut() {
        wrap_value(element, indent + 2, indent + 2);
    }
    // Measure the one-line form before deciding, because the decision is
    // exactly "does the one-line form fit".
    let mut inline = array.clone();
    inline.fmt();
    let fits = inline.to_string().chars().count() + column <= WIDTH;
    if fits {
        *array = inline;
        return;
    }
    array.fmt();
    let padding = " ".repeat(indent + 2);
    for element in array.iter_mut() {
        let decor = element.decor_mut();
        decor.set_prefix(format!("\n{padding}"));
        decor.set_suffix("");
    }
    array.set_trailing(format!(",\n{}", " ".repeat(indent)));
    array.set_trailing_comma(false);
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    /// A manifest built from arbitrary but well-shaped parts.
    ///
    /// Generated rather than listed because the property is about every
    /// manifest, and the cases that break a formatter are the ones nobody
    /// thought to write down: a key that lands exactly on the wrap boundary, a
    /// target whose keys are all unrecognized, an empty array.
    fn any_manifest() -> impl Strategy<Value = String> {
        let key = prop::sample::select(vec![
            "kind",
            "srcs",
            "deps",
            "includes",
            "cflags",
            "inputs",
            "outputs",
            "not_a_key",
        ]);
        let array = prop::collection::vec("[a-z/:._]{1,20}", 0..6);
        let entry = (key, array).prop_map(|(key, values)| {
            let items: Vec<String> = values.iter().map(|v| format!("\"{v}\"")).collect();
            format!("{key} = [{}]\n", items.join(", "))
        });
        let target = ("[a-z][a-z0-9_]{0,10}", prop::collection::vec(entry, 0..5))
            .prop_map(|(name, entries)| format!("[target.{name}]\n{}", entries.concat()));
        prop::collection::vec(target, 1..5).prop_map(|targets| targets.join("\n"))
    }

    proptest! {
        #[test]
        fn formatting_reaches_a_fixed_point_on_any_manifest(manifest in any_manifest()) {
            // Duplicate target names are legal input to the parser but not to
            // TOML, so a generated collision is not a case about formatting.
            let Ok(once) = format(&manifest) else { return Ok(()) };
            prop_assert_eq!(format(&once).unwrap(), once.clone());
            prop_assert!(is_formatted(&once).unwrap());
            // Nothing is invented and nothing is lost: every quoted string in
            // goes out, so a formatter cannot quietly drop a source file.
            for quoted in manifest.split('"').skip(1).step_by(2) {
                prop_assert!(once.contains(quoted), "{quoted} vanished from {once}");
            }
        }
    }

    #[test]
    fn formatting_is_idempotent() {
        // The property that makes a formatter usable in a commit hook: run it
        // twice and the second run is a no-op. Anything that reorders on every
        // pass, or re-wraps what it just wrapped, fails here.
        let manifests = [
            "[target.app]\nkind = \"cc_binary\"\nsrcs = [\"src/main.c\"]\n",
            "[workspace]\ndefault_targets = [\"app\"]\n\n[target.b]\nkind = \"cc_library\"\nsrcs = [\"b.c\"]\n\n[target.a]\nkind = \"cc_library\"\nsrcs = [\"a.c\"]\n",
            "[target.app]\nsrcs = [\"src/main.c\"]\nkind = \"cc_binary\"\ndeps = [\"//a:a\", \"//b:b\", \"//c:c\", \"//d:d\", \"//e:e\", \"//f:f\", \"//g:g\", \"//h:h\"]\n",
            "[toolchain]\ncflags = [\"-Wall\"]\ncc = \"clang\"\n\n[toolchain.tools]\nz = \"z\"\na = \"a\"\n",
        ];
        for manifest in manifests {
            let once = format(manifest).unwrap();
            let twice = format(&once).unwrap();
            assert_eq!(once, twice, "not idempotent for {manifest:?}");
            assert!(is_formatted(&once).unwrap());
        }
    }

    #[test]
    fn comments_and_strings_survive_exactly() {
        // A formatter that rewrote a `cmd` would be changing the build, and one
        // that dropped the comment explaining why a flag is there would be
        // deleting the only thing that made the flag reviewable.
        let manifest = "\
# Why this workspace exists.
[workspace]
default_targets = [\"app\"]

[target.app]
# The entry point. Do not reorder these flags.
kind = \"cc_binary\"
srcs = [\"src/main.c\"]
cflags = [\"-DGREETING=\\\"hello  world\\\"\", \"-O2\"] # and why
";
        let formatted = format(manifest).unwrap();
        assert!(
            formatted.contains("# Why this workspace exists."),
            "{formatted}"
        );
        assert!(
            formatted.contains("# The entry point. Do not reorder these flags."),
            "{formatted}"
        );
        assert!(formatted.contains("# and why"), "{formatted}");
        assert!(
            formatted.contains("-DGREETING=\\\"hello  world\\\""),
            "the string is not re-escaped or re-spaced:\n{formatted}"
        );
    }

    #[test]
    fn tables_and_keys_land_in_the_order_a_manifest_is_read_in() {
        let manifest = "\
[target.b]
srcs = [\"b.c\"]
kind = \"cc_library\"

[profile.release]
cflags = [\"-O2\"]

[target.a]
deps = [\"b\"]
kind = \"cc_binary\"
srcs = [\"a.c\"]

[workspace]
default_targets = [\"a\"]
";
        let formatted = format(manifest).unwrap();
        let at = |needle: &str| formatted.find(needle).unwrap_or_else(|| panic!("{needle}"));

        assert!(at("[workspace]") < at("[profile.release]"), "{formatted}");
        assert!(at("[profile.release]") < at("[target.a]"), "{formatted}");
        assert!(at("[target.a]") < at("[target.b]"), "{formatted}");
        // Within a target: what it is, what it consumes, what it depends on.
        let target_a = &formatted[at("[target.a]")..at("[target.b]")];
        assert!(
            target_a.find("kind").unwrap() < target_a.find("srcs").unwrap(),
            "{target_a}"
        );
        assert!(
            target_a.find("srcs").unwrap() < target_a.find("deps").unwrap(),
            "{target_a}"
        );
    }

    #[test]
    fn an_array_is_wrapped_only_when_it_does_not_fit() {
        let short = format("[target.a]\nkind = \"cc_library\"\nsrcs = [\n  \"a.c\",\n]\n").unwrap();
        assert!(
            short.contains("srcs = [\"a.c\"]"),
            "a short array is pulled back onto one line:\n{short}"
        );
        // Just under, so it stays: the boundary is the printed line, not the
        // array on its own.
        let fits = format(
            "[target.a]\nkind = \"cc_binary\"\ndeps = [\"//aaaaaaaaaa:a\", \"//bbbbbbbbbb:b\", \"//cccccccccc:c\", \"//dddddddddd:d\"]\n",
        )
        .unwrap();
        assert!(
            fits.lines()
                .any(|line| line.starts_with("deps = [\"//aaaaaaaaaa:a\"") && line.ends_with(']')),
            "78 columns is under the limit and stays on one line:\n{fits}"
        );

        // 7 columns of `deps = ` plus a 105-column array: over the limit
        // only once the key is counted, which is the case the measurement has
        // to get right.
        let long = format(
            "[target.a]\nkind = \"cc_binary\"\ndeps = [\"//aaaaaaaaaa:a\", \"//bbbbbbbbbb:b\", \"//cccccccccc:c\", \"//dddddddddd:d\", \"//eeeeeeeeee:e\", \"//ffffffffff:f\"]\n",
        )
        .unwrap();
        assert!(long.contains("deps = [\n"), "{long}");
        assert!(long.contains("\n  \"//aaaaaaaaaa:a\","), "{long}");
        assert!(long.contains("\n]"), "{long}");
        for line in long.lines() {
            assert!(
                line.chars().count() <= WIDTH,
                "a wrapped line still overflows: {line:?}"
            );
        }
    }

    #[test]
    fn a_manifest_that_does_not_load_can_still_be_formatted() {
        // Formatting is wanted while a manifest is being written, which is
        // exactly when it does not yet describe a valid build. Only the syntax
        // has to hold.
        let formatted = format("[target.a]\nnot_a_key = 1\nkind = \"cc_binary\"\n").unwrap();
        assert!(formatted.contains("kind = \"cc_binary\""), "{formatted}");
        assert!(formatted.contains("not_a_key = 1"), "{formatted}");
        assert!(format("[target.a").is_err(), "syntax still has to hold");
    }
}
