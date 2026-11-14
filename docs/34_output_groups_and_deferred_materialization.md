# Output groups and deferred materialization

Status: design, not shipped. This document specifies the next CI-oriented
storage feature; it does not change today's fully-materialized behavior.

## Why

Bazel's `--output_groups` and Build without the Bytes, and Buck2's deferred
materialization, optimize the same CI shape: a job needs one artifact (an
executable, a wheel, a test log), but the graph produces thousands of objects.
Downloading and writing all of them costs bandwidth and inodes that the job
never reads.

Frost v1 deliberately keeps every output locally materialized. DESIGN §7 says
so, and the BwoB "output not found" tail is the cautionary tale: a lazily
materialized tree that an action then reads is a correctness bug, not a slow
path. Any implementation has to preserve that property.

## Two separate features

The phrase "deferred materialization" hides two changes with different risk:

1. **Output groups** — name a subset of a target's declared outputs and ask a
   build to publish/materialize only those. No action changes what it runs; the
   subset only decides what leaves the CAS after a **cache hit**.
2. **Remote download on demand** — do not fetch a blob until a consumer (the
   user's `run`, an IDE, a later step) asks for it. This is where BwoB's bugs
   live and where Frost should stay conservative.

Ship (1) first. (2) is v2 and should land only if remote execution does.

## Design for output groups

### Manifest

```toml
[target.app]
kind = "cc_binary"
srcs = ["main.c"]
outputs = [".frost/out/${config}/app", ".frost/out/${config}/app.map"]

[target.app.groups]
default = [".frost/out/${config}/app"]
debug = [".frost/out/${config}/app", ".frost/out/${config}/app.map"]
```

Rules:

- Every group member must be one of the target's declared `outputs` or lie
  under one of its `output_dirs`. A group cannot introduce a new artifact.
- `default` is implicit and is the whole declared set when omitted, so an
  existing manifest is unchanged.
- A file may appear in several groups; the union is what a selection asks for.
- Files in no group are still produced and cached, but are not restored for a
  cache hit unless the selection names them.

### Selection and correctness

`frost build --output-groups=default` (comma-separated, or `--output-groups=all`).

The key rule: **a group selection may only skip outputs that no selected action
consumes and no requested runnable target needs.** The planner already has the
graph:

- Build the closure of the requested targets.
- Collect every `FileId` that is an input (declared or order-only) of an action
  in that closure, plus the runnable artifact of a `run`/`debug` request.
- Remove those from the "safe to skip" set. A group that excludes a live input
  is a load-time error, naming the consumer. This is the same fingerprint of a
  bad declaration the sandbox already gives: refuse rather than silently build
  a graph that cannot run.
- On a cache hit, restore only the selected, safe set. On a cache miss, run the
  action normally; the group decides publication to the local CAS and the
  remote upload set, not what a compiler writes.

### Journal

Restoration is driven by `JournalEntry.outputs` in `frostbuild-exec`. Add a
`materialized` filter computed per invocation; the journal format does not
change. A later build that selects a different group restores the missing files
from the CAS, so selections are not sticky.

### Interactions

- **Remote cache / REAPI**: `remote_publish` already uploads `entry.outputs`;
  intersect it with the selection. A consumer asking for another group misses
  and builds, which is correct but slower — the docs must say so.
- **Tests**: `cc_test` and `test` leave their outputs empty today (the success
  stamp is Frost-managed). Output groups are a build-target feature first.
- **`run`/`debug`/`ide`**: they force the runnable artifact's group regardless
  of the flag, because the user asked to execute it.
- **`--report`**: its HTML already lists every action; add the selected group
  and the skipped count so a CI log explains a smaller artifact set.

### Tests

- A diamond where a skipped output is consumed is a load error naming the
  consumer.
- A cache hit restores the selected subset; a second build with another group
  restores the rest without re-executing.
- `run` ignores the selection for its artifact.
- Determinism and the CAS digest of restored bytes are unchanged.

## Deferred materialization (v2)

Only after remote execution. The shape:

- `--remote-download=minimal|default`; `minimal` stages blobs into the local
  CAS but does not write workspace paths.
- `frost build --materialize-outputs` and `run`/`debug` materialize what they
  need; `frost clean` is unaffected.
- Any action that would read a lazy path is a planner error, not a runtime
  miss: the input closure is known before execution.
- The REAPI cache adapter already stores SHA-256 names beside frost digests, so
  staging is a lookup, not a rehash.

Do not ship this without a nightly that materializes on demand and compares
the tree to a fully materialized build, the way `--check-determinism` gates
nondeterminism today.
