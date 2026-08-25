# `frost.toml` manifest specification (v1)

Unknown fields are errors. Paths are UTF-8, workspace/package-relative, use `/`,
and may not contain empty, `..`, or absolute components. `srcs` and genrule
`inputs` accept deterministic sorted `*`, `?`, `[]`, and `**` globs. `.frost`,
`.git`, `.gitignore`, and `.frostignore` matches are excluded.

## Workspace and packages

A legacy single-file workspace contains one root `frost.toml` and uses bare
target names. When the root contains `[workspace]`, Frost discovers nested
`frost.toml` files (not through directory symlinks). Nested paths are package
relative. Labels are `//path/to/package:name`; `:name` and `name` resolve in the
current package, while `//:name` addresses a root target. Which packages may
depend on a target is declared with `visibility`, below.

```toml
[workspace]
name = "demo"
default_targets = ["//apps/cli:cli"]
```

## Safe scaffolding

`frost init` scans a directory with no manifest and writes the smallest build
it can describe without guessing package-manager behavior:

- C/C++ becomes native library/binary rules, with `main()` and `include/`
  recognized textually.
- Plain Java becomes one direct `javac` batch and one deterministic JAR. A
  detected package-qualified `main` becomes the JAR `Main-Class`; otherwise the
  result is a library JAR.

If both source families exist, `--language native` or `--language java` is
required so no source family disappears silently. Gradle or Maven project
markers stop automatic Java scaffolding because direct `javac` would omit
dependencies, plugins and lifecycle tasks; use an explicit `kind = "command"`
boundary, or `--language java` only when bypassing those semantics is
intentional. `--dry-run` prints without writing, and an existing `frost.toml`
is never overwritten.

## Toolchain and profiles

```toml
[toolchain]
cc = "cc"          # defaults shown; `gcc`/`g++` on a host without these names
cxx = "c++"
ar = "ar"
kofunc = "/path/to/kofun/bin/kofun" # optional; required by kofun_binary
cflags = ["-Wall"]
cxxflags = ["-std=c++20"]
ldflags = []

[toolchain.tools]
rustc = "rustc"    # named tools for kind = "command"
javac = "/opt/jdk/bin/javac"

[profile.debug]
cflags = ["-g"]

[profile.release]
cflags = ["-O3", "-DNDEBUG"]
ldflags = ["-s"]
```

`frost build --profile NAME` appends profile flags and writes
`.frost/{obj,lib,bin}/NAME/…`. Profiles coexist and have separate journal keys.
C sources use `cc`; `.cc/.cpp/.cxx/.C/.c++` use `cxx`. Any C++ source makes a
binary link with `cxx`. Compiler, C++ compiler, archiver, configured Kofun
compiler, named command tools, and sysroot identity are fingerprinted into
action keys. A named tool may be on `PATH`, absolute, or workspace-relative;
a workspace-relative wrapper is also a declared action input. C++20 modules
are not v1 functionality.

`arflags` overrides the archiver invocation. The default is `["rcsD"]`, whose
`D` asks for a byte-identical archive from identical members; on macOS it is
`["rcs"]`, because the cctools `ar` Xcode ships rejects `D` outright and would
otherwise fail every archive action. Point `ar` at `llvm-ar` and set `arflags`
to get the deterministic flag on a macOS host.

## Platforms (cross / device builds)

```toml
[platform.aarch64]
cc = "aarch64-linux-gnu-gcc"     # unset drivers inherit [toolchain]
cxx = "aarch64-linux-gnu-g++"
ar = "aarch64-linux-gnu-ar"
kofunc = "device-kofun"          # optional Kofun driver override
arflags = ["rcsD"]               # optional archiver-flag override
sysroot = "sysroots/aarch64"     # expands to --sysroot= on cflags/ldflags
cflags = ["-mcpu=cortex-a53"]    # appended after [toolchain] flags
ldflags = ["-static"]

[platform.aarch64.tools]
codegen = "tools/codegen-aarch64"
```

A platform is a toolchain overlay named in the root manifest; `host` is
reserved for the root `[toolchain]`. `frost build --platform NAME` (also on
`test`, `plan`, `graph`, `compdb`, `explain`, `clean`) selects it and is
orthogonal to `--profile`: outputs land in `.frost/{obj,lib,bin}/NAME/PROFILE/…`
and cache/journal identities carry the platform, so host and device builds stay
warm concurrently and switching between them never rebuilds. The platform's
resolved drivers are fingerprinted per build, so distinct cross-compilers never
share cache entries. Hermetic cross toolchains (for example `zig cc -target
aarch64-linux-musl` behind a wrapper script) work unchanged; genrules and shell
tests still execute on the host.

`frost build --all-platforms` and `frost test --all-platforms` run `host` and
every declared platform, keep going across platform failures, and finish with
one compact status tree. Platform runs are intentionally serialized because
the journal and content cache are shared; actions inside each run remain
parallel.

## Pinned external archives

The root manifest may pin a small, resolver-free external dependency. Package
manifests may reference it but may not declare it:

```toml
[fetch.zlib]
url = "https://example.invalid/zlib-1.3.1.tar.gz"
sha256 = "0000000000000000000000000000000000000000000000000000000000000000" # replace
strip_prefix = "zlib-1.3.1" # optional archive directory
vendor_dir = "vendor/zlib"

[target.codec]
kind = "cc_library"
srcs = ["src/codec.c"]
fetches = ["zlib"]
```

