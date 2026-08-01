# Changelog

All notable changes follow Keep a Changelog and Semantic Versioning. Before
1.0, minor versions may contain breaking manifest or CLI changes.

## [Unreleased]

### Changed

- Failure messages answer the next question, not just the current one. A
  mistyped target now offers up to three candidates instead of one, compares
  labels by their parts so `//apps/cli:cl` suggests `//apps/cli:cli` rather than
  whatever shares the longest prefix, and points at `frost query deps //...`
  instead of printing a wall of names when a workspace has many. An unknown
  manifest key adds `did you mean \`srcs\`?` to serde's list of all twenty-three
  accepted keys. A tool frost cannot find now says which `[toolchain]` line
  declared it, how many PATH entries were searched, which targets need it, and
  to run `frost doctor` — previously it said only that the tool was not on PATH.

- The test summary counts a skipped test as skipped rather than failed. A test
  that never ran because something upstream failed was reported as a failure,
  which blames the wrong file; the exit code is unchanged, since a build with
  skipped work still does not succeed.

- The site's stylesheet keeps its scales in one place. It held 51 font-size
  declarations with 26 distinct rem values, 12 letter-spacing declarations with
  12 distinct values, and 8/9/10px sitting beside 15/16/17/18px — values
  differing by as little as 0.01rem, which is accretion rather than intent.
  Those are now a type scale, a 4px spacing scale, a radius scale, a tracking
  scale and named leadings, verified against the rendered page: the largest
  move is 0.64px of type, 0.14px of tracking and 2px of corner, and no element's
  leading ratio changed. Display leadings and the negative tracking on large
  headings are named rather than merged, because those are set by eye per size.
  A test keeps it consolidated, since the next hurried change adds `0.77rem`
  and nothing notices; the one literal that remains is an `em` that must scale
  with the metric beside it, and it says so. Documentation keeps its reusable
  gaps and padding on the spacing scale while naming the large layout rhythm
  separately.

### Added

- `--build-event-json FILE`: one JSON object per line describing the build, so
  a CI job can count failures, chart durations or find the slow target without
  parsing terminal output. It is a third consumer of the progress stream the
  renderers already use, written by the renderer thread itself, so a dashboard
  and a human cannot disagree about what happened. `result` names (`cached`,
  `executed`, `flaky`, `failed`, `skipped`, `would_run`, `may_run`) are stable
  and deliberately not the display strings — the terminal says "cache miss" for
  an action that ran, which is right on a terminal and wrong in a field a
  machine switches on. Every line carries `schema` and a sequence number.

- `scripts/frost_junit.py`, which turns that stream into the JUnit XML CI
  systems already render, plus a Markdown summary for `$GITHUB_STEP_SUMMARY`. A
  script rather than a subcommand, because the shape of a test report belongs to
  the CI system reading it. Two of its decisions exist to stop it reporting a
  green build that was not: a non-test action that failed is reported too, since
  a build that broke before any test ran otherwise produces zero tests and zero
  failures, and a fully cached rerun — which emits one `all_cached` event and no
  per-action events — becomes one passing case that says so. A retried pass is a
  `flakyFailure` and a `system-out` line, so a viewer that has never heard of the
  element still shows the pass was not free; shards keep their `#0/3` marker,
  since same-named cases are deduplicated by most viewers. A stream whose
  `schema` it does not know is refused rather than guessed at; unknown events and
  fields are ignored, and an unknown `result` is an error rather than a pass. A
  CI job runs it on `sample_multi`, so the rendering is something you can look at
  rather than something the documentation asserts.

- `[target.NAME.platform.PLAT]` sections: the minimal form of a configurable
  attribute. `[platform.*]` could already swap a toolchain but not a *source*,
  which is the difference C/C++ workspaces hit first — one file for POSIX,
  another for the device. A section may set `srcs`, `deps` and `includes`, which
  replace, and `cflags`/`ldflags`, which append; flags already accumulate through
  toolchain, profile and target, so appending is that rule one level down rather
  than a new one. `kind`, `outputs` and `tool` are deliberately absent: a
  platform may change what a target is built from, never what it is, so
  `frost query` answers the same question whatever you are building for.

  No predicate language, and a section naming a platform the workspace never
  declared is refused at load with a suggestion — an overlay under a misspelled
  name would otherwise sit in the manifest looking applied and never fire, and
  the symptom is a cross build quietly compiling the wrong sources. Deps declared
  in a section are checked for existence and visibility like any other, so a
  boundary cannot hold on one platform and not another. `frost plan` names the
  sections that applied.

- `visibility` on targets, and named `[visibility.*]` groups in the root
  manifest. Multi-package labels let a workspace split into modules; this is what
  makes those modules mean something, enforced when the manifest loads rather
  than when something is built. Four spellings — `//...`, `//pkg/...`,
  `//pkg:target`, `group:NAME` — and `//pkg` on its own is refused, because it is
  one character from `//pkg/...` and means something else. A target is always
  visible inside its own package.

  The default is public, unlike Bazel. A private-by-default rule would break
  every existing workspace on upgrade, and a correctness feature that arrives as
  a wall of errors is one people turn off, after which it protects nothing.
  Instead a new `undeclared-visibility` lint names the targets where a boundary
  is *already* being crossed, so the migration is a list rather than a flag day;
  `sample_multi` now declares its own. Not action-key material, so declaring a
  boundary costs no rebuild.

- Build stamping: a `[stamp]` section whose `command` prints `KEY=VALUE` lines,
  and `${stamp.KEY}` expansion in a command target's `args` and `env`. The split
  is by rate of change and is decided by the key's *name*, so frost can classify
  a reference without running the command and the graph stays a pure function of
  the manifest. `STABLE_*` values are action-key material — a new commit
  rebuilding the binary that embeds its SHA is the correct answer, not cache
  thrash. Everything else is not: a wall clock in an action key would rebuild the
  workspace every second, so an action that reads one is re-executed
  unconditionally instead, costing one action rather than the graph above it.
  A volatile value that reaches a *compile* is rejected when the graph loads,
  since that turns one unconditional action into a full rebuild — the symptom
  ("our builds stopped being incremental") otherwise appears months later and
  nowhere near the manifest that caused it. The command runs only when something
  in the closure reads a stamp, fails the build when it fails, and is skipped by
  `--no-stamp` or downgraded to a warning by `--stamp-optional`.

### Fixed

