"""Tests for the build-event-stream to JUnit converter.

The streams below are not invented. Each was produced by running the real
`frost` binary against a workspace built to reach that outcome, then pasted in
verbatim — so a change to what frost writes shows up here as a failing test
rather than as a converter that quietly reports the wrong thing.
"""

import io
import pathlib
import sys
import tempfile
import unittest
import xml.etree.ElementTree as ET

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent / "scripts"))

import frost_junit  # noqa: E402

# `frost test --keep-going` over a workspace with a failing genrule, a test
# depending on it, a passing test, a two-way sharded test and a test that
# passes on its second attempt. Every result name the stream can carry except
# `cached`, `would_run` and `may_run` appears here.
MIXED = [
    '{"actions":6,"critical_path_ms":60,"event":"build_started","jobs":4,"schema":"frost-build-events-v1","seq":0}',
    '{"desc":"GEN lib_build","event":"action_started","id":"genrule:lib_build","schema":"frost-build-events-v1","seq":1}',
    '{"cached":false,"desc":"TEST passes","detail":null,"duration_ms":3,"event":"action_finished","id":"test:passes","result":"executed","schema":"frost-build-events-v1","seq":5}',
    '{"cached":false,"desc":"GEN lib_build","detail":"command: /bin/sh -c \'exit 1\'\\nexit: code 1\\n","duration_ms":5,"event":"action_finished","id":"genrule:lib_build","result":"failed","schema":"frost-build-events-v1","seq":7}',
    '{"cached":false,"desc":"TEST depends_on_lib","detail":"upstream failed: genrule:lib_build","duration_ms":0,"event":"action_finished","id":"test:depends_on_lib","result":"skipped","schema":"frost-build-events-v1","seq":9}',
    '{"cached":false,"desc":"TEST sharded (shard 1/2)","detail":null,"duration_ms":5,"event":"action_finished","id":"test:sharded#0/2","result":"executed","schema":"frost-build-events-v1","seq":10}',
    '{"cached":false,"desc":"TEST sharded (shard 2/2)","detail":null,"duration_ms":1,"event":"action_finished","id":"test:sharded#1/2","result":"executed","schema":"frost-build-events-v1","seq":11}',
    '{"cached":false,"desc":"TEST flaky","detail":"passed on attempt 2","duration_ms":7,"event":"action_finished","id":"test:flaky","result":"flaky","schema":"frost-build-events-v1","seq":12}',
    '{"elapsed_ms":8,"event":"build_finished","schema":"frost-build-events-v1","seq":13,"success":false}',
]

# The same workspace, rerun with nothing changed.
ALL_CACHED = [
    '{"actions":1,"critical_path_ms":0,"event":"build_started","jobs":1,"schema":"frost-build-events-v1","seq":0}',
    '{"actions":1,"event":"all_cached","schema":"frost-build-events-v1","seq":1}',
    '{"elapsed_ms":0,"event":"build_finished","schema":"frost-build-events-v1","seq":2,"success":true}',
]

CLEAN = [
    '{"actions":1,"critical_path_ms":50,"event":"build_started","jobs":1,"schema":"frost-build-events-v1","seq":0}',
    '{"cached":false,"desc":"TEST passes","detail":null,"duration_ms":2,"event":"action_finished","id":"test:passes","result":"executed","schema":"frost-build-events-v1","seq":2}',
    '{"elapsed_ms":4,"event":"build_finished","schema":"frost-build-events-v1","seq":3,"success":true}',
]


def convert(lines, all_actions=False):
    """The XML for a stream, already parsed."""
    report = frost_junit.parse(lines)
    return ET.fromstring(frost_junit.to_junit(report, "frost", all_actions))


def suite(root, name):
    found = root.find(f"./testsuite[@name='{name}']")
    assert found is not None, f"no {name} suite in {ET.tostring(root)}"
    return found


def case(root, suite_name, case_name):
    found = suite(root, suite_name).find(f"./testcase[@name='{case_name}']")
    assert found is not None, f"no {case_name} in {suite_name}"
    return found


