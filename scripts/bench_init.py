#!/usr/bin/env python3
"""Compare two release binaries on the same warm C source trees."""

import argparse
import hashlib
import json
from pathlib import Path
import platform
import statistics
import subprocess
import tempfile
import time


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--before", required=True, type=Path)
    parser.add_argument("--after", required=True, type=Path)
    parser.add_argument("--iterations", type=int, default=16)
    parser.add_argument("--out", required=True, type=Path)
    args = parser.parse_args()
    if args.iterations < 1:
        parser.error("--iterations must be positive")
    bins = {"before": args.before.resolve(), "after": args.after.resolve()}
    results = {}
    report = {
        "platform": platform.platform(),
        "binary_sha256": {key: hashlib.sha256(path.read_bytes()).hexdigest()
                          for key, path in bins.items()},
        "cases": results,
    }
    with tempfile.TemporaryDirectory() as tmp:
        parent = Path(tmp)
        for size, width in [(1, 1), (10000, 100)]:
            root = parent / f"c{size}"
            for i in range(size // width):
                folder = root / f"src{i:03d}"
                folder.mkdir(parents=True)
                for j in range(width):
                    name = "main" if i == j == 0 else f"f{i:03d}_{j:03d}"
                    (folder / f"f{j:03d}.c").write_text(f"int {name}(void){{return 0;}}\n")
            for mode, extra in [("auto", []), ("native", ["--language", "native"])]:
                samples = {key: [] for key in bins}
                digests = {}
                for index in range(args.iterations + 1):
                    for key in (list(bins) if index % 2 else list(reversed(bins))):
                        start = time.perf_counter_ns()
                        output = subprocess.check_output([bins[key], "--no-frostrc", "-C", root,
                                                          "init", "--dry-run", *extra])
                        elapsed = (time.perf_counter_ns() - start) / 1e6
                        digest = hashlib.sha256(output).hexdigest()
                        if digests.setdefault(key, digest) != digest:
                            raise ValueError(f"inconsistent manifest from {key}")
                        if index:  # the first pair warms directory and executable pages
                            samples[key].append(elapsed)
                if digests["before"] != digests["after"]:
                    raise ValueError(f"manifest changed for {size} files, {mode}")
                results[f"{size}_{mode}"] = {
                    "samples_ms": samples, "manifest_sha256": digests,
                    "median_ms": {key: statistics.median(rows) for key, rows in samples.items()},
                }
    args.out.write_text(json.dumps(report, indent=2) + "\n")


if __name__ == "__main__":
    main()
