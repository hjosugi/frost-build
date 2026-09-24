# Migrating from Bazel

Frost does not evaluate Starlark and does not claim to be a drop-in Bazel.
What it offers is two separate paths, and you can take either or both:

- **Import** the native C/C++ part of a Bazel workspace into `frost.toml` with
  `frost import-bazel`, conservatively, stopping on anything it cannot
  translate faithfully.
- **Keep Bazel** as the source of truth and use Frost only for the developer
  loop with `frost bazel-dev`: rebuild on save, restart the program only after
  a successful build.

This page is not executed in CI, unlike the other guides, because it needs a
Bazel installation and network access to fetch one. The importer itself is
covered by tests over recorded `bazel query` output
(`crates/frostbuild-cli/src/bazel.rs`) and an end-to-end test with a stand-in
`bazel`. The design record is
[docs/23_bazel_migration.md](../../23_bazel_migration.md).

## Path 1: import the native C/C++ subset

### 1. Preview

`import-bazel` asks Bazel itself to load the workspace — macros and globs are
evaluated by Bazel, not by a Frost reimplementation of Starlark — and reads
the result of `bazel query --output=xml --noimplicit_deps`. The preview writes
nothing:

```sh skip
frost -C my-bazel-workspace import-bazel --dry-run
```

It prints one manifest per Bazel package, headed `# === path/frost.toml ===`,
and a summary line such as `frost: Bazel import preview · 5 rules · 3
packages`.

On a large workspace, import one binary's closure first:

```sh skip
frost -C my-bazel-workspace import-bazel --dry-run \
  --query 'deps(//apps/cli:cli)'
```

`--bazel PATH` (or `BAZEL_BIN`) selects Bazelisk or a specific binary;
otherwise `bazel`, then `bazelisk`, is looked up on `PATH`.

### 2. Read the refusal, if there is one

The importer translates only what it can translate exactly, and it checks the
whole plan before writing anything: one unsupported rule stops the import, and
no half-migrated workspace is left behind. It stops on:

| Bazel feature | Why it stops | What to do |
|---|---|---|
| `select()` anywhere in the query result | configuration-dependent; flattening it would pick one branch silently | import one configuration's closure with `--query`, or express the difference as a `[target.NAME.platform.PLAT]` section by hand |
| external repositories (`@repo//...`) | Frost does not resolve Bazel modules | vendor the dependency, or pin it with `[fetch.NAME]` and write its target |
| `filegroup`s or generated files in `srcs` | a source that is another rule's output | write that rule as a `genrule` or `command` target first |
| `data`, `defines`, `include_prefix`, `strip_include_prefix`, `additional_linker_inputs`, `nocopts`, `win_def_file` | semantics Frost's native rules do not model | translate by hand, or keep that part in Bazel |
| `alwayslink`, `linkshared` | shared-library and whole-archive link semantics | keep in Bazel |
| `linkopts` on a `cc_library` | Frost does not export link flags transitively | move the flag to the binaries that need it |
| `$(...)` make variables in `copts`/`linkopts` | Bazel-evaluated expansions | resolve them to literal flags first |
| a `srcs` entry that is not a `.c`/`.cc`/`.cpp`/`.cxx`/`.C`/`.c++` file — including a private `.h` — or lives in another package | not a translation unit of this target | move private headers to `hdrs`; move shared sources into their own library |
| a header-only library (no `srcs`) | Frost's native rules need at least one source | keep it as an `includes` directory of its dependents, or add the source it exports |
| any rule class other than `cc_library`, `cc_binary`, `cc_test` | out of the importer's contract | leave Java, Python, TypeScript and custom rules as Bazel-owned `command` boundaries |

### 3. Import

```sh skip
frost -C my-bazel-workspace import-bazel
```

It writes one `frost.toml` per Bazel package plus a root manifest with a
`[workspace]` whose `default_targets` are the imported binaries and tests, a
`cc`/`c++` toolchain, and `debug`/`release` profiles. It never overwrites an
existing `frost.toml`. BUILD and MODULE files are left untouched.

What is carried over, per rule:

| Bazel | Frost |
|---|---|
| `cc_library` / `cc_binary` / `cc_test` | the same `kind` |
| `srcs` (C/C++ translation units in the same package) | `srcs`; `hdrs` are not carried over, because the compiler's dependency output reports every header a compile reads |
| `deps`, `implementation_deps` | `deps`, rewritten to `//package:name` labels |
| `includes` | `includes` |
| `copts`, `local_defines` | `cflags` (`-DNAME` for each define) |
| `linkopts` on binaries and tests | `ldflags` |

Target names are sanitized to `[A-Za-z0-9_-]`, so `//lib:math-core` stays
`//lib:math-core` while a name with other characters is rewritten; the import
stops if two names collide after sanitizing.

### 4. Review the toolchain, then compare

The generated `[toolchain]` is a reviewable scaffold, not an extraction of
Bazel's configured `cc_toolchain`: set the compilers and flags your Bazel
build actually used. Then build both ways and compare behavior and test
results before deleting anything:

```sh skip
bazel build //... && bazel test //...
frost -C my-bazel-workspace build
frost -C my-bazel-workspace test --all
frost -C my-bazel-workspace compdb    # diff flags against `bazel aquery` if needed
```

Keep the BUILD files until clean, incremental, test and binary behavior agree.

## Path 2: keep Bazel, add the developer loop

`bazel-dev` watches the workspace, asks Bazel to build after each coalesced
edit, and restarts the program — its whole process tree — only when that build
succeeds. A broken edit leaves the last good process running.

```sh skip
frost -C my-bazel-workspace bazel-dev //apps/server:server -- --port 3000

# Bazelisk, and Bazel flags passed through:
frost -C my-bazel-workspace bazel-dev //apps/server:server \
  --bazel /opt/bin/bazelisk --bazel-arg=--config=dev -- --port 3000
```

Bazel keeps everything it owns: BUILD evaluation, the configured graph,
runfiles, its cache and server. `.git`, `.frost` and Bazel's output symlinks
are ignored by the watcher, so Bazel's own writes do not trigger rebuilds.

## After importing

Imported targets are ordinary Frost targets, so the rest of the guide applies:
`frost run`, `frost dev` (watch and restart), `frost debug`, `frost ide`, `frost
query` and `frost test --affected`. The capability comparison — what Bazel has
that Frost does not, and which of those gaps are deliberate — is
[docs/14_bazel_gap_analysis.md](../../14_bazel_gap_analysis.md).