class ResultMappingTest(unittest.TestCase):
    def test_each_result_reaches_the_element_a_viewer_reads(self) -> None:
        root = convert(MIXED)
        tests = suite(root, "tests")
        self.assertEqual(tests.get("tests"), "5")
        self.assertEqual(tests.get("skipped"), "1")
        # The failure is a *build* action, so the test suite is not failing.
        self.assertEqual(tests.get("failures"), "0")

        self.assertIsNone(case(root, "tests", "passes").find("failure"))
        self.assertIsNotNone(case(root, "tests", "depends_on_lib").find("skipped"))
        self.assertEqual(
            case(root, "tests", "depends_on_lib").find("skipped").get("message"),
            "upstream failed: genrule:lib_build",
        )

    def test_a_shard_keeps_its_own_identity(self) -> None:
        # Two testcases with the same name would be deduplicated by most
        # viewers, hiding half the work and half the possible failures.
        root = convert(MIXED)
        names = [
            element.get("name") for element in suite(root, "tests").iter("testcase")
        ]
        self.assertIn("sharded#0/2", names)
        self.assertIn("sharded#1/2", names)

    def test_a_flake_is_visible_to_a_viewer_that_knows_the_element_and_to_one_that_does_not(
        self,
    ) -> None:
        flaky = case(root := convert(MIXED), "tests", "flaky")
        self.assertIsNotNone(flaky.find("flakyFailure"))
        self.assertEqual(flaky.find("flakyFailure").get("message"), "passed on attempt 2")
        # …and in the element every viewer renders, so a consumer that has
        # never heard of `flakyFailure` still sees that the pass was retried.
        self.assertIn("flaky", flaky.find("system-out").text)
        # A flake is still a pass: it must not be counted as a failure.
        self.assertEqual(suite(root, "tests").get("failures"), "0")

    def test_a_failed_test_becomes_a_failure_carrying_its_output(self) -> None:
        lines = [
            line.replace('"id":"genrule:lib_build"', '"id":"test:lib_test"')
            for line in MIXED
        ]
        root = convert(lines)
        failure = case(root, "tests", "lib_test").find("failure")
        self.assertIsNotNone(failure)
        self.assertIn("exit: code 1", failure.text)
        self.assertEqual(suite(root, "tests").get("failures"), "1")


class FalseGreenTest(unittest.TestCase):
    """The two ways a naive converter reports a green build that was not."""

    def test_a_build_that_broke_before_any_test_is_not_reported_as_clean(self) -> None:
        root = convert(MIXED)
        failure = case(root, "build", "lib_build").find("failure")
        self.assertIsNotNone(failure, "a failed non-test action must be reported")
        self.assertEqual(suite(root, "build").get("failures"), "1")

    def test_passing_build_actions_stay_out_of_the_way_unless_asked_for(self) -> None:
        lines = [line.replace('"failed"', '"executed"') for line in MIXED]
        root = convert(lines)
        self.assertIsNone(
            root.find("./testsuite[@name='build']"),
            "a report of a green build should not list every compile",
        )
        root = convert(lines, all_actions=True)
        self.assertEqual(suite(root, "build").get("tests"), "1")

    def test_a_fully_cached_rerun_says_so_rather_than_reporting_nothing(self) -> None:
        root = convert(ALL_CACHED)
        tests = suite(root, "tests")
        # Zero testcases and zero failures is what a build that ran nothing
        # looks like too, and a CI report cannot tell the two apart.
        self.assertEqual(tests.get("tests"), "1")
        self.assertEqual(tests.get("failures"), "0")
        text = tests.find("./testcase/system-out").text
        self.assertIn("all-cached fast path", text)

    def test_a_truncated_stream_is_reported_as_a_failure(self) -> None:
        root = convert(CLEAN[:2])
        self.assertEqual(suite(root, "stream").get("failures"), "1")
        # And the results that did arrive are still reported.
        self.assertEqual(suite(root, "tests").get("tests"), "1")

    def test_a_complete_stream_grows_no_stream_suite(self) -> None:
        self.assertIsNone(convert(CLEAN).find("./testsuite[@name='stream']"))


