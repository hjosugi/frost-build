# Persistent workers and dynamic execution

Decision record for #145, September 2026. It revisits the **Defer** row in
[14](14_bazel_gap_analysis.md) with measurements rather than the argument that
row was written from.

**Decision.**

- **Persistent workers: defer past 1.0, with the design fixed here.** The
  row's premise holds for the JVM: a warm javac worker spent **6.2x less CPU**
  than cold `javac` on a 60-class module and **68x less** on an empty one. It
  holds only weakly for the JavaScript TypeScript compiler (1.2x fresh, 1.9x
  with parse reuse), and TypeScript's own native compiler removes more than
  either worker without one (5.4x). Nothing Frost owns today is bottlenecked
  on compiler start-up, and a worker that is safe behind Frost's action cache
  needs a per-request isolation story and a cold-rerun determinism policy
  that do not exist yet. Workers would arrive as an additive, opt-in manifest
  field, so deferring costs 1.0 nothing.
- **Protocol: Bazel's, not a new one.** When workers land, Frost speaks
  Bazel's persistent-worker protocol — its protobuf framing, which existing
  workers use, and its JSON framing — singleplex first.
- **Dynamic execution: defer to v2, blocked.** It needs remote `Execute`
  (#144), per-action cancellation distinct from build cancellation (#48 is
  build-wide today), and a rule for which branch's bytes are published. None
  of the three exists; the third is a correctness decision, not a
  performance one.

## Measurements

Report: [`2026-09-24-issue-145-persistent-worker.json`](../bench/baselines/2026-09-24-issue-145-persistent-worker.json),
recorded from a clean checkout of `2abf3ac` with
[`scripts/bench_persistent_worker.py`](../scripts/bench_persistent_worker.py).
Toolchains: OpenJDK/javac 26.0.2, Node 26.8.2, TypeScript 6.0.3 (the last
JavaScript compiler, which has the in-process API a worker needs) and native
TypeScript 7.0.2 as a reference.

**Workload.** A generated chain of 60 units (records, generics, lambdas and
streams in Java; interfaces, mapped types, classes and ESM imports in
TypeScript) where every unit depends on the one before. Each iteration flips
one constant in the leaf, so every compile follows a one-file edit and
recompiles the module, as a module-granular action does. 17 iterations; the
scenario order rotates every iteration; the first 5 worker requests are the
warm-up curve and the remaining 12 the steady state. Every scenario writes to
an emptied output directory, and every output tree is digested and compared
with the cold compile of the same edit.

**The host was not quiet.** Other agents shared the 8-core Ryzen 3 7330U: load
average 61.4 at the start and 31.8 at the end (governor `performance`). Wall
times are dominated by queueing, so wall medians have wide spreads and are
reported only as a secondary figure. Process CPU time (rusage for cold
processes; the worker's own process CPU delta per request, JIT and GC threads
included) is barely affected by other tenants and is the figure the decision
uses. Ratios compare scenarios measured in the same interleaved run.

### javac

| Scenario (60-unit module) | CPU median (p10–p90) | CPU vs cold | wall median |
|---|---:|---:|---:|
| cold `javac` | 11,801.9 ms (11,209.6–13,199.7) | 1.00x | 22,125 ms |
| worker, fresh javac context per request | 1,905.0 ms (806.0–2,340.0) | **6.2x less** | 13,429 ms (1.65x) |
| worker, shared file manager | 1,700.0 ms (1,171.0–2,378.0) | 6.9x less | 9,485 ms (2.33x) |

| Per-action floor (one empty class) | CPU median | wall median |
|---|---:|---:|
| cold `javac` | 1,360.1 ms | 1,829.0 ms |
| warm worker | 20.0 ms | 79.2 ms |

Warm-up, CPU per request for a newly started fresh-context worker:
10,690 → 7,830 → 3,300 → 2,810 → 2,790 ms, then 1,905 ms steady. JVM start to
`ready` was 258 ms. The first request costs about what a cold compile does;
the gain arrives over the next few requests as the JIT compiles javac itself.
Both workers produced byte-identical class files to cold `javac` on all 17
edits.

### tsc

| Scenario (60-module program) | CPU median (p10–p90) | CPU vs cold | wall median |
|---|---:|---:|---:|
| cold `tsc` 6.0.3 (Node + compiler load + compile) | 4,764.0 ms (4,536.6–4,876.1) | 1.00x | 12,929 ms |
| worker, fresh Program per request | 3,873.6 ms (3,401.3–4,650.1) | 1.23x less | 14,064 ms (0.92x) |
| worker, content-keyed SourceFile reuse + incremental builder | 2,448.6 ms (1,868.4–2,738.6) | **1.95x less** | 9,793 ms (1.32x) |
| cold native `tsc` 7.0.2 (reference, no worker) | 881.8 ms (815.4–1,000.9) | **5.4x less** | 1,469 ms |

