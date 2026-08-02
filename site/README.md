# FrostBuild site and mark

This directory is the source for the official static site at
<https://hjosugi.github.io/frost-build/>. It has no runtime dependencies and
uses relative asset URLs so it works at the repository's Pages base path.

`index.html` is the project overview. `docs/index.html` is the public
documentation hub and links to the authoritative Markdown files in the
repository; the Markdown remains the source of truth.

`styles.css` keeps type, spacing, radius, tracking and leading scales in
`:root`. Documentation components use the 4px spacing scale for reusable gaps
and padding, with separately named tokens for the large page rhythm. Run
`python3 -m unittest tests.test_site_css` after changing these foundations.

## The mark

`assets/frostbuild-mark.svg` is the mark. Every PNG beside it is a rendering
of that file, produced by `scripts/render_mark.py`:

- `assets/frostbuild-mark.png` — 1024 px master;
- `assets/frostbuild-mark-512.png` — navigation, README and social preview;
- `assets/apple-touch-icon.png` — 180 px touch icon;
- `favicon.png` — 32 px browser icon.

Edit the SVG, run `python3 scripts/render_mark.py`, commit the lot.
`--check` renders to memory and compares instead of writing, so a change to
the SVG that was never rendered cannot land looking applied.

It is a six-point ice crystal, drawn as three lines crossing at the centre and
the core they cross on. Three lines through a centre already are six spokes,
and a round cap already is a tip, so the whole mark is two elements. Four
things about it are not taste, and `tests/test_site_mark.py` asserts each
rather than trusting the eye — nothing may be thinner than the favicon can
draw, nothing may fall below 3:1 against either the site's dark navigation or
a light theme, every colour must already be a token in `styles.css`, and the
drawing may not grow past four shapes.

The mark this began as was a 1254 px PNG from an image model, with no vector
source: it could be re-prompted but not edited, and the smaller sizes were
downscales of a picture rather than renderings of a drawing. It had two faults
that the file itself could not show. Its dominant colour was a dark navy
measuring 1.36:1 against the `#040a13` navigation the site puts the mark on,
so the largest part of the logo was invisible exactly where it was used; and
at 32 px its roughly forty facets dissolved into a smudge. Both are now
things a test fails on.

`.github/workflows/pages.yml` publishes this exact directory through
GitHub's official SHA-pinned Pages actions.
