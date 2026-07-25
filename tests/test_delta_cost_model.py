import sys
import unittest
from pathlib import Path


HARNESS = (
    Path(__file__).resolve().parents[1]
    / "build-cache-delta"
    / "pkg"
    / "harness"
)
sys.path.insert(0, str(HARNESS))

from cost_model import (  # noqa: E402
    REPORT_SCHEMA,
    break_even_bandwidth_bytes_per_s,
    comparison_from_report,
)


class DeltaCostModelTests(unittest.TestCase):
    def test_known_break_even_bandwidth(self):
        report = {
            "schema": REPORT_SCHEMA,
            "bytes": {
                "cdc+zstd": 1_000,
                "deltacdc/pos/zstd": 500,
            },
            "cpu_s_by_plan": {
                "cdc+zstd": {"total": 0.1},
                "deltacdc/pos/zstd": {"total": 0.2},
            },
        }

        comparison = comparison_from_report(report)

        self.assertEqual(comparison["bytes_saved"], 500)
        self.assertAlmostEqual(comparison["incremental_cpu_s"], 0.1)
        self.assertEqual(
            comparison["break_even_bandwidth_bytes_per_s"], 5_000.0)
        self.assertEqual(
            comparison["break_even_bandwidth_mbit_s"], 0.04)

    def test_no_byte_savings_has_no_break_even_bandwidth(self):
        self.assertIsNone(
            break_even_bandwidth_bytes_per_s(500, 500, 0.1, 0.2))
        self.assertIsNone(
            break_even_bandwidth_bytes_per_s(500, 600, 0.1, 0.2))

    def test_nonpositive_incremental_cpu_has_no_break_even_bandwidth(self):
        self.assertIsNone(
            break_even_bandwidth_bytes_per_s(1_000, 500, 0.2, 0.2))
        self.assertIsNone(
            break_even_bandwidth_bytes_per_s(1_000, 500, 0.2, 0.1))

    def test_legacy_aggregate_cpu_report_is_rejected(self):
        legacy = {
            "bytes_mb": {
                "cdc+zstd": 196.478,
                "deltacdc/pos/zstd": 93.237,
            },
            "cpu_s": {
                "zstd_chunks": 2.97,
                "delta_zstd": 594.1,
            },
        }

        with self.assertRaisesRegex(ValueError, "report schema"):
            comparison_from_report(legacy)


if __name__ == "__main__":
    unittest.main()
