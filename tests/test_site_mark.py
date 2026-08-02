"""The brand mark stays legible where it is actually used.

The mark these tests were written for was a 1254px PNG from an image model:
about forty facets and thirteen scattered nodes, with no vector source, so it
could be re-prompted but not edited. Two things were wrong with it and neither
was visible in the file itself.

Its dominant colour was a dark navy, `#022759`, measuring **1.36:1** against
the `#040a13` navigation the site puts the mark on. The largest part of the
logo was effectively invisible exactly where it was placed. And at 32px — the
favicon, the smallest and by far the most-seen rendering — the detail
dissolved into a smudge.

Both are properties of the artwork that no other test in this repository would
notice, and both are the kind of thing that comes back the next time someone
adjusts the mark to taste. So they are asserted rather than described:
contrast is computed against the backgrounds the mark is really composited on,
and the PNGs are checked to still be renderings of the SVG rather than files
that drifted from it.

`python3 -m unittest tests.test_site_mark`
"""

import pathlib
import re
import unittest

REPO = pathlib.Path(__file__).resolve().parent.parent
SITE = REPO / "site"
MARK = SITE / "assets" / "frostbuild-mark.svg"

# The two backgrounds the mark is composited on. The site's navigation is the
# one the previous mark failed against; white stands in for GitHub's light
# theme, where the README and the social preview are read.
DARK_NAV = (0x04, 0x0A, 0x13)
WHITE = (0xFF, 0xFF, 0xFF)

# WCAG's minimum for a graphic that carries meaning. A logo is not text, so
# 4.5:1 is not required of it — but 3:1 is the line below which a shape stops
# being reliably distinguishable from what is behind it.
MINIMUM_GRAPHIC_CONTRAST = 3.0

# One colour is allowed to fall under that, and only against one background.
# An exception that says why is the difference between an exception and a
# hole, so the reason is here rather than in a commit message.
CONTRAST_EXCEPTIONS = {
    # The core, and the only shape the silhouette fully encloses. Cyan is
    # 1.68:1 on white, so on its own it would be a poor edge against a light
    # theme — but it never has one: six blue spokes converge on it and the
    # blue defines its outline from every side. What the rule protects is a
    # shape having to be told apart from the background, which this one does
    # not do. It is the reason the mark is two-tone at all, and dropping it
    # would leave a flat monochrome asterisk with no node at the root.
    ((0x31, 0xD9, 0xFF), WHITE): "enclosed by the blue silhouette on every side",
}

# The favicon is 32px, so a stroke of N units on the 512 viewBox lands at
# N/16 device pixels. Below about two, antialiasing turns a line into a grey
# suggestion of one — which is what happened to the mark this replaced.
MINIMUM_FAVICON_PIXELS = 2.0
VIEWBOX = 512
FAVICON = 32

# The drawing is a stroked path and the core it crosses on. Room for one more
# is room to add a shape deliberately; it is not room to drift.
MOST_SHAPES = 4


def relative_luminance(rgb):
    channels = []
    for value in rgb:
        v = value / 255
        channels.append(v / 12.92 if v <= 0.04045 else ((v + 0.055) / 1.055) ** 2.4)
    red, green, blue = channels
    return 0.2126 * red + 0.7152 * green + 0.0722 * blue


def contrast(a, b):
    high, low = sorted((relative_luminance(a), relative_luminance(b)), reverse=True)
    return (high + 0.05) / (low + 0.05)


def parse_hex(value):
    value = value.lstrip("#")
    return tuple(int(value[i : i + 2], 16) for i in (0, 2, 4))


def painted_colours(svg):
    """Colours the mark actually paints with.

    Read from `fill` and `stroke` attributes rather than from anywhere a hex
    code appears: the SVG's own comment quotes the colours it is arguing
    about, including the one that failed, and a test that scraped those would
    report the mark for mentioning its history.
    """
    return {
        parse_hex(value)
        for value in re.findall(r'(?:fill|stroke)="(#[0-9a-fA-F]{6})"', svg)
    }


