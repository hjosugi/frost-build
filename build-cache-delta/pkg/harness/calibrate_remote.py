#!/usr/bin/env python3
"""Calibrate DeltaCDC transfer savings against measured CPU and REAPI overhead.

This model is intentionally simple and reproducible:

    total_ms = measured_cpu_s * 1000
             + bytes * 8 / bandwidth_mbit_s / 1000
             + rpc_overhead_ms

The same observed RPC constant is applied to both plans, so it does not move
their break-even point. It makes the absolute scenarios honest without
pretending that the current REAPI 2.2 server implements a delta protocol.
"""

from __future__ import annotations

import argparse
import json
from pathlib import Path
from typing import Any

from cost_model import REPORT_SCHEMA, comparison_from_report


CALIBRATION_SCHEMA = "frost-deltacdc-remote-calibration-v1"
BASELINE_PLAN = "cdc+zstd"
CANDIDATE_PLAN = "deltacdc/pos/zstd"


def transfer_ms(byte_count: int, bandwidth_mbit_s: float) -> float:
    if not isinstance(byte_count, int):
        raise TypeError("transfer byte count must be an integer")
    if byte_count < 0:
        raise ValueError("transfer byte count must be non-negative")
    if bandwidth_mbit_s <= 0:
        raise ValueError("bandwidth must be positive")
    return byte_count * 8 / (bandwidth_mbit_s * 1_000)


def plan_ms(
    byte_count: int,
    cpu_s: float,
    bandwidth_mbit_s: float,
    rpc_overhead_ms: float,
) -> float:
    if cpu_s < 0 or rpc_overhead_ms < 0:
        raise ValueError("CPU and RPC measurements must be non-negative")
    return cpu_s * 1_000 + transfer_ms(byte_count, bandwidth_mbit_s) + rpc_overhead_ms


def calibrate_corpus(
    report: dict[str, Any],
    bandwidths_mbit_s: list[float],
    rpc_overhead_ms: float,
) -> dict[str, Any]:
    if report.get("schema") != REPORT_SCHEMA:
        raise ValueError(f"expected {REPORT_SCHEMA!r} report")
    if not bandwidths_mbit_s:
        raise ValueError("at least one bandwidth is required")

    comparison = comparison_from_report(report, BASELINE_PLAN, CANDIDATE_PLAN)
    byte_counts = report["bytes"]
    cpu_by_plan = report["cpu_s_by_plan"]
    baseline_bytes = byte_counts[BASELINE_PLAN]
    candidate_bytes = byte_counts[CANDIDATE_PLAN]
    baseline_cpu = cpu_by_plan[BASELINE_PLAN]["total"]
    candidate_cpu = cpu_by_plan[CANDIDATE_PLAN]["total"]

    scenarios = []
    for bandwidth in bandwidths_mbit_s:
        baseline_ms = plan_ms(
            baseline_bytes, baseline_cpu, bandwidth, rpc_overhead_ms
        )
        candidate_ms = plan_ms(
            candidate_bytes, candidate_cpu, bandwidth, rpc_overhead_ms
        )
        scenarios.append(
            {
                "bandwidth_mbit_s": bandwidth,
                "baseline_ms": round(baseline_ms, 3),
                "candidate_ms": round(candidate_ms, 3),
                "winner": (
                    CANDIDATE_PLAN if candidate_ms < baseline_ms else BASELINE_PLAN
                ),
                "candidate_delta_ms": round(candidate_ms - baseline_ms, 3),
            }
        )

    return {
        "corpus": report.get("corpus", "unknown"),
        "avg_kib": report.get("avg_kib"),
        "verify_failures": report.get("verify_failures"),
        "baseline": {
            "plan": BASELINE_PLAN,
            "bytes": baseline_bytes,
            "cpu_s": baseline_cpu,
        },
        "candidate": {
            "plan": CANDIDATE_PLAN,
            "bytes": candidate_bytes,
            "cpu_s": candidate_cpu,
        },
        "cost_model": comparison,
        "scenarios": scenarios,
    }


def _load_json(path: Path) -> dict[str, Any]:
    with path.open(encoding="utf-8") as handle:
        value = json.load(handle)
    if not isinstance(value, dict):
        raise ValueError(f"{path}: expected a JSON object")
    return value


def build_calibration(
    reports: list[dict[str, Any]],
    bandwidths_mbit_s: list[float],
    reapi_proof: dict[str, Any],
) -> dict[str, Any]:
    proof = reapi_proof.get("proof", reapi_proof)
    cache_hit_ms = proof["action_cache"]["wall_ms"]
    execution_ms = proof["execution"]["wall_ms"]
    server = proof["server"]
    missing_probe = proof["missing_blob_probe"]
    return {
        "schema": CALIBRATION_SCHEMA,
        "model": {
            "formula": (
                "cpu_s * 1000 + bytes * 8 / (bandwidth_mbit_s * 1000) "
                "+ rpc_overhead_ms"
            ),
            "rpc_overhead_ms": cache_hit_ms,
            "bandwidths_mbit_s": bandwidths_mbit_s,
            "limitation": (
                "CPU phases were measured locally; REAPI overhead is an observed "
                "constant. The server does not implement SplitBlob/SpliceBlob."
            ),
        },
        "reapi_evidence": {
            "low_api_version": server["low_api_version"],
            "high_api_version": server["high_api_version"],
            "digest_functions": server["digest_functions"],
            "execution_enabled": server["execution_enabled"],
            "observed_cache_hit_ms": cache_hit_ms,
            "observed_execute_ms": execution_ms,
            "missing_blob_status_code": missing_probe.get("status_code"),
            "missing_blob_identified": (
                "missing" in str(missing_probe.get("message", "")).casefold()
            ),
        },
        "corpora": [
            calibrate_corpus(report, bandwidths_mbit_s, cache_hit_ms)
            for report in reports
        ],
        "decision": {
            "enable_remote_deltacdc_by_default": False,
            "status": "defer",
            "reason": (
                "The candidate only wins below the measured corpus-specific "
                "break-even bandwidths, consumes more CPU, and lacks negotiated "
                "wire-protocol support on the tested REAPI 2.2 server."
            ),
            "revisit_when": [
                "encoding CPU is materially reduced or offloaded",
                "the deployed protocol negotiates chunk delta operations",
                "production traces show bandwidth below the measured break-even",
            ],
        },
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("reports", nargs="+", type=Path)
    parser.add_argument("--reapi-proof", required=True, type=Path)
    parser.add_argument(
        "--bandwidth",
        action="append",
        type=float,
        dest="bandwidths",
        help="scenario bandwidth in Mbit/s; repeat (default: 1,10,100,1000)",
    )
    parser.add_argument("--out", type=Path)
    args = parser.parse_args()

    bandwidths = args.bandwidths or [1.0, 10.0, 100.0, 1000.0]
    value = build_calibration(
        [_load_json(path) for path in args.reports],
        bandwidths,
        _load_json(args.reapi_proof),
    )
    rendered = json.dumps(value, indent=2, sort_keys=True) + "\n"
    if args.out:
        args.out.parent.mkdir(parents=True, exist_ok=True)
        args.out.write_text(rendered, encoding="utf-8")
    else:
        print(rendered, end="")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