- `frost explain TARGET --platform NAME` names the platform it is describing.
  It reported only the profile, so `explain app` and
  `explain app --platform device` printed the same sentence for two different
  builds and a reader could not tell which one they had been told about. The
  lookup was always correct — a device query against a host-only build reports
  no record, as it should — only the label was incomplete. It now reads
  `(device/debug)`, the spelling the output tree and the journal already use.

### Changed

- `ProgressState` gained a `Flaky` variant. A retried-and-passed test was
  reported as `Executed` with the flakiness surviving only in a human-facing
  detail string, which no machine consumer could read without parsing prose.

### Added

- `.frostrc`, a config file for *how* to build, keeping it out of `frost.toml`,
  which says *what* to build. `~/.config/frost/frostrc` then `<workspace>/.frostrc`,
  with `[common]`, a section per subcommand, and named `[config.NAME]` sections
  selected by a repeatable `--config NAME`. Precedence runs built-in default <
  user file < workspace file < named section < what you typed, and `--no-frostrc`
  ignores both files. `frost doctor` lists every setting in effect with its file,
  line and section. A key no subcommand accepts is refused at startup with the
  file, the line, the key and a suggestion, checked against the real argument
  tree so a new option works in a config file the moment it exists on the command
  line. A flag from a file is spliced ahead of the real command line and parsed
  by the same code that parses a typed one, so it validates and reaches the
  action key identically — whether an option is key material is a property of the
  option, never of where its value came from.

- `frost fmt` and `frost fmt --check`, which rewrite every manifest in the
  workspace in one canonical form: a fixed key order inside each target, targets
  in name order, and arrays inline when short and one-per-line when long.
  Comments and string contents are preserved — a comment explains a decision the
  keys cannot, and a formatter that dropped them is one nobody runs twice. Two
  properties are tested rather than claimed: formatting is idempotent, without
  which `--check` could fail on its own output, and it never changes what the
  manifest means, which is the property reordering keys and tables could
  plausibly break.

- `frost lint`, and a `lint_allow` manifest key. It reports patterns that parse,
  build and cost something later: a target nothing reaches, an `-I` pointing at
  a directory nothing creates, a `pass_env` naming a variable that is
  deliberately outside the action key (so nothing that target builds is ever
  shared between machines), an absolute path in `args`/`cmd`/`env` where nothing
  else validates it, and shell metacharacters in a genrule `cmd` that mean
  different things under `/bin/sh` and `cmd.exe`. Exits 1 on findings so it
  gates CI directly, with `--json` carrying a `by_rule` count. Every rule had to
  catch something nothing else does, which excluded duplicate outputs,
  undeclared profiles, absolute paths in declared path fields and empty globs —
  all already hard errors. `lint_allow` records a finding that is true and
  unavoidable next to the target that pays for it.

- `frost query targets`: the one query with no starting point. `deps` and
  `rdeps` both need a target to walk from, which made "what is in this
  workspace" the question they could not answer — tooling was deriving it from
  the roots of `--output dot`, whose node shapes encode target kind as a
  *rendering* choice rather than a contract. It shares every filter and output
  format the other query functions have, and the VS Code extension's workaround
  and its dot parser were deleted when it landed.

- `frost journal export` and `frost journal diff`, for "CI rebuilt what my
  machine had cached". The export writes every action's key material — argv,
  environment, input digests, toolchain fingerprint, profile, platform and the
  action-key schema version — in a stable order, so two exports of one build are
  byte-identical. The diff reports the *first* field that differs per action
  rather than every field that does, and a build-wide difference such as a
  changed toolchain is reported once and alone, because four thousand
  consequences hide the one cause. Within an action the order is argv, env,
  pass_env, inputs, outputs: decisions before their effects. The format is
  versioned and a mismatch refuses rather than comparing fields whose meaning
  may have changed.

- `frost test --runs-per-test N` runs every test N times and requires all of
  them to pass. It does not read the cache — a recorded single pass cannot
  answer "does this pass N times", which is the only question worth asking N
  runs — and it suppresses `flaky_retries`, since hunting for a flake and hiding
  one are opposite tools. A failure says which run failed, because failing on
  run 7 of 10 is a flake and failing on run 1 is a broken test.

- `frost test --test-output=summary|errors|all` chooses what reaches the
  terminal. The default, `errors`, hides what passing tests wrote — that is the
  noise which buries the one failure worth reading — and replays failing tests
  in full after the run, because during the run a failure scrolls away behind
  the tests that were still going. `summary` prints the counts alone; `all`
  prints everything.

- `frost test --test-filter PATTERN`, `--test-env KEY=VALUE` and `--test-arg ARG`
  supply from the command line what the manifest supplies statically. The filter
  travels as `TESTBRIDGE_TEST_ONLY` and `GTEST_FILTER` rather than as a flag,
  because Frost cannot know a runner's filter syntax and inventing one spelling
  per language is how a build tool acquires a table of special cases — the
  environment is the protocol runners already implement, exactly as with
  sharding. Nothing new enters the action key to make these safe: argv and env
  are already key material, so a filtered run simply is a different action and
  cannot be served an unfiltered result. The command line wins over a manifest
  value of the same name, and an overridden name is dropped from `pass_env` so
  the key does not also carry the host value that no longer applies.

- `flaky_retries = N` on a test or `cc_test` target (default 0, maximum 9): a
  failing test gets N more attempts before the failure is its verdict. Each
  retry starts from the state a first attempt would see — the partial success
  stamp is removed and clean directories are reset — so attempt two does not run
  in the world attempt one left behind. A test that only passes on a retry is
  reported as flaky and its success is **not recorded**, locally or remotely:
  the build is green and dependents proceed, but the next run executes it again,
  because caching a verdict the test reached only on the second try would hide
  the flake from every later build. The summary line gains `N flaky` so the cost
  is visible instead, and a test that fails every attempt says `failed all N
  attempts` rather than looking like a single run. The field is deliberately not
  action-key material — it says how hard to look for a verdict, not what the
  test does, so turning it on does not invalidate a clean pass.

- `${dep:LABEL}` and `${deps:LABEL}` now resolve in a command target's `env` and
  in a genrule's `cmd`, not only in argv. A consumer names the dependency it
  wants and Frost supplies that dependency's declared output path, so the
  producer's layout convention stops being copied into every manifest that
  reads from it — and moving an output stops being a breaking change. A genrule
  `cmd` is one shell string, so the plural form joins on a space the way `${in}`
  already does there; an `env` value is one string with no such convention, so
  the plural form is an error rather than a separator Frost invents. Everything
  else in an `env` value passes through untouched, because that value belongs to
  another program and `${...}` in one is routinely its own syntax. Both
  expansions are action-key material, so a dependency that relocates its output
  reruns its consumers instead of replaying a command naming a path that is gone.