class SiteMarkTest(unittest.TestCase):
    def setUp(self):
        self.svg = MARK.read_text()

    def test_the_svg_is_the_source_the_pngs_are_rendered_from(self):
        # Every PNG is a build product. If one of them stopped matching, the
        # mark in the browser is not the mark in the repository, and the file
        # a designer would edit is no longer the file anyone sees.
        try:
            import cairosvg  # noqa: F401
        except ImportError:
            self.skipTest("cairosvg is not installed; run scripts/render_mark.py")

        import sys

        sys.path.insert(0, str(REPO / "scripts"))
        import render_mark

        stale = [
            relative
            for relative, size in render_mark.OUTPUTS.items()
            if (REPO / relative).read_bytes() != render_mark.render(REPO, size)
        ]
        self.assertEqual(stale, [], "re-run scripts/render_mark.py")

    def test_nothing_in_the_mark_disappears_into_the_background(self):
        # The failure this exists for: a colour that looks right in a design
        # tool on white, and is invisible on the navigation the site puts it
        # on. Checked against both, because the mark is one file serving both.
        colours = painted_colours(self.svg)
        self.assertTrue(colours, "the mark declares no colours")

        for colour in sorted(colours):
            spelling = "#%02x%02x%02x" % colour
            for background, name in ((DARK_NAV, "the site's dark navigation"),
                                     (WHITE, "a light theme")):
                if (colour, background) in CONTRAST_EXCEPTIONS:
                    continue
                ratio = contrast(colour, background)
                with self.subTest(colour=spelling, background=name):
                    self.assertGreaterEqual(
                        ratio,
                        MINIMUM_GRAPHIC_CONTRAST,
                        f"{spelling} measures {ratio:.2f}:1 against {name}; the "
                        f"mark this replaced failed here at 1.36:1",
                    )

    def test_every_contrast_exception_still_names_a_colour_in_use(self):
        # An exemption that outlives the shape it was written for excuses
        # nothing and hides the next thing that needs excusing.
        painted = painted_colours(self.svg)
        for (colour, _), reason in CONTRAST_EXCEPTIONS.items():
            with self.subTest(colour="#%02x%02x%02x" % colour):
                self.assertIn(colour, painted, f"exempted for: {reason}")

    def test_no_stroke_is_thinner_than_the_favicon_can_render(self):
        # A mark is drawn at 512 and seen at 32. A weight chosen at the size
        # it is drawn is a weight chosen for the rendering nobody looks at.
        widths = [float(w) for w in re.findall(r'stroke-width="([0-9.]+)"', self.svg)]
        self.assertTrue(widths, "the mark declares no stroke widths")
        for width in widths:
            pixels = width * FAVICON / VIEWBOX
            with self.subTest(stroke_width=width):
                self.assertGreaterEqual(
                    pixels,
                    MINIMUM_FAVICON_PIXELS,
                    f"a {width}-unit stroke is {pixels:.2f}px in the favicon",
                )

    def test_the_mark_uses_the_site_palette_and_adds_nothing_to_it(self):
        # A brand mark that invents its own blue is a second palette nobody
        # maintains. Every colour here has to be a token in the stylesheet.
        stylesheet = (SITE / "styles.css").read_text()
        tokens = {
            parse_hex(value)
            for value in re.findall(r"--[a-z-]+:\s*(#[0-9a-fA-F]{6})", stylesheet)
        }
        for colour in painted_colours(self.svg):
            with self.subTest(colour="#%02x%02x%02x" % colour):
                self.assertIn(colour, tokens, "not a token in site/styles.css")

    def test_the_mark_stays_something_a_reader_can_take_in(self):
        # Not a measure of beauty — a ceiling on element count. The mark two
        # revisions ago had roughly forty facets, which is what made it a
        # picture rather than a mark and what made it dissolve at favicon
        # size. The drawing is two shapes; the ceiling is set just above that
        # rather than at a round number, because a limit of twenty would not
        # notice the drift back until it had already happened.
        shapes = len(
            re.findall(r"<(path|circle|polygon|rect|line|ellipse)\b", self.svg)
        )
        self.assertLessEqual(shapes, MOST_SHAPES, "the mark is drifting back to a picture")


if __name__ == "__main__":
    unittest.main()
