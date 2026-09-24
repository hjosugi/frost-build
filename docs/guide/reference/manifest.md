# `frost.toml` reference

Every table and key `frost.toml` accepts, with its type and default. Unknown
keys are errors, so this list is complete: a test in
`crates/frostbuild-core/src/manifest.rs` asks the manifest loader itself which
keys each table accepts and fails when this page and the loader disagree.

For *why* a key behaves as it does — output ownership, action keys, early
cutoff — follow the links into the normative specification,
[docs/06_manifest_spec.md](../../06_manifest_spec.md). Worked examples are in
the [tutorials](../README.md#tutorials).

Conventions used below:

- **Paths** are UTF-8, relative, written with `/`, and may not contain empty,
  `..` or absolute components. In a package manifest they are relative to that
  package's directory; in the root manifest, to the workspace root.
- **Globs** (`*`, `?`, `[]`, `**`) are allowed where the table says so; matches
  are sorted, and `.frost`, `.git`, `.gitignore` and `.frostignore` are never
  matched.
- **Labels** are `//path/to/package:name`; `:name` and a bare `name` resolve in
  the current package.
- **`${config}`** is the output-tree key: `PROFILE` on the host platform and
  `PLATFORM/PROFILE` otherwise. Every declared output of a command must
  contain it.

## Top level

A manifest is a set of tables. The workspace-level tables — `[workspace]`,
`[toolchain]`, `[platform.*]`, `[profile.*]`, `[fetch.*]`, `[stamp]` and
`[visibility.*]` — belong in the root manifest, and package manifests
contribute `[target.*]` tables. `[fetch.*]`, `[stamp]` and `[visibility.*]` in
a package manifest are errors; the other workspace-level tables have no effect
there.

<!-- frost-keys: root -->
| Key | Type | Meaning |
|---|---|---|
| `workspace` | table | marks the root of a multi-package workspace and names its default targets |
| `toolchain` | table | compilers, archiver, flags and named tools for the host platform |
| `platform` | table of tables | named cross/device toolchain overlays, `[platform.NAME]` |
| `profile` | table of tables | named flag sets such as `debug` and `release`, `[profile.NAME]` |
| `target` | table of tables | the build graph: one `[target.NAME]` per target |
| `fetch` | table of tables | pinned external archives, `[fetch.NAME]` (root only) |
| `stamp` | table | the workspace-status command for build stamping (root only) |
| `visibility` | table of tables | named visibility groups, `[visibility.NAME]` (root only) |

## `[workspace]`

Present only in the root manifest of a multi-package workspace. When it is
present, Frost discovers nested `frost.toml` files as packages. Without it, the
manifest is a single package and target names are bare.

<!-- frost-keys: workspace -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `name` | string | none | a human-readable name; not used by the build |
| `default_targets` | array of labels | every binary | what `frost build` builds when no target is named; when empty, every `cc_binary` and `kofun_binary`, or every target if there is none |

## `[toolchain]`

The host platform's tools. Every driver's identity is fingerprinted into the
action keys of the actions that use it. See
[Toolchain and profiles](../../06_manifest_spec.md#toolchain-and-profiles).

<!-- frost-keys: toolchain -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `cc` | string | `cc` | C compiler driver, also the linker for C-only binaries |
| `cxx` | string | `c++` | C++ compiler driver; links any binary with a C++ source |
| `ar` | string | `ar` | archiver for `cc_library` |
| `arflags` | array of strings | `["rcsD"]` (`["rcs"]` on macOS) | archiver flags, replacing the default |
| `gcov` | string | `gcov` | coverage reporter used only by `frost test --coverage` |
| `kofunc` | string | none | Kofun compiler, required by `kofun_binary` targets |
| `cflags` | array of strings | `[]` | flags for every C and C++ compile |
| `cxxflags` | array of strings | `[]` | additional flags for C++ compiles |
| `ldflags` | array of strings | `[]` | flags for every link |
| `tools` | table of strings | `{}` | named tools for `kind = "command"` and named-tool tests (below) |

### `[toolchain.tools]`

A free-form table: each key is a tool name a target refers to with `tool =
"NAME"`, and each value is the executable — a name looked up on `PATH`, an
absolute path, or a workspace-relative path (which is then also a declared
input of every action that uses it).

```toml
[toolchain.tools]
javac = "javac"
rustc = "rustc"
pack_jar = "frost"
```

## `[platform.NAME]`

A toolchain overlay for a cross or device build, selected with `--platform
NAME`. Unset drivers inherit from `[toolchain]`; flags are appended after the
root toolchain's. `host` is reserved for the root `[toolchain]`. See
[Platforms](../../06_manifest_spec.md#platforms-cross--device-builds).

<!-- frost-keys: platform -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `cc` | string | inherited | C compiler driver for this platform |
| `cxx` | string | inherited | C++ compiler driver |
| `ar` | string | inherited | archiver |
| `arflags` | array of strings | inherited | archiver flags |
| `gcov` | string | inherited | coverage reporter matching this platform's compiler |
| `kofunc` | string | inherited | Kofun compiler |
| `sysroot` | path | none | expands to `--sysroot=` on compile and link flags |
| `cflags` | array of strings | `[]` | appended after `[toolchain] cflags` |
| `cxxflags` | array of strings | `[]` | appended after `[toolchain] cxxflags` |
| `ldflags` | array of strings | `[]` | appended after `[toolchain] ldflags` |
| `tools` | table of strings | `{}` | per-platform overrides and additions to `[toolchain.tools]` |

## `[profile.NAME]`

A named flag set, selected with `--profile NAME` (default `debug`). Profiles
have separate output trees and cache identities, so switching between them
never rebuilds.

<!-- frost-keys: profile -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `cflags` | array of strings | `[]` | appended to C and C++ compile flags |
| `cxxflags` | array of strings | `[]` | appended to C++ compile flags |
| `ldflags` | array of strings | `[]` | appended to link flags |

## `[target.NAME]`

One node of the build graph. `kind` decides which of the other keys apply; a
key that makes no sense for a kind is an error rather than silently ignored
wherever the loader can tell.

<!-- frost-keys: target-kinds -->
| `kind` | What it builds | Specification |
|---|---|---|
| `cc_library` | one deterministic static archive from C/C++ sources | [C/C++ targets](../../06_manifest_spec.md#cc-targets) |
| `cc_binary` | one linked executable | [C/C++ targets](../../06_manifest_spec.md#cc-targets) |
| `cc_test` | a linked executable plus a cached run of it | [C/C++ targets](../../06_manifest_spec.md#cc-targets) |
| `genrule` | declared outputs from a host-shell command | [Genrules and tests](../../06_manifest_spec.md#genrules-and-tests) |
| `test` | a cached run of a shell command or a named tool | [Genrules and tests](../../06_manifest_spec.md#genrules-and-tests) |
| `command` | declared outputs from a directly executed named tool | [Language-neutral command targets](../../06_manifest_spec.md#language-neutral-command-targets) |
| `kofun_binary` | one executable from a single `.kofun` source | [Kofun targets](../../06_manifest_spec.md#kofun-targets) |

<!-- frost-keys: target -->
| Key | Type | Default | Applies to | Meaning |
|---|---|---|---|---|
| `kind` | string | required | all | one of the kinds above |
| `srcs` | array of globs | `[]` | `cc_*`, `kofun_binary` | source files; required for native kinds |
| `deps` | array of labels | `[]` | all | targets this one depends on; their outputs are its inputs |
| `fetches` | array of strings | `[]` | all | `[fetch.NAME]` entries whose vendored trees this target reads |
| `includes` | array of paths | `[]` | `cc_*`, `genrule` | `-I` directories, exported transitively to dependents |
| `cflags` | array of strings | `[]` | `cc_*` | compile flags, after toolchain, platform and profile flags |
| `ldflags` | array of strings | `[]` | `cc_binary`, `cc_test` | link flags |
| `cmd` | string | none | `genrule`, `test` | a host-shell command (`/bin/sh -c`, `cmd.exe /C` on Windows) |
| `tool` | string | none | `command`, `test` | a `[toolchain.tools]` name, run as direct argv with no shell |
| `args` | array of strings | `[]` | `command`, `test` with `tool` | arguments for `tool`, with `${...}` substitutions |
| `env` | table of strings | `{}` | `command`, `test` with `tool` | fixed environment variables; action-key material |
| `pass_env` | array of strings | `[]` | `command`, `test` with `tool` | host variables passed through; their values are action-key material |
| `steps` | array of tables | `[]` | `command` | further tools run in the same action, see [`steps`](#steps) |
| `clean_dirs` | array of paths | `[]` | `command` | intermediate directories reset before every run; must contain `${config}` |
| `preserve_outputs` | boolean | `false` | `command` | keep outputs in place for a tool with its own incremental state |
| `timeout` | integer (seconds) | none | all | stop the action after this long; wins over `--timeout` |
| `resources` | table | `{ cpu = 1 }` | all | scheduler admission, see [`resources`](#resources) |
| `shard_count` | integer | `1` | `test`, `cc_test` | split one test into N cached, scheduled actions |
| `flaky_retries` | integer | `0` | `test`, `cc_test` | extra attempts before a failure is the verdict (at most 9) |
| `lint_allow` | array of strings | `[]` | all | `frost lint` rule ids this target accepts |
| `visibility` | array of strings | public | all | who may depend on this target: `//...`, `//pkg/...`, a label or `group:NAME` |
| `platform` | table of tables | `{}` | all | per-platform overlays, see [`platform.PLAT`](#targetnameplatformplat) |
| `depfile` | path | none | `command` | a dependency report the tool writes; must contain `${config}` |
| `depfile_format` | string | `make` | `command` | `make`, `lines` or `showincludes` |
| `inputs` | array of globs | `[]` | `genrule`, `test`, `command` | declared input files |
| `outputs` | array of paths | `[]` | `genrule`, `command` | declared output files; each output has exactly one producer |
| `output_dirs` | array of paths | `[]` | `command` | directories Frost owns outright, for tools that name outputs by content |
| `sandbox` | boolean | `true` | all | `false` opts this target out of `--sandbox` |

### Substitutions

`args`, `steps[].args` and `env` values of a `command` target, and the `cmd` of
a genrule, expand these; the full rules are in the
[specification](../../06_manifest_spec.md#language-neutral-command-targets).

| Variable | Expands to |
|---|---|
| `${in}` | one argument per declared input |
| `${out}` / `${outs}` | the first declared output / one argument per output |
| `${out_dir}` | the directory of the first output |
| `${output_dir}` / `${output_dirs}` | the first / every owned output directory |
| `${clean_dir}` / `${clean_dirs}` | the first / every clean intermediate directory |
| `${deps}` | one argument per output of every declared dependency |
| `${dep:LABEL}` / `${deps:LABEL}` | the single output / every output of one declared dependency |
| `${depfile}` | the configured depfile path |
| `${config}`, `${profile}`, `${platform}` | the selected configuration names |
| `${pathsep}` | the host path separator (genrule `cmd` only) |
| `${stamp.KEY}` | a value from the `[stamp]` command (`command` targets only) |

### `steps`

Each entry of `steps = [...]` is an inline table run after the primary tool, in
order, inside the same atomic action.

<!-- frost-keys: target-steps -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `tool` | string | required | a `[toolchain.tools]` name |
| `args` | array of strings | `[]` | its arguments, with the same substitutions as `args` |

### `resources`

`resources = { cpu = 4, ram_mb = 8192 }` reserves scheduler tokens while the
action runs. It changes when an action starts, never what it builds. See
[Resource-aware scheduling](../../06_manifest_spec.md#resource-aware-scheduling).

<!-- frost-keys: target-resources -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `cpu` | integer | `1` | CPU tokens, in the same unit as `--local-cpu-resources` |
| `ram_mb` | integer | `0` | MiB of RAM, in the same unit as `--local-ram-resources` |
| `exclusive` | boolean | `false` | run this action alone |

### `[target.NAME.platform.PLAT]`

What changes about a target when it is built for platform `PLAT`, which must be
declared as `[platform.PLAT]`. Lists that are identities replace; flags append.
See [Per-platform target sections](../../06_manifest_spec.md#per-platform-target-sections).

<!-- frost-keys: target-platform -->
| Key | Type | Rule | Meaning |
|---|---|---|---|
| `srcs` | array of globs | replace | the sources for this platform |
| `deps` | array of labels | replace | the dependencies for this platform |
| `includes` | array of paths | replace | the include directories for this platform |
| `cflags` | array of strings | append | extra compile flags |
| `ldflags` | array of strings | append | extra link flags |

## `[fetch.NAME]`

A pinned, resolver-free external archive, declared in the root manifest and
materialized only by `frost fetch`. Builds never touch the network. See
[Pinned external archives](../../06_manifest_spec.md#pinned-external-archives).

<!-- frost-keys: fetch -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `url` | string | required | absolute `http://` or `https://` URL of a `.tar.gz`, `.tgz` or ZIP |
| `sha256` | string | required | the archive's SHA-256, 64 hexadecimal characters |
| `strip_prefix` | path | none | a directory inside the archive to extract instead of its root |
| `vendor_dir` | path | required | where the verified tree is materialized; vendor directories may not overlap |

## `[stamp]`

The workspace-status command behind `${stamp.KEY}`. See
[Build stamping](../../06_manifest_spec.md#build-stamping).

<!-- frost-keys: stamp -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `command` | array of strings | `[]` | direct argv, run once per build from the workspace root, printing `KEY=VALUE` lines |
| `stable_prefix` | string | `STABLE_` | keys with this prefix are action-key material; all others are volatile |

## `[visibility.NAME]`

A named visibility list, referenced from a target as `group:NAME`. Groups are
one level deep. See [Visibility](../../06_manifest_spec.md#visibility).

<!-- frost-keys: visibility -->
| Key | Type | Default | Meaning |
|---|---|---|---|
| `allow` | array of strings | `[]` | `//...`, `//pkg/...` or labels this group admits |

## Files that are not `frost.toml`

- `.frostrc` (workspace) and `~/.config/frost/frostrc` (user) hold command-line
  defaults, not build definitions; their keys are the long option names of the
  [CLI reference](cli.md). See [.frostrc](../../06_manifest_spec.md#frostrc).
- `.frostignore` excludes paths from glob matching, like `.gitignore`.
- `.frost-version` pins the frost release `./frostw` runs.