| Per-action floor (one empty module) | CPU median | wall median |
|---|---:|---:|
| cold `tsc` 6.0.3 | 3,647.2 ms | 7,580 ms |
| fresh-Program worker | 2,414.9 ms | 7,281 ms |
| reuse worker | 1,426.3 ms | 3,483 ms |
| cold native `tsc` 7.0.2 | 681.3 ms | 789 ms |

The reuse worker reused 123 parsed source files (the 60 modules' unchanged
neighbours plus every `lib*.d.ts`) on every request after the first. Both
workers produced JavaScript byte-identical to cold `tsc` on all 17 edits; the
native compiler produced exactly two distinct trees, one per edit variant.

The reuse worker still type-checks and emits the whole program each request,
which is the cold contract, so its figure is a lower bound on what a
TypeScript worker could remove; even so it cannot close the gap to the native
compiler, which spends less CPU cold than the JavaScript compiler spends on
an empty project in a warm worker.

### What the numbers decide

- **JVM compilers are the real case.** The warm javac floor (20 ms CPU per
  action against 1,360 ms cold) is what would make fine Java partitions
  viable. [17](17_java_gradle_maven_comparison.md) measured 100 one-source
  `javac` VMs at 41.3x the clean time of one batch; that failure is almost
  entirely this floor. A worker is therefore the enabler for per-source Java
  partitions, not a speed-up for today's module batches.
- **TypeScript does not need a Frost worker.** Native `tsc` 7 already beats the
  best measured JavaScript worker by 2.8x in CPU without any hermeticity
  cost, and [21](21_typescript_tsc_comparison.md) already builds on it.
- **Frost has no target kind that would use one yet.** Java runs through
  command targets or Gradle/Maven boundaries, where the incumbent owns its own
  daemon. A worker needs a Frost-owned javac rule to be worth the protocol.

## What can make a worker non-hermetic

A cold action starts from the toolchain closure, argv, environment and
declared inputs, all of which are in its action key. A worker adds a fourth
source of truth — whatever the process remembers from earlier requests — and
that source is in no key. Each row below is state that survives a request and
can therefore change an output without changing the key.

| State that outlives a request | Toolchains | What goes wrong | Detection or containment |
|---|---|---|---|
| Parsed or indexed inputs cached by stat (mtime, size, inode) | tsc/tsserver `SourceFile` caches, javac's shared `StandardJavaFileManager`, any "reuse unchanged" layer | an edit that keeps the stat — same-length change inside one timestamp tick, `touch -r`, a restored checkout — is served from the old parse. **Measured: silent wrong output (H2).** | key reuse on content digests, which Frost already has for every input; cold rerun catches it |
| Open archives on the class path / module path | javac, kotlinc, scalac (jar and `ct.sym` file systems) | a jar republished under the same path is read through the old open file system or a stale central directory. **Measured (H1):** see below | per-request file manager, or a worker key that includes class-path digests; cold rerun |
| Parse options not in the cache key | tsc (`impliedNodeFormat` comes from the nearest `package.json` `type`, not the file) | the same bytes parse as ESM or CJS depending on a file the cache key never saw | cache key includes parse options (the measured worker does); package.json is a declared input |
| Process environment and working directory captured at start | every worker | a request that needs a different `JAVA_TOOL_OPTIONS`, locale, `TZ`, `NODE_OPTIONS` or cwd runs with the first request's | the worker key includes the whole action environment and cwd; a different value is a different worker |
| Static state in plug-ins | javac annotation processors, compiler plug-ins, TypeScript transformers | counters, registries or generated-name tables that are not reset between compilations produce different output on the Nth request | cold rerun; per-request class loader for processors |
| Toolchain replaced under a running worker | all | the worker keeps executing the old compiler's code while the action key already names the new toolchain digest | worker key includes the toolchain closure digest Frost already computes; mismatch kills the worker |
| Temporary files and output directories reused between requests | all | a stale `.class` or `.js` from an earlier request is picked up as an input (javac `-implicit`, tsc `outDir` reads) | `clean_dirs` reset per request (existing mechanism), fresh output directory per request |
| Undeclared file reads | all | a long-lived worker can read anything the first request's sandbox allowed, and later requests inherit it | per-request materialized input root; not available today (below) |
| JIT and class-loading state | JVM, V8 | none observed — affects time, not bytes | nothing to detect; the measured workers produced cold-identical bytes on every request |

### Measured hazards

