#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")/.."

bazel="${BAZEL_BIN:-$(command -v bazel || true)}"
if [[ -z "$bazel" || ! -x "$bazel" ]]; then
  echo "Bazel is required. Install Bazel/Bazelisk or set BAZEL_BIN." >&2
  exit 2
fi

jobs="${FROST_BENCH_JOBS:-$(getconf _NPROCESSORS_ONLN 2>/dev/null || echo 4)}"
sizes="${FROST_BENCH_SIZES:-1000}"
iterations="${FROST_BENCH_ITERATIONS:-5}"
out="${1:-bench/results/$(date -u +%Y%m%dT%H%M%SZ)-frost-bazel.json}"

mkdir -p "$(dirname "$out")"
BAZEL_BIN="$bazel" ./frost-bench run \
  --suite standard \
  --tools frost,bazel \
  --sizes "$sizes" \
  --iterations "$iterations" \
  --jobs "$jobs" \
  --workdir .frost-bench/frost-bazel \
  --out "$out"

python3 - "$out" <<'PY'
import json
import pathlib
import sys

report = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
statuses = {result["tool"]: result["status"] for result in report["results"]}
missing = [tool for tool in ("frost", "bazel") if statuses.get(tool) != "ok"]
if missing:
    raise SystemExit("comparison did not execute successfully: " + ", ".join(missing))
PY

echo "Wrote real Frost/Bazel comparison: $out" >&2