- A root `frost.toml`: the repository builds itself. `frost test --all` runs the
  pre-PR gate as five declared stages — cargo test, clippy, fmt, the Python
  suite and the extension's — each naming what it reads, so a stage that already
  passed on those exact inputs is a cached success rather than a repeat.
  Measured here, a second run of an unchanged tree is 1.6 s against 103 s, and
  editing a `.rs` file reruns the three Rust stages while leaving the Python and
  extension suites cached. `scripts/check.sh` stays: bootstrapping cannot depend
  on the thing being bootstrapped. The manifest deliberately has no
  `[workspace]` section, because that would make Frost discover the nested
  sample manifests and pull every sample workspace in as a package of this one.

### Added

- A VS Code extension at `tools/vscode/`, unpublished and built in CI. It
  provides a task provider, target and test pickers, "build the targets owning
  this file", an optional build-on-save, and compiler diagnostics routed into
  the Problems panel with the target frost attributed them to — attribution a
  declarative problem matcher cannot do, because it cannot see frost's action
  framing. Everything under `src/frost/` except the one module that spawns
  frost is pure and never imports `vscode`, so the suite runs under plain
  `node --test` with no editor download and no display server; an architecture
  test enforces that rather than documenting it. The extension reads only
  documented surfaces — the `--json` payloads and `--output label-kind` — with
  one exception noted in its source: no CLI primitive lists every target, so
  the universe is derived from `graph --dot` topology and the kinds come from
  `query`.

## [0.9.0] - 2026-08-01

### Added

- A test or `cc_test` target may declare `shard_count = N`, becoming N
  independently keyed, cached and scheduled actions. Frost does not divide the
  cases — it cannot know them — it tells the runner which slice is its own
  through `TEST_SHARD_INDEX`, `TEST_TOTAL_SHARDS`, `TEST_SHARD_STATUS_FILE` and
  googletest's spelling of the first two, so a gtest binary shards without a
  wrapper. Each shard writes its own success stamp, so one shard failing leaves
  the others cached and a rerun repeats only the failure. A runner that ignores
  the protocol runs every case in every shard, which is why the field is
  declared per target rather than applied by Frost, and why declaring one of
  those variables alongside `shard_count` is an error rather than a silent
  override. Omitting the field, or writing `shard_count = 1`, reproduces the
  exact action identity and stamp path Frost has always used, so adding
  sharding to a workspace cannot invalidate an existing journal.

- `${dep:LABEL}` and `${deps:LABEL}` in `command` and `test` arguments resolve
  to the declared outputs of one named dependency, so a consumer stops
  repeating the producer's output-path convention and a layout change stops
  being a breaking change spread across the manifest. Only a declared
  dependency resolves — reaching anything else would let the argv name a file
  this target has no edge to, so the build could run before it existed.
  `${dep:LABEL}` requires exactly one output, because a dependency with several
  has no single path it could mean and first-wins would be silently wrong; the
  error names the plural form. A dependency declaring only `output_dirs` is an
  error for both, since the tree stamp Frost writes for an owned directory is
  its bookkeeping rather than a path to hand to a tool. The expansion is argv,
  and argv is action-key material, so a dependency that moves its output
  rebuilds its consumers instead of replaying a command that names a path which
  no longer exists. `sample_java` now uses it.

- `frost query` answers the questions it claimed to. `owners <paths...>` is the
  file→target direction — "what must rebuild when this changes" — over declared
  action inputs, including the generated headers a genrule dependency
  contributes transitively; a header discovered only through a depfile is build
  state rather than configuration, and the empty result says so instead of
  looking like a file nobody owns. `allpaths <from> <to>` returns every route
  where `somepath` commits to one, which is the difference between explaining a
  rebuild and being able to cut a dependency; the count is exponential on
  stacked diamonds, so the walk is bounded by `--limit` and reports that it
  stopped rather than implying completeness. `--kind` and `--attr NAME=PATTERN`
  (deps, srcs, outputs, sandbox, timeout) filter every function against closed
  sets, so a typo fails instead of silently widening the answer, and
  `--output text|json|label-kind|dot` replaces the format guesswork. `--json`
  keeps its exact payload and means `--output json`; the two spellings
  disagreeing is an error rather than a silent preference. Path globs are
  uniform: `*` and `?` stop at `/`, `**` crosses it.
- Three more sample workspaces, and `docs/29_sample_workspaces.md` covering all
  five. They exist to make one decision concrete: whether Frost should own the
  compiler or own the boundary around a tool that owns its own dependency
  graph. `sample_java/` is multi-module Java in the Gradle/Maven directory
  layout, built without either — `${deps}` puts the dependency module's jar on
  javac's classpath so the application module never writes down where the
  library puts its output. `sample_spring/` is a Spring Boot application built
  by Gradle and `sample_maven/` one packaged by Maven, each wrapped as a single
  `command` target with declared inputs and an owned output tree: Frost caches
  and restores the tree without reproducing the task graph, which is the honest
  answer when reproducing it would mean reimplementing the tool. Warm builds
  skip the ecosystem tool entirely (Gradle 12.3 s → 1 ms), and deleting the
  output tree restores it from the content-addressed store in milliseconds
  without running it. The two wrapped samples are not built in CI, because
  resolving from the network is a dependency the correctness suite does not
  take.
- `sample_multi/`, a multi-package workspace with the shape `sample_c` cannot
  have: four packages, four target kinds, a generated header consumed from a
  package other than the one that writes it, and a diamond where `cli` reaches
  `core` through both `text` and `render`. An E2E builds it, runs it, edits the
  bottom of the diamond and requires the change to reach the top by both
  routes.

- Actions can be stopped. A target may declare `timeout = <seconds>`,
  `--timeout` imposes one on a whole invocation, and test actions carry a
  300-second default because a hanging test otherwise holds a CI job open by
  itself. Build actions stay unbounded unless asked: the watchdog costs a
  thread per action, and a long link is not a hang. On expiry the process
  group is terminated through the same path cancellation uses and escalated to
  a kill if it is ignored, the output collected so far is still reported, and
  the failure names the limit and which of the three places set it. A limit is
  not action-key material and a timed-out action records nothing, so the next
  build runs it again rather than replaying a verdict about the clock.

