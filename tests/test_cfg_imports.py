"""Code behind a `cfg` the host does not compile still has its imports.

A host build never compiles a `#[cfg(windows)]` block, so an import that only
that block needs looks unused on Linux. `cargo fix` and `cargo clippy --fix`
then delete it, `cargo build` stays green, and the first thing to notice is
Windows CI — several minutes and a push later.

That is not hypothetical; it is where this test comes from. It happened to
`OsStr` and anyhow's `Context` in the CLI's `spawn_daemon`, and again to
`Command` in the executor's `terminate_process_tree`, both times as the
tail of a refactor that ended with `cargo fix`.

The fix in the source is to gate the import the same way as the code that
needs it (`#[cfg(windows)] use std::process::Command;`), which both states
the reason and stops the next `cargo fix` from making the same edit. This
test is what makes an ungated one fail here rather than there.

It reads the source rather than compiling it: the workspace cannot be
cross-compiled to Windows without a C cross-compiler, because blake3 and
zstd-sys build native code.

    python3 -m unittest tests.test_cfg_imports
"""

import pathlib
import re
import unittest

REPO = pathlib.Path(__file__).resolve().parent.parent

# The cfgs a Linux CI host never compiles. `cfg(unix)` and `cfg(target_os =
# "linux")` are excluded on purpose: those *are* compiled here, so the
# ordinary build already checks them.
UNBUILT_CFG = re.compile(r"#\[cfg\((?:windows|not\(unix\)|target_os\s*=\s*\"(?:windows|macos)\")")

# Names that need no import: primitives, prelude items, and the crate's own
# `Self`-shaped vocabulary.
ALWAYS_IN_SCOPE = {
    "Self", "Ok", "Err", "Some", "None", "String", "Vec", "Box", "Option",
    "Result", "Default", "Drop", "Clone", "Copy", "Send", "Sync", "Sized",
    "Iterator", "IntoIterator", "From", "Into", "TryFrom", "TryInto",
    "ToString", "ToOwned", "AsRef", "AsMut", "Fn", "FnMut", "FnOnce",
}

# Traits whose methods are called without the trait ever being named, so a
# missing import shows up as "no method named X" rather than "cannot find X".
TRAIT_METHODS = {
    "Context": r"\.(?:with_)?context\(",
}


def rust_sources():
    for path in sorted(REPO.glob("crates/*/src/**/*.rs")):
        yield path
    for path in sorted(REPO.glob("crates/*/tests/*.rs")):
        yield path


def gated_regions(lines):
    """(start, end, text) for each region behind a cfg this host skips."""
    index = 0
    while index < len(lines):
        if UNBUILT_CFG.search(lines[index]):
            depth, cursor, opened = 0, index, False
            while cursor < len(lines):
                depth += lines[cursor].count("{") - lines[cursor].count("}")
                if "{" in lines[cursor]:
                    opened = True
                if opened and depth <= 0:
                    break
                cursor += 1
            yield index + 1, cursor + 1, "\n".join(lines[index : cursor + 1])
            index = cursor
        index += 1


def names_in_scope(text):
    """Everything the file can name without qualifying it."""
    scope = set(ALWAYS_IN_SCOPE)
    for statement in re.findall(r"^\s*(?:#\[[^\]]*\]\s*)?(?:pub(?:\([^)]*\))?\s+)?use\s+([^;]+);",
                                text, re.M | re.S):
        scope.update(re.findall(r"\b([A-Za-z_]\w*)\b", statement.split("::")[-1]))
        scope.update(re.findall(r"\b(\w+)\s*(?:,|\}|$)", statement))
    # items the file defines itself
    scope.update(re.findall(r"(?:struct|enum|trait|type|union)\s+([A-Za-z_]\w*)", text))
    scope.update(re.findall(r"(?:const|static)\s+([A-Z_][A-Z0-9_]*)", text))
    scope.update(re.findall(r"\bmod\s+([a-z_]\w*)", text))
    return scope


class CfgImportTest(unittest.TestCase):
    def test_types_used_only_behind_an_unbuilt_cfg_are_still_imported(self):
        missing = []
        for path in rust_sources():
            text = path.read_text(encoding="utf-8")
            lines = text.splitlines()
            scope = names_in_scope(text)
            for start, end, region in gated_regions(lines):
                # `Name::` at the head of a path, which is where an unimported
                # type shows up. A qualified `std::process::Command::new` has a
                # lowercase segment before it and is skipped by the lookbehind.
                for name in set(re.findall(r"(?<![\w:])([A-Z]\w*)::", region)):
                    if name not in scope:
                        missing.append(f"{path.relative_to(REPO)}:{start} uses `{name}::`")
        self.assertEqual(missing, [], "gate the import the way the code is gated")

    def test_traits_used_only_behind_an_unbuilt_cfg_are_still_imported(self):
        # The half of the failure that does not name anything: `.context(...)`
        # needs `anyhow::Context` in scope and never spells it.
        missing = []
        for path in rust_sources():
            text = path.read_text(encoding="utf-8")
            lines = text.splitlines()
            scope = names_in_scope(text)
            for start, end, region in gated_regions(lines):
                for trait_name, call in TRAIT_METHODS.items():
                    if re.search(call, region) and trait_name not in scope:
                        missing.append(
                            f"{path.relative_to(REPO)}:{start} calls a `{trait_name}` method"
                        )
        self.assertEqual(missing, [], "gate the import the way the code is gated")

    def test_the_check_would_have_caught_the_two_that_reached_ci(self):
        # A test for absence is worth nothing if it cannot detect presence, and
        # both of these compiled cleanly on Linux.
        no_ostr = 'use anyhow::Result;\n#[cfg(windows)]\nfn f() {\n    let _ = OsStr::new("x");\n}\n'
        lines = no_ostr.splitlines()
        scope = names_in_scope(no_ostr)
        found = [
            name
            for _, _, region in gated_regions(lines)
            for name in re.findall(r"(?<![\w:])([A-Z]\w*)::", region)
            if name not in scope
        ]
        self.assertEqual(found, ["OsStr"])

        no_context = 'use anyhow::Result;\n#[cfg(windows)]\nfn f() {\n    g().context("x")?;\n}\n'
        lines = no_context.splitlines()
        scope = names_in_scope(no_context)
        self.assertTrue(
            any(
                re.search(TRAIT_METHODS["Context"], region) and "Context" not in scope
                for _, _, region in gated_regions(lines)
            )
        )

    def test_it_accepts_the_gated_form_the_repository_uses(self):
        # The shape the fix takes has to pass, or the test teaches nothing.
        gated = (
            "#[cfg(windows)]\nuse std::process::Command;\n\n"
            '#[cfg(windows)]\nfn f() {\n    let _ = Command::new("taskkill");\n}\n'
        )
        scope = names_in_scope(gated)
        self.assertIn("Command", scope)


if __name__ == "__main__":
    unittest.main()
