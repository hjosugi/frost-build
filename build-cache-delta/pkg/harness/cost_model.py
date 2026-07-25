"""Pure helpers for DeltaCDC CPU/bandwidth calibration reports.

This module deliberately has no third-party dependencies so the arithmetic and
report-schema checks can run in the repository's normal Python CI job.
"""

from __future__ import annotations

from typing import Any


REPORT_SCHEMA = "frost-deltacdc-v2"


def break_even_bandwidth_bytes_per_s(
        full_bytes: int,
        delta_bytes: int,
        full_cpu_s: float,
        delta_cpu_s: float,
) -> float | None:
    """Return the bandwidth where byte savings equal incremental CPU cost.

    Below this bandwidth the smaller transfer wins under the simple
    ``bytes / bandwidth + cpu`` model. ``None`` means that there is no finite,
    positive crossing to report: either the candidate saves no bytes or it
    consumes no additional CPU.
    """
    _validate_inputs(full_bytes, delta_bytes, full_cpu_s, delta_cpu_s)
    bytes_saved = full_bytes - delta_bytes
    incremental_cpu_s = delta_cpu_s - full_cpu_s
    if bytes_saved <= 0 or incremental_cpu_s <= 0:
        return None
    return bytes_saved / incremental_cpu_s


def compare_transfer_plans(
        baseline_plan: str,
        candidate_plan: str,
        full_bytes: int,
        delta_bytes: int,
        full_cpu_s: float,
        delta_cpu_s: float,
) -> dict[str, Any]:
    """Build the machine-readable comparison embedded in a v2 report."""
    bandwidth = break_even_bandwidth_bytes_per_s(
        full_bytes, delta_bytes, full_cpu_s, delta_cpu_s)
    return {
        "baseline_plan": baseline_plan,
        "candidate_plan": candidate_plan,
        "bytes_saved": full_bytes - delta_bytes,
        "incremental_cpu_s": round(delta_cpu_s - full_cpu_s, 6),
        "break_even_bandwidth_bytes_per_s": (
            round(bandwidth, 3) if bandwidth is not None else None
        ),
        "break_even_bandwidth_mbit_s": (
            round(bandwidth * 8 / 1_000_000, 6)
            if bandwidth is not None else None
        ),
    }


def comparison_from_report(
        report: dict[str, Any],
        baseline_plan: str = "cdc+zstd",
        candidate_plan: str = "deltacdc/pos/zstd",
) -> dict[str, Any]:
    """Read exact bytes and per-plan CPU totals from a v2 report.

    Aggregate-only legacy reports are rejected instead of silently deriving a
    misleading break-even threshold from CPU that combines several selectors.
    """
    if report.get("schema") != REPORT_SCHEMA:
        raise ValueError(
            f"cost calibration requires report schema {REPORT_SCHEMA!r}")

    byte_counts = report.get("bytes")
    cpu_by_plan = report.get("cpu_s_by_plan")
    if not isinstance(byte_counts, dict) or not isinstance(cpu_by_plan, dict):
        raise ValueError(
            "cost calibration requires exact bytes and per-plan CPU totals")

    try:
        full_bytes = byte_counts[baseline_plan]
        delta_bytes = byte_counts[candidate_plan]
        full_cpu_s = cpu_by_plan[baseline_plan]["total"]
        delta_cpu_s = cpu_by_plan[candidate_plan]["total"]
    except (KeyError, TypeError) as error:
        raise ValueError(
            f"missing calibration data for {baseline_plan!r} or "
            f"{candidate_plan!r}") from error

    return compare_transfer_plans(
        baseline_plan,
        candidate_plan,
        full_bytes,
        delta_bytes,
        full_cpu_s,
        delta_cpu_s,
    )


def _validate_inputs(
        full_bytes: int,
        delta_bytes: int,
        full_cpu_s: float,
        delta_cpu_s: float,
) -> None:
    if not isinstance(full_bytes, int) or not isinstance(delta_bytes, int):
        raise TypeError("transfer byte counts must be integers")
    if full_bytes < 0 or delta_bytes < 0:
        raise ValueError("transfer byte counts must be non-negative")
    if full_cpu_s < 0 or delta_cpu_s < 0:
        raise ValueError("CPU measurements must be non-negative")