- `docs/28_compatibility_contract.md` states which surfaces a release promises
  not to break — the `frost.toml` grammar, CLI subcommand and option names,
  exit codes, `--json` schemas, `frost info` keys and the artifact layout — and
  which are explicitly implementation: everything under `.frost/`, the
  action-key construction, the internal crates, human-facing text and the
  daemon protocol. Each promise names the test that enforces it. The CLI
  surface is checked in at `crates/frostbuild-cli/tests/cli-surface.txt` and
  compared on every run, so an unintended rename fails rather than ships;
  additive changes refresh it with `UPDATE_CLI_SURFACE=1`.

### Fixed

- A journal written by another version is replaced instead of appended to.
  `record` decided to write the header by file length alone, so it appended
  records behind a header this build could not read: every later load decoded
  an empty journal and every build stayed cold, with no way back short of
  deleting `.frost/`. An unrecognized journal now costs exactly one cold build.
  Covered at the unit level and by an E2E that gives every stored format
  (graph store, journal, hash cache, no-op certificate) a version this build
  cannot claim, then requires a correct rebuild that converges to warm.

## [0.8.0] - 2026-07-30

### Added

- `frost init` detects Rust, Go, TypeScript and Python in addition to C/C++ and
  Java, and `--language` accepts all six. Generated manifests explain their
  direct action and next build command; automatic detection refuses where
  Cargo, Go modules, npm, Python packaging, Gradle/Maven, Bazel or Ninja owns
  semantics that Frost cannot safely infer, while naming an explicit override
  or importer. Four real-tool E2E fixtures cover init, build, cache hits,
  source changes and deterministic Python wheel rebuilds.
- `frost info [KEY] [--json]` reports the version, action-key schema,
  workspace root, manifest path, configuration key, output/bin/obj/tmp trees,
  CAS, journal, hash cache, graph store and daemon socket. A single key prints
  its bare value for shell substitution; it answers without a manifest or a
  graph, which is when a wrapper needs it most.
- `frost completions --install` adds the dynamic completion hook to the
  startup file of the shell detected from `$SHELL`, or of the shell named on
  the command line. It is idempotent, `--dry-run` prints the exact lines
  without touching the file, a hand-written hook is left alone rather than
  duplicated, and PowerShell/Nushell — whose profile locations are not
  reliably discoverable — get the snippet to paste instead of a guessed path.
- Completion covers the rest of the CLI: `--remote-cache` offers the
  `file://`/`http://`/`https://` schemes plus directories, `import-npm
  --script` reads script names from `package.json`, `frost info` completes its
  keys, and every path argument declares a file, directory or executable hint.
  A test walks the command tree and fails when a value-taking argument
  declares no candidates and is not on the explicit free-text list.

## [0.7.2] - 2026-07-30

### Added

- The official Pages site now has a responsive documentation hub that guides
  readers from quick start and normative specifications through language
  adapters, benchmark evidence, architecture studies and delivery process,
  while keeping the repository Markdown authoritative.

## [0.7.1] - 2026-07-29

### Added

- Releases and branch cleanup are CI operations rather than terminal ritual.
  `Release` accepts a `workflow_dispatch` version and validates it against
  `Cargo.toml` and the CHANGELOG before spending three runners, then creates
  the tag and the GitHub release together so a failed build cannot leave a
  dangling tag; pushing a `vX.Y.Z` tag still takes the same path. A new
  `Branch cleanup` workflow, weekly and on demand, deletes only branches whose
  every commit is already contained in the default branch and never one with
  an open pull request. `scripts/release.sh`, `scripts/cleanup-branches.sh`
  and `scripts/check.sh` cover the same ground from a clone.

## [0.7.0] - 2026-07-28

### Added

- `frost import-npm --vite-builds` conservatively imports recognized
  non-watch `vite build` scripts as command targets with profile-specific
  owned `dist` trees. Custom output-directory contracts remain explicit.
- An official FrostBuild dependency-crystal mark and dependency-free project
  site, deployed through SHA-pinned GitHub Pages automation.
- Reproducible external BuildGrid/BuildBox REAPI, multi-corpus DeltaCDC remote
  calibration, and real npm/Vite production-adoption certificates.

### Changed

- Remote DeltaCDC is a measured defer decision: it remains off by default
  until encoding CPU, negotiated protocol support and production bandwidth
  cross the checked gates.
- Persistent browser HMR remains framework-owned and imported `node_modules`
  remains an explicit npm-owned, non-hermetic boundary.

## [0.6.1] - 2026-07-28

### Changed

- `sha2` moved to 0.11 (`digest` 0.11). The CAS hex digest is now encoded
  explicitly rather than through `LowerHex`, which `digest` 0.11 no longer
  implements for its output type; the emitted strings are byte-for-byte
  unchanged and a regression test pins them, so existing CAS entries and
  remote-cache keys stay valid.

## [0.6.0] - 2026-07-27

### Added

- Genrules can use `${pathsep}` for the host path separator, allowing one
  manifest to invoke paired extension-neutral POSIX and Windows launchers.

### Changed

- Windows CI runs every host-reachable E2E serially instead of a named subset.
  Host exclusions remain declared beside the tests and documented in the
  platform-support matrix.

### Fixed

- Built-in C, C++ and Kofun binary targets declare the host executable suffix,
  so Windows linkers, the CAS, `run`, `dev`, `debug`, IDE configuration and
  native tests all agree on `.exe` output names. Serialized graphs rebuild once
  under the new output-path schema.
- Direct actions resolve workspace-relative executable paths against their
  working directory before spawning, which lets Windows run freshly linked test
  binaries rather than searching the parent process's directory.
- A Windows daemon is created detached with handle inheritance disabled, so it
  cannot keep a launching client's captured output alive and hang
  `build --daemon`.
- Action environments pass through Windows `LOCALAPPDATA`, allowing tools such
  as Go to use their ordinary host cache without making its scratch path
  action-key material.

## [0.5.0] - 2026-07-25

### Added

- `--remote-cache=<endpoint>` consults a shared cache when the local journal
  misses, and `--remote-upload` publishes what the build produced. A shared
  directory and plain HTTP are supported. The lookup key covers declared inputs
  only, with the producing run's discovered inputs recorded as a verified trace,
  so a workspace with no journal can reuse a compile whose real inputs include
  headers it has never read. Responses are digest verified, executable mode is
  recovered from the digest, and every miss, corruption or transport failure
  falls back to local execution; a per-build summary reports hits, misses, bytes
  moved, rejections and errors.

