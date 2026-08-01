# Developer loop: run, watch, restart and debugging

Frost's interactive path is a product surface, not just repeated `build`
invocations.

## Build and run without finding the output

```bash
frost run app
frost run app -- --port 3000
frost run device-app --platform aarch64 --runner qemu-aarch64 -- --flag
frost run app --print       # build, then print exact JSON argv only
```

`run` resolves exactly one target, builds it, finds its linked or first
declared artifact and executes direct argv. Native binaries run directly;
`.jar`, JavaScript and Python outputs select `java -jar`, Node and Python.
`JAVA_BIN`, `NODE_BIN` and `PYTHON_BIN` override runtime discovery. A foreign
platform is never executed accidentally: it requires an explicit `--runner`.
Wheels are installable artifacts and receive an actionable error instead of an
attempt to execute the ZIP.

## Watch and process restart

```bash
# Rebuild the default target after a 50 ms quiet period.
frost watch

# Rebuild one target with more action parallelism.
frost watch app -j 8 --debounce-ms 30

# Infer the runnable artifact, start it, and restart after successful builds.
frost dev app -- --port 3000

# Cross/device development requires an explicit emulator/runner.
frost dev device-app --platform aarch64 --runner qemu-aarch64

# Start a direct-argv development process after the first successful build,
# then restart it only after later successful builds. Put --run last.
frost watch web --run node .frost/out/debug/js/main.js --inspect=9229
```

The recursive `notify` watcher uses inotify on Linux and the corresponding
native backend on supported hosts. It coalesces editor rename/write bursts,
prints a compact tree of changed paths, and ignores read/open/close access
events, `.git`, Frost's internal tree, declared outputs and owned clean
directories. That prevents executed tool wrappers, builds and the action
materializer from retriggering the watch loop themselves.

`--run` is direct argv, not a shell string. Frost keeps the last successful
development process alive while a later build is broken, and replaces it only
after a successful rebuild. Restart stops the complete child process tree, not
only its top-level launcher. This is generic process-restart hot reload. A web
framework may provide browser-state-preserving HMR inside that process, but
Frost does not claim to implement Vite/Webpack's module-update protocol.

`dev` is the zero-path variant of the same loop: it requires exactly one
target, finds the produced native/JAR/JavaScript/Python artifact and applies
the same runtime inference as `frost run`. `--runner` supports emulation and
custom artifacts. The E2E deliberately uses an injected runner and proves that
it receives the inferred artifact on both the initial build and a source edit.

An existing Bazel workspace can use the same policy without migration via
`frost bazel-dev //package:target`; Bazel retains BUILD/Starlark, configured
graph, server, cache and runfiles ownership. See
[23_bazel_migration.md](23_bazel_migration.md).

## Native and language debugger launch

For native C/C++, `frost init` writes explicit profiles:

```toml
[profile.debug]
cflags = ["-O0", "-g"]

[profile.release]
cflags = ["-O3", "-DNDEBUG"]
```

Launch a native binary under the first available GDB or LLDB:

```bash
frost debug app
frost debug app --debugger lldb -- --example-flag value
FROST_DEBUGGER=/opt/tools/gdb frost debug app
frost debug app --print       # exact JSON argv, useful for IDE integration
```

The command resolves exactly one target, verifies that native compile actions
contain a recognized symbol flag, builds the selected profile/platform, finds
the link output and then launches the debugger. Missing symbols and missing
debuggers fail with an actionable message before an opaque debugger session.

Command artifacts select a language-native built-in debugger by extension:

```bash
frost debug java-service      # jdb, Main-Class read from the JAR manifest
frost debug web-app           # node inspect when its output is JavaScript
frost debug python-tool       # python -m pdb when its output is Python
frost debug web-app --print   # exact IDE/script-facing argv
```

Here the arguments name targets whose produced artifacts have those suffixes,
not arbitrary files. `JDB_BIN`, `NODE_BIN` and `PYTHON_BIN` override discovery;
`--debugger PATH` injects an exact executable while retaining the correct
language argument shape. JARs without `Main-Class` fail with a repair hint.

These are terminal debugger launchers. Browser DevTools session management,
TypeScript source-map generation, automatic extension installation and a
portable cross-IDE DAP contract remain open.

## VS Code handoff

```bash
frost ide app --dry-run
frost ide app
```

`ide` first builds one target, then derives `.vscode/tasks.json` and
`.vscode/launch.json` from the configured artifact. It emits `cppdbg`, Java,
Node or `debugpy` launch types and a process-type pre-launch build task. Java's
main class comes from the actual JAR manifest. Node `sourceMaps` is true only
when a `.map` file is part of the target closure, so missing TypeScript mapping
is visible rather than implied.