`url` must be absolute HTTP(S), `sha256` is exactly 64 hexadecimal characters,
and `strip_prefix`/`vendor_dir` are safe relative paths. Vendor directories may
not overlap. Archives are `.tar.gz`/`.tgz` or ZIP and may contain only regular
files and directories; absolute paths, `..`, links, special files, non-UTF-8
paths and the reserved `.frost-fetch.json` state file are rejected.

`frost fetch [NAME ...]` downloads explicitly, verifies SHA-256 before
publication, stores the verified archive in the local CAS, extracts into a
sibling staging directory, and renames the complete tree into `vendor_dir`.
A mismatch or extraction failure leaves an existing vendor tree unchanged.
The command is a no-op when the declaration, state and current tree agree;
`--force` downloads again. `--offline` never accesses the network and fails if
the requested materialization is missing or stale. Frost refuses to replace a
directory without matching ownership state.

Build, test, query and graph commands never fetch. A target naming a missing or
stale materialization fails with `run frost fetch NAME`. Every regular file in
the fetched tree plus its state file is a declared input of every action for
that target, so file content and executable-mode changes enter the existing
action key. Tree entry additions/removals invalidate the stored graph and are
enumerated on reload. Changing a fetched tree therefore rebuilds dependents by
content; it never turns a build into a network operation.

## C/C++ targets

```toml
[target.util]
kind = "cc_library"              # or cc_binary / cc_test
srcs = ["src/**/*.cpp"]           # required
deps = ["//generated:headers"]
includes = ["include"]            # transitively exported -I paths
cflags = ["-Werror"]
ldflags = ["-lm"]                 # binary/test only
```

Each translation unit gets `-MD -MF`; discovered headers become content inputs.
Generated outputs begin as order-only edges, so an unused generated header does
not invalidate every TU. Libraries use deterministic archives. `cc_test` links
like a binary and adds a cached execution action.

## Kofun targets

```toml
[toolchain]
kofunc = "/path/to/kofun/bin/kofun"

[target.compiler_seed]
kind = "kofun_binary"
srcs = ["src/compiler_seed.kofun"]
```

A `kofun_binary` has exactly one `.kofun` source, matching the current Kofun
CLI's single-input build contract. Frost runs
`kofunc build SOURCE -o BINARY --emit-c GENERATED_C` as one cacheable action.
Both artifacts are declared outputs, while the binary is the target's exported
output. The source and outputs of declared target dependencies are content
inputs. The active Kofun CLI does not expose a library artifact or Make-style
depfile, so `kofun_library` and dynamic Kofun dependency ingestion are not v1
functionality. An unchanged action is served from Frost's action cache.

## Genrules and tests

```toml
[target.generate]
kind = "genrule"
cmd = "tool ${in} -o ${out}"
inputs = ["schema/*.json"]
outputs = ["generated/model.c"]
deps = []
includes = ["generated"]

[target.integration]
kind = "test"
cmd = "scripts/integration.sh"
inputs = ["scripts/integration.sh"]
deps = ["app"]

# Prefer direct argv for language test runners.
[toolchain.tools]
pytest = "python3"

[target.python_unit]
kind = "test"
tool = "pytest"
args = ["-m", "pytest", "-q", "tests/unit"]
inputs = ["pyproject.toml", "src/**/*.py", "tests/unit/**/*.py"]
env = { PYTHONHASHSEED = "0" }
pass_env = ["PYTHONPATH"]
sandbox = false
```

Genrule substitutions are `${in}`, `${out}`, `${outs}`, `${pathsep}`,
`${dep:LABEL}` and `${deps:LABEL}`. `${pathsep}` expands to the host path
separator, so an extension-neutral launcher such
as `tools${pathsep}generate` can select `tools/generate` on POSIX and
`tools\generate.cmd` through `PATHEXT` on Windows. Genrules execute through the
host command shell (`/bin/sh -c` on Unix, `cmd.exe /C` on Windows) at the
workspace root. Authors must quote for that host shell intentionally.
All genrule outputs must exist after success and output ownership is unique.
A genrule `cmd` is one shell string, so `${deps:LABEL}` joins its paths with a
space — the separator `${in}` and `${outs}` already use there. That convention
belongs to the shell, which is why `env` refuses the same form rather than
borrowing a separator it has no basis for.
Tests choose exactly one of `cmd` or `tool`. A named-tool test uses direct argv
and supports `${in}`, `${deps}`, `${dep:LABEL}`, `${deps:LABEL}`, `${config}`,
`${profile}` and `${platform}`;
the multi-value forms occupy a whole argument. Its tool, args, declared inputs,
dependency outputs, `env` and `pass_env` are action-key material. Both forms
write the same Frost-owned success stamp only after a zero exit, so result
caching, failure cleanup, `test --affected` and `test --no-cache` behave
identically. Test targets do not declare `outputs`, steps, clean directories or
depfiles.

## Resource-aware scheduling

Any target may declare scheduler admission requirements. The declaration is
copied to each action produced for that target; it changes when an action may
start, never what the action builds:

```toml
[target.lto_link]
kind = "command"
tool = "linker"
args = ["--output", "${out_dir}/app"]
inputs = ["objects/**"]
outputs = [".frost/out/${config}/lto_link/app"]
resources = { cpu = 4, ram_mb = 8192 }

[target.hardware_test]
kind = "test"
tool = "runner"
args = ["tests/hardware.py"]
inputs = ["tests/hardware.py"]
resources = { exclusive = true }
```

