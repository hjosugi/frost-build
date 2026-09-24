# Scale, soak and failure recovery

This is the v1 quality gate from #149: evidence that Frost stays correct and
bounded on a monorepo-sized workspace, over a long stream of edits, and after a
crash, a full disk or damaged state. It records limits, not performance
claims. Where something breaks, this document says where.

Everything below is reproducible from one script,
[`scripts/frost_scale.py`](../scripts/frost_scale.py), and one test file,
[`crates/frostbuild-cli/tests/recovery.rs`](../crates/frostbuild-cli/tests/recovery.rs).
[`.github/workflows/quality.yml`](../.github/workflows/quality.yml) runs the
recovery tests on every push and pull request, and the scale measurement and
the soak nightly and on demand.

## The workspaces

The generators that already existed (`frost_bench.py`, `frost-bench-rs
daemon-graph`) produce one linear chain, because a head-to-head with Ninja,
Make and Bazel needs a graph all four can express. That is the wrong shape for
finding where Frost itself stops scaling, which needs width, many manifests,
generated headers feeding real compiles, and owned output trees.
`frost_scale.py generate` writes any combination of those from one `Shape`:

| Preset | Targets | Packages | Fan-in | Files / target | Generated headers / package | C units / package | Owned trees × files |
|---|---:|---:|---:|---:|---:|---:|---|
| `linear` | 1,000 | 1 | chain | 1 | — | — | — |
| `wide` | 1,000 | 1 | 32 | 1 | — | — | — |
| `packages` | 1,000 | 50 | 8 | 1 | — | — | — |
| `headers` | 200 | 10 | 8 | 1 | 20 | 2 | — |
| `output-dirs` | 100 | 1 | 8 | 1 | — | — | 10 × 200 |
| `soak` | 2,000 | 40 | 8 | 1 | 4 | 1 | 4 × 100 |
| `monorepo` | 50,000 | 500 | 16 | 2 | 8 | 2 | 20 × 500 |

`monorepo` is the target shape the issue names: 50,000 genrule targets reading
100,000 source files, 500 nested manifests two directories deep, 4,000
generated headers compiled by 1,000 C translation units, and 10,000 files in
owned output directories. Every field can be overridden on the command line.

Every genrule output is `cksum` of what it reads. A change therefore always
propagates (there is no accidental early cutoff), outputs stay a few bytes at
any depth, and the harness knows two things without running Frost: the exact
number of actions a change must rerun, and the exact bytes of a leaf's output.
Each package root also reads up to two earlier package roots, so a leaf change
crosses package boundaries.

Generation is deterministic: the same `Shape` (including its seed) writes a
byte-identical tree, which `tests/test_frost_scale.py` asserts by digest, along
with each preset having the property it is named for.

```bash
python3 scripts/frost_scale.py generate --shape wide --out /tmp/wide
python3 scripts/frost_scale.py digest /tmp/wide
```

## How results are judged

A measurement is only recorded if the build was right:

- every cold build executes exactly the model's action count, every warm no-op
  executes none, and every one-file or header change executes exactly the
  model's affected set — standalone and through the daemon;
- at the end, the same sources are copied into a fresh directory, built from
  nothing, and every declared output and owned-tree file is compared byte for
  byte.

A run that fails either check writes its JSON anyway, with `"ok": false` and
the error, because where a run breaks is the finding.

## Scale at the target shape

The `scale` job of `quality.yml` measures the `monorepo` shape on a GitHub-hosted
runner; its report is checked in beside the other baselines once a run on
`main` completes.

## Soak

`frost_scale.py soak` keeps `frost daemon serve` and `frost watch` resident on
the `soak` workspace and applies a seeded stream of edits: one to three files
at a time across leaf sources, generated-header inputs and owned-tree inputs,
of which 15% return a file to an earlier content (an ABA edit a cache may
serve) and 10% touch a file without changing it. After each edit it waits for
`frost watch` to finish a successful build, checks that each edited leaf's
output is the `cksum` of its new inputs, and then asks the daemon for a build.
Every 50 edits, and at the end, the outputs are compared with a clean build.

Between edits it samples, for both resident processes, the open descriptor
count and resident set from `/proc`, plus the journal and `.frost/` sizes. A
leak is judged against the warmed-up state (the second tenth of the samples)
rather than the first sample: the run fails if the last tenth holds more than
8 descriptors above the baseline, or a median RSS more than 1.5× the baseline
plus 32 MiB, or if the journal ever exceeds twice its compaction threshold.