### Changed

- The default C and C++ drivers are the host's conventional names: `cc`/`c++` on
  Unix and `gcc`/`g++` on Windows, where the POSIX names generally do not exist.
  A scaffolded workspace and the bundled sample now compile on Windows without
  an explicit `[toolchain]` driver.
- Windows CI runs a named host-portable E2E subset using the image's MinGW
  toolchain.

### Fixed

- Executables named without an extension resolve on Windows: `PATH` search now
  tries the host's `PATHEXT` candidates, so a workspace asking for `gcc` no
  longer fails with "not found in PATH" while the same name works in the shell.
- Command text in a Windows manifest no longer relies on an `if not exist`
  guard: `cmd` binds the rest of the line to the if-branch, so the guarded chain
  was skipped once frost had created the output's parent.

## [0.4.0] - 2026-07-25

### Added

- `depfile_format` selects the dependency report an action produces: `make`
  (default), `lines` for a wrapper-friendly path list, or `showincludes` for
  `cl.exe`, which is read from captured output because MSVC has no `-MF`. The
  notes are stripped from the build log, and the path is taken after the last
  `: ` so a localized toolchain still parses.
- `command` targets may declare `output_dirs`: directories Frost owns entirely,
  for tools whose output file names cannot be written down in advance. The tree
  is scanned after execution, digested, journalled and published to the CAS;
  a hit restores exactly the recorded tree; a `.frost/tree/CONFIG/TARGET/contents`
  stamp represents it in the graph so dependents get ordinary edges and early
  cutoff. `${output_dir}` / `${output_dirs}` expand in command arguments.

### Changed

- Action-key schema v4 adds the declared owned-directory set, so changing it
  does not reuse an earlier result. Existing journals rebuild once.
- macOS CI runs the whole workspace test suite instead of one smoke test, and
  Windows runs a named subset covering the paths its image can reach. Host
  exclusions are declared in the tests and tabulated in
  `docs/09_platform_support.md`.

### Documentation

- `docs/README.md` indexes every document by purpose and records why the
  duplicated numeric prefixes are not renamed.

### Security

- Arbitrarily corrupted no-op certificates and CAS chunk manifests are gated by
  property tests, not only by the designed failure injections: a manifest or
  certificate that is not byte-exact can never publish bytes or declare a build
  finished.

### Fixed

- `frost init --language java` passes `JAVA_HOME` to the build. `javac` and
  `java` are stubs on macOS that select a JDK from it, so a build that cleared
  it compiled for a different JDK than the developer's own `java` could load.
- One workspace means one daemon however its path is spelled: the socket name is
  derived from the resolved path, so a client that resolved a symlinked prefix
  (`/var` on macOS) and one that did not no longer miss each other.
- A client that hangs up before reading its answer no longer takes the daemon
  down with it; a failed reply ends only that connection.
- The daemon watcher strips the resolved workspace root from event paths, so a
  workspace reached through a symlinked prefix (`/var` on macOS) records
  workspace-relative dirty paths and recognises its own barriers.
- Default `arflags` are `["rcs"]` on macOS. `rcsD` asks for a deterministic
  archive, but the cctools `ar` Xcode ships rejects `D` outright, so every
  archive action failed on a macOS host with a default manifest.
- A daemon build now runs with the invoking client's environment instead of the
  environment the daemon inherited when it started. `CPATH=a frost build
  --daemon` against a daemon started with `CPATH=b` built, and then reported as
  cached, a binary matching neither request; only the no-op certificate check
  had used the client's environment.
- A resident daemon honours a shutdown request regardless of protocol version,
  and a client that meets a daemon from another frost version replaces it and
  retries, or builds in-process, rather than failing the build.

## [0.3.3] - 2026-07-25

### Added

- `frost import-npm` discovers npm workspaces and selected package-script
  gates, tracks transitive in-repository workspace dependencies, fingerprints
  npm and Node, and refuses to overwrite an existing manifest.

### Performance

- CI reuses Rust dependency caches across lockfile changes, avoids duplicate
  release and host-portability compilation, cancels superseded pull-request
  runs, installs `cargo-deny` from its pinned action, and runs the four nightly
  fuzz targets in parallel.
- DeltaCDC corpus reports record exact bytes and per-plan chunking, selection,
  encode and verified-decode CPU, then derive the bandwidth where delta CPU
  breaks even with full transfer.

### Fixed

- `import-npm` refuses conventional output-producing and persistent package
  scripts instead of incorrectly representing them as cacheable test gates.
- Action-key schema v3 includes declared output paths, and journal reuse
  requires an exact recorded output set, preventing stale reuse after only the
  manifest output declaration changes.

## [0.3.2] - 2026-07-23

### Added

- `frost-bench-rs daemon-graph` generates equivalent large linear graphs for
  Frost and Ninja, rotates standalone/daemon/socket/Ninja no-op samples and
  records alternating one-file leaf rebuilds with raw host/load evidence.

### Performance

- A fully validated daemon certificate can remain resident behind a filesystem
  event barrier when every recorded path is normal and workspace-watched. The
  checked 10k-target median fell to 2.271 ms end-to-end versus Ninja 58.556 ms;
  the direct socket path measured 0.203 ms.

### Fixed

- Watcher-backed no-op proofs include `.frost` output events and retain the
  complete validation/fallback path for changed toolchains/environments,
  external, missing or symlinked evidence, watcher errors and barrier timeouts.

## [0.3.1] - 2026-07-23

### Documentation

- Preserve the reusable correctness and performance lessons from the 20 July
  investigation as a dated snapshot, while recording the later v0.3.0 daemon,
  DeltaCDC and language-adapter results separately.
- Record which v0.3.0 issue gates are complete and which external evidence
  remains intentionally open, without treating publication as acceptance.

## [0.3.0] - 2026-07-22

### Added

- Tagged releases publish checksummed `frost` + `frostd` archives for static
  x86_64 Linux, the current macOS runner architecture and x86_64 Windows.
- Language-neutral `command` targets run a named `[toolchain.tools]` executable
  with direct argv, declared configuration-isolated file outputs, optional
  Makefile depfiles, static environment and explicit `pass_env`. Named tools
  can be overridden per platform and are fingerprinted. Real-tool E2E coverage
  exercises Rust, Go, Java, Python and TypeScript/Node when installed.
- `kind = "test"` accepts either the existing shell `cmd` or a named `tool`
  with direct `args`, `env` and `pass_env`. Direct language tests share
  Frost's success-only stamp, cache, failure cleanup, `--all` and `--affected`
  behavior; a real Python E2E verifies success, cache and failed-stamp cleanup.
