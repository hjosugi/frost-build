#!/usr/bin/env python3
"""Execute the user guide's tutorials and migration guides as written.

A tutorial that nobody runs drifts: a flag is renamed, a default changes, an
output moves, and the page keeps showing a session that no longer happens. So
the pages under ``docs/guide/`` that start with a ``guide-test`` marker are
executed by this script, in a fresh empty directory each, against a real
``frost`` binary — the page *is* the test, and CI runs it.

The page stays ordinary Markdown. Only the fenced-block info string, which
renderers ignore after the language word, carries the extra meaning:

``<!-- guide-test: requires=cc,javac dir=hello -->``
    Near the top of a page: execute it. ``requires`` names executables that must
    be on ``PATH``; a page whose tools are missing is reported as skipped, and
    ``--require-all`` turns that skip into a failure (CI passes it, so a runner
    image that lost a compiler cannot turn the job silently green). ``dir``
    names the empty directory the page starts in, for pages whose output
    depends on it (``frost init`` names targets after the directory).

```` ```c file=src/main.c ````
    Write the block's contents to that path, relative to the page's directory.
    Writing the same path again replaces the file, which is how a page shows an
    edit.

```` ```sh ````
    Run the block with ``bash -eu -o pipefail`` in the page's directory. It
    must exit 0.

```` ```sh fails ````
    The same, but it must exit non-zero: a page that demonstrates an error
    message proves the error still happens.

```` ```text output ````
    Every non-blank line must occur, as a substring, in what the preceding
    command printed (stdout and stderr together). Only stable text belongs
    here — never a timing.

Any other fence (``toml``, a ``sh`` block with ``skip``, a bare ``text``) is
prose for the reader and is not executed.

Usage::

    python3 scripts/run_guides.py --frost target/debug/frost docs/guide
    python3 scripts/run_guides.py --frost target/debug/frost --require-all \\
        docs/guide/tutorials/c.md

Exit codes follow frost's own split (docs/28): ``0`` every selected page ran
(or was skipped for a missing tool without ``--require-all``), ``1`` a page
failed, ``2`` the pages could not be run as asked.
"""

from __future__ import annotations

import argparse
import os
import re
import shutil
import subprocess
import sys
import tempfile
from dataclasses import dataclass, field
from pathlib import Path

MARKER = re.compile(r"<!--\s*guide-test:(?P<body>[^>]*)-->")
FENCE = re.compile(r"^(?P<indent> {0,3})(?P<fence>`{3,}|~{3,})(?P<info>.*)$")

#: How long one command block may run. The slowest real step is a cold
#: ``javac``; anything near this limit is a hang, not a slow machine.
BLOCK_TIMEOUT_SECONDS = 300


@dataclass
class Step:
    """One executable thing a page asks for, in page order."""

    kind: str  # "file", "run", "output"
    line: int
    body: str
    path: str = ""
    expect_failure: bool = False


@dataclass
class Page:
    path: Path
    requires: list[str] = field(default_factory=list)
    directory: str = "project"
    steps: list[Step] = field(default_factory=list)


class GuideError(Exception):
    """The page itself is malformed, as opposed to a step failing."""


def parse_page(path: Path, text: str) -> Page | None:
    """Return the executable content of a page, or ``None`` if it has none."""
    marker = MARKER.search(text)
    if marker is None:
        return None
    page = Page(path=path)
    for item in marker.group("body").split():
        key, _, value = item.partition("=")
        if key == "requires":
            page.requires = [tool for tool in value.split(",") if tool]
        elif key == "dir" and re.fullmatch(r"[A-Za-z0-9_-]+", value):
            page.directory = value
        else:
            raise GuideError(f"{path}: unknown guide-test option {item!r}")

    lines = text.splitlines()
    index = 0
    while index < len(lines):
        opening = FENCE.match(lines[index])
        if opening is None:
            index += 1
            continue
        fence = opening.group("fence")
        info = opening.group("info").split()
        start = index + 1
        body: list[str] = []
        index += 1
        while index < len(lines):
            closing = lines[index].strip()
            if closing.startswith(fence[0] * len(fence)) and not closing.strip(fence[0]):
                break
            body.append(lines[index])
            index += 1
        else:
            raise GuideError(f"{path}:{start}: unterminated code fence")
        index += 1
        step = classify(path, start, info, "\n".join(body) + "\n")
        if step is not None:
            page.steps.append(step)
    if not any(step.kind == "run" for step in page.steps):
        raise GuideError(f"{path}: marked guide-test but has no ```sh block to run")
    return page


def classify(path: Path, line: int, info: list[str], body: str) -> Step | None:
    if not info:
        return None
    language, options = info[0], info[1:]
    for option in options:
        if option.startswith("file="):
            target = option[len("file=") :]
            if not target or target.startswith("/") or ".." in Path(target).parts:
                raise GuideError(f"{path}:{line}: file= must be a relative path, got {target!r}")
            return Step(kind="file", line=line, body=body, path=target)
    if language == "sh":
        if "skip" in options:
            return None
        unknown = [option for option in options if option not in ("fails",)]
        if unknown:
            raise GuideError(f"{path}:{line}: unknown sh block option(s) {unknown}")
        return Step(kind="run", line=line, body=body, expect_failure="fails" in options)
    if language == "text" and "output" in options:
        return Step(kind="output", line=line, body=body)
    return None


