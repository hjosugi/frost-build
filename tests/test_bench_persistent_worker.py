"""Tests for the #145 persistent-worker harness and its checked-in report.

The harness itself needs a JDK and two TypeScript compilers, so CI does not
rerun the measurement. What it can check without them is everything the
measurement's validity rests on: that the fixtures are deterministic, that the
edit loop really changes content without changing size (the stat-keyed hazard
depends on it), that the statistics are what they claim, and that the
checked-in report still says what docs/31 quotes from it.
"""

import json
import pathlib
import sys
import tempfile
import unittest

ROOT = pathlib.Path(__file__).resolve().parent.parent
sys.path.insert(0, str(ROOT / "scripts"))

import bench_persistent_worker as harness  # noqa: E402

BASELINE = ROOT / "bench" / "baselines" / "2026-09-24-issue-145-persistent-worker.json"


class FixtureTest(unittest.TestCase):
    def test_generators_are_deterministic(self):
        for generate in (harness.java_sources, harness.typescript_sources):
            with self.subTest(generator=generate.__name__):
                self.assertEqual(generate(12, 0), generate(12, 0))
                self.assertEqual(generate(12, 1), generate(12, 3))

    def test_the_edit_touches_only_the_leaf_and_keeps_its_size(self):
        for generate, leaf in (
            (harness.java_sources, "src/bench/Unit000.java"),
            (harness.typescript_sources, "src/m000.ts"),
        ):
            with self.subTest(generator=generate.__name__):
                first, second = generate(8, 0), generate(8, 1)
                changed = sorted(path for path in first if first[path] != second[path])
                self.assertEqual(changed, [leaf])
                self.assertEqual(len(first[leaf].encode()), len(second[leaf].encode()))

    def test_every_unit_depends_on_the_previous_one(self):
        java = harness.java_sources(5, 0)
        for index in range(1, 5):
            self.assertIn(
                f"{harness.java_unit_name(index - 1)}.fold(x)",
                java[f"src/bench/{harness.java_unit_name(index)}.java"],
            )
        typescript = harness.typescript_sources(5, 0)
        for index in range(1, 5):
            self.assertIn(
                f'from "./{harness.ts_module_name(index - 1)}.js"',
                typescript[f"src/{harness.ts_module_name(index)}.ts"],
            )

    def test_a_chain_needs_two_units(self):
        with self.assertRaises(ValueError):
            harness.java_sources(1, 0)
        with self.assertRaises(ValueError):
            harness.typescript_sources(1, 0)

    def test_write_tree_leaves_unchanged_files_alone(self):
        with tempfile.TemporaryDirectory() as name:
            root = pathlib.Path(name)
            harness.write_tree(root, harness.typescript_sources(4, 0))
            untouched = root / "src" / "m003.ts"
            before = untouched.stat().st_mtime_ns
            harness.write_tree(root, harness.typescript_sources(4, 1))
            self.assertEqual(untouched.stat().st_mtime_ns, before)
            self.assertIn("LEAF = 2", (root / "src" / "m000.ts").read_text())

    def test_tree_digest_sees_paths_and_bytes(self):
        with tempfile.TemporaryDirectory() as name:
            root = pathlib.Path(name)
            (root / "a").write_bytes(b"x")
            first = harness.tree_digest(root)
            (root / "a").rename(root / "b")
            self.assertNotEqual(harness.tree_digest(root), first)
            (root / "b").rename(root / "a")
            self.assertEqual(harness.tree_digest(root), first)
            (root / "a").write_bytes(b"y")
            self.assertNotEqual(harness.tree_digest(root), first)


class StatisticsTest(unittest.TestCase):
    def test_summary(self):
        summary = harness.summarize([5.0, 1.0, 3.0, 2.0, 4.0])
        self.assertEqual(summary["median"], 3.0)
        self.assertEqual(summary["min"], 1.0)
        self.assertEqual(summary["max"], 5.0)
        self.assertEqual(summary["mad"], 1.0)
        self.assertEqual(summary["n"], 5)
        self.assertEqual(summary["samples"], [5.0, 1.0, 3.0, 2.0, 4.0])
        self.assertAlmostEqual(summary["p10"], 1.4)
        self.assertAlmostEqual(summary["p90"], 4.6)

    def test_summary_rejects_nothing(self):
        with self.assertRaises(ValueError):
            harness.summarize([])

    def test_rotation_puts_every_scenario_first_equally(self):
        names = ["cold", "fresh", "reuse"]
        firsts = [harness.rotation(names, i)[0] for i in range(9)]
        self.assertEqual(sorted(firsts), sorted(names * 3))
        for i in range(9):
            self.assertEqual(sorted(harness.rotation(names, i)), sorted(names))

    def test_failure_modes(self):
        self.assertEqual(harness.failure_mode({"matches_cold": True, "exit": 0}, 0), "correct")
        self.assertEqual(harness.failure_mode({"matches_cold": False, "exit": 0}, 0), "silent_wrong_output")
        self.assertEqual(harness.failure_mode({"matches_cold": False, "exit": 1}, 0), "spurious_failure")
        self.assertEqual(harness.failure_mode({"matches_cold": False, "exit": 0}, 1), "spurious_success")
        hazards = harness.classify_hazards(
            [
                {
                    "cold_exit": 0,
                    "workers": {
                        "a": {"matches_cold": False, "exit": 0},
                        "b": {"matches_cold": False, "exit": 1},
                    },
                }
            ]
        )
        self.assertTrue(hazards[0]["workers"]["a"]["detected_by_cold_rerun"])
        self.assertFalse(hazards[0]["workers"]["b"]["detected_by_cold_rerun"])


