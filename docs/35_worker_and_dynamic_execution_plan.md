# Persistent workers and dynamic execution: implementation plan

Status: deferred, with a concrete landing plan. The measurements and hazards
are in [31_persistent_workers.md](31_persistent_workers.md) and
[11_remote_execution_study.md](11_remote_execution_study.md). This page is the
engineering sequence that turns those studies into code when the blocking
preconditions are met.

## Why this is not implemented yet

The existing defer is a decision, not a backlog gap:

- A warm javac worker spent 6.2x less CPU on a 60-unit one-edit module, but a
  replay showed a shared javac file manager compiling a **stale inlined
  constant** after an atomic jar rename, and a stat-keyed tsc cache emitting
  stale JavaScript. Both are silent-wrong-answer classes, which Frost treats as
  the only unacceptable failure.
- Workers need per-request confinement. The bubblewrap sandbox is per-process,
  so a worker that reuses a process cannot get the same visible set per action
  without protocol support.

Landing workers before request confinement would trade a measured speed win for
a correctness hole. The plan below is ordered so that never happens.

## Persistent workers — sequence

1. **Worker protocol, no sharing.** Speak Bazel's worker protocol
   (`@<flagfile>` request, stdout `WORKER_*` lines) but start a fresh process per
   action. This proves the protocol and the key. Gate: byte-identical outputs
   versus cold, on javac and tsc, under `--check-determinism`.
2. **Key the worker.** Include toolchain closure, resolved tool path, selected
   environment and cwd in the worker key, exactly as an action key includes
   them. Never reuse by tool name alone.
3. **Reuse by input digest.** A worker may serve an action only when its
   declared input digests match the request; the worker's own cache is not
   trusted. Run cold under `--check-determinism`, where a disagreement is a
   failure rather than a cache hit.
4. **Per-request confinement.** On Linux, put the worker in a namespace whose
   visible set is re-derived per request (bind-mount update), or run the worker
   in a Frost-owned hermetic tree that is rebuilt per request. On hosts without
   that, workers stay off behind the feature flag.
5. **Isolation test.** A worker must see exactly the current action's inputs: an
   undeclared header read is a build failure, the same verdict `--hermetic`
   gives today.

Until step 4 works, `--workers` remains unspelled. A half-working worker pool is
worse than no pool because its failures are stale outputs, not slow builds.

## Dynamic execution — sequence

Dynamic execution races a local and a remote attempt and keeps the first valid
result. It depends on remote `Execute`:

1. **Wire `Execute` into the executor.** `frostbuild-reapi` already has the
   client and the certificate; the remaining work is `ActionResult`
   file/directory/mode/stdout translation, remote toolchain packaging and
   `Execute` invocation in `frostbuild-exec` (docs/11).
2. **Per-action cancellation.** A losing attempt must be cancelled, not left
   running. Process-group cancellation exists locally; remote needs a REAPI
   `Execute` stream cancel and a bounded drain.
3. **Determinism-gated publication.** Publish only the winning result, and only
   after `--check-determinism` or a single-attempt success. Two racing
   attempts that disagree are an error, never a coin flip.
4. **Policy.** `--dynamic=local|remote|race`, default `local`. Off unless a
   remote executor is configured; the existing fallback rule (remote failure
   costs speed and nothing else) still holds.
5. **Measurement.** The `frost-bench` harness must report race overhead on a
   cache-hit build and on a wide graph before any default changes.

## What is safe to do now

Already shipped and unchanged by this plan:

- `--runs-per-test` and `flaky_retries` make nondeterministic tests visible.
- `--check-determinism` turns a nondeterministic action into a failure.
- The REAPI cache adapter verifies every blob and falls back locally.
- The scheduler already has resource admission, so a worker pool will not
  bypass CPU/RAM budgets.

The next concrete step, when prioritized, is worker protocol step 1: a
no-sharing worker that is byte-identical to cold. That is a self-contained PR
and the prerequisite for everything after it.