class SchemaTest(unittest.TestCase):
    def test_a_different_schema_is_refused_rather_than_guessed_at(self) -> None:
        lines = [line.replace("-v1", "-v2") for line in CLEAN]
        with self.assertRaises(frost_junit.SchemaMismatch) as raised:
            frost_junit.parse(lines)
        message = str(raised.exception)
        self.assertIn("frost-build-events-v2", message)
        self.assertIn("frost-build-events-v1", message)

    def test_a_line_without_a_schema_is_not_assumed_to_be_this_one(self) -> None:
        lines = [CLEAN[0].replace(',"schema":"frost-build-events-v1"', "")] + CLEAN[1:]
        with self.assertRaises(frost_junit.SchemaMismatch) as raised:
            frost_junit.parse(lines)
        self.assertIn("no schema field", str(raised.exception))

    def test_an_added_event_or_field_does_not_break_the_reader(self) -> None:
        # The compatibility promise in docs/28 is that fields and events are
        # added. A reader that refused anything it had not heard of would turn
        # every addition into a broken CI job.
        lines = list(CLEAN)
        lines.insert(
            1,
            '{"event":"target_analysed","label":"//x:y","schema":"frost-build-events-v1","seq":9}',
        )
        lines[2] = lines[2].replace(
            '"result":"executed"', '"result":"executed","worker":3'
        )
        report = frost_junit.parse(lines)
        self.assertEqual(len(report.tests), 1)
        self.assertTrue(report.success)

    def test_an_unknown_result_is_an_error_rather_than_a_pass(self) -> None:
        # The one addition a reader cannot absorb silently. Reporting an
        # outcome it does not understand as green would hide precisely the
        # case it was too old to read.
        lines = [line.replace('"executed"', '"timed_out"') for line in CLEAN]
        root = convert(lines)
        error = case(root, "tests", "passes").find("error")
        self.assertIsNotNone(error)
        self.assertIn("timed_out", error.get("message"))
        self.assertEqual(suite(root, "tests").get("errors"), "1")


class CorruptionTest(unittest.TestCase):
    def test_a_broken_line_in_the_middle_is_refused(self) -> None:
        lines = [CLEAN[0], "{not json", CLEAN[1], CLEAN[2]]
        with self.assertRaises(frost_junit.CorruptStream) as raised:
            frost_junit.parse(lines)
        self.assertIn("line 2", str(raised.exception))

    def test_a_half_written_last_line_is_a_killed_build_not_a_corruption(self) -> None:
        report = frost_junit.parse([CLEAN[0], CLEAN[1], '{"event":"build_fin'])
        self.assertTrue(report.truncated)
        self.assertEqual(len(report.tests), 1)

    def test_blank_lines_are_not_content(self) -> None:
        report = frost_junit.parse(["", CLEAN[0], "", CLEAN[1], CLEAN[2], ""])
        self.assertFalse(report.truncated)
        self.assertEqual(len(report.tests), 1)


class XmlValidityTest(unittest.TestCase):
    def test_control_characters_and_colour_codes_do_not_produce_invalid_xml(
        self,
    ) -> None:
        # Real compiler output reaches `detail` with both in it. XML 1.0
        # forbids most C0 controls outright, and a document containing one is
        # rejected by strict parsers — a failure the CI job blames on us.
        detail = "\\u001b[0;31merror:\\u001b[0m bad\\u0000 thing\\u000c here"
        lines = [
            CLEAN[0],
            CLEAN[1].replace('"detail":null', f'"detail":"{detail}"'),
            CLEAN[2],
        ]
        xml = frost_junit.to_junit(frost_junit.parse(lines), "frost", False)
        root = ET.fromstring(xml)  # would raise on an invalid document
        text = root.find(".//system-out").text
        self.assertIn("error: bad thing here", text)
        self.assertNotIn("\x1b", text)
        self.assertNotIn("\x00", text)
        self.assertNotIn("[0;31m", text)

    def test_a_label_with_a_package_splits_into_classname_and_name(self) -> None:
        lines = [
            CLEAN[0],
            CLEAN[1].replace('"id":"test:passes"', '"id":"test:core/text:tokenize"'),
            CLEAN[2],
        ]
        element = convert(lines).find(".//testcase")
        self.assertEqual(element.get("classname"), "core/text")
        self.assertEqual(element.get("name"), "tokenize")

    def test_a_long_detail_is_clipped_in_the_attribute_and_the_body(self) -> None:
        detail = "x" * 40_000
        lines = [
            CLEAN[0],
            CLEAN[1]
            .replace('"detail":null', f'"detail":"{detail}"')
            .replace('"executed"', '"failed"'),
            CLEAN[2],
        ]
        failure = convert(lines).find(".//failure")
        self.assertLessEqual(len(failure.get("message")), 260)
        self.assertLess(len(failure.text), 9000)
        self.assertIn("more characters", failure.text)