`cpu` is an integer token count (default 1), `ram_mb` is MiB (default 0,
meaning no declared RAM reservation), and `exclusive = true` requires the
action to run alone. Frost admits ready actions only while their combined
requirements fit `--local-cpu-resources` and `--local-ram-resources`; defaults
are the host's available CPU count and physical RAM. `-j` remains a separate
upper bound on process count. An action declaring more than the configured
host budget is admitted alone while consuming that whole budget, rather than
deadlocking.

`--local-test-jobs` independently caps running test actions without lowering
compile/link parallelism. `frost simulate` accepts the same three flags and
uses the same deterministic admission rules; `build --stats` reports limits,
observed peaks, and whether admission constrained the run.

A test or `cc_test` target may declare `shard_count = N`, which makes it N
independently keyed, cached and scheduled actions instead of one. Frost does not
divide the cases — it cannot know them — it tells the runner which slice is its
own, through the environment test runners already implement:

| Variable | Value |
|---|---|
| `TEST_SHARD_INDEX` | this shard, `0`-based |
| `TEST_TOTAL_SHARDS` | `N` |
| `TEST_SHARD_STATUS_FILE` | path a runner touches to announce it understood |
| `GTEST_SHARD_INDEX` / `GTEST_TOTAL_SHARDS` | the same values under googletest's names |

**A runner that ignores these runs every case in every shard.** That is why
`shard_count` is declared per target rather than applied by Frost on its own,
and why the value belongs next to a test whose runner is known to honour it.
Frost passes `TEST_SHARD_STATUS_FILE` but does not yet check whether the runner
created it.

Each shard gets its own success stamp under
`.frost/test/<config>/<target>/shard-<i>-of-<N>/`, so one shard failing or being
invalidated leaves the others cached. Omitting the field, or writing
`shard_count = 1`, reproduces exactly the single action, identity and stamp that
Frost has always used, so adding the field to a workspace does not invalidate an
existing journal. Declaring any of the variables above in `env` or `pass_env`
alongside `shard_count` is an error rather than a silent override.

A test or `cc_test` target may also declare `flaky_retries = N` (default 0,
maximum 9), which gives a failing test that many more attempts before the
failure is its verdict. Each retry starts from the state a first attempt would
see: the partial success stamp is removed and clean directories are reset, so
attempt two does not run in the world attempt one left behind.

A test that passes only on a retry is reported as **flaky** and its success is
**not recorded** — not in the journal, not in the remote cache. The build is
green and dependents proceed, but the next run executes the test again. Caching
a verdict the test reached only on the second try would hide the flake from
every later build, including the one that would have caught it; the summary
line gains `N flaky` so the cost is visible instead. A test that fails every
attempt fails, and its output says `failed all N attempts` so the retries are
not mistaken for a single run.

Three options supply the same kinds of value from the command line instead of
the manifest: `--test-filter PATTERN`, `--test-env KEY=VALUE` and `--test-arg
ARG`, each repeatable except the filter. They are folded into every test action
of that invocation, and the command line wins over a manifest value of the same
name — it is the person typing now, and the override is visible because it
lands in the action key.

`--test-filter` travels as `TESTBRIDGE_TEST_ONLY` and `GTEST_FILTER` rather
than as a flag. Frost cannot know a runner's filter syntax, and inventing one
spelling per language is how a build tool acquires a table of special cases;
the environment is the protocol runners already implement, exactly as with
sharding.

Nothing new enters the action key to make these safe. `argv` and `env` are
already key material, so a filtered run simply *is* a different action and
cannot be served an unfiltered result. The converse cost is worth knowing: the
journal keeps one entry per action, so a filtered run replaces the unfiltered
one and alternating between the two re-executes each time. That is a cost, not
a correctness problem — what never happens is being handed the other question's
answer.

`--runs-per-test N` runs every test N times and requires all of them to pass.
It does not read the cache: a recorded single pass cannot answer "does this
pass N times", which is the only question worth asking N runs. It also
suppresses `flaky_retries` — hunting for a flake and hiding one are opposite
tools, and letting each run paper over its own failure would make the
repetition prove nothing. A failure says which run failed, because failing on
run 7 of 10 is a flake and failing on run 1 is a broken test.

`--test-output` chooses what reaches the terminal: `summary` for the counts
alone, `errors` (the default) for failing tests replayed in full after the run,
`all` for everything including what passing tests wrote. The default hides a
passing suite's output because that is the noise which buries the one failure
worth reading, and it replays failures at the end because during the run a
failure scrolls away behind the tests that were still going.

`flaky_retries` is deliberately **not** action-key material. It describes how
hard to look for a verdict, not what the test does, so turning it on does not
invalidate a result that already passed cleanly. It applies to test kinds only:
on anything else the field would parse and do nothing, and retrying a failed
compile is a different and much worse idea than retrying a test.

## Language-neutral command targets

Use `command` when the underlying tool has a real argv interface. Unlike a
genrule, Frost does not invoke a shell, so spaces and metacharacters are passed
literally and the executable is an explicit, fingerprinted toolchain input.

```toml
[toolchain.tools]
javac = "javac"
pack_jar = "frost"

[target.hello_java]
kind = "command"
tool = "javac"
inputs = ["src/Hello.java"]
outputs = [".frost/out/${config}/hello.jar"]
clean_dirs = [".frost/tmp/${config}/hello-classes"]
args = ["-d", "${clean_dir}", "${in}"]
steps = [
  { tool = "pack_jar", args = ["pack-jar", "--input", "${clean_dir}",
                                "--output", "${out}"] }
]
env = { SOURCE_DATE_EPOCH = "0" }
pass_env = ["JAVA_HOME"]
depfile = ".frost/out/${config}/hello.d" # optional Makefile syntax
preserve_outputs = true # opt in only for a compiler that incrementally reuses outputs
sandbox = false
```

