#!/usr/bin/env python3
"""Compare resource-aware simulated makespan with the real executor."""

from __future__ import annotations

import argparse
import json
import os
import pathlib
import platform
import re
import shutil
import statistics
import subprocess
import tempfile
from datetime import datetime, timezone


MAKESPAN = re.compile(r"^  makespan\s+(\d+) ms,", re.MULTILINE)


def run(command: list[str], *, cwd: pathlib.Path) -> subprocess.CompletedProcess[str]:
    completed = subprocess.run(command, cwd=cwd, text=True, capture_output=True)
    if completed.returncode != 0:
        raise RuntimeError(
            f"command failed ({completed.returncode}): {' '.join(command)}\n"
            f"{completed.stdout}{completed.stderr}"
        )
    return completed


def version(command: list[str], *, cwd: pathlib.Path) -> str:
    completed = run(command, cwd=cwd)
    return (completed.stdout + completed.stderr).strip().splitlines()[0]


def source_state(repo: pathlib.Path) -> dict[str, object]:
    commit = version(["git", "rev-parse", "HEAD"], cwd=repo)
    dirty = bool(run(["git", "status", "--porcelain"], cwd=repo).stdout.strip())
    return {"commit": commit, "working_tree_dirty": dirty}


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--frost", required=True, type=pathlib.Path)
    parser.add_argument("--iterations", type=int, default=7)
    parser.add_argument("--out", required=True, type=pathlib.Path)
    args = parser.parse_args()
    if args.iterations < 3:
        parser.error("--iterations must be at least 3")

    repo = pathlib.Path(__file__).resolve().parent.parent
    fixture = repo / "bench/fixtures/resource-scheduling"
    frost = args.frost.resolve()
    samples: list[int] = []
    load_before = list(os.getloadavg()) if hasattr(os, "getloadavg") else None

    with tempfile.TemporaryDirectory(prefix="frost-resource-bench-") as temporary:
        workspace = pathlib.Path(temporary) / "workspace"
        shutil.copytree(fixture, workspace)
        command = [
            str(frost),
            "-C",
            str(workspace),
            "build",
            "--jobs",
            "4",
            "--local-cpu-resources",
            "4",
            "--local-ram-resources",
            "1200",
            "--stats",
            "--no-tui",
        ]
        for iteration in range(args.iterations):
            (workspace / "salt.txt").write_text(f"{iteration}\n", encoding="utf-8")
            completed = run(command, cwd=repo)
            output = completed.stdout + completed.stderr
            match = MAKESPAN.search(output)
            if not match or "(admission constrained)" not in output:
                raise RuntimeError(f"resource stats missing from build output:\n{output}")
            samples.append(int(match.group(1)))

        simulated = json.loads(
            run(
                [
                    str(frost),
                    "-C",
                    str(workspace),
                    "simulate",
                    "--jobs",
                    "4",
                    "--local-cpu-resources",
                    "4",
                    "--local-ram-resources",
                    "1200",
                    "--json",
                ],
                cwd=repo,
            ).stdout
        )
        point = next(
            point
            for point in simulated["points"]
            if point["scheduler"] == "critical-path"
            and point["estimator"] == "journal"
            and point["jobs"] == 4
        )

    actual_median = statistics.median(samples)
    simulated_ms = point["makespan_ms"]
    ratio = actual_median / simulated_ms
    report = {
        "schema": "frost-resource-scheduling-bench-v1",
        "recorded_at_utc": datetime.now(timezone.utc).isoformat(),
        "host": {
            "platform": platform.platform(),
            "machine": platform.machine(),
            "cpu_count": os.cpu_count(),
            "load_avg_before": load_before,
            "load_avg_after": list(os.getloadavg()) if hasattr(os, "getloadavg") else None,
        },
        "tools": {
            "frost": version([str(frost), "--version"], cwd=repo),
            "python": version(["python3", "--version"], cwd=repo),
        },
        "source": source_state(repo),
        "fixture": "bench/fixtures/resource-scheduling",
        "reproduce": (
            "python3 scripts/bench_resource_scheduling.py "
            "--frost /absolute/path/to/cargo-target/release/frost "
            f"--iterations {args.iterations} --out {args.out}"
        ),
        "limits": {"jobs": 4, "cpu": 4, "ram_mb": 1200, "test_jobs": 4},
        "actions": 4,
        "action_resources": {"cpu": 1, "ram_mb": 600, "exclusive": False},
        "action_sleep_ms": 120,
        "actual_makespan_ms": {"samples": samples, "median": actual_median},
        "simulated_makespan_ms": simulated_ms,
        "actual_over_simulated": ratio,
        "within_25_percent": 0.75 <= ratio <= 1.25,
        "simulation_point": point,
    }
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(json.dumps(report, indent=2) + "\n", encoding="utf-8")
    if not report["within_25_percent"]:
        raise RuntimeError(
            f"actual/simulated ratio {ratio:.3f} is outside [0.75, 1.25]"
        )
    print(args.out)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
