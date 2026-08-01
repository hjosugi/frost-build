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
current package, while `//:name` addresses a root target. Visibility is a v1
non-goal.

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
`${dep:LABEL}` and `${deps:LABEL}`. The
last expands to the host path separator, so an extension-neutral launcher such
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
and the action is reported as failed. Nothing is journaled, so the next build
runs it again. A limit is not action-key material — the same inputs produce the
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

## Incrementality and diagnostics

The BLAKE3 action key covers canonical argv/cwd, environment whitelist,
toolchain closure, declared output paths, declared owned output directories, and
declared and discovered input content. The binary journal is append-only and ignores incomplete crash tails.
The CAS restores missing output without execution; byte-identical output cuts
off downstream work.

`frost plan`, `build --explain`, `explain TARGET`, `graph --dot`, `compdb`, and
`build --trace FILE` expose planning and execution. `--sandbox` hides undeclared
workspace paths on Linux; `--check-determinism` reruns selected actions.