Five replays in the same report, each started with fresh workers. A cold
compile of the post-edit inputs is the reference; a worker result is
**correct** (same exit status and bytes), a **spurious failure** (fails where
cold succeeds — loud), or a **silent wrong output** (exit 0, different bytes
— what a cache would then serve).

| Replay | Edit between two requests | fresh-state worker | worker with the shortcut |
|---|---|---|---|
| `javac-classpath-api-rename` | class-path jar republished by atomic rename; the client calls a method only the new jar has | correct | shared file manager: **spurious failure** |
| `javac-classpath-constant-rename` | same publication; an inlined `static final` constant changes, jar size unchanged | correct | shared file manager: **silent wrong output** — the old value compiled into the client |
| `javac-classpath-api-in_place` | jar overwritten in place | correct | shared file manager: correct |
| `javac-classpath-constant-in_place` | jar overwritten in place | correct | shared file manager: correct |
| `tsc-same-stat-edit` | leaf content changed, size and `mtime_ns` restored | content-keyed reuse: correct | mtime-keyed reuse: **silent wrong output** |

Two findings matter for Frost specifically. First, Frost publishes outputs
by atomic rename, and the rename case is the one javac's shared file manager
gets wrong: a JavaBuilder-style worker with archive caching behind Frost would
compile against a jar that no longer exists at that path. Second, both silent
cases were caught by comparing with a cold rerun, which is what point 3 of
the policy below makes `--check-determinism` do. The spurious failure is
visible without any check, because the build fails.

### Detection policy

1. **The worker key is the action key's non-input part.** Toolchain closure
   digest, the worker's own startup argv, the complete action environment and
   the working directory select a worker; any difference starts a new one.
   Frost already computes every one of those for the action key, so this is
   a lookup, not new modelling.
2. **Inputs travel by digest in every request**, as Bazel's `WorkRequest`
   `inputs` do, and a worker may reuse a cached parse only for an identical
   digest. Stat-keyed reuse is not permitted inside a Frost worker, because H2
   shows it is silently wrong.
3. **`--check-determinism` reruns worker actions cold.** The existing check
   (`crates/frostbuild-exec/src/determinism.rs`) already reruns an action and
   compares output digests; for a worker action the second run must be an
   ordinary spawn, never the same or another worker. Every silent hazard above
   then surfaces as a determinism failure naming the action, which is exactly
   what the measured experiments did by hand.
4. **Sampled cold reruns in CI.** A configurable fraction of worker actions
   rerun cold in determinism mode, so worker leakage is found by the build
   that caused it rather than by the cache entry it poisoned.
5. **Any worker failure falls back to a cold spawn and retires the worker.**
   A crash, protocol error, timeout or cancellation kills the process; the
   action reruns cold. A worker never gets a second chance with its state.

## Why isolation is a blocker, not a detail

`--sandbox` wraps one process in `bwrap` with read-only binds for exactly the
action's declared inputs (`crates/frostbuild-exec/src/sandbox.rs`). A worker is
one process serving many actions, so it cannot be wrapped per request: it is
either jailed to one action's inputs forever or to none of them. Bazel solves
this with per-request sandbox directories that the worker must honour
(`sandbox_dir` in `WorkRequest`, with worker cooperation for multiplex
workers).

#146 has since shipped the materialization half: `--hermetic` builds each
action a private tree under `.frost/hermetic/` holding exactly the sandbox's
visible set. That tree is what a worker request's `sandbox_dir` would name.
What is still missing is confinement: `--hermetic` relies on the action
running inside its tree, and a worker process started once cannot be moved
into the next request's tree by the OS, only asked to use it. A worker under
`--sandbox` or `--hermetic` therefore needs either a worker that honours
`sandbox_dir` and is trusted to, or a bwrap jail around the worker that sees
only request trees. Until one of those is built and tested, isolated builds
must bypass workers entirely — so shipping workers first would ship them into
exactly the builds that cannot check them.

## Bazel's protocol or a Frost protocol

Bazel's persistent-worker protocol is small: a `WorkRequest` carries
`arguments`, `inputs` (path plus digest), `request_id`, `cancel`,
`verbosity` and `sandbox_dir`; a `WorkResponse` carries `exit_code`,
`output`, `request_id` and `was_cancelled`. It has two framings — varint
length-delimited protobuf, and newline-delimited JSON selected per action with
`requires-worker-protocol=json` — plus opt-in multiplexing
(`supports-multiplex-workers`), cancellation (`supports-worker-cancellation`)
and sandboxing.

