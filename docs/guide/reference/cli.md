# `frost` command reference

<!-- Generated from the binary's own help by
     crates/frostbuild-cli/src/lib.rs (the_cli_reference_matches_help).
     Do not edit by hand; regenerate with
     UPDATE_CLI_REFERENCE=1 cargo test -p frostbuild-cli --lib cli_surface_tests -->

Every command's `--help`, exactly as `frost` prints it. A test fails when
this page and the binary disagree, so it is safe to read instead of
running the binary. The same text is installed as man pages
(`man frost-build`) by the release archives.

Options that change what is built (`--profile`, `--platform`, `--sandbox`,
...) can also be set in `.frostrc`; see the
[manifest reference](manifest.md#files-that-are-not-frosttoml).

- [`frost`](#frost)
- [`frost bazel-dev`](#frost-bazel-dev)
- [`frost build`](#frost-build)
- [`frost cache`](#frost-cache)
- [`frost cache stats`](#frost-cache-stats)
- [`frost clean`](#frost-clean)
- [`frost compdb`](#frost-compdb)
- [`frost completions`](#frost-completions)
- [`frost coverage-lcov`](#frost-coverage-lcov)
- [`frost daemon`](#frost-daemon)
- [`frost daemon restart`](#frost-daemon-restart)
- [`frost daemon start`](#frost-daemon-start)
- [`frost daemon status`](#frost-daemon-status)
- [`frost daemon stop`](#frost-daemon-stop)
- [`frost debug`](#frost-debug)
- [`frost dev`](#frost-dev)
- [`frost doctor`](#frost-doctor)
- [`frost explain`](#frost-explain)
- [`frost fetch`](#frost-fetch)
- [`frost fmt`](#frost-fmt)
- [`frost graph`](#frost-graph)
- [`frost ide`](#frost-ide)
- [`frost import-bazel`](#frost-import-bazel)
- [`frost import-ninja`](#frost-import-ninja)
- [`frost import-npm`](#frost-import-npm)
- [`frost info`](#frost-info)
- [`frost init`](#frost-init)
- [`frost journal`](#frost-journal)
- [`frost journal diff`](#frost-journal-diff)
- [`frost journal export`](#frost-journal-export)
- [`frost lint`](#frost-lint)
- [`frost lsp`](#frost-lsp)
- [`frost pack-jar`](#frost-pack-jar)
- [`frost pack-wheel`](#frost-pack-wheel)
- [`frost pick`](#frost-pick)
- [`frost plan`](#frost-plan)
- [`frost query`](#frost-query)
- [`frost query allpaths`](#frost-query-allpaths)
- [`frost query deps`](#frost-query-deps)
- [`frost query owners`](#frost-query-owners)
- [`frost query rdeps`](#frost-query-rdeps)
- [`frost query somepath`](#frost-query-somepath)
- [`frost query targets`](#frost-query-targets)
- [`frost run`](#frost-run)
- [`frost self-update`](#frost-self-update)
- [`frost simulate`](#frost-simulate)
- [`frost test`](#frost-test)
- [`frost watch`](#frost-watch)

## `frost`

```text
frostbuild: correct, fast incremental builds

Usage: frost [OPTIONS] <COMMAND>

Commands:
  fetch          Download and materialize manifest-pinned archives
  build          Build targets (default: workspace default_targets)
  run            Build one target and execute its native or language artifact
  watch          Rebuild on source/manifest changes and optionally restart a dev process
  dev            Watch one runnable target and restart its inferred artifact on success
  debug          Build one target and launch its native or language debugger
  ide            Build one target and generate VS Code build/debug configuration
  doctor         Diagnose workspace, required tools and optional developer integrations
  test           Build and run test/cc_test targets
  plan           Show which actions would run and why, without executing anything
  clean          Remove build outputs (--cache also removes the journal and hash cache)
  graph          Print the target dependency graph
  compdb         Export JSON Compilation Database for clangd/IDE integrations
  coverage-lcov  Merge gcov coverage data into an lcov tracefile
  explain        Explain the most recently recorded decision for a target
  init           Write a safe native C/C++ or Java starter frost.toml from sources here
  simulate       Compare scheduling strategies without building anything
  query          Query the target dependency graph (configuration-free)
  cache          Inspect local content-addressed cache storage and chunk reuse
  fmt            Rewrite frost.toml in its canonical form
  lint           Report manifest patterns that build but cost something later
  journal        Explain why a build reused a result or did not
  daemon         Manage the per-workspace build daemon
  import-ninja   Convert the supported Ninja rule/build subset to frost.toml
  import-bazel   Import a conservative native C/C++ subset from Bazel query XML
  import-npm     Import npm workspace validation gates and explicit Vite build boundaries
  bazel-dev      Watch, incrementally build, and restart a Bazel runnable target
  pack-jar       Pack a directory into a deterministic compressed Java archive
  pack-wheel     Pack a pure-Python source tree into a deterministic standards-compliant wheel
  lsp            Speak the Language Server Protocol for frost.toml on stdin/stdout
  info           Report workspace, output and cache locations for scripts and editors
  completions    Generate completion code for a shell, or install the dynamic hook
  self-update    Check for or install the latest verified GitHub release
  pick           Select build or test targets interactively with fzf

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help

  -V, --version
          Print version
```

## `frost bazel-dev`

```text
Watch, incrementally build, and restart a Bazel runnable target

Usage: frost bazel-dev [OPTIONS] <TARGET> [-- <ARGS>...]

Arguments:
  <TARGET>
          Canonical Bazel runnable label, for example //app:server

  [ARGS]...
          Arguments passed to the target after `--`

Options:
      --bazel <BAZEL>
          Bazel or Bazelisk executable (defaults to BAZEL_BIN, bazel, bazelisk)

  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --debounce-ms <DEBOUNCE_MS>
          Quiet period used to coalesce editor filesystem events

          [default: 50]

      --bazel-arg <BAZEL_ARGS>
          Build option forwarded to both `bazel build` and `bazel run`

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost build`

```text
Build targets (default: workspace default_targets)

Usage: frost build [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>
          Number of parallel jobs (default: number of CPUs)

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --local-cpu-resources <N>
          CPU admission tokens available to local actions (default: host CPUs)

      --local-ram-resources <MIB>
          RAM admission budget in MiB (default: physical host RAM)

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --local-test-jobs <N>
          Maximum concurrently running test actions (default: -j)

  -k, --keep-going
          Keep building independent actions after a failure

      --explain
          After the build, print why each action ran or was cached

  -v, --verbose
          Print full command lines as they run

      --profile <PROFILE>
          Build profile; outputs and caches are isolated per profile

          [default: debug]

      --platform <PLATFORM>
          Target platform from [platform.<name>] for cross/device builds; outputs and caches are isolated per platform

          [default: host]

      --all-platforms
          Build host and every declared [platform.*] configuration

      --no-cache
          Disable successful test-result cache

      --no-stamp
          Skip the workspace's [stamp] command. Every ${stamp.KEY} then expands to nothing, which changes the action key of anything that reads a stable value — a stamp-free build is a different build, and says so rather than reusing results that embedded a value

      --stamp-optional
          A failing [stamp] command leaves the values empty instead of failing the build. Off by default: a status script that stopped working should be noticed, not silently ship a binary with no version in it

      --remote-cache <ENDPOINT>
          Shared cache consulted when the local journal misses: a directory path, file:///path, or http://host/prefix. Never required for correctness — every response is verified and any failure falls back to building locally

      --remote-upload
          Also publish what this build produces to --remote-cache

      --remote-timeout <SECONDS>
          Seconds to wait for one remote cache request

          [default: 10]

      --sandbox
          Isolate actions from undeclared workspace files with bubblewrap (Linux). `--hermetic` reaches the same verdict on every host

      --hermetic
          Run each action in a private tree holding only the files it may read, so an undeclared input fails the build on any host. No extra tool; not a security boundary

      --materialize <STRATEGY>
          How --hermetic places inputs in each action's tree: the host's first working strategy, or one named (`frost doctor` shows which work here)

          Possible values:
          - auto:     The first of the host's order that works where the trees live
          - reflink:  A copy-on-write clone (Linux FICLONE on btrfs/XFS, macOS APFS clonefile)
          - hardlink: A hard link; a write through it is detected and fails the action
          - copy:     A byte copy, which always works

          [default: auto]

      --check-determinism[=<CHECK_DETERMINISM>]
          Execute each selected action twice and compare output digests

      --timeout <SECONDS>
          Stop any action still running after this many seconds. A target's own `timeout` wins; tests carry a default limit without this flag

      --trace <TRACE>
          Write a Chrome/Perfetto trace JSON

      --report[=<PATH>]
          Write a self-contained HTML report of this build; `--report=PATH` chooses where, plain `--report` writes under .frost/report/

      --stats
          Report scheduling measurements: makespan, worker utilization and distance from the estimated critical path

      --no-tui
          Disable the interactive terminal UI and print plain progress lines

      --daemon
          Execute through the per-workspace frostd service

      --scheduler <SCHEDULER>
          [default: critical-path]
          [possible values: critical-path, fifo]

      --estimator <ESTIMATOR>
          [default: journal]
          [possible values: heuristic, journal, static, learned]

  -h, --help
          Print help (see a summary with '-h')
```

## `frost cache`

```text
Inspect local content-addressed cache storage and chunk reuse

Usage: frost cache [OPTIONS] <COMMAND>

Commands:
  stats  Report blob/chunk storage and persistent deduplication ratios

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost cache stats`

```text
Report blob/chunk storage and persistent deduplication ratios

Usage: frost cache stats [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --json
          Emit one machine-readable JSON object

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost clean`

```text
Remove build outputs (--cache also removes the journal and hash cache)

Usage: frost clean [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --cache


      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>


      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>


      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost compdb`

```text
Export JSON Compilation Database for clangd/IDE integrations

Usage: frost compdb [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --output <OUTPUT>
          [default: compile_commands.json]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost completions`

```text
Generate completion code for a shell, or install the dynamic hook

Usage: frost completions [OPTIONS] [SHELL]

Arguments:
  [SHELL]
          Omit with --install to detect the shell from $SHELL

          [possible values: bash, zsh, fish, powershell, elvish, nushell]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --install
          Add the workspace-aware completion hook to this shell's startup file

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --dry-run
          Print what --install would write without touching any file

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost coverage-lcov`

```text
Merge gcov coverage data into an lcov tracefile

Reads every `.gcda` a run produced, pairs it with the `.gcno` the compile wrote, and emits lcov. frost emits the format itself because neither `lcov` nor `gcovr` ships with a toolchain, and delegating would put a Perl dependency in every CI image that wanted coverage.

Usage: frost coverage-lcov [OPTIONS] --gcda <GCDA> --objects <OBJECTS> --output <OUTPUT>

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --gcda <GCDA>
          Directory holding the run's `.gcda` counter files

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --objects <OBJECTS>
          Object tree holding the matching `.gcno` notes files

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --output <OUTPUT>
          Where to write the tracefile

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --gcov <GCOV>
          gcov executable, when it is not the one on PATH

          [default: gcov]

  -h, --help
          Print help (see a summary with '-h')
```

## `frost daemon`

```text
Manage the per-workspace build daemon

Usage: frost daemon [OPTIONS] <COMMAND>

Commands:
  start
  status
  stop
  restart

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost daemon restart`

```text
Usage: frost daemon restart [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost daemon start`

```text
Usage: frost daemon start [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost daemon status`

```text
Usage: frost daemon status [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --json
          Emit one machine-readable JSON object. A stopped daemon is a state, not a command failure, when this form is requested

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost daemon stop`

```text
Usage: frost daemon stop [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost debug`

```text
Build one target and launch its native or language debugger

Usage: frost debug [OPTIONS] [TARGET] [-- <PROGRAM_ARGS>...]

Arguments:
  [TARGET]


  [PROGRAM_ARGS]...
          Arguments passed to the program being debugged (after `--`)

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>


      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --debugger <DEBUGGER>
          Debugger/runtime executable, or auto for GDB/LLDB, jdb, Node or pdb

          [default: auto]

      --print
          Print the exact debugger argv without launching it

  -h, --help
          Print help
```

## `frost dev`

```text
Watch one runnable target and restart its inferred artifact on success

Usage: frost dev [OPTIONS] [TARGET] [-- <PROGRAM_ARGS>...]

Arguments:
  [TARGET]


  [PROGRAM_ARGS]...
          Arguments passed to the restarted program (after `--`)

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>


      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --debounce-ms <DEBOUNCE_MS>
          [default: 50]

      --runner <RUNNER>
          Explicit executable prefix for cross/emulated or custom artifacts

  -h, --help
          Print help
```

## `frost doctor`

```text
Diagnose workspace, required tools and optional developer integrations

Usage: frost doctor [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --profile <PROFILE>
          [default: debug]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --platform <PLATFORM>
          [default: host]

      --json
          Emit machine-readable JSON

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost explain`

```text
Explain the most recently recorded decision for a target

Usage: frost explain [OPTIONS] <TARGET>

Arguments:
  <TARGET>


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --profile <PROFILE>
          [default: debug]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --platform <PLATFORM>
          [default: host]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost fetch`

```text
Download and materialize manifest-pinned archives

Usage: frost fetch [OPTIONS] [NAMES]...

Arguments:
  [NAMES]...
          Fetch names (default: every [fetch.*] declaration)

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --force
          Download again even when the current materialization is valid

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --offline
          Never access the network; fail if a requested fetch is missing

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost fmt`

```text
Rewrite frost.toml in its canonical form

Usage: frost fmt [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --check
          Report whether anything would change and exit non-zero if so, without writing. For CI

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost graph`

```text
Print the target dependency graph

Usage: frost graph [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --dot
          Emit Graphviz dot instead of text

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost ide`

```text
Build one target and generate VS Code build/debug configuration

Usage: frost ide [OPTIONS] [TARGET]

Arguments:
  [TARGET]


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>


      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --output <OUTPUT>
          Workspace-relative VS Code directory

          [default: .vscode]

      --dry-run
          Print the generated file map without writing it

  -h, --help
          Print help
```

## `frost import-bazel`

```text
Import a conservative native C/C++ subset from Bazel query XML

Usage: frost import-bazel [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --query <QUERY>
          Bazel query expression to import

          [default: //...]

      --bazel <BAZEL>
          Bazel or Bazelisk executable (defaults to BAZEL_BIN, bazel, bazelisk)

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --dry-run
          Print every generated manifest without writing

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost import-ninja`

```text
Convert the supported Ninja rule/build subset to frost.toml

Usage: frost import-ninja [OPTIONS] [NINJA]

Arguments:
  [NINJA]
          [default: build.ninja]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --output <OUTPUT>
          [default: frost.toml]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost import-npm`

```text
Import npm workspace validation gates and explicit Vite build boundaries

Usage: frost import-npm [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --script <SCRIPTS>
          Non-interactive validation script to import; repeat or comma-separate

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --vite-builds
          Also import recognized `vite build` scripts with profile-specific dist trees

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --npm <NPM>
          npm executable recorded as a fingerprinted named tool

          [default: npm]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --node <NODE>
          Node executable recorded with npm's toolchain closure

          [default: node]

      --dry-run
          Print the generated root manifest without writing it

  -h, --help
          Print help
```

## `frost info`

```text
Report workspace, output and cache locations for scripts and editors

Usage: frost info [OPTIONS] [KEY]

Arguments:
  [KEY]
          Print only this key's value; omit for the whole table

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --profile <PROFILE>
          [default: debug]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --platform <PLATFORM>
          [default: host]

      --json


      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost init`

```text
Write a safe native C/C++ or Java starter frost.toml from sources here

Usage: frost init [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --dry-run
          Print the manifest instead of writing it

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --language <LANGUAGE>
          Source family; omit to auto-detect (mixed families require a choice)

          [possible values: native, java, rust, go, typescript, python]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --wrapper
          Write only frostw, frostw.cmd and .frost-version, pinned to this frost, into a workspace that already has a manifest

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost journal`

```text
Explain why a build reused a result or did not

Usage: frost journal [OPTIONS] <COMMAND>

Commands:
  export  Write this build's action-key material: argv, environment, input digests, toolchain, profile and platform, in a stable order
  diff    Compare two exports and report, per action, the first field that differs — the cause, not every consequence of it

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost journal diff`

```text
Compare two exports and report, per action, the first field that differs — the cause, not every consequence of it

Usage: frost journal diff [OPTIONS] <FIRST> <SECOND>

Arguments:
  <FIRST>


  <SECOND>


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost journal export`

```text
Write this build's action-key material: argv, environment, input digests, toolchain, profile and platform, in a stable order

Usage: frost journal export [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --out <FILE>
          Where to write it. Defaults to stdout

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost lint`

```text
Report manifest patterns that build but cost something later

Usage: frost lint [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --json
          Emit findings as one machine-readable JSON object

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost lsp`

```text
Speak the Language Server Protocol for frost.toml on stdin/stdout

Usage: frost lsp [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost pack-jar`

```text
Pack a directory into a deterministic compressed Java archive

Usage: frost pack-jar [OPTIONS] --input <INPUT> --output <OUTPUT>

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --input <INPUT>
          Workspace-relative directory whose contents become JAR entries

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --output <OUTPUT>
          Workspace-relative output JAR

      --main-class <MAIN_CLASS>
          Optional Java binary name for the Main-Class manifest attribute

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost pack-wheel`

```text
Pack a pure-Python source tree into a deterministic standards-compliant wheel

Usage: frost pack-wheel [OPTIONS] --input <INPUT> --distribution <DISTRIBUTION> --version <VERSION> --output <OUTPUT>

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --input <INPUT>
          Workspace-relative source root whose contents install into purelib

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --distribution <DISTRIBUTION>
          Python distribution name written to wheel metadata

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --version <VERSION>
          Normalized numeric Python release version (for example 1.2.3)

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --output <OUTPUT>
          Workspace-relative output wheel (must use the standard wheel filename)

  -h, --help
          Print help
```

## `frost pick`

```text
Select build or test targets interactively with fzf

Usage: frost pick [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --tests
          Select only test targets and run `frost test`

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --print
          Print selected labels instead of building

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --profile <PROFILE>
          [default: debug]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --platform <PLATFORM>
          [default: host]

  -h, --help
          Print help
```

## `frost plan`

```text
Show which actions would run and why, without executing anything

Usage: frost plan [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --profile <PROFILE>
          [default: debug]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --platform <PLATFORM>
          [default: host]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost query`

```text
Query the target dependency graph (configuration-free)

Usage: frost query [OPTIONS] <COMMAND>

Commands:
  deps      Transitive dependencies of a target (itself included)
  rdeps     Targets that transitively depend on a target ("what does this affect?")
  somepath  One dependency path between two targets
  allpaths  Every dependency path between two targets ("what would I have to cut?")
  targets   Every target in the workspace
  owners    Targets that declare these files among their action inputs

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost query allpaths`

```text
Every dependency path between two targets ("what would I have to cut?")

Usage: frost query allpaths [OPTIONS] <FROM> <TO>

Arguments:
  <FROM>


  <TO>


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --limit <LIMIT>
          Stop after this many paths. The count is exponential on a graph of stacked diamonds, so the walk is bounded and says when it stopped

          [default: 4096]

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --kind <KIND>
          Keep only targets of this kind (cc_binary, cc_library, cc_test, genrule, test, kofun_binary, command)

      --attr <NAME=PATTERN>
          Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable; every one must match. NAME is deps, srcs, outputs, sandbox or timeout

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --output <OUTPUT>
          Output format

          [possible values: text, json, label-kind, dot]

      --json
          Alias for --output json, kept for compatibility

  -h, --help
          Print help
```

## `frost query deps`

```text
Transitive dependencies of a target (itself included)

Usage: frost query deps [OPTIONS] <TARGET>

Arguments:
  <TARGET>


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --kind <KIND>
          Keep only targets of this kind (cc_binary, cc_library, cc_test, genrule, test, kofun_binary, command)

      --attr <NAME=PATTERN>
          Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable; every one must match. NAME is deps, srcs, outputs, sandbox or timeout

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --output <OUTPUT>
          Output format

          [possible values: text, json, label-kind, dot]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --json
          Alias for --output json, kept for compatibility

  -h, --help
          Print help
```

## `frost query owners`

```text
Targets that declare these files among their action inputs

Usage: frost query owners [OPTIONS] <PATHS>...

Arguments:
  <PATHS>...
          Workspace-relative paths or globs

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --kind <KIND>
          Keep only targets of this kind (cc_binary, cc_library, cc_test, genrule, test, kofun_binary, command)

      --attr <NAME=PATTERN>
          Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable; every one must match. NAME is deps, srcs, outputs, sandbox or timeout

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --output <OUTPUT>
          Output format

          [possible values: text, json, label-kind, dot]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --json
          Alias for --output json, kept for compatibility

  -h, --help
          Print help
```

## `frost query rdeps`

```text
Targets that transitively depend on a target ("what does this affect?")

Usage: frost query rdeps [OPTIONS] <TARGET>

Arguments:
  <TARGET>


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --kind <KIND>
          Keep only targets of this kind (cc_binary, cc_library, cc_test, genrule, test, kofun_binary, command)

      --attr <NAME=PATTERN>
          Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable; every one must match. NAME is deps, srcs, outputs, sandbox or timeout

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --output <OUTPUT>
          Output format

          [possible values: text, json, label-kind, dot]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --json
          Alias for --output json, kept for compatibility

  -h, --help
          Print help
```

## `frost query somepath`

```text
One dependency path between two targets

Usage: frost query somepath [OPTIONS] <FROM> <TO>

Arguments:
  <FROM>


  <TO>


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --kind <KIND>
          Keep only targets of this kind (cc_binary, cc_library, cc_test, genrule, test, kofun_binary, command)

      --attr <NAME=PATTERN>
          Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable; every one must match. NAME is deps, srcs, outputs, sandbox or timeout

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --output <OUTPUT>
          Output format

          [possible values: text, json, label-kind, dot]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --json
          Alias for --output json, kept for compatibility

  -h, --help
          Print help
```

## `frost query targets`

```text
Every target in the workspace

The one query with no starting point. `deps` and `rdeps` both need a target to walk from, which makes "what is in this workspace" the question they cannot answer — tooling was deriving it from the roots of `--output dot`, which encodes kind in a node *shape* and is a rendering choice rather than a contract.

Usage: frost query targets [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --kind <KIND>
          Keep only targets of this kind (cc_binary, cc_library, cc_test, genrule, test, kofun_binary, command)

      --attr <NAME=PATTERN>
          Keep only targets whose attribute matches, as NAME=PATTERN. Repeatable; every one must match. NAME is deps, srcs, outputs, sandbox or timeout

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --output <OUTPUT>
          Output format

          [possible values: text, json, label-kind, dot]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --json
          Alias for --output json, kept for compatibility

  -h, --help
          Print help (see a summary with '-h')
```

## `frost run`

```text
Build one target and execute its native or language artifact

Usage: frost run [OPTIONS] [TARGET] [-- <PROGRAM_ARGS>...]

Arguments:
  [TARGET]


  [PROGRAM_ARGS]...
          Arguments passed to the built program (after `--`)

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>


      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --runner <RUNNER>
          Explicit executable prefix for cross/emulated or custom artifacts

      --print
          Print the exact direct argv without executing it

  -h, --help
          Print help
```

## `frost self-update`

```text
Check for or install the latest verified GitHub release

Usage: frost self-update [OPTIONS]

Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --check
          Report whether a newer release exists without replacing this binary

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

  -h, --help
          Print help
```

## `frost simulate`

```text
Compare scheduling strategies without building anything

Usage: frost simulate [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

      --jobs <JOBS>
          Worker counts to sweep (default: 1,2,4,8,16 capped at this host)

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --local-cpu-resources <N>
          CPU admission tokens available to local actions (default: host CPUs)

      --local-ram-resources <MIB>
          RAM admission budget in MiB (default: physical host RAM)

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --local-test-jobs <N>
          Maximum concurrently simulated test actions (default: point's -j)

      --profile <PROFILE>
          [default: debug]

      --platform <PLATFORM>
          [default: host]

      --json


  -h, --help
          Print help
```

## `frost test`

```text
Build and run test/cc_test targets

Usage: frost test [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>


      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --local-cpu-resources <N>
          CPU admission tokens available to local actions (default: host CPUs)

      --local-ram-resources <MIB>
          RAM admission budget in MiB (default: physical host RAM)

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --local-test-jobs <N>
          Maximum concurrently running test actions (default: -j)

  -k, --keep-going


      --affected


      --predictive


      --all


      --no-cache


      --no-stamp
          Skip the workspace's [stamp] command. Every ${stamp.KEY} then expands to nothing, which changes the action key of anything that reads a stable value — a stamp-free build is a different build, and says so rather than reusing results that embedded a value

      --stamp-optional
          A failing [stamp] command leaves the values empty instead of failing the build. Off by default: a status script that stopped working should be noticed, not silently ship a binary with no version in it

      --test-filter <PATTERN>
          Run only cases matching this pattern. Passed to the runner through TESTBRIDGE_TEST_ONLY and GTEST_FILTER, and part of the action key, so a filtered run is a separate result rather than one that satisfies an unfiltered request

      --test-env <KEY=VALUE>
          Set an environment variable for every test, as KEY=VALUE. Overrides a manifest value of the same name, and participates in the action key

      --test-arg <ARG>
          Append an argument to every test's command line. Participates in the action key. Hyphens are allowed, since a runner's own flags are the usual thing to pass here

      --runs-per-test <N>
          Run every test this many times, requiring all runs to pass. Does not read the cache — a recorded single pass cannot answer whether a test passes repeatedly — and suppresses `flaky_retries`, which would otherwise hide the failures this is looking for

          [default: 1]

      --test-output <TEST_OUTPUT>
          How much test output to show: `summary` for the counts alone, `errors` for failing tests replayed after the run, `all` for everything including passing tests

          Possible values:
          - summary: Counts only. For a run whose result is the exit code
          - errors:  Failing tests, replayed after the run so the log that matters is the last thing on screen rather than scrolled away by later work
          - all:     Everything, passing tests included

          [default: errors]

      --remote-cache <ENDPOINT>
          Shared cache consulted when the local journal misses: a directory path, file:///path, or http://host/prefix. Never required for correctness — every response is verified and any failure falls back to building locally

      --remote-upload
          Also publish what this build produces to --remote-cache

      --remote-timeout <SECONDS>
          Seconds to wait for one remote cache request

          [default: 10]

      --explain


      --report[=<PATH>]
          Write a self-contained HTML report of this run; `--report=PATH` chooses where, plain `--report` writes under .frost/report/

      --timeout <SECONDS>
          Stop any test still running after this many seconds; overrides the default limit and is itself overridden by a target's own `timeout`

      --profile <PROFILE>
          [default: debug]

      --platform <PLATFORM>
          [default: host]

      --all-platforms
          Test host and every declared [platform.*] configuration

      --coverage
          Build instrumented for coverage and write an lcov tracefile per test target under .frost/coverage. A separate configuration: its own output tree, journal identity and cache, so an ordinary build is neither disturbed by it nor able to satisfy it from cache. C/C++ only, with gcc's gcov

      --sandbox
          Isolate actions from undeclared workspace files with bubblewrap (Linux). `--hermetic` reaches the same verdict on every host

      --hermetic
          Run each action in a private tree holding only the files it may read, so an undeclared input fails the build on any host. No extra tool; not a security boundary

      --materialize <STRATEGY>
          How --hermetic places inputs in each action's tree: the host's first working strategy, or one named (`frost doctor` shows which work here)

          Possible values:
          - auto:     The first of the host's order that works where the trees live
          - reflink:  A copy-on-write clone (Linux FICLONE on btrfs/XFS, macOS APFS clonefile)
          - hardlink: A hard link; a write through it is detected and fails the action
          - copy:     A byte copy, which always works

          [default: auto]

      --no-tui
          Disable the interactive terminal UI and print plain progress lines

      --daemon


      --scheduler <SCHEDULER>
          [default: critical-path]
          [possible values: critical-path, fifo]

      --estimator <ESTIMATOR>
          [default: journal]
          [possible values: heuristic, journal, static, learned]

  -h, --help
          Print help (see a summary with '-h')
```

## `frost watch`

```text
Rebuild on source/manifest changes and optionally restart a dev process

Usage: frost watch [OPTIONS] [TARGETS]...

Arguments:
  [TARGETS]...


Options:
  -C, --workspace <WORKSPACE>
          Workspace root (frost.toml for Frost commands; Bazel workspace for bazel-dev)

          [default: .]

  -j, --jobs <JOBS>
          Number of parallel build jobs (default: number of CPUs)

      --config <NAME>
          Apply a named `[config.NAME]` section from `.frostrc`. Repeatable; applied in the order given

      --profile <PROFILE>
          [default: debug]

      --no-frostrc
          Ignore `.frostrc` entirely, so only the command line and built-in defaults apply

      --platform <PLATFORM>
          [default: host]

      --build-event-json <FILE>
          Write one JSON object per line describing the build, for CI and dashboards. Independent of the terminal output

      --debounce-ms <DEBOUNCE_MS>
          Quiet period used to coalesce editor save events

          [default: 50]

      --run <RUN>...
          Direct argv to start after a successful build and restart on success; place this option last when its arguments begin with '-'

  -h, --help
          Print help
```