The `soak` job of `quality.yml` runs this for 45 minutes nightly; its report is
checked in beside the other baselines once a run on `main` completes.

## Recovery

Each case drives the real binary into one failure and then requires the same
thing: the next ordinary build succeeds, its outputs equal a from-scratch build
of the same sources byte for byte, and the build after it runs nothing.

| Failure | How it is produced | Test |
|---|---|---|
| SIGKILL while an action runs | kill the process group once every other action is journalled; exactly the killed action and its dependent rerun | `sigkill_while_an_action_runs_keeps_finished_work_and_converges` |
| SIGKILL anywhere | twelve kills at 0–450 ms into builds of freshly edited sources, then one ordinary build | `sigkill_at_arbitrary_moments_always_converges` |
| Torn journal tail | a frame whose length promises more bytes than follow | `a_torn_journal_tail_after_a_kill_is_ignored` |
| Disk full (`ENOSPC`) | the workspace on a 16 MiB tmpfs filled to leave 16 KiB – 3 MiB free, cold and warm | `builds_on_a_full_filesystem_converge_once_space_returns` (runs where `FROST_TEST_ENOSPC_DIR` names such a mount; the `recovery` CI job mounts one) |
| File too large (`EFBIG`) | `RLIMIT_FSIZE` from 2 KiB to 3 MiB, cold and warm, with `SIGXFSZ` ignored | `builds_that_exceed_the_file_size_limit_converge` |
| Damaged journal, hash cache, graph store, no-op certificate, toolchain stamp | each emptied, halved, shortened by a byte, given a bad magic, overwritten with garbage, and six single-bit flips — then converge, edit a source, converge again | `a_damaged_*_costs_work_never_correctness` |
| Damaged CAS objects | a byte flipped in every object, outputs deleted so restoring from the CAS is the cheap path | `damaged_cas_objects_are_never_restored` |
| Random damage | six rounds of truncation, bit flips and appended bytes across every file under `.frost/`, with a source edit each round | `random_damage_anywhere_in_frost_state_converges` |
| inotify watch limit | a user namespace whose `max_inotify_watches` is below the workspace's directory count | `past_the_watch_limit_daemon_builds_fall_back_to_building_in_process` |

The disk-full sizes are chosen so the failure lands in different writers. On
the tmpfs, the first failing write was, depending on the space left and on
which action got there first: the command's own output (`tr: write error`),
Frost's copy of an output into the CAS (`failed to store output in CAS`), and
the stamp Frost writes for an owned tree (`.frost/tree/…/contents`). In every
case the build failed with the OS error in its message, and the build after
space returned matched a clean build. `RLIMIT_FSIZE` limits the size of one
file rather than the space on a device, so it produces `EFBIG` rather than
`ENOSPC`; its smallest limits fail Frost's own graph and state writes before
any command runs, and the rest fail the command. It needs no privilege, so it
runs in every `cargo test`; the real `ENOSPC` case needs a small filesystem,
which an unprivileged test cannot mount.

### What the gate found and fixed

Five defects surfaced while building this gate. Each is fixed, and each has a
test that fails without its fix.

1. **A journal with an unreadable tail lost everything appended after it.**
   The decoder stops at the first frame it cannot parse — correctly, since a
   crash mid-append leaves a torn frame — but the writer kept appending behind
   that frame, so every record written after a crash or a damaged byte was
   unreadable too. The build after recovery produced the right outputs and
   then redid the same work on every later build until compaction at 32 MiB.
   The first append of a build now cuts the file back to its last whole
   record, using what the load already learned, so no second decode is paid.
   (`Journal::recorder`, `records_appended_after_an_unreadable_tail_stay_readable`.)

2. **A damaged journal record could restore the wrong file.** Records had no
   checksum, and a flipped bit inside one can still decode. For an owned
   output tree the journal's file list *is* the recorded tree, so one flipped
   byte (`tree001/f1.txt` → `f7.txt`) made the next full check delete `f1.txt`,
   restore its bytes as `f7.txt`, and report `up to date` — wrong outputs that
   persisted until the tree's inputs changed. The random-damage recovery test
   found it. Every record now carries a checksum (journal format
   `FRSTJR02`; an older journal is read as foreign and costs one cold build),
   and a damaged record ends the readable prefix like a torn one.
   (`a_record_damaged_in_place_ends_the_readable_prefix` flips every bit of a
   small journal.)

