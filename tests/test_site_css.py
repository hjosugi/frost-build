"""The site's stylesheet keeps its scales in one place, and its links resolve.

Before this existed, `site/styles.css` held 51 font-size declarations with 26
distinct rem values, 12 letter-spacing declarations with 12 distinct values, and
8px/9px/10px sitting beside 15px/16px. None of that was design: values that
differ by 0.01rem — a sixth of a pixel — are what accretion looks like.

Consolidating it once is easy. Keeping it consolidated is the part that needs a
test, because the next hurried change adds `font-size: 0.77rem` and nothing
notices. So this asserts the rule rather than documenting it, the way the CLI
surface snapshot and the extension's architecture test do.

The kinds of rot here all render perfectly and all mislead: a scale that grows
a twenty-seventh value, a spacing number written twice because the scale was
easier to bypass than to read, a `var(--typo)` that silently drops its
declaration, and a card linking to a doc that has since been renamed. None of
them raises anything anywhere else.

Exceptions are listed explicitly and each one says why, which is the difference
between an exception and a hole.
"""

import pathlib
import re
import unittest

REPO = pathlib.Path(__file__).resolve().parent.parent
SITE = REPO / "site"
STYLESHEET = SITE / "styles.css"
PAGES = (SITE / "index.html", SITE / "docs" / "index.html")

# A declaration may hold a literal only when a token would be wrong, not merely
# when one is inconvenient.
ALLOWED_LITERALS = {
    # Proportional to the metric it sits beside, so it must scale with it. A
    # rem token would sever exactly the relationship this em expresses.
    ("font-size", "0.38em"),
}

TOKENISED_PROPERTIES = (
    "font-size",
    "border-radius",
    "letter-spacing",
    "line-height",
)


# Spacing is held to a weaker rule than the scales above, on purpose. A page
# has genuine one-off compositions — the 36px over a card heading exists
# because of that heading, not because 36 is a size the design believes in —
# and turning those into tokens invents decisions nobody made. What is not
# defensible is the *same* number appearing twice: the second use is either
# the same intent, in which case it wants a name, or a coincidence, in which
# case the next edit will move one and not the other.
SPACING_PREFIXES = ("padding", "margin", "gap", "row-gap", "column-gap", "scroll-margin")

# Only the documentation hub is held to it. The marketing page is a separate
# pass, and a rule that fails for work nobody has started is a rule people
# learn to skip.
DOCS_SELECTOR = re.compile(r"\.docs?[-.]")

# A card that links to `blob/main/docs/23_bazel_migration.md` is naming a file
# in this repository. GitHub serves a 404 for a renamed one, and the page looks
# fine either way, so the check has to happen here.
BLOB = re.compile(r"^https://github\.com/hjosugi/frost-build/blob/main/(.+)$")


def _root_block(css: str) -> str:
    """The `:root` block, where the scales are allowed to name numbers."""
    start = css.index(":root")
    return css[start : css.index("}", start) + 1]


def _repo_path_of(target: str):
    """The repository path a link names, or None if it names something else."""
    match = BLOB.match(target.split("#")[0])
    return match.group(1) if match else None


def _rules(css: str):
    """(selector, body) for every innermost rule, media queries included.

    `[^{}]*` cannot span a nested block, so a `@media` opener never matches as
    a selector and the rules inside it do.
    """
    for match in re.finditer(r"([^{}]+)\{([^{}]*)\}", css):
        yield match.group(1).strip(), match.group(2)


def _docs_spacing_literals(css: str):
    """Every px literal a docs rule spends on spacing, with where it came from."""
    for selector, body in _rules(css):
        if not DOCS_SELECTOR.search(selector):
            continue
        for prop, value in re.findall(r"([a-z-]+):\s*([^;]+);", body):
            if not prop.startswith(SPACING_PREFIXES):
                continue
            for literal in re.findall(r"(?<![\w.-])(\d+(?:\.\d+)?px)", value):
                yield literal, f"{selector} {{ {prop}: {value.strip()} }}"


def _declarations(css: str, prop: str):
    """(value, line) for every declaration of `prop` outside `:root`."""
    body = css.replace(_root_block(css), "", 1)
    for match in re.finditer(rf"(?m)^\s*{prop}:\s*([^;]+);", body):
        yield match.group(1).strip(), body[: match.start()].count("\n") + 1