| | Bazel protocol | Frost-specific minimal protocol |
|---|---|---|
| Existing workers | Bazel's own JavaBuilder and the worker modes of the Kotlin, Scala and TypeScript rule sets implement it | none; every tool would need a Frost adapter |
| Inputs by digest | yes (`inputs[].digest`) | would have to be designed the same way |
| Cancellation | yes, opt-in | would have to be designed the same way |
| Framing cost for Frost | JSON: `serde_json`, already a dependency. Protobuf: two small messages of strings, bytes, integers and booleans, hand-encodable without a protobuf dependency | the same work |
| What Frost would give up | nothing it needs; multiplex and `sandbox_dir` are optional capabilities | compatibility with every existing worker |

The measured workers in this study use a one-line request because they are
instruments, not a proposal. Everything a Frost-specific protocol would
contain is already in Bazel's, and the ecosystem is on Bazel's side. The
decision is to implement Bazel's protocol, singleplex, with input digests,
cancellation and `sandbox_dir` from the start. Protobuf framing comes first,
because it is what existing workers speak; JSON framing is the cheap second
path for a worker written where no protobuf runtime is convenient.

## Dynamic execution

Dynamic execution races a local branch (usually a worker) against a remote
one and keeps whichever finishes first. It is attractive exactly where this
study found workers help — short, JIT-heavy actions — and it has four
prerequisites that Frost does not meet:

1. **A remote branch.** Remote `Execute` is #144; today Frost has only a remote
   *cache* (#108). Without `Execute` there is nothing to race.
2. **Complete inputs before start.** REAPI needs the full Merkle input root
   before `Execute`, while Frost discovers compiler inputs constructively
   during local execution ([11](11_remote_execution_study.md)). The remote
   branch may only start from the last verified trace, and must lose — not
   run — when that trace is absent or stale.
3. **Per-action cancellation.** Cancellation (#48) is build-wide: one
   `CANCELLED` flag, and `request_cancellation()` terminates every running
   process group (`crates/frostbuild-exec/src/process.rs`). The losing branch
   of a race must be cancelled alone, reported as neither success nor
   failure, and must not trip the build's cancellation path. A worker branch
   additionally needs protocol-level cancellation, or the worker has to be
   killed and restarted, which would forfeit the warm state the race exists
   to use.
4. **A publication rule.** Frost publishes outputs through the verified CAS
   boundary (`UnverifiedBytes` → verified blob, `crates/frostbuild-core/src/cas.rs`)
   and records them in a crash-safe journal. With two branches, the loser's
   partial outputs must never reach that boundary, which requires each branch
   to write into its own private tree — the same per-request materialization
   the worker sandbox needs. And "first finisher wins" is only sound when
   both branches would have produced the same bytes: an action that has not
   passed `--check-determinism` must not be raced, or the published bytes
   depend on network latency.

Dynamic execution therefore stays a v2 item behind #144. It should be
reopened as a design issue only once remote `Execute` and per-action
cancellation have shipped.

## Follow-up

- Persistent workers are reopened as an implementation issue when **all** of:
  a worker can be confined to a request's `--hermetic` tree (#146 built the
  tree, not the confinement); a Frost-owned target kind exists whose measured
  critical path is dominated by compiler start-up — the likely first one is a
  native javac rule with per-source partitions, where the 1,360 ms floor is
  the whole problem; and the worker key and cold-rerun policy above are
  accepted as the design. The measurement
  harness in this study is the baseline that issue must beat.
- The harness and its fixture stay checked in so the premise can be re-measured
  when a JDK or TypeScript release changes start-up costs.
- [14](14_bazel_gap_analysis.md)'s row now points here; [13](13_issue_implementation_matrix.md)
  records #145 as a measured defer decision.

## Reproduce

Toolchains are pinned by the report's `tools` block. On Linux x86-64:

```bash
npm install --prefix /tmp/ts6 typescript@6.0.3
npm install --prefix /tmp/ts7 typescript@7.0.2
python3 scripts/bench_persistent_worker.py \
  --typescript-js /tmp/ts6/node_modules/typescript \
  --tsc-native /tmp/ts7/node_modules/@typescript/typescript-linux-x64/lib/tsc \
  --units 60 --iterations 17 --warmup 5 \
  --out bench/baselines/2026-09-24-issue-145-persistent-worker.json
```

`java`/`javac` come from `PATH` and must be a JDK. The workers are
[`JavacWorker.java`](../bench/fixtures/persistent-worker/JavacWorker.java)
and [`ts_worker.mjs`](../bench/fixtures/persistent-worker/ts_worker.mjs);
their headers document the one-line protocols. `tests/test_bench_persistent_worker.py`
checks the fixture generators, the statistics and the facts this memo quotes
from the checked-in report, so it runs in CI without either toolchain.
