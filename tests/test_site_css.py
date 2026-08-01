"""The site's stylesheet keeps its scales in one place.

Before this existed, `site/styles.css` held 51 font-size declarations with 26
distinct rem values, 12 letter-spacing declarations with 12 distinct values, and
8px/9px/10px sitting beside 15px/16px. None of that was design: values that
differ by 0.01rem — a sixth of a pixel — are what accretion looks like.

Consolidating it once is easy. Keeping it consolidated is the part that needs a
test, because the next hurried change adds `font-size: 0.77rem` and nothing
notices. So this asserts the rule rather than documenting it, the way the CLI
surface snapshot and the extension's architecture test do.

Exceptions are listed explicitly and each one says why, which is the difference
between an exception and a hole.
"""

import pathlib
import re
import unittest

SITE = pathlib.Path(__file__).resolve().parent.parent / "site"
STYLESHEET = SITE / "styles.css"

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


def _root_block(css: str) -> str:
    """The `:root` block, where the scales are allowed to name numbers."""
    start = css.index(":root")
    return css[start : css.index("}", start) + 1]


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

    def test_docs_hero_contains_intrinsic_code_width(self):
        # The quick-start block contains white-space: pre. Without a zero
        # minimum its intrinsic width expands the mobile grid past the viewport.
        self.assertRegex(
            self.css,
            r"(?s)\.docs-hero > \* \{\s*min-width: 0;\s*\}",
        )


if __name__ == "__main__":
    unittest.main()