Every output and optional depfile must contain `${config}`. It expands to
`PROFILE` on `host` and `PLATFORM/PROFILE` otherwise, preventing debug,
release and cross-device writes from colliding. Command arguments support:

| Variable | Expansion |
|---|---|
| `${in}` | one argv item per declared `inputs` path |
| `${deps}` | one argv item per output of declared target dependencies |
| `${dep:LABEL}` | the single declared output of one declared dependency |
| `${deps:LABEL}` | one argv item per output of one declared dependency |
| `${outs}` | one argv item per declared output |
| `${out}` | first declared output |
| `${out_dir}` | parent directory of the first output |
| `${output_dir}` | first declared owned output directory |
| `${output_dirs}` | one argv item per owned output directory |
| `${clean_dir}` | first declared clean intermediate directory |
| `${clean_dirs}` | one argv item per clean intermediate directory |
| `${depfile}` | configured depfile path |
| `${config}` | profile or platform/profile output-tree key |
| `${profile}` / `${platform}` | selected names |

`${dep:LABEL}` and `${deps:LABEL}` name one dependency instead of all of them,
so a consumer does not repeat the producer's output-path convention:

```toml
[target.app]
kind = "command"
tool = "javac"
deps = ["//greeting:greeting"]
args = ["-cp", "${dep://greeting:greeting}", "-d", "${clean_dir}", "${in}"]
```

`LABEL` must appear in this target's `deps`; resolving anything else would let
the argv name a file this target has no edge to, so the build could run before
that file existed. `${dep:LABEL}` requires exactly one declared output — a
dependency with several has no single path it could mean, and first-wins would
be silently wrong, so it is an error naming `${deps:LABEL}` instead. A
dependency that declares only `output_dirs` has no file output to substitute
and is an error for both forms: the tree stamp Frost writes for an owned
directory is its record of the contents, not a path for a tool. Reference the
directory through the producing target's own `${output_dir}`, or declare a file
output.

Both forms are also available in a genrule's `cmd`. `env` values take the
single-valued `${dep:LABEL}` only: an environment variable is one string, and
choosing a separator for several paths — `:`, `;`, a space — would be the
string-expression language this deliberately is not, so `${deps:LABEL}` in an
`env` value is an error saying so. Everything else in an `env` value passes
through untouched, because that value is handed to another program and `${...}`
in one is routinely that program's own syntax rather than a mistake:

```toml
[target.app]
kind = "command"
tool = "packager"
deps = ["//greeting:greeting"]
env = { GREETING_JAR = "${dep://greeting:greeting}", PS1 = "${HOME} $ " }
```

The expansion lands in argv and `env`, both action-key material, so a
dependency that moves its output rebuilds its consumers rather than replaying a
command naming a path that no longer exists.

The multi-value forms `${in}`, `${deps}`, `${deps:LABEL}`, `${outs}`,
`${output_dirs}` and `${clean_dirs}` must occupy a complete argument.
`${dep:LABEL}` is single-valued and composes inside a larger argument, so
`--flag=${dep:LABEL}` works. Neither plural form joins its items into one
path-separated string; an argument like a Java classpath that needs several
paths in one item is still written out by the author. Static `env` values and the present-or-absent value of every
`pass_env` name participate in the action key. All other host variables are
cleared; Frost then supplies its normal deterministic baseline and forces the
locale to `C`.

`steps` adds ordered named-tool invocations to the same atomic action. Every
step is direct argv, uses the same substitutions/environment/sandbox, and joins
the action key together with its tool identity. Frost stops at the first failed
step and never journals partial success. `clean_dirs` names
configuration-isolated intermediate directories that Frost removes and
recreates before the initial execution and a determinism rerun. This prevents
stale generated files—such as a removed Java inner class—from leaking into a
later archive without using an untracked shell wrapper.

Each clean directory is exclusively owned by one action. Configuration rejects
equal or nested clean directories across actions, and rejects a clean directory
that contains any declared graph input or output. Only undeclared intermediate
files belong there; stable final artifacts remain in `outputs`.

`timeout = <seconds>` stops any action of that target once it has run that
long: the process group is terminated, escalated to a kill if it ignores that,
and the action is reported as failed. The *group*, not just the child frost
waited on — killing only that pid would leave whatever it started running with
no parent and no limit, which is the same hang moved somewhere nobody is
looking. Declared outputs written before the limit expired are removed, since a
file that exists but is incomplete is worse than none. Nothing is journaled, so
the next build runs it again. A limit is not action-key material — the same inputs produce the
same result whatever the clock says — and the precedence is deliberate: the
target's own declaration wins over `--timeout`, which wins over the default
that only test actions carry.

By default Frost removes every declared output immediately before an action
reruns. Set `preserve_outputs = true` on a `command` target only when the tool's
incremental protocol reads or deliberately leaves prior outputs in place (for
example native `tsc` with `.tsbuildinfo`). The action key includes this choice.
Every retained file is still content-verified after success, and all compiler
state needed for a safe retry should itself be a declared output; a failed
action removes the possibly mixed output set. Clean builds and `frost clean`
continue to remove the whole configuration output state.

### Owned output directories

Some tools name their outputs after their content, so the file list cannot be
written down in advance: a bundler emits `assets/index-<hash>.js`, and `tsc
--outDir` emits whatever the module graph implies. A `command` target may
declare those directories instead of their files.