- Command targets support ordered direct-argv `steps` and
  configuration-isolated `clean_dirs`. Compile/package pipelines such as
  `javac` → `jar` can publish one stable artifact without shell parsing; stale
  intermediate files are removed before normal and determinism executions.
  `${clean_dir}` / `${clean_dirs}` reuse the owned path in argv without
  duplicating configuration or package prefixes.
- Command targets can opt into `preserve_outputs` for incremental compilers
  that update only an affected subset. The mode is action-keyed, every retained
  output is still verified, and compiler state can be declared alongside final
  artifacts for safe failure cleanup. Native TypeScript 7 E2E coverage guards
  the output-preservation path.
- `frost pack-jar` creates sorted, fixed-timestamp, compressed Java archives
  with a standards-compliant manifest and optional `--main-class`. It avoids a
  second JVM in `javac` → JAR actions while remaining a normal fingerprinted
  direct-argv step.
- `frost pack-wheel` creates a deterministic pure-Python wheel with a
  normalized standard filename, required metadata and complete SHA-256/size
  `RECORD`. Paths and symlinks are validated, bytecode/cache files are omitted,
  and the archive is atomically published. Real Python E2E imports it.
- `build` and `test` accept `--all-platforms`, preserving parallel action
  execution inside each platform and ending with a compact host/device status
  tree.
- Bash, Zsh, Fish, PowerShell and Elvish dynamic completion resolves targets,
  profiles and platforms from the selected workspace. Static scripts are
  available for those shells and Nushell through `frost completions`.
- `frost pick` offers optional multi-target/test selection through `fzf`; it
  also has a script-friendly `--print` mode.
- `frost watch` debounces recursive native filesystem events, excludes Frost/
  Git/declared-output self-writes, rebuilds affected graphs and optionally
  restarts a direct-argv development process only after success. Broken builds
  keep the last successful process alive.
- `frost dev` adds the target-aware hot-reload loop: it infers the built
  native/JAR/JavaScript/Python artifact and runtime, restarts only after
  success, and accepts an explicit runner for emulated/custom outputs.
- `frost run` resolves one target to its artifact and executes native,
  Java/JAR, JavaScript or Python direct argv. Foreign-platform execution
  requires an explicit runner; `--print` exposes exact argv.
- `frost debug` validates native symbol flags and launches GDB/LLDB, or selects
  jdb from an executable JAR's manifest, Node inspect, or Python pdb for
  language artifacts. All paths remain direct argv and support `--print`.
  Native `frost init` scaffolds `-O0 -g` debug and `-O3 -DNDEBUG` release
  profiles.
- `frost ide` builds one target and generates artifact-aware VS Code
  `tasks.json`/`launch.json` for native, Java, JavaScript or Python debugging.
  It exposes a JSON dry run and never overwrites either existing file.
- `frost doctor` checks the configured graph and every required executable,
  then separately reports optional runtimes, debuggers, `fzf`, bubblewrap and
  Graphviz. Human output is a compact tree; `--json` preserves required vs
  optional status for setup automation.
- `frost import-bazel` consumes Bazel's own query XML and writes a conservative
  multi-package `cc_library`/`cc_binary`/`cc_test` migration scaffold. It has a
  full dry-run, refuses overwrite and stops on configurable or unsupported
  semantics instead of silently flattening them.
- `frost bazel-dev` keeps Bazel's configured graph/server/cache authoritative,
  watches workspace changes, and restarts the complete `bazel run` process
  tree only after a successful incremental `bazel build`; broken builds keep
  the last healthy target alive.
- Host portability is now enforced structurally: test success stamps are
  executor-owned, Windows uses `cmd.exe /C`, daemon transport uses a
  workspace-published loopback endpoint, cancellation terminates the Windows
  child tree, and CI defines native macOS/Windows compile, unit, daemon and
  command-build/no-op gates.
- Eligible default-target daemon no-ops validate the whole-closure certificate
  inside `frostd` instead of spawning a second `frost`. The invoking client's
  key environment is explicit; certificates with arbitrary `pass_env` values
  conservatively use the normal path. A rotating benchmark separately records
  standalone CLI, daemon CLI and direct socket latency.
- Blobs over 2 MiB now populate a Bazel-compatible FastCDC 2020
  chunk-addressable store and versioned blob manifest. Materialization verifies
  each SHA-256 chunk and the final BLAKE3+executable digest in a private staging
  file before publication; `frost cache stats` reports persistent chunk/byte
  reuse. A dedicated CI job injects corruption, missing/wrong/truncated/single
  chunks, ordering changes and producer/consumer parameter mismatches.
- Residual chunks can carry a positional previous-artifact zstd level-19
  delta when it is smaller than a normal level-3 full-chunk transfer. Restore
  tries exact blob, exact chunk and verified delta before reporting a miss;
  patch, reconstructed chunk and final blob digests are independent gates.
- Independent FastCDC chunk hashing/publication and positioned writes into the
  private restore file now use the bounded Rayon pool while retaining ordered
  manifests and final-blob verification. The checked 64 MiB alternating A/B
  measured 1.41x faster cold publication, 1.89x faster chunk restore and 1.88x
  faster delta restore; the report records all samples and host load.

### Performance

- A checksummed whole-closure no-op certificate bypasses graph, journal and
  per-action validation only after graph-source, toolchain, keyed environment
  and every closure file stat identity match. On the checked 10k linear-chain
  rerun, Frost measured 15.620 ms median versus Ninja's 42.419 ms (2.72x).
  This is a no-op workload result, not a universal language/build claim. A
  separate rotating one-target median-of-31 measured standalone CLI at 2.043
  ms, end-to-end daemon CLI at 1.711 ms and its socket roundtrip at 0.238 ms,
  meeting the local warm-daemon 5-ms target without conflating it with 10k.
- Multi-output CAS publication deduplicates equal digests and publishes
  independent objects in parallel. File hashing now sizes its read buffer from
  8 KiB to 4 MiB instead of allocating 4 MiB for every tiny class/depfile.
  On the alternating-order 100-source Java comparison, Frost batch measured
  510.959 ms clean / 511.646 ms one-change / 2.060 ms no-op versus Gradle's
  574.947 / 578.634 / 553.540 ms (median-of-15).
