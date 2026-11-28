#!/usr/bin/env python3
"""Measure Python prototype cache restores for the two-action sample closure."""

import argparse
import json
from pathlib import Path
import platform
import shutil
import statistics
import subprocess
import sys
import tempfile

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))
import frost


def measure(parent, iterations):
    samples = []
    with tempfile.TemporaryDirectory(dir=parent) as directory:
        root = Path(directory)
        shutil.copyfile(ROOT / "sample/frost.toml", root / "frost.toml")
        ws = frost.Workspace(root)
        targets = ws.target_closure({"pkg00_mod06"})
        (root / "src").mkdir()
        for target in targets:
            source = Path(ws.targets[target].src)
            shutil.copyfile(ROOT / "sample" / source, root / source)
        warm = frost.execute_plan(ws, targets, jobs=2)
        if warm["executed"] != 2 or warm["failed"]:
            raise RuntimeError("expected two successful actions to populate the cache")
        expected = {name: ws.output_path(name).read_bytes() for name in targets}
        for _ in range(iterations):
            shutil.rmtree(ws.out_dir)
            result = frost.execute_plan(ws, targets, jobs=2)
            if result["cache_hit"] != 2 or result["executed"] or result["failed"]:
                raise RuntimeError("expected two cache restores and no executions")
            if any(ws.output_path(name).read_bytes() != data for name, data in expected.items()):
                raise RuntimeError("restored bytes differ from the warm build")
            samples.append({r["target"]: r["lookup_latency_ms"] for r in result["results"]})
    # execute_plan's p50 for two sorted observations is the larger one.
    return samples, statistics.median(max(pair.values()) for pair in samples)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--workdir", type=Path, default=Path(tempfile.gettempdir()))
    parser.add_argument("--iterations", type=int, default=21)
    parser.add_argument("--budget-ms", type=float)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be positive")
    filesystem = subprocess.check_output(
        ["findmnt", "-n", "-o", "FSTYPE", "-T", str(args.workdir)], text=True
    ).strip()
    samples, median = measure(args.workdir, args.iterations)
    report = {
        "filesystem": filesystem, "platform": platform.platform(), "python": platform.python_version(),
        "jobs": 2, "samples_ms": samples, "median_pair_max_ms": median, "budget_ms": args.budget_ms,
    }
    args.out.write_text(json.dumps(report, indent=2) + "\n")
    print(f"{len(samples)} restores of two actions on {filesystem}: {median:.3f} ms")
    return int(args.budget_ms is not None and median >= args.budget_ms)


if __name__ == "__main__":
    sys.exit(main())
