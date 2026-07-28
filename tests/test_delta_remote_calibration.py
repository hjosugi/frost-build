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

from calibrate_remote import (  # noqa: E402
    CANDIDATE_PLAN,
    CALIBRATION_SCHEMA,
    calibrate_corpus,
    build_calibration,
    plan_ms,
    transfer_ms,
)
from cost_model import REPORT_SCHEMA  # noqa: E402


def sample_report():
    return {
        "schema": REPORT_SCHEMA,
        "corpus": "fixture",
        "avg_kib": 512,
        "verify_failures": 0,
        "bytes": {
            "cdc+zstd": 1_000_000,
            "deltacdc/pos/zstd": 500_000,
        },
        "cpu_s_by_plan": {
            "cdc+zstd": {"total": 0.1},
            "deltacdc/pos/zstd": {"total": 0.2},
        },
    }


def sample_proof():
    return {
        "schema": "frost-reapi-poc-v1",
        "server": {
            "low_api_version": {"major": 2, "minor": 0, "patch": 0},
            "high_api_version": {"major": 2, "minor": 2, "patch": 0},
            "digest_functions": ["SHA256"],
            "execution_enabled": True,
        },
        "missing_blob_probe": {
            "status_code": 13,
            "message": "At least one blob is missing",
        },
        "execution": {"wall_ms": 1500.0},
        "action_cache": {"wall_ms": 20.0},
    }


class DeltaRemoteCalibrationTests(unittest.TestCase):
    def test_transfer_and_plan_arithmetic(self):
        self.assertEqual(transfer_ms(1_000_000, 8), 1_000.0)
        self.assertEqual(plan_ms(1_000_000, 0.1, 8, 20), 1_120.0)

    def test_candidate_wins_below_break_even(self):
        value = calibrate_corpus(sample_report(), [1.0, 100.0], 20.0)

        self.assertEqual(value["scenarios"][0]["winner"], CANDIDATE_PLAN)
        self.assertEqual(value["scenarios"][1]["winner"], "cdc+zstd")
        self.assertEqual(
            value["cost_model"]["break_even_bandwidth_mbit_s"], 40.0
        )

    def test_certificate_keeps_delta_opt_in(self):
        value = build_calibration([sample_report()], [10.0], sample_proof())

        self.assertEqual(value["schema"], CALIBRATION_SCHEMA)
        self.assertFalse(value["decision"]["enable_remote_deltacdc_by_default"])
        self.assertTrue(value["reapi_evidence"]["missing_blob_identified"])
        self.assertEqual(value["model"]["rpc_overhead_ms"], 20.0)

    def test_invalid_bandwidth_is_rejected(self):
        with self.assertRaisesRegex(ValueError, "positive"):
            transfer_ms(100, 0)


if __name__ == "__main__":
    unittest.main()