```toml
[target.web]
kind = "command"
tool = "npm"
args = ["run", "build", "--", "--outDir", "${output_dir}"]
inputs = ["src/**/*.ts", "package.json", "package-lock.json"]
output_dirs = ["dist/${config}"]
```

Frost owns a declared directory outright:

- it is removed before the action reruns, so the recorded tree is exactly what
  that run produced
- after success every file under it is scanned in a deterministic order,
  digested, recorded in the journal and published to the CAS, exactly like a
  declared output
- a cache hit restores the recorded tree and nothing else; a file the previous
  run left behind, or one Frost never recorded, does not survive a republish
- a missing or modified file inside it is restored from the CAS without
  rerunning the action
- the declared directory set is action-key material, so changing it does not
  reuse the earlier result

`outputs` may be empty when `output_dirs` is not. Because a directory is not a
graph file, Frost writes a stamp under `.frost/tree/CONFIG/TARGET/contents`
listing every recorded path with its digest, and that stamp is the target's
graph output: dependents take an ordinary edge to it, and a rerun that
reproduces the same tree produces the same stamp, so early cutoff applies to
trees as it does to single files.

Ownership must be unambiguous: an owned directory may not nest inside another,
and no declared output, clean directory or depfile may live inside one. Symlinks
inside an owned directory are rejected rather than republished as regular files.
Every entry must contain `${config}`.

An ecosystem command whose tree Frost should not own can still expose one stable
boundary artifact (for example a jar), pack the tree deterministically with a
small adapter, or remain wholly owned by Cargo/npm/Gradle/Maven.

### Dependency report formats

`depfile_format` selects how an action reports the inputs it actually read:

| Value | Source | Shape |
|---|---|---|
| `make` (default) | the declared `depfile` path | `gcc -MD -MF` output |
| `lines` | the declared `depfile` path | one path per line; blank lines and `#` comments ignored |
| `showincludes` | the action's captured output | `cl.exe /showIncludes` notes |

`showincludes` takes no `depfile` path, because MSVC has no `-MF` and writes its
includes to stdout; the notes are removed from the build log so a rebuild does
not print the whole include tree. The path is read after the last `: ` on the
line, which keeps it correct on a localized toolchain. `lines` exists so a
wrapper around a tool with some other dependency protocol can report what it
read without reproducing Makefile escaping. Reported paths under the workspace
root are recorded workspace-relative, and the recorded list is sorted, so it does
not depend on which spelling the tool printed. `--sandbox` also requires every workspace input to be declared,
so package managers that traverse a module cache normally use `sandbox = false`.

## Coverage

For native `cc_test` targets, `frost test --coverage` owns the complete gcc
pipeline:

```sh
frost test --coverage --explain
# one deterministic tracefile per test target:
# .frost/coverage/debug+coverage/<target>.lcov
```

Coverage is a configuration axis, not a profile. Compile and link actions get
`--coverage`; objects, the graph store, journal entries, raw counters and final
tracefiles use the collision-free `<profile>+coverage` configuration. Switching
back to an ordinary test therefore reuses its ordinary cache instead of being
invalidated by the instrumented run. `--explain` names the `coverage:<target>`
action and every compile/link/test action it depended on.

Each test shard owns a separate `.gcda` directory. Frost resets it before every
execution (including retries and forced reruns), records its files in CAS, and
writes a content stamp used by the merge action. A test-success stamp is empty
and is deliberately not used as the counter content: changing `--test-env`, a
filter, or a test argument may execute different lines while still passing.
One merge action per test target keeps invalidation local; changing one
independent test does not re-merge the others.

The automatic path supports C/C++ compiled with GCC and reported by gcov. Set
`gcov = "..."` beside `cc` in `[toolchain]`, or in a `[platform.NAME]` overlay,
when the reporter is not the default `gcov`; a cross compiler normally needs
its matching reporter such as `aarch64-linux-gnu-gcov`. Clang/`llvm-cov`, HTML
rendering and automatic collection from `kind = "test"` or `kind = "command"`
are not provided. Such adapters may declare their raw outputs and invoke the
manual lcov command below when they produce gcov-compatible data.

`frost coverage-lcov` merges one run's gcov data into an lcov tracefile:

```sh
cc --coverage -c m.c -o obj/m.o && cc --coverage obj/m.o -o m
GCOV_PREFIX=gcda GCOV_PREFIX_STRIP=99 ./m
frost coverage-lcov --gcda gcda --objects obj --output coverage.lcov
```

frost emits the format itself rather than shelling out. Neither `lcov` nor
`gcovr` ships with a toolchain — `gcov` does — so delegating would put a Perl
dependency in every CI image that wanted coverage, for a mapping that is a few
record types wide.

**`.gcda` counters accumulate across executions.** Run the same instrumented
binary twice and the hit counts double, so a tracefile built from counters left
where they fell differs on every rerun — and it reads as nondeterminism in the
build rather than in gcov's data model. The counter directory must therefore be
reset before each run: `GCOV_PREFIX` puts it somewhere that can be, since gcc
otherwise writes `.gcda` into the object tree, which holds declared outputs and
cannot be cleared. With that, the same inputs produce a byte-identical
tracefile, which a test pins.

`SF:` paths are workspace-relative and records are sorted by file and by line.
gcov reports absolute paths in directory and discovery order, and neither
survives a move to another machine, which is where a coverage report usually
goes. A file outside the workspace — a system header — is dropped rather than
recorded with a path only this machine has.

A run that produced no data is refused rather than written as an empty
tracefile: 0% is a number someone would act on, and "not measured" is a
different statement from "nothing covered".

