# Next language adapter: Kotlin, C#/.NET and Swift

Issue #153 asked which ecosystem Frost should add next, and asked for the
answer to come from evidence rather than from which language is fashionable.
It named this file `docs/28_language_expansion.md`; `28` was already taken by
[28_compatibility_contract.md](28_compatibility_contract.md), and
[README.md](README.md) explains why numbers are never reused, so this memo is
`32`.

**Decision.** C#/.NET is the next language, as a **generated command
boundary**: MSBuild evaluates each project once, and Frost owns the Roslyn
invocations MSBuild would have made. Kotlin stays an **incumbent-owned
boundary** (a Gradle project boundary) until the persistent-worker research
(#145) decides that a JVM compiler worker is worth owning. Swift is **not taken now**. Nothing in this memo makes any of
the three a supported language: C# support is three follow-up issues, listed
at the end, and none of them is done.

The same rules as [18_polyglot_win_matrix.md](18_polyglot_win_matrix.md)
apply. Wrapping an incumbent and skipping its unchanged boundary is not a
language win, and no number below is compared unless the artifacts behind it
were validated as equal.

## What the three incumbents own

Each candidate already has a build tool that owns dependency resolution and
the inner incremental state. The useful question is not "can Frost run the
compiler" -- it always can, through `kind = "command"` -- but which part of the
incumbent's job Frost can take over without making the result less correct.

| Axis | Kotlin/JVM (Gradle) | C#/.NET (MSBuild) | Swift (SwiftPM) |
|---|---|---|---|
| Graph discovery | Build scripts are Kotlin/Groovy programs; the module graph exists only after Gradle configuration (Tooling API). KMP adds per-target source sets and `expect`/`actual` | SDK-style `.csproj` with `ProjectReference`; evaluation applies imports, `Directory.Build.*`, conditions and globs. `dotnet msbuild -getItem/-getProperty` and the design-time build return the evaluated graph **and the exact compiler argv** without compiling (proved below) | `Package.swift` is a Swift program run by SwiftPM; `swift package describe --type json` returns the evaluated graph, and SwiftPM writes an llbuild manifest (`.build/*.yaml`) with the exact commands |
| Stable partition | Gradle project (module); a `kotlinc` action per module is possible but pays JVM start-up per action | Project = one `csc` invocation = one assembly; the natural and cheap partition | Module = one swift-driver invocation that itself fans out into per-file frontend jobs |
| Correctness key | kotlinc + JDK used to run it, `-jvm-target`, stdlib, compiler plugins (kapt/KSP, serialization, Compose) and their options, resolved classpath, generated sources | SDK/Roslyn version, targeting pack (reference assemblies), analyzers **and source generators** (they write code into the assembly), `.editorconfig`/`.globalconfig`, generated `AssemblyInfo`, defines/`LangVersion`/nullable, NuGet restore graph, `global.json` roll-forward | swift-frontend/driver, SDK/sysroot (Xcode SDK on Darwin, Foundation/Glibc on Linux), macro plugins (executables built from source and loaded by the compiler), build-tool plugins, C/ObjC module maps, resources |
| Artifact contract | `.class` set, JAR, `META-INF/*.kotlin_module` (the module name is part of the output) | assembly and reference assembly, PDB, `runtimeconfig.json`, `deps.json`, apphost; `.nupkg` from pack | executable/library, `.swiftmodule`/`.swiftinterface`, resource bundles; Apple app bundles need Xcode signing |
| Incumbent's inner state | Gradle daemon, Kotlin daemon, incremental compilation with classpath ABI snapshots, build cache | MSBuild node reuse, Roslyn compiler server (`VBCSCompiler`), up-to-date checks, reference-assembly compile avoidance | llbuild + swift-driver per-file dependency graph (`.swiftdeps`/priors), explicit module builds |
| What Frost would take over | Nothing safely today (see the start-up probe); later a `kotlinc` action per module with `jvm-abi-gen` ABI jars as the edge | Scheduling, caching and cutoff of the `csc` actions; MSBuild keeps evaluation and restore | Nothing that SwiftPM does not already do incrementally; at most a package boundary |
| Frost gaps | persistent worker (#145), JVM ABI extraction, dependency resolution (only pinned `[fetch]` jars), IDE model | an importer, a built-in copy step (Windows), `deps.json`/apphost, NuGet restore ownership, per-action tool-closure keys, PDB path mapping | macro/plugin build-and-load ordering, C interop, the Apple toolchain, per-file incrementality |
| Linux / macOS / Windows CI | JDK everywhere (`setup-java`), kotlinc zip; Gradle downloads plugins | pinned SDK on all three through `actions/setup-dotnet`; no network for a package-free graph | macOS has Xcode's Swift; Linux needs a separate swift.org toolchain download; Windows needs that toolchain plus Visual Studio components |
| Adoption cost | Gradle scripts cannot be translated without running Gradle; any generated config inherits Gradle's model | measured below: five authored MSBuild files → a generated 449-line `frost.toml` plus one shared response file, regenerated whenever a project file changes | a `swift build` wrapper is a few lines; anything finer means importing llbuild's manifest |

Two rows decide the ranking. First, **graph discovery**: MSBuild answers
"what would you run?" per project without running it -- through the same
design-time build IDEs use to configure their compilers -- and the answer is an
argv Frost can execute directly. SwiftPM's llbuild manifest
is the closest equivalent, but a SwiftPM build is already fine-grained and
incremental below the module, so Frost has less to add. Gradle has no
equivalent short of the Tooling API plus a compiler invocation Frost would
have to reconstruct. Second, **per-invocation cost**: `csc` is a
ReadyToRun-compiled .NET program; `kotlinc` is a JVM program that loads the
whole Kotlin compiler, and without a worker every Frost action would pay that
start-up (measured below).

## Per-candidate decision

| Candidate | Decision | Why |
|---|---|---|
| **C#/.NET** | **Generated command boundary** — implement next | MSBuild hands over the exact compiler argv; SDK projects compile deterministically by default, so Frost's direct invocations can be checked **byte for byte** against MSBuild's; a project is exactly one cheap compiler invocation; reference assemblies give an ordinary content-addressed edge for compile avoidance; one pinned SDK is available on Linux, macOS and Windows runners. MSBuild keeps evaluation and restore, and remains the fallback boundary for anything the importer does not model |
| **Kotlin/JVM** | **Incumbent-owned boundary** (Gradle project), native rule deferred | Gradle's model is a program, not data, and the Kotlin incremental compiler plus daemon is exactly the inner state docs/10 says not to duplicate. A native `kotlinc` rule is only competitive with a persistent compiler worker: the probe below shows one cold compile of the same 25-class library costing several times `javac`, which docs/17 already showed dominates fine partitions. Revisit when #145's decision adopts a JVM worker; the edge would then be a `jvm-abi-gen` ABI jar, the JVM analogue of the C# reference assembly used here |
| **Swift** | **Not now** (見送り) | SwiftPM + swift-driver already give per-file incremental builds and an explicit command manifest, so a Frost module boundary would add caching but remove incrementality. The largest Swift population builds Apple-platform apps through Xcode, which Frost cannot own. The toolchain is absent from this host and is expensive on Linux/Windows CI, so no prototype was run and nothing here claims otherwise. A polyglot user who needs Swift today can wrap `swift build` as an ordinary command target (integration only, no language claim) |

## C# prototype

`frost-bench csharp` ([`frost_bench_csharp.py`](../frost_bench_csharp.py))
generates one four-project SDK-style graph -- `App → Service → Model → Core`
with a direct `Service → Core` edge -- and builds it four ways:

- `dotnet`: the incumbent, `dotnet build --no-restore -c Release`, with
  MSBuild node reuse and the compiler server left on;
- `frost-dotnet`: an **incumbent-owned boundary**, one Frost action wrapping
  that same `dotnet build` with an owned output directory;
- `frost-csc`: the **generated command boundary**. For each project the
  generator runs MSBuild's design-time `Compile` with
  `SkipCompilerExecution=true` and `ProvideCommandLineArgs=true`, reads the
  exact `csc` argv from the `CscCommandLineArgs` item, and rewrites it into a
  Frost action. MSBuild's `obj/` paths become Frost outputs; the compiler
  (`csc.dll` and the `Microsoft.CodeAnalysis` assemblies), the 167 reference
  assemblies, the analyzers and source generators, and the SDK analysis-level
  config are hard-linked into `.dotnet-sdk/` and declared as inputs; the
  files MSBuild generates (`AssemblyInfo.cs`, the target-framework attribute,
  the generated `.editorconfig`, `runtimeconfig.json`) are copied into
  `.dotnet-gen/` and declared. The SDK closure every project shares goes into
  one response file, itself a declared input. Each library also gets a
  one-line copy action that republishes its reference assembly; consumers
  compile against that copy, so when an implementation-only edit leaves the
  reference assembly's bytes unchanged, early cutoff stops there. That is
  MSBuild's `ProduceReferenceAssembly` avoidance expressed as an ordinary
  content-addressed edge, not a special case.
- `frost-csc-shared`: the same generated actions plus `/shared`, so each
  action's short-lived `csc` process hands the compile to Roslyn's compiler
  server (`VBCSCompiler`), which the first action starts from the bundled
  compiler and which then outlives the build. This is the persistent worker
  MSBuild already uses. It runs outside Frost's process tree, action key and
  sandbox, which is precisely the hazard the persistent-worker research
  (#145) weighs, so it is measured as a separate frontend rather than folded
  into `frost-csc`.

All frontends share `Directory.Build.props`: .NET 10, `Deterministic`,
`DebugType=none` (so a PDB path, which necessarily differs between an
MSBuild `obj/` tree and a Frost output tree, is not inside the assembly),
`UseAppHost=false` and no source-revision stamping. PDBs and the apphost are
listed as follow-up work below, not silently dropped.

After **every** timed build the harness runs `dotnet App.dll` and requires the
exact expected total, then hashes each of the four assemblies. For the three
Frost frontends it reads Frost's build event stream to record
which actions executed and how long they ran; for `dotnet` and `frost-dotnet`
it records which projects MSBuild actually recompiled (from the intermediate
assembly's modification time). Nothing about incrementality is inferred from
the manifest.

Reproduce (the SDK and kotlinc are pinned downloads verified against the
publishers' SHA-512/SHA-256):

```bash
cargo build --release --locked -p frostbuild-cli --bin frost
FROST_BIN=target/release/frost \
DOTNET_BIN=/path/to/dotnet-sdk-10.0.401/dotnet \
KOTLINC_BIN=/path/to/kotlinc-2.4.20/bin/kotlinc \
./frost-bench csharp --size 25 --iterations 7 --jobs 4 \
  --jvm-probe-iterations 5 \
  --out bench/baselines/2026-09-24-issue-153-csharp.json
```

### Result

The checked report is
[`2026-09-24-issue-153-csharp.json`](../bench/baselines/2026-09-24-issue-153-csharp.json):
25 value files per project (105 sources), `--jobs 4`, median-of-7 in
alternating order, .NET SDK 10.0.401 (Roslyn 5.9.0, runtime and targeting
pack 10.0.12), Frost 0.13.2 release build. **The host was heavily loaded by
unrelated jobs**: the 1/5/15-minute load average was 77/70/67 on 8 CPUs when
the run started and 48/50/54 when it ended. Every sample is in the report;
read the numbers below as relative, same-run evidence only.

| Scenario (median ms) | `frost-csc` | `frost-csc-shared` | `frost-dotnet` | `dotnet build` |
|---|---:|---:|---:|---:|
| clean | 21,694 | 11,880 | 24,509 | 24,695 |
| warmed no-op | 306 | 463 | 393 | 10,233 |
| one leaf changed (App body) | 6,387 | 2,018 | 19,509 | 13,682 |
| shared dependency changed (Core body) | 4,019 | 902 | 13,095 | 6,103 |

Relative to `dotnet build` (higher is faster):

| Scenario | `frost-csc` | `frost-csc-shared` | `frost-dotnet` |
|---|---:|---:|---:|
| clean | 1.14x (within noise) | 2.08x | 1.01x (within noise) |
| warmed no-op | 33.5x | 22.1x | 26.1x |
| one leaf changed | 2.14x | 6.78x | 0.70x |
| shared dependency changed | 1.52x | 6.77x | 0.47x |

**Artifacts.** All four frontends produced the same four assemblies, byte for
byte (`byte_identical_assemblies: true`), and the same `runtimeconfig.json`
and application output; the application printed the expected total after
every one of the 28 timed builds per frontend. The generated boundary is
therefore not an approximation of MSBuild's compile: it is the same compile.

**Incrementality, observed.** Frost's event stream shows exactly one action
(`app`) for every leaf sample, and exactly `core` plus its reference-assembly
copy for every shared-dependency sample: the copied reference assembly was
byte-identical, early cutoff stopped there, and Model, Service and App were
never recompiled. MSBuild's own record agrees for `dotnet` and
`frost-dotnet` (only `App`, respectively only `Core`, rewrote its
intermediate assembly). The generated boundary reaches the same compiler
scope as MSBuild; it wins because it skips MSBuild's evaluation and target
execution for the three unchanged projects, not because it compiles less.

**Inner time versus Frost overhead.** For the wrapper, the single `dotnet
build` action accounted for a median 22.4 s / 17.1 s / 12.2 s of the clean /
leaf / shared builds, and everything else Frost did took 2.6 s / 1.9 s /
0.9 s. The same `dotnet build` ran slower inside the wrapper than outside it
on the leaf and shared scenarios; the likely causes -- Frost's cleared
environment defeating MSBuild node reuse, and `-o` copying every project's
output -- were not isolated here, so they are recorded rather than claimed.
For the generated boundary the executed `csc` time was 5.0 s (cold) versus
1.1 s (compiler server) for the leaf change: the persistent worker, not
Frost, is most of the difference between the two Frost columns. Frost's own
share of a changed build (wall minus executed action time) was 0.3-1.2 s on
this host and was not decomposed; every generated action declares the
230-file, 58 MB bundled toolchain, and whether keying that closure once
instead of per file removes most of it is a question for #248 and #249.

**Import cost.** Generation ran one MSBuild design-time build per project
plus one for `runtimeconfig.json`: 112 s and 76 s for the two generated
workspaces on this host, reported as `import_ms` and never inside a timed
build. It is paid again whenever a project file changes. Batching the
evaluation into one MSBuild invocation is part of #248.

**Configuration.** The authored MSBuild input is 36 lines (1,322 bytes)
across five files. The generated `frost.toml` is 449 lines (14,829 bytes),
because every source is listed twice (as an input and in argv) and every
reference goes through the shared response file; the wrapper's hand-written
`frost.toml` is 32 lines. The generated manifest is generated: its size is a
readability cost, not an authoring cost.

### Correctness probes

Run once per frontend after the timed samples, and recorded in the report:

- **Dependency change -- a public constant.** `CoreConstants.Offset` changes
  from 0 to 5 and no other file changes. C# inlines constants into the
  consumer's IL, so this is the change a compile-avoidance scheme gets wrong
  if it trusts anything but the reference assembly's bytes. Core's reference
  assembly changes, so every project compiled against it reran (`core`,
  `model`, `service`, `app`; `service_api` was cut off because Service's own
  output did not change), and the application printed the new total. MSBuild
  recompiled the same four projects. All four frontends passed.
- **Toolchain change -- the compiler closure.** In both generated
  workspaces the SDK's `analysislevel_10_default.globalconfig`, which every
  project passes to the compiler, was replaced by a copy with one extra
  comment line; no source changed. All four `csc` actions reran (a missing
  closure input would have made all four cache hits), produced identical
  bytes, and early cutoff stopped the copy actions. Passed for `frost-csc`
  and `frost-csc-shared`. The wrapper boundary has no equivalent check to
  pass: its key sees the `dotnet` host executable, not the SDK behind it, so
  an in-place SDK change would be a false cache hit there. That is a reason
  the wrapper is only the fallback.

### Kotlin cold-compiler probe

The same run timed a cold one-shot compile of the same 25-class,
dependency-free library with `javac` 26.0.2 and `kotlinc` 2.4.20 (JRE
26.0.2), alternating order, one discarded warm-up round, median-of-5:
**2,411 ms for `javac` and 9,243 ms for `kotlinc` (3.83x)**. docs/17 already
showed per-action JVM start-up making fine Java partitions 41x slower on a
clean build; a native Kotlin rule would start from a cost about four times
higher per action. This is the measured reason the Kotlin decision waits for
#145, not a statement about Gradle's Kotlin performance, which was not
measured.

### Swift

No Swift toolchain exists on this host, so nothing was run and the report
contains no Swift result. The decision above rests on the comparison table,
not on a measurement.

## What this does not show

- **Not a supported language.** The generator is Python inside a benchmark
  harness. `frost` has no C# knowledge; #248 is the work that would give it
  some.
- **One graph, one host, one SDK.** Four projects, no NuGet packages, no
  resources, no source generators from packages, one target framework, .NET
  SDK 10.0.401 on Linux. The host was a shared 8-core machine running many
  unrelated jobs (the load average is in the report); the order alternated
  every iteration so that noise lands on every frontend, but the absolute
  milliseconds are not portable and a quiet-host baseline is part of #249.
- **No PDB, no apphost, no `deps.json`.** The shared props turn PDBs off so
  the byte comparison is meaningful, and the app runs as `dotnet App.dll`.
  All three are listed in #248.
- **The import is not free and not incremental.** It runs one MSBuild
  design-time build per project and is reported as `import_ms`, outside every
  timed build. Any `.csproj` or `Directory.Build.props` change requires it
  again; the harness refuses to time a build whose evaluation inputs changed
  after import rather than trusting a stale boundary.
- **The design-time build is an IDE contract, not a CLI one.**
  `SkipCompilerExecution` and `ProvideCommandLineArgs` are what the Roslyn
  project system uses; they are stable in practice but not documented as a
  command-line interface, so #248 has to pin and test the SDK versions it
  imports from and fail loudly on an argv it cannot classify (the prototype
  already refuses any compiler input it cannot place).
- **Windows is not covered.** Two prototype steps use `cp`; the
  response-file and quoting rules have not met `cmd.exe`. #250 is the gate.
- **The compiler server is not owned by Frost.** `frost-csc-shared` shows
  what a persistent worker is worth here, not that Frost should adopt this
  one; its state lives outside the action key, and that decision belongs to
  #145.

## Follow-up

Split so that implementation, benchmark and platform CI never share an
issue, per the #153 acceptance criteria:

| Issue | Scope | Depends on |
|---|---|---|
| #248 | `frost import-dotnet`: design-time evaluation to `csc` actions, toolchain closure in the key, reference-assembly edges, stale-import detection, NuGet restore ownership, a built-in copy step, PDB/apphost decisions | — |
| #249 | `frost-bench csharp` nightly on a pinned SDK, measured against `frost import-dotnet` output, quiet-host baseline | #248 |
| #250 | C# E2E on Linux, macOS and Windows runners; `skipped` when no SDK | #248 |

Kotlin and Swift get no implementation issue. Kotlin's native-rule question
reopens with #145's decision; Swift reopens only with a concrete polyglot
user who needs more than a `swift build` wrapper.
