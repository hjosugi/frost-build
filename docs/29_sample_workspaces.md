# Sample workspaces

Five checked-in workspaces. They exist to be run, not only read, and each one
is the smallest thing that demonstrates a decision you will face in a real
repository.

| Workspace | Shows | Built in CI |
|---|---|---|
| [`sample_c/`](../sample_c) | The single-manifest starting point: native rules, a genrule, early cutoff | yes |
| [`sample_multi/`](../sample_multi) | Packages, labels, a diamond, cross-package generated headers | yes |
| [`sample_java/`](../sample_java) | Multi-module Java where Frost owns `javac` | yes, where a JDK is present |
| [`sample_spring/`](../sample_spring) | Spring Boot via Gradle, wrapped as one cached action | no — needs the network |
| [`sample_maven/`](../sample_maven) | The same wrapping with Maven | no — needs the network |

`sample/` is not in this list: it is generated benchmark input for
`frost-bench` and the Bazel comparison, not a workspace to read.

## The decision these samples are really about

There are two ways to build something with Frost, and choosing wrong is more
expensive than any amount of tuning.

**Frost owns the tool.** Every compiler invocation is a Frost action. Frost
sees each source, each output, each discovered header, so it caches at the
translation-unit level, cuts off early when a regenerated file is byte
identical, and rebuilds exactly what a change reached. `sample_c`,
`sample_multi` and `sample_java` work this way.

**Frost owns the boundary.** An ecosystem tool — Gradle, Maven, npm, Cargo —
owns dependency resolution and its own task graph. Frost runs the whole thing
as one action with declared inputs and an owned output tree, caches the tree,
and does not look inside. `sample_spring` and `sample_maven` work this way.

The second is not a lesser mode; it is the honest one whenever reproducing the
tool's semantics would mean reimplementing it. A build tool that guesses at
another build tool's dependency graph ships wrong answers, and the wrongness
shows up as a stale artifact months later. The cost is granularity: a miss
reruns the whole module.

Pick by asking who resolves dependencies. If the answer is not Frost, wrap it.

---

## `sample_c` — one manifest

```bash
frost -C sample_c build
./sample_c/.frost/bin/debug/app          # frost: 42   (.exe on Windows)
frost -C sample_c build                  # frost: up to date
frost -C sample_c build --explain
```

Bare target names, no `[workspace]` section: the legacy single-package form,
and the right shape for a small repository. A `genrule` writes `gen/config.h`
through paired `tools/gen_config` and `tools/gen_config.cmd` launchers, which
`${pathsep}` selects between, so the same manifest works on POSIX and Windows.

The generator's output is deliberately constant. Re-running it produces
identical bytes, so early cutoff stops there and no downstream compile reruns —
the property to watch with `--explain`.

`src/util_internal.h` is included only by `util.c`. Editing it must recompile
`util.c` and not `main.c`; that is a checked E2E, and it is the difference
between depfile-narrowed inputs and rebuilding a directory.

## `sample_multi` — packages, labels, and a shape

```bash
frost -C sample_multi build
./sample_multi/.frost/bin/debug/apps_cli_cli    # frost 1: 42
frost -C sample_multi test --all
```

Four packages (`core`, `text`, `render`, `apps/cli`), each with its own
`frost.toml`, discovered from the root `[workspace]`. Labels are
`//path/to/package:name`; `//:gen_version` addresses the root package.

The shape is the point:

```
              //apps/cli:cli
              /            \
      //text:text      //render:render
              \            /
              //core:core
                   |
              //:gen_version   (writes gen/version.h)
```

**The diamond.** `cli` reaches `core` two ways. That is why the graph queries
need this workspace and cannot use `sample_c`:

```bash
frost -C sample_multi query somepath //apps/cli:cli //core:core   # one route
frost -C sample_multi query allpaths //apps/cli:cli //core:core   # both
```

`somepath` is enough to explain a rebuild. It is not enough to remove a
dependency: cutting the single edge it names leaves the other route intact.

**The cross-package generated header.** `//:gen_version` writes `gen/version.h`
in the root package; `//core:core` compiles against it, and the three targets
above `core` inherit it transitively. So:

```bash
frost -C sample_multi query owners gen/version.h
```

reports all five consumers and not the genrule that writes it. That is the
file→target direction — "what must rebuild when this changes" — and it is
answered from the configuration alone, with no build required.

A header that Frost learns about only from a depfile is *not* reported: that is
build state, and making the answer depend on whether someone built recently
would be worse than the gap. `frost explain` covers that case.

**The E2E worth reading.** `multi_package_sample_builds_runs_and_caches` edits
`core/src/core.c` and requires the new answer to arrive at `cli`. A stale
`render` would still link and still print — which is exactly the class of bug a
diamond exists to catch.

## `sample_java` — multi-module Java, Frost owning `javac`

```bash
frost -C sample_java build
java -cp sample_java/app/.frost/out/debug/app.jar:sample_java/greeting/.frost/out/debug/greeting.jar \
     com.example.app.App                     # frost: 42
```

`frost` must be on `PATH`: the jar step is `frost pack-jar`, which writes a
deterministic archive — sorted entries, fixed timestamps — so an unchanged
input yields identical bytes and dependents stay cached. An ordinary `jar`
embeds a timestamp and defeats that.

The directory layout is Gradle's and Maven's
(`<module>/src/main/java/<package>/`) on purpose: the same tree, without either
tool. Two modules, `greeting` and `app`, each a `command` target — `command`
rather than a native rule because `javac` has a real argv interface, so Frost
passes arguments directly with no shell to quote for and fingerprints the
compiler as a toolchain input.

**What is not in `app/frost.toml`: where `greeting` puts its jar.**