Existing `tasks.json` or `launch.json` is never overwritten. The command stops
and points to `--dry-run`, whose single JSON object is suitable for manual or
scripted merge. This generator supplies launch topology; the matching VS Code
debug extension (C/C++, Java, built-in Node, or Python/debugpy) still belongs
to the developer environment.

## Diagnose the machine before a build

```bash
frost doctor
frost doctor --profile release --platform aarch64
frost doctor --json
```

`doctor` loads the exact configured graph and separates prerequisites from
enhancements. The configured C/C++/archive drivers, shell, Kofun driver and all
named command tools are required and make the command nonzero when missing or
non-executable. `fzf`, GDB, LLDB, jdb/Java, Node, Python, bubblewrap and
Graphviz are reported as optional integrations; their absence does not make an
otherwise buildable workspace look broken. JSON carries the same distinction
for bootstrap scripts and CI images.

## Checked behavior

End-to-end tests drive the real filesystem watcher, edit a C source, observe a
second successful build and verify that the direct development process runs
again. A separate real-compiler test checks native `frost init`'s symbol
profile and the exact GDB-style argv delivered to an injected debugger. The
Java init E2E starts with only a packaged source, generates the manifest,
builds its deterministic executable JAR, runs it both through `java -jar` and
`frost run`, and checks its generated jdb classpath/main class. Mixed Java and
native sources are refused until `--language` makes the choice explicit. The
IDE E2E parses both generated files, checks the
pre-launch task reference and proves a second invocation refuses overwrite.
Doctor E2E covers both a fully buildable scaffold and a missing required named
tool while retaining optional-integration results.

## The version this repository requires

```bash
./frostw build             # runs the frost named in .frost-version
frost init --wrapper       # add the wrapper to a workspace that has a manifest
```

`frost init` writes `.frost-version`, `frostw` and `frostw.cmd` alongside the
manifest; `frost init --wrapper` adds only those three to a workspace that
already has one. Committing them makes the build instruction `./frostw build`
on every machine, instead of a README paragraph asking for a particular frost
first.

The wrapper prefers, in order: a `frost` already on `PATH` that reports the
declared version, a copy under `$FROST_HOME/versions/<version>` from an earlier
run, and finally that version's GitHub release — whose archive is verified
against the release's `SHA256SUMS` before it is unpacked, into a staging
directory that is renamed into place, so a rejected or truncated download
leaves nothing behind for the next run to trust. Every failure names what to
put where to continue by hand: no network, an unpublished version and a
checksum mismatch are all dead ends otherwise.

`.frost-version` is a file rather than a `frost.toml` key on purpose: reading
the manifest requires a frost, and which frost to run is the question being
asked. It names one exact version — no ranges, no `latest` — because the reason
to check it in is that two machines run the same build.

Running `frost` directly is not prevented. It warns, once, on stderr, naming
the declared version and this one, because before 1.0 a minor release may
change the manifest grammar and the resulting error is correct while saying
nothing about the version difference that caused it.

## Frost builds Frost

The repository's own `frost.toml` runs the pre-PR gate as five declared stages
and produces `frost` and `frostd` themselves:

```bash
./frostw test --all       # the gate, incrementally, needing no frost installed
./frostw test --all --explain   # which input made each stage rerun
./frostw build binaries   # frost and frostd, release
```

Cargo still owns crate resolution, feature unification and rustc invocation
order. Every stage wraps a whole `cargo`, `python3` or `npm` invocation with a
declared input set — the same boundary `sample_spring` and `sample_maven` draw
around Gradle and Maven — so Frost decides *whether* the invocation has to
happen, not *what* it does. That is what `scripts/check.sh` cannot do: it runs
all five every time, while a stage whose inputs did not move is a cached
success. Editing a `.rs` file reruns the three Rust stages and leaves the
Python and extension suites alone.

The manifest has no `[workspace]` section, deliberately: that would make Frost
discover the nested sample manifests and pull every sample workspace in as a
package of this one. Bare target names are the legacy single-manifest form, and
they are right for a repository whose subdirectories are not its packages.

The gate stages are `kind = "test"` because a stage has no artifact, it has a
verdict, and Frost owns the success stamp — a passing stage caches, a failing
one records nothing and runs again. `binaries` is the exception and produces
the artifact this repository ships.

