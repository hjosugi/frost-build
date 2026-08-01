#!/usr/bin/env python3
"""Turn a ``frost --build-event-json`` stream into a JUnit XML report.

The event stream is ndjson because it is easy to write while a build runs.
JUnit XML is what CI systems already render. This is the adapter between the
two, and it is a script rather than a subcommand on purpose: the shape of a
test report is a property of the CI system reading it, and baking one vendor's
dialect into the build engine would be the wrong place to put that knowledge.

Two failure modes of a naive converter are worth naming, because both produce a
*green* report for a build that did not pass, which is worse than no report:

* A build that broke before any test ran has no test events at all. A report
  containing zero testcases and zero failures reads as "nothing wrong". So a
  non-test action that did not pass is reported too, in its own suite.
* A fully cached rerun takes frost's all-cached fast path and emits one
  ``all_cached`` event instead of one event per action. That is also zero
  testcases. It becomes one passing case that says so.

Usage::

    frost test --all --keep-going --no-tui --build-event-json events.ndjson
    python3 scripts/frost_junit.py events.ndjson -o junit.xml \\
        --summary "$GITHUB_STEP_SUMMARY"

Exit codes follow the same split as frost itself (docs/28): ``0`` the report
was written, ``2`` the stream could not be read as asked. A report full of
failures still exits ``0`` — converting is the work, and the step that ran the
build already failed the job.
"""

from __future__ import annotations

import argparse
import json
import re
import sys
import xml.etree.ElementTree as ET
from dataclasses import dataclass, field

#: The one schema this reader understands. See docs/28 for the compatibility
#: promise: fields are added, never removed or retyped, and ``event`` and
#: ``result`` names keep their meaning. So a *new* field or event is safe to
#: ignore, and a *different* schema is not safe to guess at.
SCHEMA = "frost-build-events-v1"

#: Prefix of the action ids frost gives test targets.
TEST_KIND = "test"

#: Longest ``message=`` attribute. The full text goes in the element body; the
#: attribute is what a collapsed row shows, and a viewer that renders 4 KB of
#: compiler output on one line is unreadable.
MESSAGE_CHARS = 200

#: Longest per-case body. A runaway test log should not produce a report no
#: browser will open.
BODY_CHARS = 8000

#: ANSI escape sequences: a compiler writing to a pipe frost captured may still
#: have colourised, and the bytes are not valid XML text once stripped of their
#: escape character. Remove the whole sequence rather than leaving `[0;31m`.
ANSI_RE = re.compile(r"\x1b\[[0-9;?]*[ -/]*[@-~]|\x1b[@-Z\\-_]")

#: Characters XML 1.0 forbids outright. Real test output contains them (a NUL
#: from a binary diff, a form feed from a linker); a document containing one is
#: rejected by strict parsers, which is a failure the CI job blames on us.
ILLEGAL_XML_RE = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")

#: How each frost result becomes JUnit. ``error`` is deliberately reserved for
#: "this reader does not know what happened" — see ``_case_element``.
PASSING = {"executed", "cached", "flaky"}


def _plural(count: int, noun: str) -> str:
    return f"{count} {noun}" if count == 1 else f"{count} {noun}s"


class SchemaMismatch(Exception):
    """The stream is not the version this reader knows how to read."""


class CorruptStream(Exception):
    """A line in the middle of the stream is not JSON."""


@dataclass
class Case:
    """One action frost finished."""

    id: str
    desc: str
    result: str
    duration_ms: int
    detail: str

    @property
    def kind(self) -> str:
        """``test`` for a test target, ``compile``/``link``/``genrule`` else."""
        return self.id.partition(":")[0]

    @property
    def package(self) -> str:
        """The package half of the label, empty for a single-manifest name."""
        label = self.id.partition(":")[2]
        return label.rpartition(":")[0]

    @property
    def name(self) -> str:
        """The target half of the label, keeping any ``#0/3`` shard marker."""
        label = self.id.partition(":")[2]
        return label.rpartition(":")[2] or label

    @property
    def is_test(self) -> bool:
        return self.kind == TEST_KIND

    @property
    def passed(self) -> bool:
        return self.result in PASSING


