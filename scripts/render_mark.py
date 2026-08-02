#!/usr/bin/env python3
"""Render every brand PNG from site/assets/frostbuild-mark.svg.

The mark used to be a 1254px PNG produced by an image model, which made the
largest asset the only source: it could be re-prompted but not edited, and the
smaller sizes were downscales of a picture rather than renderings of a drawing.
This makes the SVG the source and every PNG a build product of it.

    python3 scripts/render_mark.py            # rewrite the PNGs
    python3 scripts/render_mark.py --check    # fail if any is stale

`--check` is what CI wants: it renders to memory and compares, so a change to
the SVG that was never rendered cannot reach main looking applied.

Needs cairosvg. It is not in any requirements file because nothing else here
needs it and the PNGs are checked in — this runs when the mark changes, which
is rarely.
"""

from __future__ import annotations

import argparse
import pathlib
import sys

# Every PNG the site and README reference, and the size each is used at.
# `frostbuild-mark.png` is the general-purpose master; the rest are the exact
# sizes their consumers ask for, rendered rather than downscaled so each gets
# its own antialiasing pass.
OUTPUTS = {
    "site/assets/frostbuild-mark.png": 1024,
    "site/assets/frostbuild-mark-512.png": 512,
    "site/assets/apple-touch-icon.png": 180,
    "site/favicon.png": 32,
}

SOURCE = "site/assets/frostbuild-mark.svg"


def render(root: pathlib.Path, size: int) -> bytes:
    import cairosvg

    return cairosvg.svg2png(
        url=str(root / SOURCE), output_width=size, output_height=size
    )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="compare instead of writing; exit 1 if a PNG is out of date",
    )
    args = parser.parse_args()

    root = pathlib.Path(__file__).resolve().parent.parent
    stale = []
    for relative, size in OUTPUTS.items():
        want = render(root, size)
        path = root / relative
        if args.check:
            if not path.exists() or path.read_bytes() != want:
                stale.append(relative)
            continue
        path.write_bytes(want)
        print(f"wrote {relative} ({size}px, {len(want)} bytes)")

    if args.check:
        if stale:
            print("stale, re-run scripts/render_mark.py:", file=sys.stderr)
            for relative in stale:
                print(f"  {relative}", file=sys.stderr)
            return 1
        print(f"all {len(OUTPUTS)} PNGs match {SOURCE}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