**gcc only.** clang writes a different format that `llvm-cov` reads; the
toolchain, not the host, is what decides, and the real-tool tests skip when
`cc` is not gcc.

## Visibility

Multi-package labels let a workspace split into modules. Visibility is what
makes those modules mean something: without it any target may depend on any
other, so the boundary exists only in whatever discipline the team keeps, and
the first deadline erases it.

```toml
[target.core]
kind = "cc_library"
srcs = ["src/core.c"]
visibility = ["group:middle"]      # or ["//apps/...", "//tools/cli:cli"]
```

```toml
# Root manifest only: a boundary shared by more than one target is written
# down once, so widening it is a single reviewable edit.
[visibility.middle]
allow = ["//text/...", "//render/..."]
```

Four spellings, and nothing else:

| entry | admits |
|---|---|
| `//...` | every package |
| `//apps/...` | that package and everything under it |
| `//apps/cli:cli` | that one target |
| `group:NAME` | whatever the root manifest's `[visibility.NAME]` allows |

`//apps` on its own is refused. It is one character from `//apps/...` and means
something different, and a boundary that opens because frost guessed which was
meant is worse than an error.

**A target is always visible inside its own package.** A package is the unit
people already treat as one thing; requiring a declaration to use your own
neighbour would make the feature nothing but noise.

**The default is public**, which is not what Bazel does. A private-by-default
rule would break every existing workspace on upgrade, and a correctness feature
that arrives as a wall of errors is one people turn off — after which it
protects nothing. Instead `frost lint`'s `undeclared-visibility` names the
targets where a boundary is *already being crossed*, so the migration is a list
you can work through. `visibility = []` is how a target says "my own package
only".

Enforced when the manifest loads, not when something is built: a dependency
that is not permitted is a statement about the manifest, and reporting it only
when you happen to build that path would make the boundary depend on what you
asked for. The error names both ends, the rule that applied, and the narrowest
entry that would admit the dependent.

Groups are one level deep — a group listing another group is refused. Query
commands are unaffected: `frost query` answers what the graph *is*, and
visibility is about what a build may ask for.

Not action-key material (docs/16): visibility says who may ask for a target,
not what building it produces, so declaring a boundary costs no rebuild.

## Per-platform target sections

`[platform.*]` swaps a toolchain. It cannot swap a *source*, which is the
difference C/C++ workspaces hit first: one file for POSIX, another for the
device.

```toml
[platform.device]
cflags = ["-DDEVICE=1"]

[target.lib]
kind = "cc_library"
srcs = ["src/common.c", "src/host.c"]
cflags = ["-DTARGET=1"]

[target.lib.platform.device]
srcs = ["src/common.c", "src/device.c"]   # replaces
cflags = ["-DEXTRA=1"]                    # appends
```

A section may set `srcs`, `deps`, `includes`, `cflags` and `ldflags`, and
nothing else. `kind`, `outputs` and `tool` are absent on purpose: a platform may
change what a target is built from, never what it *is*, so `frost query`
answers the same question whatever you are building for.

| key | rule | why |
|---|---|---|
| `srcs`, `deps`, `includes` | replace | a set is an identity; appending would compile `host.c` and `device.c` into the same library |
| `cflags`, `ldflags` | append | flags already accumulate — toolchain, then profile, then target — so this is that rule one level down, not a new one |

The accumulation order is `[toolchain]`, `[platform.NAME]`, the target, then the
target's platform section.

There is no predicate language, and there will not be one: a section names a
platform the workspace already declared, and an undeclared name is refused at
load with a suggestion. That check matters more than it looks — an overlay under
a misspelled name would otherwise sit in the manifest looking applied and never
fire, and the symptom is a cross build quietly compiling the wrong sources.

Deps declared in a section are checked for existence and visibility like any
other, so a boundary cannot hold on one platform and not another. The target set
does not change per platform: a section chooses among targets that exist either
way.

The resolved value is what reaches the action key, so a platform section changes
the key exactly as writing the same value at the top level would — being
conditional is a property of how a value was written, not of the value. Each
platform keeps its own outputs and cache entries, and `frost plan` names the
sections that applied.

## Build stamping

A binary that reports its version has to get that version from outside the
build. Doing it the obvious way — a git SHA in a compile flag — makes every
commit change every action key, and an incremental build tool stops being one.

```toml
[stamp]
command = ["tools/workspace_status.sh"]
# stable_prefix = "STABLE_"   # the default
```

The command runs once per build, from the workspace root, and prints one
`KEY=VALUE` per line:

```
STABLE_GIT_SHA=9f2c1ab
STABLE_VERSION=1.4.0
BUILD_TIME=1764691200
```

`${stamp.KEY}` then expands in a `kind = "command"` target's `args` and `env`.
The split is by **rate of change**, decided by the key's name:

| | in the action key | when the value changes |
|---|---|---|
| `STABLE_*` | yes | the action re-runs, and so does anything whose inputs its output changed |
| everything else | no | only the action that reads it re-runs, every build |

A stable value rebuilding the binary that embeds it is the correct answer, not
cache thrash: a binary reporting the wrong commit is worse than a rebuild. A
volatile value in an action key would rebuild the workspace every second, so an
action that reads one is instead re-executed unconditionally — one action, not
the graph above it. If its output bytes come out the same, early cutoff stops
there.

Deciding by **name** rather than by value is what lets frost classify a
reference without running the command, so the graph can be built and a manifest
validated at load, and the graph stays a pure function of the manifest.

