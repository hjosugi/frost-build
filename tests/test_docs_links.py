"""Every relative link and anchor in the repository's Markdown resolves.

The documentation cross-references itself heavily — the user guide points into
the specification by section, the specification points at decision records,
and README points at all of them. A renamed heading or a moved file breaks
those links silently: GitHub renders a dead link exactly like a live one. So
this walks every Markdown file that is part of the documentation and checks
each relative link against the tree, and each ``#anchor`` against the headings
GitHub would generate for the page it points into.

External ``http(s)`` links are deliberately not fetched: a test that depends on
someone else's server being up is a flaky test, and the failure would not be
this repository's to fix.
"""

from __future__ import annotations

import re
import tempfile
import unittest
import urllib.parse
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]

#: The documentation set. Generated trees, vendored packages and the site's
#: HTML (checked by tests.test_site_script) are not part of it.
SOURCES = ("README.md", "CONTRIBUTING.md", "DESIGN.md", "docs", "site/README.md", "bench")

SKIPPED_PARTS = {"node_modules", ".frost", "target", ".git", "graphify-out"}

INLINE_LINK = re.compile(r"(?<!\\)!?\[(?:[^\[\]]|\[[^\]]*\])*\]\(\s*<?([^)\s>]+)>?(?:\s+\"[^\"]*\")?\s*\)")
REFERENCE_DEFINITION = re.compile(r"^\s{0,3}\[[^\]]+\]:\s*<?(\S+?)>?(?:\s+.*)?$")
HTML_ANCHOR = re.compile(r"<a\s+[^>]*(?:id|name)=\"([^\"]+)\"", re.IGNORECASE)
FENCE = re.compile(r"^\s{0,3}(`{3,}|~{3,})")
INLINE_CODE = re.compile(r"(`+)(?:(?!\1).)+?\1")


def documentation_files() -> list[Path]:
    files: list[Path] = []
    for source in SOURCES:
        path = ROOT / source
        if path.is_file():
            files.append(path)
        elif path.is_dir():
            files.extend(
                candidate
                for candidate in sorted(path.rglob("*.md"))
                if not SKIPPED_PARTS.intersection(candidate.relative_to(ROOT).parts)
            )
    return files


def prose_lines(text: str) -> list[tuple[int, str]]:
    """Lines outside fenced code blocks, with inline code spans blanked."""
    lines: list[tuple[int, str]] = []
    fence: str | None = None
    for number, line in enumerate(text.splitlines(), start=1):
        opening = FENCE.match(line)
        if fence is None and opening:
            fence = opening.group(1)
            continue
        if fence is not None:
            stripped = line.strip()
            if stripped.startswith(fence) and not stripped.strip(fence[0]):
                fence = None
            continue
        lines.append((number, INLINE_CODE.sub(lambda match: " " * len(match.group(0)), line)))
    return lines


def slug(heading: str) -> str:
    """GitHub's heading anchor: lowercase, punctuation dropped, spaces to ``-``."""
    text = re.sub(r"<[^>]+>", "", heading)
    text = re.sub(r"\[([^\]]*)\]\([^)]*\)", r"\1", text)
    text = text.strip().lower()
    return "".join(
        "-" if character == " " else character
        for character in text
        if character.isalnum() or character in " -_"
    )


def anchors(path: Path) -> set[str]:
    text = path.read_text(encoding="utf-8")
    found: set[str] = set()
    counts: dict[str, int] = {}
    previous = ""
    fence: str | None = None
    for line in text.splitlines():
        opening = FENCE.match(line)
        if fence is None and opening:
            fence = opening.group(1)
            previous = ""
            continue
        if fence is not None:
            stripped = line.strip()
            if stripped.startswith(fence) and not stripped.strip(fence[0]):
                fence = None
            continue
        heading = None
        atx = re.match(r"^\s{0,3}#{1,6}\s+(.*?)\s*#*\s*$", line)
        if atx:
            heading = atx.group(1)
        elif previous.strip() and re.match(r"^\s{0,3}(=+|-+)\s*$", line) and not previous.lstrip().startswith(("|", "-", "*")):
            heading = previous
        if heading is not None:
            base = slug(heading)
            seen = counts.get(base, 0)
            counts[base] = seen + 1
            found.add(base if seen == 0 else f"{base}-{seen}")
        found.update(HTML_ANCHOR.findall(line))
        previous = line
    return found


def links(path: Path) -> list[tuple[int, str]]:
    found: list[tuple[int, str]] = []
    for number, line in prose_lines(path.read_text(encoding="utf-8")):
        for match in INLINE_LINK.finditer(line):
            found.append((number, match.group(1)))
        definition = REFERENCE_DEFINITION.match(line)
        if definition:
            found.append((number, definition.group(1)))
    return found


def check(path: Path, cache: dict[Path, set[str]], root: Path = ROOT) -> list[str]:
    problems: list[str] = []
    for number, target in links(path):
        if re.match(r"^[a-zA-Z][a-zA-Z0-9+.-]*:", target):
            continue  # http:, https:, mailto: and friends are not ours to check
        location, _, fragment = target.partition("#")
        location = urllib.parse.unquote(location.split("?", 1)[0])
        where = f"{path.relative_to(root)}:{number}"
        if location:
            resolved = (root / location.lstrip("/")) if location.startswith("/") else (path.parent / location)
            resolved = resolved.resolve()
            if not resolved.exists():
                problems.append(f"{where}: {target} -> no such file")
                continue
            try:
                resolved.relative_to(root.resolve())
            except ValueError:
                problems.append(f"{where}: {target} -> leaves the repository")
                continue
        else:
            resolved = path
        if not fragment or resolved.suffix != ".md":
            continue
        if resolved not in cache:
            cache[resolved] = anchors(resolved)
        if urllib.parse.unquote(fragment) not in cache[resolved]:
            problems.append(f"{where}: {target} -> no heading with anchor #{fragment}")
    return problems


class DocumentationLinkTests(unittest.TestCase):
    def test_every_relative_link_and_anchor_resolves(self):
        cache: dict[Path, set[str]] = {}
        problems: list[str] = []
        files = documentation_files()
        self.assertGreater(len(files), 30, "the documentation set was not found")
        for path in files:
            problems.extend(check(path, cache))
        self.assertEqual([], problems, "broken documentation links:\n" + "\n".join(problems))

    def test_the_checker_catches_what_it_claims_to(self):
        # A checker that silently accepts everything is worse than none, so
        # its two failure modes are demonstrated on a throwaway tree.
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            (root / "README.md").write_text("# Title\n\n## Tutorials\n", encoding="utf-8")
            probe = root / "probe.md"
            probe.write_text(
                "# Probe\n\n[gone](missing-file.md) [bad](README.md#no-such-heading) "
                "[ok](README.md#tutorials) [self](#probe) `[code](ignored.md)`\n\n"
                "```\n[fenced](ignored.md)\n```\n",
                encoding="utf-8",
            )
            problems = check(probe, {}, root)
        self.assertEqual(2, len(problems), problems)
        self.assertIn("missing-file.md -> no such file", problems[0])
        self.assertIn("#no-such-heading", problems[1])

    def test_slugs_follow_githubs_rules(self):
        self.assertEqual("frost-build", slug("`frost build`"))
        self.assertEqual("platforms-cross--device-builds", slug("Platforms (cross / device builds)"))
        self.assertEqual("frostrc", slug(".frostrc"))
        self.assertEqual("targetnameplatformplat", slug("`[target.NAME.platform.PLAT]`"))


if __name__ == "__main__":
    unittest.main()