@dataclass
class Report:
    """Everything a report needs from one build."""

    cases: list[Case] = field(default_factory=list)
    elapsed_ms: int = 0
    success: bool | None = None
    #: Number of actions the all-cached fast path reported, when it was taken.
    all_cached: int | None = None
    #: ``build_finished`` never arrived: the build was killed, or the file was
    #: read while it was still being written.
    truncated: bool = False

    @property
    def tests(self) -> list[Case]:
        """The test cases to report, which is not always the ones on the wire.

        A fully cached rerun reports one ``all_cached`` event and no per-action
        events at all. Passing that through as an empty list would produce a
        report saying "0 tests, 0 failures", which reads as a build that had
        nothing to check rather than one that was already green.
        """
        found = [case for case in self.cases if case.is_test]
        if found or self.all_cached is None:
            return found
        return [
            Case(
                id="test:all cached",
                desc="every action was already up to date",
                result="cached",
                duration_ms=self.elapsed_ms,
                detail=(
                    f"{_plural(self.all_cached, 'action')} already up to date, "
                    f"so frost took the all-cached fast path and reported one "
                    f"event instead of one per action. Nothing was rerun; the "
                    f"previous run's results still stand."
                ),
            )
        ]

    @property
    def build_actions(self) -> list[Case]:
        return [case for case in self.cases if not case.is_test]


def parse(lines: list) -> Report:
    """Read the stream. Raises ``SchemaMismatch`` or ``CorruptStream``.

    Takes the lines as a sequence rather than a file, because deciding whether
    a bad line is a truncation or a corruption needs to know whether anything
    follows it.

    Unknown ``event`` values are ignored rather than refused: the compatibility
    promise allows new ones, and a reader that stops at the first event it has
    not heard of would break on every addition.
    """
    report = Report()
    for index, text in enumerate(lines):
        number = index + 1
        text = text.strip()
        if not text:
            continue
        try:
            event = json.loads(text)
        except json.JSONDecodeError as error:
            # A half-written *last* line is a killed build, not a corrupt file.
            # Anything with content after it is genuinely broken, and guessing
            # would silently drop results.
            if not any(rest.strip() for rest in lines[number:]):
                report.truncated = True
                break
            raise CorruptStream(f"line {number} is not JSON: {error}") from error
        if not isinstance(event, dict):
            raise CorruptStream(f"line {number} is not a JSON object")
        _check_schema(event, number)
        _absorb(report, event)
    if report.success is None:
        report.truncated = True
    return report


def _check_schema(event: dict, number: int) -> None:
    schema = event.get("schema")
    if schema == SCHEMA:
        return
    found = "no schema field" if schema is None else repr(schema)
    raise SchemaMismatch(
        f"line {number}: this reader understands {SCHEMA!r}, the stream says "
        f"{found}. A schema bump means a field changed meaning or left, so "
        f"reading it as {SCHEMA!r} would report the wrong thing. Update "
        f"scripts/frost_junit.py, or produce the stream with the frost that "
        f"matches it."
    )


def _absorb(report: Report, event: dict) -> None:
    name = event.get("event")
    if name == "action_finished":
        report.cases.append(
            Case(
                id=str(event.get("id", "")),
                desc=str(event.get("desc", "")),
                result=str(event.get("result", "")),
                duration_ms=int(event.get("duration_ms") or 0),
                # Absent and null both mean "nothing went wrong".
                detail=str(event.get("detail") or "").rstrip(),
            )
        )
    elif name == "build_finished":
        report.elapsed_ms = int(event.get("elapsed_ms") or 0)
        report.success = bool(event.get("success"))
    elif name == "all_cached":
        report.all_cached = int(event.get("actions") or 0)
    # build_started, action_started and anything added later carry nothing a
    # finished report needs.


def clean(text: str) -> str:
    """Text that is safe to put in an XML document."""
    return ILLEGAL_XML_RE.sub("", ANSI_RE.sub("", text))


def _clip(text: str, limit: int) -> str:
    if len(text) <= limit:
        return text
    return text[:limit] + f"\n… {len(text) - limit} more characters"


def _message(detail: str) -> str:
    """The one line a collapsed viewer row shows.

    Clipped without a newline, unlike a body: a newline in an XML attribute is
    legal but arrives as a literal `&#10;` in every viewer that shows one.
    """
    first = detail.strip().splitlines()[0] if detail.strip() else ""
    if len(first) <= MESSAGE_CHARS:
        return first
    return first[:MESSAGE_CHARS] + "…"