class SiteStylesheetTestCase(unittest.TestCase):
    def setUp(self):
        self.css = STYLESHEET.read_text(encoding="utf-8")

    def test_scales_are_declared_once_in_root(self):
        root = _root_block(self.css)
        for name in (
            "--text-xs",
            "--space-4",
            "--docs-layout-gap",
            "--radius-md",
            "--track-wide",
            "--leading-normal",
        ):
            self.assertIn(name, root, f"{name} must be defined in :root")

    def test_no_literal_sizes_outside_the_scales(self):
        offenders = []
        for prop in TOKENISED_PROPERTIES:
            for value, line in _declarations(self.css, prop):
                if "var(" in value:
                    continue
                if (prop, value) in ALLOWED_LITERALS:
                    continue
                offenders.append(f"{prop}: {value} (styles.css:~{line})")
        self.assertEqual(
            offenders,
            [],
            "use a scale token, or add the value to ALLOWED_LITERALS with a reason",
        )

    def test_every_referenced_token_exists(self):
        # A typo in `var(--text-xls)` is silent: the declaration is dropped and
        # the element inherits, which looks like a styling accident rather than
        # a mistake.
        root = _root_block(self.css)
        defined = set(re.findall(r"(--[a-z0-9-]+):", root))
        referenced = set(re.findall(r"var\((--[a-z0-9-]+)", self.css))
        self.assertEqual(
            sorted(referenced - defined),
            [],
            "referenced custom properties that :root does not define",
        )

    def test_every_defined_token_is_used(self):
        # A scale step nothing references is a step that was not needed, and it
        # invites the next author to reach for it because it is there.
        root = _root_block(self.css)
        defined = set(re.findall(r"(--[a-z0-9-]+):", root))
        referenced = set(re.findall(r"var\((--[a-z0-9-]+)", self.css))
        # The responsive override redefines --shell; it is used through the
        # same name in the base block.
        self.assertEqual(sorted(defined - referenced), [])

    def test_the_type_scale_has_no_imperceptible_neighbours(self):
        # The failure this whole exercise fixes: two steps so close that no
        # reader can tell them apart, which means one of them is noise.
        root = _root_block(self.css)
        sizes = sorted(
            float(value)
            for value in re.findall(r"--text-[a-z0-9]+:\s*([0-9.]+)rem", root)
        )
        for smaller, larger in zip(sizes, sizes[1:]):
            gap_px = (larger - smaller) * 16
            self.assertGreaterEqual(
                gap_px,
                0.7,
                f"{smaller}rem and {larger}rem differ by {gap_px:.2f}px",
            )

    def test_spacing_scale_uses_the_four_pixel_grid(self):
        root = _root_block(self.css)
        steps = re.findall(r"--space-([0-9]+):\s*([0-9]+)px", root)
        self.assertGreaterEqual(len(steps), 4)
        for step, pixels in steps:
            self.assertEqual(int(pixels), int(step) * 4)

    def test_a_repeated_docs_spacing_value_is_a_token(self):
        # The grid test above says the scale is well formed; it cannot say the
        # rules use it. This is the half that notices 80px written twice.
        seen: dict[str, list[str]] = {}
        for literal, where in _docs_spacing_literals(self.css):
            seen.setdefault(literal, []).append(where)
        repeated = {
            literal: sites for literal, sites in seen.items() if len(sites) > 1
        }
        self.assertEqual(
            repeated,
            {},
            "a spacing value used more than once is a decision; give it a token",
        )

    def test_the_stylesheet_parses(self):
        # `var(--x` with the paren left off, or a rule left unclosed, drops
        # everything after it. The browser recovers silently and the page just
        # looks wrong, so nothing surfaces this but a render.
        self.assertEqual(
            self.css.count("{"),
            self.css.count("}"),
            "unbalanced braces",
        )
        self.assertEqual(
            self.css.count("var("),
            len(re.findall(r"var\((--[a-z0-9-]+)[^)]*\)", self.css)),
            "a var() reference is malformed or unclosed",
        )
        for selector, body in _rules(self.css):
            stripped = body.strip()
            if stripped and not stripped.endswith((";", "*/")):
                self.fail(f"declaration without a semicolon in `{selector}`")

    def test_every_link_the_pages_make_resolves(self):
        # An `href="#start-here"` with no such id, or a doc renamed out from
        # under a card, is a dead link that renders perfectly.
        broken = []
        for page in PAGES:
            html = page.read_text(encoding="utf-8")
            # Both pages are `index.html`; the message has to say which.
            name = page.relative_to(REPO).as_posix()
            ids = set(re.findall(r'id="([^"]+)"', html))
            for target in re.findall(r'(?:href|src)="([^"]+)"', html):
                if target.startswith(("mailto:", "data:")):
                    continue
                repo_file = _repo_path_of(target)
                if repo_file is not None:
                    if not (REPO / repo_file).exists():
                        broken.append(f"{name}: {target} (no {repo_file})")
                    continue
                if target.startswith(("http://", "https://")):
                    continue  # someone else's server; not ours to assert on
                path, _, fragment = target.partition("#")
                if not path:
                    if fragment not in ids:
                        broken.append(f"{name}: #{fragment} matches no id")
                    continue
                destination = (page.parent / path).resolve()
                if not destination.exists():
                    broken.append(f"{name}: {target} does not exist")
                    continue
                if not fragment:
                    continue
                # `../#why` names an id on the *other* page, and the check
                # above cannot see it: `../` is a directory, and a directory
                # exists whatever the page inside it happens to say. The docs
                # hub links back into the front page this way, so the anchors
                # that break are exactly the ones nothing was watching.
                if destination.is_dir():
                    destination /= "index.html"
                if not destination.is_file():
                    broken.append(f"{name}: {target} has no page to anchor in")
                    continue
                elsewhere = set(
                    re.findall(r'id="([^"]+)"', destination.read_text(encoding="utf-8"))
                )
                if fragment not in elsewhere:
                    broken.append(
                        f"{name}: {target} matches no id in "
                        f"{destination.relative_to(REPO).as_posix()}"
                    )
        self.assertEqual(broken, [])

    def test_docs_hero_contains_intrinsic_code_width(self):
        # The quick-start block contains white-space: pre. Without a zero
        # minimum its intrinsic width expands the mobile grid past the viewport.
        self.assertRegex(
            self.css,
            r"(?s)\.docs-hero > \* \{\s*min-width: 0;\s*\}",
        )


if __name__ == "__main__":
    unittest.main()
