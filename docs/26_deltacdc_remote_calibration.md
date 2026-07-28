# DeltaCDC remote calibration

Decision: keep verified positional DeltaCDC in the local data plane, but do not
enable a remote delta protocol by default. On the measured implementations it
wins only on sufficiently slow links, costs materially more CPU, and the tested
REAPI 2.2 server does not negotiate SplitBlob/SpliceBlob.

This closes the research gate with a measured defer decision rather than
shipping transfer code whose cost model is upside down.

## Reproducible evidence

The 28 July 2026 rerun uses `frost-deltacdc-v2`, which records exact bytes and
separate chunking, selection, encoding and decode-plus-verification CPU time.
Every reconstruction ends with the chunk digest and final blob digest checks.
Both reports record zero verification failures.

| Corpus | Commit sequence | Build | Artifacts |
|---|---|---|---|
| Lua `lua-O2` | 12 commits: `0da6d320f757`, `36c1f6d949a4`, `53b41d0cddd8`, `0465c23b3ee2`, `40b76de2d77e`, `bc4bbcef651b`, `b996f8fd1be7`, `6ca33260d26f`, `84938a7d2b68`, `9130ceb19d32`, `d5bbe955840c`, `7579fc9d7ed9` | `make -j8 MYCFLAGS=-O2` | 36 objects/archive/binary files per commit |
| ripgrep `rg-debug` | 8 commits: `be739c791028`, `a9dc2228fbdd`, `59e318f5ace4`, `2ed0c006fee4`, `d57de83a944c`, `d99ac34406f8`, `8372866810a1`, `f9c05a949d1a` | `cargo build --quiet` | one 40–50 MB debug binary per commit |

Raw reports:

- [`2026-07-28-lua-O2-v2.json`](../build-cache-delta/pkg/results/deltacdc/2026-07-28-lua-O2-v2.json)
- [`2026-07-28-rg-debug-v2.json`](../build-cache-delta/pkg/results/deltacdc/2026-07-28-rg-debug-v2.json)

The harness used FastCDC average 512 KiB, zstd 0.25.0 and its level-19
raw-content dictionary mode. The positional candidate selects the overlapping
chunk from the previous version of the same graph artifact; it does not need a
similarity index.

## Exact cost result

| Corpus | CDC + zstd | Positional DeltaCDC | Bytes saved | Extra CPU | Break-even |
|---|---:|---:|---:|---:|---:|
| Lua | 3,045,583 B / 1.117048 s | 600,114 B / 1.939476 s | 2,445,469 B | 0.822428 s | 23.787799 Mbit/s |
| ripgrep | 51,117,310 B / 56.478994 s | 25,167,626 B / 100.943828 s | 25,949,684 B | 44.464834 s | 4.668801 Mbit/s |

The ripgrep rerun also corrects a stale generalization: whole-blob delta
transferred 67,756,357 bytes, worse than the 51,117,310-byte CDC+zstd baseline
for this slice. The checked result is corpus-specific; neither direction is a
universal property.

## RPC and bandwidth scenarios

[`calibrate_remote.py`](../build-cache-delta/pkg/harness/calibrate_remote.py)
combines the v2 CPU/byte measurements with the external BuildGrid certificate:
23.877 ms observed Action Cache return and 1487.287 ms execution on a loopback
container network. The same RPC constant is applied to both transfer plans, so
it makes absolute scenarios visible without moving the mathematical
break-even:

```text
total_ms = cpu_s * 1000
         + bytes * 8 / (bandwidth_mbit_s * 1000)
         + rpc_overhead_ms
```

Reproduce the checked calibration:

```bash
python build-cache-delta/pkg/harness/calibrate_remote.py \
  build-cache-delta/pkg/results/deltacdc/2026-07-28-lua-O2-v2.json \
  build-cache-delta/pkg/results/deltacdc/2026-07-28-rg-debug-v2.json \
  --reapi-proof bench/baselines/2026-07-28-buildgrid-reapi-poc.json \
  --out bench/baselines/2026-07-28-deltacdc-remote.json
```

The checked
[`remote calibration`](../bench/baselines/2026-07-28-deltacdc-remote.json)
models 1, 10, 100 and 1000 Mbit/s. DeltaCDC wins Lua at 1 and 10 Mbit/s but
loses at 100 and 1000. It wins ripgrep only at 1 Mbit/s and already loses at
10 Mbit/s. The loopback observation is real RPC evidence, not a WAN latency
claim; bandwidth remains an explicit scenario axis.

## Protocol result and ship policy

The external server advertised REAPI 2.0–2.2, SHA-256 and conventional CAS /
Action Cache / Execution behavior. It does not provide the later
SplitBlob/SpliceBlob surface needed to negotiate chunk deltas. Frost's current
plain HTTP/shared-directory remote cache similarly moves whole blobs.

Therefore:

- exact blob and exact chunk reuse remain ahead of delta in the planner;
- local positional deltas remain verified and bounded by the 2 MiB maximum
  chunk;
- no remote peer receives a private delta format without negotiated support;
- remote DeltaCDC stays off by default;
- revisit only when encoding CPU is materially lower or offloaded, a deployed
  protocol negotiates the operation, and production traces lie below the
  relevant corpus break-even.

Similarity continues to affect cost only. Exact digests remain the only reuse
gate.