3. **A damaged graph store was trusted.** The warm path proves that the
   manifests are unchanged, not that the stored bytes are, and a flipped bit in
   a command string or output path decodes into a different, plausible graph.
   On a 69-action generated workspace, 12 of 40 random single-bit flips in
   the store, each followed by one source edit, broke the next build: 9 failed
   (`failed to spawn "/bil/sh"`, an `-I` path pointing at a package that does
   not exist, an output declared under a misspelled directory) and 3 succeeded
   with outputs that differ from a clean build. Nothing short of editing a
   manifest recovered. The store now carries a BLAKE3 digest
   of its payload (graph store version 13), checked before decoding; a
   mismatch recompiles. The same 40 flips against the fixed binary all
   converge. (`a_damaged_payload_is_recompiled_rather_than_decoded`
   flips every bit position of a small store.)

4. **`--build-event-json` wrote nothing when the no-op certificate or the
   daemon answered.** A CI job asking for the stream got no file exactly when
   nothing had changed, and through `--daemon` never. The certificate path now
   writes the same three events as a planned all-cached build, and the daemon
   forwards the option (resolving a relative path in the client) and runs the
   child build rather than answering from its certificate when a stream is
   requested.
   (`the_event_stream_is_written_on_every_path_that_finishes_a_build`.)

5. **`build --daemon` failed when the daemon could not start.** Past the
   inotify watch limit `frost daemon serve` exits at startup, and `build
   --daemon` reported `frostd did not become ready` instead of building. It now
   warns and builds in process, which is what every other reason for the
   daemon to decline already did.

## Known limits

- **One build per workspace at a time.** Frost takes no cross-process lock on
  `.frost/`. The daemon serializes the builds it runs, but a `frost build`
  started beside `frost watch` or another `frost build` on the same workspace
  races on the journal and outputs. The soak waits for `frost watch` to go
  quiet before asking the daemon for a build for this reason.

- **Watchers stop at the OS watch limit.** Linux inotify needs one watch per
  directory. When a workspace has more directories than
  `fs.inotify.max_user_watches` allows, `frost watch` and `frost daemon serve`
  exit at startup with `OS file watch limit reached`; `build --daemon` falls
  back to an in-process build and warns on every invocation. The `monorepo`
  scale report records how many directories a built workspace has. Raise the limit
  (`sysctl fs.inotify.max_user_watches=…`) or do without the daemon; there is no
  polling fallback.

- **The journal keeps every action it has ever recorded.** It is append-only
  and compacted (rewritten from the in-memory map) when a build ends with it
  above 32 MiB, so its size is bounded by the number of distinct action ids
  ever built plus one build's appends. Entries for actions that no longer
  exist — renamed targets, deleted sources — are carried through compaction
  rather than pruned.

- **The CAS grows until its cap.** Every distinct output ever produced is kept
  until the store passes 10 GiB (`DEFAULT_CAS_MAX_BYTES`), then the least
  recently used objects are collected; an unchanged build skips the scan for a
  bounded interval. `.frost/` therefore grows with edits by roughly the bytes
  of new outputs (the soak records the rate), and on a small disk it is the
  cap, not the workspace, that decides how much space Frost uses.

- **Memory is proportional to the graph.** Planning holds the whole compiled
  graph, and the daemon keeps it resident; the scale report records peak RSS
  for planning, building and the resident daemon.

- **Cold builds of many tiny actions are process-spawn bound.** Each genrule
  is at least one shell plus its commands; at the target shape the cold build
  spawns hundreds of thousands of processes. This is the cost of the workload,
  not of the graph, and it is why the scale report separates planning from
  execution.

- **Not measured here.** A real open-source monorepo (the issue keeps it out of
  CI; the generator is the reproducible substitute), network filesystems,
  macOS/Windows soak (the soak reads `/proc`), and more than one workspace per
  daemon host.

## Reproducing

```bash
cargo build --release --locked -p frostbuild-cli

# scale: the target shape, five samples per scenario (≈ 20 min on a 4-core CI runner)
python3 scripts/frost_scale.py scale --frost target/release/frost \
  --shape monorepo --iterations 5 --digest --out scale.json

# soak: 45 minutes of edits against resident daemon + watch (Linux)
python3 scripts/frost_scale.py soak --frost target/release/frost \
  --duration 2700 --out soak.json

# recovery, including real ENOSPC when a small filesystem is available
sudo mount -t tmpfs -o size=16m tmpfs /mnt/frost-full && sudo chown "$USER" /mnt/frost-full
FROST_TEST_ENOSPC_DIR=/mnt/frost-full cargo test -p frostbuild-cli --test recovery
```

Or dispatch the workflow: `gh workflow run quality.yml -f shape=monorepo -f
soak_minutes=45`.