[Task](https://taskfile.dev) supplies names for those command lines and nothing
else:

```bash
task check        # ./frostw test --all
task build        # ./frostw build binaries
task --list       # the rest
task bootstrap    # cargo build --release, for a machine with no frost at all
```

Every task is one line long on purpose. Task has no dependency graph and no
cache, `frost.toml` is where the deciding happens, and a task that grew logic
would be logic in the wrong file.

`scripts/check.sh` stays, and CI takes it: bootstrapping cannot depend on the
thing being bootstrapped, and a contributor with no network needs a gate that
`./frostw` cannot give them.

## Explain one build, in one file

```bash
frost build --report                       # .frost/report/<platform>-<profile>.html
frost build --report=build.html --trace t.json
frost test --all --report=tests.html
```

`--report` writes a self-contained HTML file: the critical path with each
action's measured duration, the cache breakdown per kind of work, the slowest
actions that ran, the invalidation reasons grouped by `--explain`'s vocabulary,
the test results including shards, and the failing actions with the tail of
their output. No server, no network, no JavaScript, no external stylesheet —
it opens from `file://` and survives being attached to a message.

The three views divide as follows. `--stats` is the counters, for a terminal.
`--trace` is the raw timeline, for `chrome://tracing`, and when both are asked
for the report links to it relatively so the pair can be copied together.
`--report` is the summary — the one meant to be handed to someone else, which
is what `chrome://tracing`'s "open this in the right tool first" makes a Chrome
trace bad at. Comparing *across* builds is not this file's job; that is
`frost journal export` / `diff`.

Nothing here is measured for the report's benefit. The critical path is the one
the scheduler used to order its ready queue, the durations are the journal's,
and the reasons are the strings `--explain` prints. Rendering happens after the
build has been timed, summarized and had its failures printed, so it cannot
move a number it goes on to show.

It does have one cost, and it is not the rendering. `--report` forgoes the
no-op certificate, because a certificate answers "nothing to do" without ever
planning a build and so has nothing to report. On a 1000-action workspace that
is about 10 ms on an otherwise 7 ms no-op; rendering itself is under a
millisecond there, and inside the noise floor at every larger size:

| scenario | build | rendering | `--report` total |
|---|---|---|---|
| clean | 2870 ms | +26 ms (+0.9%) | −73 ms (noise) |
| incremental leaf | 56 ms | +1.1 ms (+2.1%) | −2.0 ms (noise) |
| no-op | 7.3 ms | +0.3 ms (+1.7%) | +10.2 ms |

Medians of 15 interleaved iterations, 4 workers, from
`frost-bench report`; the run is `bench/baselines/2026-08-01-vm-report-overhead.json`,
with its host metadata. The "rendering" column compares against `--stats`,
which takes the same full check path without writing anything, so the
certificate's absence is not attributed to the renderer.

## The manifest, in an editor

```bash
frost lsp        # Language Server Protocol on stdin/stdout
```

`frost.toml` is edited as plain TOML today, so an editor knows nothing about
labels or kinds: a typo in `deps = ["//core:core"]` is valid TOML and stays
silent until a build says otherwise — correctly, in a terminal, which is not
where the cursor is. `frost lsp` provides:

- **diagnostics** — the manifest loader's own errors, byte for byte the
  sentence `frost build` prints, placed on the token the message names
- **completion** — labels across every package, `kind` values, `[toolchain.tools]`
  names, and the keys the target's kind accepts
- **definition** — a label to the `[target.<name>]` line that declares it, in
  whichever package that is
- **references** — `graph.rdeps_closure`, the function `frost query rdeps`
  calls, each dependent reported at its own declaration
- **hover** — kind, declared outputs, direct dependencies, and the size of the
  closure `frost query deps` prints

None of it is a second analysis. References and hover call the same functions
the query subcommands call, which is what
`frost_lsp_hover_and_references_are_the_answers_query_gives` enforces: a
disagreement there would mean the editor had grown its own idea of the graph,
and the editor's would be the untested one.

Two boundaries are worth knowing. The workspace is re-read on save, through the
graph store's warm path, so an unchanged tree costs a stamp check rather than a
parse of every manifest; cross-package errors therefore appear when a file is
saved, while syntax and per-target errors are reported against the buffer as it
is typed. And when a manifest does not parse, or a label names nothing, the
server keeps answering from the declarations that did load — that is the state
a manifest is in for most of the time it is being edited, and going quiet then
would be going quiet exactly when it is wanted.

The server implements no formatting (that is `frost fmt`'s to provide once it
exists), no rename, and nothing about files other than `frost.toml`; source
code belongs to its own language's server. Any LSP client works — the VS Code
extension in `tools/vscode/` is the first, and Neovim or a JetBrains IDE gets
the same features from the same server.