def to_junit(report: Report, suite_name: str, all_actions: bool) -> str:
    """The report as JUnit XML."""
    root = ET.Element("testsuites", name=suite_name)
    total_time = f"{report.elapsed_ms / 1000:.3f}"
    root.set("time", total_time)

    _suite(root, "tests", report.tests)

    build = report.build_actions
    if not all_actions:
        # A passing compile is noise in a test report. A failing one is the
        # reason there are no test results.
        build = [case for case in build if not case.passed]
    if build:
        _suite(root, "build", build)

    if report.truncated:
        # Visible as a case rather than only on stderr: a CI job shows the
        # report, not the converter's log.
        _suite(
            root,
            "stream",
            [
                Case(
                    id="stream:truncated",
                    desc="the event stream has no build_finished event",
                    result="failed",
                    duration_ms=0,
                    detail=(
                        "The stream ends without a build_finished event, so "
                        "the build was killed or the file was read while it "
                        "was still being written. Results above are the ones "
                        "that made it to disk and may be incomplete."
                    ),
                )
            ],
        )

    body = ET.tostring(root, encoding="unicode")
    return f'<?xml version="1.0" encoding="UTF-8"?>\n{body}\n'


def _suite(root: ET.Element, name: str, cases: list[Case]) -> ET.Element:
    suite = ET.SubElement(root, "testsuite", name=name)
    failures = errors = skipped = 0
    duration = 0
    for case in cases:
        child = _case_element(suite, name, case)
        duration += case.duration_ms
        if child == "failure":
            failures += 1
        elif child == "error":
            errors += 1
        elif child == "skipped":
            skipped += 1
    suite.set("tests", str(len(cases)))
    suite.set("failures", str(failures))
    suite.set("errors", str(errors))
    suite.set("skipped", str(skipped))
    suite.set("time", f"{duration / 1000:.3f}")
    return suite


def _case_element(suite: ET.Element, suite_name: str, case: Case) -> str:
    """Append one testcase. Returns which child element it got, or ``""``."""
    testcase = ET.SubElement(
        suite,
        "testcase",
        name=clean(case.name),
        classname=clean(case.package or suite_name),
        time=f"{case.duration_ms / 1000:.3f}",
    )
    detail = clean(case.detail)
    if case.result == "failed":
        child = ET.SubElement(
            testcase, "failure", message=_message(detail) or "failed", type="failure"
        )
        child.text = _clip(detail, BODY_CHARS)
        return "failure"
    if case.result == "skipped":
        ET.SubElement(testcase, "skipped", message=_message(detail) or "skipped")
        return "skipped"
    if case.result in ("would_run", "may_run"):
        ET.SubElement(
            testcase,
            "skipped",
            message=_message(detail) or f"not run ({case.result})",
        )
        return "skipped"
    if case.result == "flaky":
        # Surefire's spelling for "it passed, but not on the first try". A
        # viewer that does not know the element ignores it and still sees a
        # pass; one that does shows why the pass is not free.
        child = ET.SubElement(
            testcase, "flakyFailure", message=_message(detail) or "flaky", type="flaky"
        )
        child.text = _clip(detail, BODY_CHARS)
        # Also in the element every viewer renders, prefixed so it is not a
        # word-for-word duplicate of the structured one above: a viewer that
        # has never heard of `flakyFailure` would otherwise show a flake as an
        # ordinary pass, which is the thing this was added to stop.
        _system_out(testcase, f"flaky: {detail}" if detail else "flaky")
        return "flaky"
    if case.result == "cached":
        _system_out(testcase, detail or "cache hit: not rerun")
        return ""
    if case.result == "executed":
        if detail:
            _system_out(testcase, detail)
        return ""
    # Not "pass, probably". The compatibility promise allows a result name to
    # be added, and a reader that guesses green would hide exactly the outcome
    # it was too old to understand.
    child = ET.SubElement(
        testcase,
        "error",
        message=f"unrecognised result {case.result!r}",
        type="unknown-result",
    )
    child.text = _clip(
        f"{case.id} finished with result {case.result!r}, which this converter "
        f"does not know. Reporting it as an error rather than a pass: update "
        f"scripts/frost_junit.py.\n{detail}",
        BODY_CHARS,
    )
    return "error"


def _system_out(testcase: ET.Element, text: str) -> None:
    element = ET.SubElement(testcase, "system-out")
    element.text = _clip(text, BODY_CHARS)


#: Row markers. Plain ASCII would work; these survive a narrow column and are
#: what a reader scans for.
MARKS = {
    "failed": "❌",
    "skipped": "⏭️",
    "flaky": "⚠️",
    "cached": "💾",
    "executed": "✅",
}

#: GitHub caps a step summary at 1 MiB and truncates without saying so.
SUMMARY_CHARS = 60_000