- The equal-compiler 100-module Rust harness validates executable stdout after
  every sample. Frost's direct crate action measured 282.877 ms clean /
  204.870 ms one-change / 3.575 ms no-op versus Cargo's 417.192 / 237.924 /
  32.455 ms (median-of-7); a median-of-15 focused run confirmed the close
  changed-module result at 209.125 versus 243.408 ms.
- The Go harness separates a `go build` wrapper from a native package
  compiler/linker boundary and validates execution plus normalized module/build
  metadata after every sample. For 100 files, Frost native measured 112.492 ms
  one-change / 3.720 ms no-op versus Go's 156.880 / 137.847 ms; the focused
  median-of-15 clean result was 151.074 versus 160.333 ms. The full
  median-of-7 clean reversal is retained in the report, and multi-package/cgo/
  embed/test plus generated-configuration usability remain open gates.
- The native TypeScript 7 harness byte-compares 101 emitted JavaScript files
  and executes them after every sample. A forward/reverse 14-sample checker
  sweep found four workers best for Frost and two for direct `tsc`. In the
  optimized median-of-7 report Frost measured 259.409 ms clean / 49.391 ms
  one-change / 2.468 ms no-op versus `tsc` at 228.080 / 42.467 / 41.318 ms:
  only the no-op boundary is won (16.7x), while compiler-running scenarios and
  the project-reference/watch/bundling ecosystem remain open.
- The TypeScript project-reference harness compares eight Frost project
  actions with one native `tsc --build` solution and validates 416 emitted
  JavaScript/declaration files plus eight executions after every sample. Outer
  `-j8 × 1 checker` was Frost's best worker split. Frost won no-op at 3.200 vs
  6.556 ms but lost clean (940.313 vs 656.893 ms) and one-project change
  (50.792 vs 44.386 ms); environment load remains recorded in the report.
- The pure-Python wheel harness validates exact 101-source contents,
  Name/Version/tag, every `RECORD` hash and extracted execution after every
  sample. Frost measured 21.295 ms clean / 2.600 ms unchanged / 7.806 ms after
  one source change versus `uv build` at 326.911 / 290.841 / 290.786 ms and
  `python -m build` at 766.806 / 619.512 / 612.785 ms. This wins the minimal
  pure-wheel contract; arbitrary PEP 517 metadata, extensions and pytest stay
  open.

### Fixed

- The manifest-free graph warm path trusted directory mtimes to detect added,
  removed or renamed source entries. NTFS can expose the same parent timestamp
  immediately across an entry mutation, allowing a stale graph to remain
  current. Graph-store v6 also fingerprints sorted native entry names and
  filesystem kinds, so discovery correctness no longer depends on timestamp
  resolution.
- Filesystem access/open/close notifications were treated as source edits.
  Executing a workspace-local Bazel wrapper could therefore trigger an
  unbounded `bazel-dev` build/restart feedback loop. Generic watch, Bazel watch
  and daemon dirty tracking now accept create/modify/remove events and ignore
  access-only events; the success-only Bazel E2E guards the loop.
- Concurrent actions publishing the same CAS digest now use distinct staging
  paths. The former digest-plus-pid name could let two executor threads copy
  through the same inode while one renamed it into the immutable store.
- **A corrupt CAS object was restored and the build reported as current.**
  `materialize` copied an object into place without checking it against the
  digest that names it, so bit rot or a truncated write produced an artifact
  that never existed, delivered as a cache hit. Reproduced by flipping one
  byte: frost said `up to date` and left a binary differing from a correct
  build. Objects are now verified on restore; a bad one is removed and the
  action re-runs. The cost is one hash, only on the restore path.

### Fixed

- The shell frost runs every genrule and shell test through was the one tool
  frost chooses and did not account for. `/bin/sh` now sits in the toolchain
  fingerprint beside the C drivers, so replacing it invalidates the actions
  that depend on it. The manifest has no way to name the shell, which is
  precisely why frost has to.

### Fixed

- A file's mode was not part of its digest, so `chmod -x` on a script a
  genrule runs changed no bytes and frost reported the build as current —
  while a clean build of the same tree failed. The executable bit now joins
  the content digest, and the stat check notices a mode change so the cached
  digest is not reused. The hash cache format is bumped, so the first build
  after upgrading re-hashes.

### Added

- `docs/16_action_key_audit.md` enumerates every input that can change what an
  action produces, whether it reaches the action key, and the argument for
  each deliberate exclusion. Three known gaps are named rather than left to be
  rediscovered: interpreters a genrule invokes, umask, and filesystems with
  whole-second mtime.

### Added

- `frost init` writes a starter manifest for native C/C++ or plain Java sources,
  and the missing-manifest error names it. Native sources become library/
  binary rules; Java becomes one `javac` batch plus a deterministic executable
  or library JAR. Generated builds are exercised as written. Mixed source
  families require `--language`, while Gradle/Maven markers stop Java
  auto-detection so existing dependency/plugin semantics are never silently
  bypassed. It refuses overwrite and supports `--dry-run`.

### Fixed

- A `srcs` or `inputs` glob that matched no files was accepted. A typo like
  `srcs/**/*.c` for `src/**/*.c` produced a library with nothing in it, built
  without complaint, and failed later at the link with a message about symbols
  rather than about the glob. An empty match is now an error naming the target
  and the pattern.

- **Wrong binary returned from cache when an include-path environment variable
  changed.** `CPATH`, `C_INCLUDE_PATH`, `CPLUS_INCLUDE_PATH`, `LIBRARY_PATH`,
  `SDKROOT`, `MACOSX_DEPLOYMENT_TARGET` and `SystemRoot` select which headers
  and libraries a compiler finds, with no change to the command line or to any
  declared input, and none of them were part of the action key. Building with
  `CPATH=/a` and then `CPATH=/b` reported everything cached and left the
  binary built against `/a` in place. These variables are now keyed;
  `PATH`, `HOME`, `TMPDIR`, `TMP` and `TEMP` stay out of the key, since PATH's
  effect on the compiler is already captured by hashing the resolved driver
  binaries and the rest name scratch locations that must not change output.
- Two toolchain fingerprint functions computed different values — one mixed in
  `cc --print-sysroot`, the other did not — and the CLI only ever called the
  weaker one. The unused function is gone, with a note on why the sysroot
  needs no separate treatment: an explicit `--sysroot=` reaches the key
  through argv, a default sysroot is a property of the hashed driver binary,
  and the headers read from it arrive as depfile-discovered inputs.