**A volatile value must not reach a compile.** A command target that writes
`version.h` containing a build time re-runs every build by design — that part is
cheap. But the header's bytes then differ every build, so every translation unit
including it recompiles and everything above them relinks. One unconditional
action becomes a full rebuild. frost rejects that at load, naming the value, the
file and the way out, because the symptom ("our builds stopped being
incremental") shows up months later and nowhere near the manifest that caused
it.

Genrules do not expand `${stamp.…}`. A genrule runs through a shell, where
frost cannot tell a value it substituted from one the shell produced; the error
says so rather than reporting an unknown variable.

**When it runs.** Only when something in the closure actually reads a stamp. A
workspace that stamps its release binary does not pay for a `git describe` — or
get broken by a status script that stopped working — when it builds a library.
The command inherits the invoking environment rather than frost's action
baseline: it is not an action, its output is not cached or sandboxed, and a
status script needs the PATH and credentials of whoever ran frost.

**When it fails.** The build fails, and the script's own diagnostic is kept.
`--stamp-optional` downgrades that to a warning and leaves every value empty —
off by default, because a status script that quietly stopped working is how a
release binary ends up reporting no version at all in a build that looked
green. `--no-stamp` skips the command entirely and expands every reference to
nothing; a stamp-free build is a different build and its action keys say so,
rather than reusing results that embedded a value.

## Incrementality and diagnostics

The BLAKE3 action key covers canonical argv/cwd, environment whitelist,
toolchain closure, declared output paths, declared owned output directories, and
declared and discovered input content. The binary journal is append-only and ignores incomplete crash tails.
The CAS restores missing output without execution; byte-identical output cuts
off downstream work.

`frost plan`, `build --explain`, `explain TARGET`, `graph --dot`, `compdb`, and
`build --trace FILE` expose planning and execution. `--sandbox` hides undeclared
workspace paths on Linux; `--check-determinism` reruns selected actions.

## frost lint

`frost lint` reports manifest patterns that parse, build, and cost something
later. It exits 1 when it finds anything and 0 when it does not, so it gates CI
without a wrapper that interprets its output.

Every rule catches something nothing else does — that is the entry requirement,
and it excluded several obvious candidates. Duplicate outputs, an undeclared
profile, an absolute path in a declared path field and a glob matching no files
are all already hard errors; a lint that restates an error teaches people that
lints are noise.

| Rule | What it finds | Why it costs |
|---|---|---|
| `unreachable-target` | not a default, not a test, and nothing depends on it | never built unless named, so it rots unnoticed |
| `missing-include-dir` | an `includes` entry that is not a directory and nothing generates | the compiler gets a `-I` that resolves nothing, so a missing header fails further away |
| `volatile-pass-env` | `pass_env` naming `PATH`, `HOME`, `TMPDIR`, `TMP` or `TEMP` | those are deliberately outside the action key (docs/16); naming one puts it back, so nothing the target builds is shared between machines |
| `absolute-path` | an absolute path in `args`, `cmd` or an `env` value | those fields are free text and nothing else validates them, so the build works on one machine |
| `shell-dependent-cmd` | `&&`, `\|\|`, `\|`, `>`, `<` or `;` in a genrule `cmd` | a genrule runs through `/bin/sh` on Unix and `cmd.exe` on Windows |

A finding can be true and unavoidable. A Maven build genuinely needs
`$HOME/.m2`, so `volatile-pass-env` is correct and the workspace still has to
pass `HOME`. `lint_allow` records that per target:

```toml
pass_env = ["HOME", "JAVA_HOME"]
lint_allow = ["volatile-pass-env"]
```

Written next to the thing that pays the cost, which a global ignore file would
not be.

### `--json`

```json
{
  "findings": [
    {
      "rule": "volatile-pass-env",
      "target": "boot_jar",
      "message": "pass_env names \"HOME\"",
      "why": "its value differs per machine and per CI step, ..."
    }
  ],
  "count": 1,
  "by_rule": { "volatile-pass-env": 1 }
}
```

`by_rule` is there so a CI job can threshold one rule without parsing
`findings`. Findings are ordered by target then rule, so two runs can be
diffed.

## frost fmt

`frost fmt` rewrites every manifest in the workspace in one canonical form, and
`frost fmt --check` reports whether anything would change without writing,
exiting 1 if so.

The point is not that any particular order is better. It is that two people
writing the same target produce the same bytes, so a review shows what changed
rather than who wrote it.

- Keys inside a `[target.*]` table follow a fixed order, grouped by what a
  reader is asking: what kind of thing this is, what it reads, what it
  produces, how it runs. An unrecognized key — a manifest from a newer frost,
  or a typo the parser rejects a moment later — is kept, after the known ones,
  in the order it was written.
- `[target.*]` tables are emitted in name order.
- An array whose entries exceed 76 characters goes one per line with a trailing
  comma; a shorter one stays inline. Both spellings are canonical for their
  width, so neither is rewritten on a second run.

Comments and string contents are preserved: `# needs HOME for the dependency
cache` explains a decision the keys around it cannot, and a formatter that
dropped them is one nobody would run twice.

Two properties are tested rather than asserted in prose. Formatting is
**idempotent**, without which `--check` could fail on its own output. And
formatting **never changes what the manifest means** — the same manifest parses
to the same targets, sources, dependencies and flags before and after, which is
the property reordering keys and tables could plausibly break.

## .frostrc

`frost.toml` says *what* to build. `.frostrc` says *how*, so a team default does
not have to live in a shell alias.

Two files are read, in this order: `~/.config/frost/frostrc` (or
`$XDG_CONFIG_HOME/frost/frostrc`) and then `<workspace>/.frostrc`. Within each,
sections apply in a fixed order: `[common]`, then the subcommand's own section,
then each `--config NAME` in the order it was given.

```toml
[common]
jobs = 16

[build]
profile = "release"

[config.ci]
sandbox = true
remote-cache = "https://cache.example.com/frost"
```

Precedence, lowest to highest:

    built-in default  <  user file  <  workspace file  <  --config section  <  what you typed

Keys are long option names, with `_` or `-` — `no_tui` and `no-tui` are the same
option. A boolean `true` is the flag; `false` contributes nothing, so turning
off a workspace default does what it looks like. An array repeats the option.
`--no-frostrc` ignores both files entirely, and `frost doctor` lists every
setting in effect with the file, line and section, because "a setting applies"
is not useful without the line to go and change.

Two questions, kept apart. A key in a **subcommand's own section** must be an
option of that subcommand, because naming the section is naming the command:
`[build] test-filter` is refused. A key in `[common]` or a `[config.*]` set must
be an option of *some* subcommand; it is applied where it fits and skipped where
it does not, which is what "common" has to mean to be worth writing —
`[common] jobs` would otherwise break `frost doctor`, which has no `--jobs`.

Either way, a key no subcommand accepts anywhere is a typo and is refused at
startup with the file, the line, the key and a suggestion — checked against the
real argument tree, so a new option works in a config file the moment it exists
on the command line. `frost doctor` lists every setting in effect, including the
ones a subcommand skipped.

**A flag from a file is a flag.** It is spliced ahead of the real command line
and parsed by exactly the code that parses a typed one, so it is validated the
same way and reaches the action key the same way. Whether an option is key
material is a property of the option, never of where its value came from —
changing `profile` in `.frostrc` rebuilds, and `sandbox` does not, for the same
reasons they do or do not on the command line.

Not supported, deliberately: conditional syntax like `build:linux --foo`
(platform differences belong to `[platform.*]`), and `--config` sections that
reference other `--config` sections. One level only.

## Build event stream

`--build-event-json FILE` writes one JSON object per line describing the build,
so a CI job can count failures, chart durations or find the slow target without
parsing terminal output. It is independent of the display: asking for it does
not change what a person sees.

```json
{"event":"build_started","actions":5,"jobs":4,"schema":"frost-build-events-v1","seq":0}
{"event":"action_started","id":"compile:util:src/util.c","desc":"CC src/util.c (util)","schema":"...","seq":1}
{"event":"action_finished","id":"compile:util:src/util.c","result":"executed","cached":false,"duration_ms":31,"schema":"...","seq":2}
{"event":"build_finished","success":true,"elapsed_ms":55,"schema":"...","seq":11}
```

`result` is one of `cached`, `executed`, `flaky`, `failed`, `skipped`,
`would_run`, `may_run`. These are stable names of their own, deliberately not
the display strings — the terminal says "cache miss" for an action that ran,
which is right on a terminal and wrong in a field a machine switches on.
`detail` is `null` unless there is something to say, so a consumer tests it for
null rather than comparing against an empty string.

The events are the same ones the progress display consumes, written by the same
thread, so a dashboard and a human cannot disagree about what happened.

**On determinism.** The *content* is deterministic: the same build reports the
same actions with the same results. The *order* is emission order, and under
parallelism that follows whichever worker finished first — real information
about the run rather than noise. Promising a stable order would mean buffering
the whole build before writing a line, which defeats a stream a dashboard reads
while the build is going. A consumer comparing two runs should sort by `id`; at
`-j 1` the stream repeats exactly, timings aside.

A fully cached rerun takes the all-cached fast path and reports a single
`all_cached` event rather than one per action. That is what keeps that path
O(1), and it is why comparing a cold build's events to a warm one's compares
two different things.

### A JUnit report from the stream

`scripts/frost_junit.py` converts the stream to the JUnit XML that CI systems
already render, and to a Markdown summary for `$GITHUB_STEP_SUMMARY`:

```sh
frost test --all --keep-going --no-tui --build-event-json events.ndjson
python3 scripts/frost_junit.py events.ndjson \
    --output junit.xml --summary "$GITHUB_STEP_SUMMARY"
```

A script rather than a subcommand, because the shape of a test report is a
property of the CI system reading it, and one vendor's dialect does not belong
inside the build engine. `.github/workflows/ci.yml` runs it on `sample_multi`,
which is what makes the rendering something you can look at rather than
something this document asserts.

Two of its decisions are about *not* reporting a green build that was not:

- A build that broke before any test ran has no test events, and a report of
  zero tests and zero failures reads as "nothing wrong". So a non-test action
  that did not pass is reported too, in a `build` suite. `--all-actions` adds
  the ones that passed, which are noise in a test report.
- A fully cached rerun has no per-action events either. It becomes one passing
  case that says so.

A test that passed only on a retry is a `flakyFailure` — surefire's spelling —
*and* a `system-out` line, so a viewer that has never heard of the element
still shows that the pass was not free. Shards keep their `#0/3` marker in the
case name, because two cases with the same name are deduplicated by most
viewers, hiding half the work.

**On a schema it does not know.** A stream whose `schema` is not the one the
script was written against is refused, with the exit code frost uses for "could
not run the work as asked" — a bump means a field changed meaning or left, and
reading it anyway would report the wrong thing confidently. Unknown *events*
and unknown *fields* are ignored instead, which is the other half of the
additive promise: a stream that grew a field must not break a reader. The one
addition it cannot absorb silently is an unknown `result`, which is reported as
an `error` rather than a pass — guessing green would hide exactly the outcome
the reader was too old to understand.
