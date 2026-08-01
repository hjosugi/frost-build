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

The repository's own `frost.toml` builds `frost` and `frostd` and runs the four
gates `scripts/check.sh` runs. [Task](https://taskfile.dev) supplies the names:

```bash
task              # build the binaries
task check        # every gate, skipping the ones whose inputs did not move
task --list       # the rest
task bootstrap    # cargo build --release, for a machine with no frost at all
```

The split between the two is deliberate. Task has no dependency graph and no
cache and does not pretend to: each task is a name for a command. `frost.toml`
is where the decision lives — declared inputs, a content-addressed journal, and
concurrency across independent gates — so `task check` is one command that asks
one question, rather than four commands in sequence.

Cargo still owns crate resolution, feature unification and rustc invocation
order; every target wraps a whole `cargo` or `python3` invocation with a
declared input set, the same boundary `sample_spring` and `sample_maven` draw
around Gradle and Maven. Frost decides *whether* the invocation has to happen,
not *what* it does. An unchanged tree answers in about a millisecond where the
four gates otherwise cost seconds each; a touched `.rs` file reruns a whole
gate, and Cargo's own incrementality is what makes that affordable.

`scripts/check.sh` stays, and CI takes it: it is the path that requires no
frost, which the bootstrap case still needs.
