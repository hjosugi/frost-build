#!/usr/bin/env bash
# Re-measure Frost against Bazel across graph shapes (#159).
#
# Requires Bazel/Bazelisk (`BAZEL_BIN`) and Ninja. Each shape gets its own
# report; Bazel's `--disk_cache` is exercised by the harness's
# `cache_hit_rebuild` scenario, and `frost-daemon` measures the daemon path
# beside plain `frost`.
set -euo pipefail
cd "$(dirname "$0")/.."

bazel="${BAZEL_BIN:-$(command -v bazel || true)}"
if [[ -z "$bazel" || ! -x "$bazel" ]]; then
  echo "Bazel is required. Install Bazel/Bazelisk or set BAZEL_BIN." >&2
  exit 2
fi

jobs="${FROST_BENCH_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
shapes="${FROST_BENCH_SHAPES:-linear wide packages}"
sizes="${FROST_BENCH_SIZES:-1000}"
iterations="${FROST_BENCH_ITERATIONS:-5}"
out_dir="${1:-bench/baselines}"

mkdir -p "$out_dir"
for shape in $shapes; do
  BAZEL_BIN="$bazel" ./frost-bench run \
    --suite standard --shape "$shape" \
    --tools frost,frost-daemon,ninja,make,bazel \
    --sizes "$sizes" \
    --iterations "$iterations" \
    --jobs "$jobs" \
    --workdir .frost-bench/frost-bazel \
    --out "$out_dir/frost-bazel-$shape.json"
done

python3 - "$out_dir" $shapes <<'PY'
import json
import pathlib
import sys

out_dir = pathlib.Path(sys.argv[1])
failures = []
for shape in sys.argv[2:]:
    report = json.loads((out_dir / f"frost-bazel-{shape}.json").read_text(encoding="utf-8"))
    statuses = {result["tool"]: result["status"] for result in report["results"]}
    for tool in ("frost", "bazel"):
        if statuses.get(tool) != "ok":
            failures.append(f"{shape}:{tool}")
if failures:
    raise SystemExit("comparison did not execute successfully: " + ", ".join(failures))
PY

echo "Wrote Frost/Bazel comparisons under $out_dir" >&2