```toml
deps = ["//greeting:greeting"]
args = ["-cp", "${deps}", "-d", "${clean_dir}", "${in}"]
```

`${deps}` expands to the declared outputs of the targets this one declared a
dependency on. Writing `../greeting/.frost/out/${config}/greeting.jar` by hand
would work today and break the moment the layout convention changes — which is
the leak [issue #158](https://github.com/hjosugi/frost-build/issues/158) is
about.

The honest limitation: `${deps}` produces one argv item per output, which is
what `-cp` wants for **one** dependency and not for several. A classpath
joining two dependency jars into a single path-separated argument is not
expressible yet; that is the other half of #158.

Two details that are about Java rather than about Frost:

- `pass_env = ["JAVA_HOME"]`, because on macOS `javac` is a stub that selects a
  JDK from it. A build that cleared it would compile against a different JDK
  than the one that runs the result. The variable's present-or-absent value is
  action-key material, so switching JDKs invalidates correctly.
- `clean_dirs` is removed and recreated before every run, so a class deleted
  from the sources cannot survive inside the jar.

- `sandbox = false`, because `javac` reads a JDK installation outside the
  workspace.

## `sample_spring` and `sample_maven` — wrapping a tool that owns its graph

```bash
frost -C sample_spring build
java -jar sample_spring/build/debug/libs/sample-spring-0.0.1.jar   # frost: 42

frost -C sample_maven build
java -jar sample_maven/target/debug/sample-maven-0.0.1.jar         # frost: 42
```

Neither runs in CI. Gradle and Maven resolve plugins and dependencies from the
network, which is a dependency the correctness suite deliberately does not
take; a test that fails when a repository is slow teaches people to ignore test
failures. Run them locally with the tool on `PATH`.

Both are one `command` target around one invocation:

```toml
[target.boot_jar]
kind = "command"
tool = "gradle"
args = ["bootJar", "--no-daemon", "--console=plain", "-PfrostBuildDir=build/${config}"]
inputs = ["build.gradle", "settings.gradle", "src/main/java/**/*.java"]
output_dirs = ["build/${config}"]
```

`output_dirs` is the mechanism that makes this work. Frost owns that directory
entirely: it scans it after the action, digests every file, and publishes the
tree to the content-addressed store. A tool whose output *names* cannot be
written down in advance — a jar named from the project version — is still
cacheable, because the declaration is the directory, not the file list.

Measured on this repository, cold to warm:

| | cold | warm | output tree deleted, restored from the CAS |
|---|---|---|---|
| `sample_spring` | 12.3 s | 1 ms | 24 ms |
| `sample_maven` | 3.5 s | 1 ms | 3 ms |

The third column is the one that matters on a fresh checkout or a CI runner:
the artifact comes back without the tool running at all.

### The one thing you must handle: the output directory

Every Frost-owned directory has to carry the profile/platform key, so a debug
build, a release build and a cross build cannot write over each other. Neither
Gradle's `build/` nor Maven's `target/` does. Both samples therefore make the
location a property, and both keep their plain default when Frost is not
driving:

```groovy
// sample_spring/build.gradle
layout.buildDirectory = file(providers.gradleProperty('frostBuildDir').getOrElse('build'))
```

```xml
<!-- sample_maven/pom.xml -->
<properties>
  <frost.buildDir>target</frost.buildDir>
</properties>
<build>
  <directory>${frost.buildDir}</directory>
</build>
```

Maven's build directory comes from the POM, not the command line: passing
`-Dproject.build.directory=...` is silently ignored and the build writes to
`target/` anyway. The property indirection above is not optional there.

### Declaring the environment, not inheriting it

Frost clears the environment and supplies a deterministic baseline, so anything
these tools genuinely need is named:

```toml
pass_env = ["HOME", "JAVA_HOME", "GRADLE_USER_HOME",
            "HTTP_PROXY", "HTTPS_PROXY", "NO_PROXY"]
```

`HOME` for the dependency cache, a JDK, and the proxy configuration to reach
the repositories. Each name's present-or-absent value is action-key material,
so a build that resolved through a different proxy does not reuse this result.

### What you give up

The granularity is the whole module. Change one source and the entire Gradle or
Maven invocation reruns — Gradle's own incrementality still applies within that
miss, but Frost cannot cut it finer without understanding the task graph, and
understanding it is exactly what it refuses to guess at.

If a module is large enough that this hurts, the answer is to split it into
several wrapped targets with narrower `inputs`, not to teach Frost about
Gradle.

### Spring Boot specifically

Nothing in `sample_spring/frost.toml` is Spring-aware. The Spring Boot Gradle
plugin's `bootJar` task produces a repackaged fat jar with a nested layout that
only Spring's loader understands; Frost treats it as bytes in an owned
directory, which is the correct amount of knowledge to have about it. A Spring
Boot project built with Maven is `sample_maven`'s target with `package`
replaced by `spring-boot:repackage` in the lifecycle — the frost.toml does not
change shape.

---

## Related documents

- [`06_manifest_spec.md`](06_manifest_spec.md) — normative `frost.toml`
  grammar, including `command` targets, `output_dirs` and every `${...}`
  expansion used above
- [`14_bazel_gap_analysis.md`](14_bazel_gap_analysis.md) — what Frost adopts
  from Bazel and what it rejects, including the query surface
- [`17_java_gradle_maven_comparison.md`](17_java_gradle_maven_comparison.md) —
  measured Frost/Gradle/Maven comparison on a synthetic Java corpus
- [`23_bazel_migration.md`](23_bazel_migration.md) — importing an existing
  Bazel workspace
- [`09_platform_support.md`](09_platform_support.md) — which E2Es run on which
  host, and why the Java cases skip