def run_page(page: Page, frost: Path, workdir: Path, verbose: bool) -> list[str]:
    """Execute one page in ``workdir``; return the failures, empty on success."""
    env = dict(os.environ)
    env["PATH"] = str(frost.parent) + os.pathsep + env.get("PATH", "")
    # A developer's own ~/.config/frost/frostrc must not change what a page
    # shows, and colour codes would break the output comparisons.
    env["XDG_CONFIG_HOME"] = str(workdir.parent / "config")
    env["NO_COLOR"] = "1"
    env.pop("CLICOLOR_FORCE", None)

    last_output: str | None = None
    last_line = 0
    for step in page.steps:
        where = f"{page.path}:{step.line}"
        if step.kind == "file":
            destination = workdir / step.path
            destination.parent.mkdir(parents=True, exist_ok=True)
            destination.write_text(step.body, encoding="utf-8")
            continue
        if step.kind == "output":
            if last_output is None:
                return [f"{where}: an output block must follow a ```sh block"]
            missing = [
                expected.strip()
                for expected in step.body.splitlines()
                if expected.strip() and expected.strip() not in last_output
            ]
            if missing:
                return [
                    f"{where}: the command at line {last_line} did not print:\n"
                    + "\n".join(f"    {text}" for text in missing)
                    + "\n  it printed:\n"
                    + indent(last_output)
                ]
            continue
        if verbose:
            print(f"--- {where}\n{step.body}", end="", flush=True)
        try:
            completed = subprocess.run(
                ["bash", "-eu", "-o", "pipefail", "-c", step.body],
                cwd=workdir,
                env=env,
                stdout=subprocess.PIPE,
                stderr=subprocess.STDOUT,
                text=True,
                timeout=BLOCK_TIMEOUT_SECONDS,
            )
        except subprocess.TimeoutExpired:
            return [f"{where}: did not finish within {BLOCK_TIMEOUT_SECONDS} s"]
        last_output = completed.stdout
        last_line = step.line
        if verbose and completed.stdout:
            print(indent(completed.stdout), end="", flush=True)
        failed = completed.returncode != 0
        if failed != step.expect_failure:
            expectation = "fail" if step.expect_failure else "succeed"
            return [
                f"{where}: expected the block to {expectation}, it exited "
                f"{completed.returncode}:\n{indent(step.body)}  output:\n"
                + indent(completed.stdout)
            ]
    return []


def indent(text: str) -> str:
    return "".join(f"    {line}\n" for line in text.splitlines())


def collect(paths: list[Path]) -> list[Path]:
    pages: list[Path] = []
    for path in paths:
        if path.is_dir():
            pages.extend(sorted(path.rglob("*.md")))
        elif path.is_file():
            pages.append(path)
        else:
            raise GuideError(f"{path}: no such file or directory")
    return pages


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    parser.add_argument("paths", nargs="+", type=Path, help="pages or directories of pages")
    parser.add_argument("--frost", type=Path, required=True, help="the frost binary to test")
    parser.add_argument(
        "--require-all",
        action="store_true",
        help="fail, instead of skipping, a page whose required tools are missing",
    )
    parser.add_argument("--keep", action="store_true", help="keep each page's directory")
    parser.add_argument("-v", "--verbose", action="store_true", help="echo commands and output")
    args = parser.parse_args(argv)

    frost = args.frost.resolve()
    if not frost.is_file() or not os.access(frost, os.X_OK):
        print(f"run_guides: {args.frost} is not an executable frost binary", file=sys.stderr)
        return 2
    try:
        pages = []
        for path in collect(args.paths):
            page = parse_page(path, path.read_text(encoding="utf-8"))
            if page is not None:
                pages.append(page)
    except GuideError as error:
        print(f"run_guides: {error}", file=sys.stderr)
        return 2
    if not pages:
        print("run_guides: none of the given pages is marked guide-test", file=sys.stderr)
        return 2

    failures = 0
    skipped: list[str] = []
    for page in pages:
        missing = [tool for tool in page.requires if shutil.which(tool) is None]
        if missing:
            skipped.append(f"{page.path} (needs {', '.join(missing)})")
            print(f"SKIP {page.path}: {', '.join(missing)} not on PATH")
            continue
        scratch = Path(tempfile.mkdtemp(prefix="frost-guide-"))
        workdir = scratch / page.directory
        workdir.mkdir()
        try:
            problems = run_page(page, frost, workdir, args.verbose)
        finally:
            if args.keep:
                print(f"     kept {workdir}")
            else:
                shutil.rmtree(scratch, ignore_errors=True)
        if problems:
            failures += 1
            print(f"FAIL {page.path}")
            for problem in problems:
                print(problem)
        else:
            steps = sum(step.kind == "run" for step in page.steps)
            print(f"ok   {page.path} ({steps} command blocks)")

    if skipped and args.require_all:
        print(f"run_guides: --require-all and {len(skipped)} page(s) skipped", file=sys.stderr)
        return 1
    return 1 if failures else 0


if __name__ == "__main__":
    sys.exit(main())