class CheckedInReportTest(unittest.TestCase):
    """docs/31 quotes this report; these are the facts the memo relies on."""

    @classmethod
    def setUpClass(cls):
        cls.report = json.loads(BASELINE.read_text(encoding="utf-8"))

    def test_schema_and_provenance(self):
        report = self.report
        self.assertEqual(report["schema"], harness.SCHEMA)
        for field in ("platform", "cpu_count", "load_avg_before", "load_avg_after"):
            self.assertIn(field, report["host"])
        for field in ("java", "javac", "node", "typescript_js", "typescript_native"):
            self.assertTrue(report["tools"][field])
        self.assertIn("scripts/bench_persistent_worker.py", report["reproduce"])

    def test_raw_samples_back_every_median(self):
        iterations = self.report["fixture"]["iterations"]
        warmup = self.report["fixture"]["warmup_requests"]
        for toolchain, workers in (
            ("javac", ("worker_fresh_context", "worker_shared_file_manager")),
            ("tsc", ("worker_fresh_program", "worker_reuse_program")),
        ):
            section = self.report[toolchain]
            cold = section["cold_compile_ms"]
            self.assertEqual(len(cold["samples"]), iterations)
            # Samples are stored rounded to microseconds, so a median of two
            # of them can differ from the stored median in the last digit.
            self.assertAlmostEqual(cold["median"], harness.summarize(cold["samples"])["median"], delta=0.002)
            for worker in workers:
                with self.subTest(toolchain=toolchain, worker=worker):
                    data = section[worker]
                    self.assertEqual(len(data["warmup_curve_roundtrip_ms"]), warmup)
                    steady = data["steady_roundtrip_ms"]
                    self.assertEqual(len(steady["samples"]), iterations - warmup)
                    self.assertAlmostEqual(
                        steady["median"], harness.summarize(steady["samples"])["median"], delta=0.002
                    )
                    self.assertEqual(len(data["steady_cpu_ms"]["samples"]), iterations - warmup)
                    self.assertGreater(data["start_to_ready_ms"], 0)
            for scenario in ("cold", workers[0]):
                floor = section["per_action_floor"][scenario]
                self.assertEqual(len(floor["cpu_ms"]["samples"]), iterations)

    def test_workers_produced_the_cold_bytes(self):
        # A speedup measured on different bytes would not be a speedup.
        self.assertTrue(self.report["javac"]["outputs_identical_to_cold"])
        self.assertTrue(self.report["tsc"]["outputs_identical_to_cold"])
        self.assertEqual(self.report["tsc"]["native_reference"]["distinct_output_digests"], 2)

    def test_hazard_outcomes_quoted_by_the_memo(self):
        hazards = {hazard["id"]: hazard for hazard in self.report["hazards"]}
        stat = hazards["tsc-same-stat-edit"]["workers"]
        self.assertEqual(stat["worker_reuse_content_key"]["failure_mode"], "correct")
        self.assertEqual(stat["worker_reuse_mtime_key"]["failure_mode"], "silent_wrong_output")
        self.assertTrue(stat["worker_reuse_mtime_key"]["detected_by_cold_rerun"])
        # Frost publishes by atomic rename, so the rename rows are the ones
        # that describe a worker behind Frost's output publication.
        shared = "worker_shared_file_manager"
        api = hazards["javac-classpath-api-rename"]["workers"][shared]
        self.assertEqual(api["failure_mode"], "spurious_failure")
        constant = hazards["javac-classpath-constant-rename"]["workers"][shared]
        self.assertEqual(constant["failure_mode"], "silent_wrong_output")
        self.assertTrue(constant["detected_by_cold_rerun"])
        for edit in ("api", "constant"):
            for publication in ("rename", "in_place"):
                fresh = hazards[f"javac-classpath-{edit}-{publication}"]["workers"]["worker_fresh_context"]
                self.assertEqual(fresh["failure_mode"], "correct")


if __name__ == "__main__":
    unittest.main()