- `--profile` accepted any name. A typo built with no profile flags into its
  own output tree and said nothing, so `--profile relase` quietly produced a
  different binary than `--profile release`. Once a workspace declares any
  profile, an undeclared name is now an error; `debug` always works, and an
  empty `[profile.<name>]` section still asks for a bare tree on purpose.
- The daemon could not start from a workspace more than a few directories
  deep. Its socket lived inside the workspace, and a Unix socket address is
  capped near 100 bytes, so `frost daemon start` failed with `SUN_LEN` and no
  mention of paths. The socket is now a short, stable name in the user's
  runtime directory, derived from the workspace path so each workspace still
  gets its own daemon.
- A daemon killed rather than shut down left a socket file that blocked every
  later start. A stale socket is now detected and replaced; a live one reports
  that the daemon is already running.
- `frost build --daemon` slept 20 ms after every successful build, to let the
  watcher deliver events for the build's own writes before clearing a counter
  that only `daemon status` reads. Every build paid it. Removed.

### Changed

- The line every build ends with now leads with what happened and drops every
  term that is zero. `frost: 0 executed, 5 cached (5 actions, 0 pruned of 5)
  in 12 ms` reads `frost: up to date · 5 actions · 12 ms`; a partial build
  reads `frost: 2 built, 3 cached · 5 actions · 40 ms`; a failure leads with
  the failure. The share of the graph left out appears only when a subset was
  built (`2 of 9 actions`), since a full build does not need to be told it
  built everything.
- `--stats` no longer reports `0 ms, 0.0%, 0.00x` for a build that executed
  nothing, and distinguishes three cases it previously conflated: the graph
  bounds the build, there is scheduling headroom, or the recorded durations
  are stale and predict a longer critical path than the run took.

- Unknown target, profile and platform names suggest the closest declared one
  instead of printing the whole list: `unknown target "ap". did you mean
  "app"?`. A name that resembles nothing still gets the list, because a wrong
  suggestion is worse than none.

### Performance

- 10k-target no-op build: 285 ms -> 176 ms (-38%), closing the gap to Ninja
  from 6.0x to 3.9x on the same workspace. Three findings, in the order the
  measurements produced them:
  - Completing an action woke every worker. On a dependency chain only one
    action becomes runnable at a time, so `notify_all` cost `actions * jobs`
    wakeups to do `actions` units of work — 50,925 condvar wakeups for 10,000
    actions. Workers are now woken one per newly runnable action.
  - The toolchain fingerprint loaded the workspace-wide content cache, megabytes
    covering every source file, to digest three compiler binaries. It now keeps
    its own stamp and re-hashes only when a driver actually changed.
  - A path that is one action's output and the next action's input was stat'd
    twice. A build is a single point in time, so the second check reuses the
    first result; frost invalidates whenever it writes a path itself.
- The hash cache read path no longer takes a lock, and journal reads take none
  at all: entries recorded by the previous build are immutable during this one.
  (Measured at -1.3% on its own — the contention this removed was not the
  bottleneck. Kept because it is simpler, not because it was the win.)

### Added

- `frost simulate`: compares every scheduler/estimator pair over a sweep of
  worker counts by planning the build rather than running it. Durations come
  from the journal, ordering from the same `Schedule` the engine uses, and no
  cache is touched, so the comparison is deterministic and safe to run
  mid-session. `--json` for CI gating.
- `build --stats`: makespan, worker utilization and distance from the
  estimated critical path, so a real run can calibrate the simulator.
- `frostbuild-bench` is now a measurement library (`Sweep`, `Point`,
  `render_table`) rather than a stub binary.

### Changed

- `--scheduler` and `--estimator` are real. `--estimator` was previously
  accepted and then ignored: every build used journal-or-constant regardless
  of the flag. `learned` now differs from `journal` where it matters — an
  action with no history gets the median duration of its kind from this
  workspace's journal instead of a hardcoded constant.

### Fixed

- The critical-path scheduler degraded after the first wave: actions unlocked
  later were re-prioritized by a cruder key than the one used to build the
  initial ready queue.
- Actions inherited stdin, so a command that reads it (`cat > out` when
  `${in}` expanded to nothing) blocked forever with no diagnostic. Actions now
  get `/dev/null`.
- A Ctrl-C arriving after the raw-TTY dashboard started but before a newly
  spawned action registered its process group could miss that action and leave
  Frost waiting for it. Cancellation and process-group registration now share
  one lock and close that race.

## [0.2.0] - 2026-07-19

### Added

- Multi-platform device builds: `[platform.<name>]` toolchain overlays with
  driver/`arflags`/`sysroot`/flag overrides and a `--platform` flag on build,
  test, plan, graph, compdb, explain and clean. Outputs, graph caches and
  journal identities are isolated per platform, so host and cross builds stay
  warm concurrently; verified end-to-end by an aarch64 (`zig cc`) E2E test.
- `frost query {deps,rdeps,somepath}`: configuration-free target-graph
  queries with `--json` output; `rdeps` is the "what does this change
  affect?" monorepo-CI primitive.
- `docs/14_bazel_gap_analysis.md`: adopt/solved/reject decision record against
  Bazel's capabilities and chronic pain points.
- `docs/15_research_cache_layers.md`: layered cache research direction
  (equivalence / dimension hashes / distance) with adoption priorities.
- Refreshed benchmark evidence on a desktop host (frost vs ninja vs make,
  1k/10k, clean/incremental/no-op): `bench/baselines/2026-07-19-E14-v0.2.0.json`.

### Performance

- Graph construction on deep dependency chains dropped from O(n^3) to
  O(n + edges) via structurally shared transitive export sets (#78):
  a 10k-target linear chain now configures in 275 ms instead of ~19 min,
  with action argv and cache keys byte-for-byte unchanged.
- Manifest-free warm path: the graph store embeds a sources stamp
  (manifest/ignore-file bytes + per-directory mtime_ns) plus the resolved
  toolchain and default targets, so warm invocations of every subcommand
  skip TOML parsing entirely; the hash cache moved from JSON to versioned
  postcard. 10k-target no-op build: 445 ms → 241 ms. Remaining gap to
  Ninja's ~50 ms is tracked in #81 (resident daemon targets <5 ms, #25).

### Changed

- Graph store format bumped to version 3 (platform axis, sources stamp,
  embedded toolchain); stale caches recompile transparently.
- Hash cache lives at `.frost/hashcache.bin`; the legacy JSON file is
  removed opportunistically.

## [0.1.0] - 2026-07-12

- Initial production-capable local engine and reference benchmark suite.