def to_markdown(report: Report, suite_name: str) -> str:
    """A step summary: the counts, then only the rows worth looking at."""
    tests = report.tests
    counts = _counts(tests)
    verdict = {True: "passed", False: "failed", None: "unknown"}[report.success]
    lines = [
        f"### {suite_name}: {_plural(len(tests), 'test')}, build {verdict}",
        "",
        " · ".join(
            [
                f"**{counts['executed'] + counts['cached'] + counts['flaky']} passed**",
                f"{counts['failed']} failed",
                f"{counts['flaky']} flaky",
                f"{counts['skipped']} skipped",
                f"{counts['cached']} cached",
                f"{report.elapsed_ms} ms",
            ]
        ),
        "",
    ]
    if report.all_cached is not None:
        lines += [
            f"Every one of {_plural(report.all_cached, 'action')} was already "
            "up to date; frost took the all-cached fast path, so there are no "
            "per-action events to report.",
            "",
        ]
    if report.truncated:
        lines += [
            "> **The stream is truncated** — no `build_finished` event. "
            "The results below are the ones that reached disk.",
            "",
        ]

    interesting = [case for case in report.cases if _is_interesting(case)]
    if interesting:
        lines += ["| | target | time | detail |", "|---|---|---|---|"]
        for case in interesting:
            mark = MARKS.get(case.result, "❓")
            detail = _message(clean(case.detail)).replace("|", "\\|")
            lines.append(
                f"| {mark} | `{case.id}` | {case.duration_ms} ms | {detail} |"
            )
        lines.append("")

    for case in interesting:
        if case.result == "failed" and case.detail:
            lines += [
                f"<details><summary><code>{case.id}</code></summary>",
                "",
                "```",
                _clip(clean(case.detail), 4000),
                "```",
                "",
                "</details>",
                "",
            ]

    silent = len(report.cases) - len(interesting)
    if silent:
        lines.append(f"_{silent} actions passed without comment._")
    return _clip("\n".join(lines) + "\n", SUMMARY_CHARS)


def _is_interesting(case: Case) -> bool:
    """Rows a reader would want: anything that is not a plain pass."""
    if case.result in ("executed", "cached"):
        return False
    return True


def _counts(cases: list[Case]) -> dict:
    counts = {name: 0 for name in ("executed", "cached", "flaky", "failed", "skipped")}
    for case in cases:
        counts[case.result] = counts.get(case.result, 0) + 1
    return counts


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(
        description="Convert a frost --build-event-json stream to JUnit XML.",
        epilog="Exit 0 when the report was written, 2 when the stream could "
        "not be read. A report full of failures is still exit 0.",
    )
    parser.add_argument(
        "events",
        nargs="?",
        default="-",
        help="ndjson file written by --build-event-json, or - for stdin",
    )
    parser.add_argument(
        "-o", "--output", default="-", help="JUnit XML destination, or - for stdout"
    )
    parser.add_argument(
        "--summary",
        help="append a Markdown summary here, for $GITHUB_STEP_SUMMARY; "
        "- writes it to stderr so it never mixes with the report on stdout",
    )
    parser.add_argument(
        "--suite-name", default="frost", help="name of the top-level testsuites"
    )
    parser.add_argument(
        "--all-actions",
        action="store_true",
        help="report every non-test action, not only the ones that did not pass",
    )
    args = parser.parse_args(argv)

    try:
        if args.events == "-":
            report = parse(sys.stdin.read().splitlines())
        else:
            with open(args.events, encoding="utf-8", errors="replace") as handle:
                report = parse(handle.read().splitlines())
    except OSError as error:
        print(f"frost_junit: {error}", file=sys.stderr)
        return 2
    except (SchemaMismatch, CorruptStream) as error:
        print(f"frost_junit: {error}", file=sys.stderr)
        return 2

    xml = to_junit(report, args.suite_name, args.all_actions)
    try:
        if args.output == "-":
            sys.stdout.write(xml)
        else:
            with open(args.output, "w", encoding="utf-8") as handle:
                handle.write(xml)
        if args.summary:
            markdown = to_markdown(report, args.suite_name)
            if args.summary == "-":
                sys.stderr.write(markdown)
            else:
                with open(args.summary, "a", encoding="utf-8") as handle:
                    handle.write(markdown)
    except OSError as error:
        print(f"frost_junit: {error}", file=sys.stderr)
        return 2

    if report.truncated:
        print(
            "frost_junit: the stream has no build_finished event; the report "
            "may be incomplete",
            file=sys.stderr,
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