class MarkdownTest(unittest.TestCase):
    def test_the_summary_names_what_went_wrong_and_stays_quiet_about_the_rest(
        self,
    ) -> None:
        text = frost_junit.to_markdown(frost_junit.parse(MIXED), "frost")
        self.assertIn("genrule:lib_build", text)
        self.assertIn("test:depends_on_lib", text)
        self.assertIn("test:flaky", text)
        # Three actions passed cleanly; a table listing them helps nobody.
        self.assertNotIn("test:sharded#0/2", text)
        self.assertIn("3 actions passed without comment", text)

    def test_the_summary_of_a_clean_build_has_no_table(self) -> None:
        text = frost_junit.to_markdown(frost_junit.parse(CLEAN), "frost")
        self.assertIn("1 test", text)
        self.assertIn("build passed", text)
        self.assertNotIn("|---|", text)

    def test_a_pipe_in_a_detail_cannot_break_the_table(self) -> None:
        lines = [
            CLEAN[0],
            CLEAN[1]
            .replace('"detail":null', '"detail":"got a|b, wanted c"')
            .replace('"executed"', '"failed"'),
            CLEAN[2],
        ]
        text = frost_junit.to_markdown(frost_junit.parse(lines), "frost")
        row = next(line for line in text.splitlines() if "test:passes" in line)
        self.assertIn(r"a\|b", row)
        self.assertEqual(row.count("|"), 6, row)


class CommandLineTest(unittest.TestCase):
    def run_main(self, argv):
        out, err = io.StringIO(), io.StringIO()
        stdout, stderr = sys.stdout, sys.stderr
        sys.stdout, sys.stderr = out, err
        try:
            code = frost_junit.main(argv)
        finally:
            sys.stdout, sys.stderr = stdout, stderr
        return code, out.getvalue(), err.getvalue()

    def test_a_report_of_failures_still_exits_zero(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            events = pathlib.Path(directory) / "events.ndjson"
            events.write_text("\n".join(MIXED), encoding="utf-8")
            report = pathlib.Path(directory) / "junit.xml"
            summary = pathlib.Path(directory) / "summary.md"
            code, _, _ = self.run_main(
                [str(events), "-o", str(report), "--summary", str(summary)]
            )
        # Converting is the work. The step that ran the build already failed
        # the job; a converter that also fails it says nothing new and stops
        # the report from being uploaded.
        self.assertEqual(code, 0)

    def test_a_schema_it_cannot_read_is_an_invocation_error(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            events = pathlib.Path(directory) / "events.ndjson"
            events.write_text("\n".join(line.replace("-v1", "-v9") for line in CLEAN))
            code, _, err = self.run_main([str(events), "-o", "-"])
        # 2, not 1: docs/28's split is "your code" versus "your invocation",
        # and a stream this reader cannot read is the latter.
        self.assertEqual(code, 2)
        self.assertIn("frost-build-events-v9", err)

    def test_a_missing_file_is_reported_rather_than_traced(self) -> None:
        code, _, err = self.run_main(["/nonexistent/events.ndjson"])
        self.assertEqual(code, 2)
        self.assertIn("frost_junit:", err)

    def test_the_summary_appends_so_a_step_summary_keeps_what_was_there(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            events = pathlib.Path(directory) / "events.ndjson"
            events.write_text("\n".join(CLEAN), encoding="utf-8")
            summary = pathlib.Path(directory) / "summary.md"
            summary.write_text("earlier step\n", encoding="utf-8")
            code, _, _ = self.run_main(
                [str(events), "-o", "-", "--summary", str(summary)]
            )
            text = summary.read_text(encoding="utf-8")
        self.assertEqual(code, 0)
        self.assertTrue(text.startswith("earlier step\n"), text[:40])
        self.assertIn("build passed", text)


if __name__ == "__main__":
    unittest.main()
