# Host and target platform support

Linux remains the reference host: inotify via `notify`, Unix-domain daemon
sockets and bubblewrap sandboxing. macOS uses the native `notify` backend and
the same Unix-domain daemon transport. Seatbelt (`sandbox-exec`) is not used and
is out of scope; the isolation that macOS and Windows get is `--hermetic`,
described below, which checks the same thing without a kernel sandbox.

## Execution modes on every host

Isolation used to mean bubblewrap, so it used to mean Linux. It now has two
backends that share one definition of what an action may see
(`crates/frostbuild-exec/src/sandbox.rs`, `visible_set`): the directory of
every declared input, every `-I` directory inside the workspace, each input
file (including the ones the previous run discovered) and each order-only
input; outputs, depfiles, owned output directories and clean directories are
writable.

| Mode | Hosts | How undeclared files are hidden | Isolation |
|---|---|---|---|
| `--sandbox` | Linux with `bwrap` on `PATH` | a mount namespace binds only the visible set over a tmpfs workspace | mount namespace, not a security boundary |
| `--hermetic` | Linux, macOS, Windows | the visible set is materialized into `.frost/hermetic/<action>` and the action runs there | none: an absolute path into the workspace, the network and other processes stay reachable |
| neither | every host | nothing is hidden | none |

`--hermetic` runs each action with its working directory in a private tree,
then moves back only what the action is answerable for: declared outputs, its
depfile (paths inside the tree are rewritten to the workspace), owned output
directories and clean directories. Anything else it wrote is dropped with the
tree. The tree's name is derived from the action's journal identity, so a tool
that embeds its working directory — debug information does — embeds the same
one on every run and `--check-determinism` compares like with like. It does
differ from the workspace path a plain build embeds, just as a build in a
different checkout directory would. `.frost` and `.git` are never copied in
through a directory input; `sandbox = false` on a target opts it out of both
modes; `--hermetic` cannot be combined with `--coverage`, because GCC writes
counters to the absolute object directory it compiled in, which the tree no
longer is by the time the test runs.

`frost build --sandbox` on a host without bubblewrap is now refused before any
work starts, with exit code 2 and a sentence that names `--hermetic`, instead
of the same spawn error from every action. The E2E
`hermetic_mode_rejects_undeclared_workspace_header_on_every_host` runs on all
three hosts; where bubblewrap exists it also runs `--sandbox` over the same two
cases (an undeclared `../secret.h`, and the full sample with a generated
header, include directories, a static library and a link) and asserts the two
backends reach identical verdicts.

### Materialization strategy

A hermetic tree is filled for every execution, so how a file is placed matters.
`--materialize auto` (the default) takes the first strategy in the host's order
that works in `.frost/hermetic`; naming one (`reflink`, `hardlink`, `copy`)
uses exactly that one or refuses with the probe's error — it never silently
substitutes. The probe runs where the trees live because support is a property
of the filesystem, not the OS: ext4 and btrfs are both Linux.

| Host | `auto` order | Notes |
|---|---|---|
| Linux | reflink → hardlink → copy | `FICLONE` works on btrfs, XFS and bcachefs; ext4 and tmpfs refuse it, so GitHub's Ubuntu runners select `hardlink` |
| macOS | reflink → hardlink → copy | `clonefile` on APFS, the default volume format; GitHub's macOS runners select `reflink` |
| Windows | hardlink → copy | ReFS block cloning is not implemented; NTFS hard links need no privilege |

A hard link shares the inode with the workspace file, so an action that writes
to its input in place would write the workspace. Every hard-linked input's size
and modification time are recorded before the action and checked after it; a
change fails the action with the file named and `--materialize copy` suggested.
Files an action writes — the previous outputs of a `preserve_outputs` tool —
are cloned or copied, never linked. The executable bit is carried by all three
strategies (a Linux clone copies data only, so the mode is set afterwards).

`frost doctor` reports the selection and why each strategy was or was not
usable, and `frost doctor --json` carries the same under `execution`:

```text
|-- execution
|   |-- materialization  hardlink  (auto tries reflink > hardlink > copy)
|   |     reflink   unsupported: Operation not supported (os error 95)
|   |     hardlink  ok
|   |     copy      ok
|   |-- --sandbox        available  /usr/bin/bwrap
|   `-- --hermetic       available  (inputs placed by hardlink)
```

The E2E `doctor_reports_the_materialization_strategy_this_host_selects` runs on
all three CI hosts and asserts the host's order, that `auto` selected the first
working entry, `reflink` on macOS and `hardlink` on Windows.

The CAS is separate from this choice. Restoring an output from the CAS never
hard-links, because an in-place write to a restored output would then corrupt
the object that claims its digest; it uses the platform copy, which already
clones where the OS does (`clonefile` inside `std::fs::copy` on macOS,
`copy_file_range` on Linux).

### Paths, letter case and the executable bit

Manifest paths are workspace-relative with forward slashes on every host. The
rules that exist because of Windows are now explicit:

- **Drive designators** (`C:/x`, `c:foo`) are refused on every host: on Windows
  joining one to the workspace root yields a path outside the workspace.
- **Names Windows cannot store** — reserved device names (`CON`, `PRN`, `AUX`,
  `NUL`, `COM1`–`COM9`, `LPT1`–`LPT9`, with or without an extension), a
  component ending in a dot or space, and `< > : " |` or control characters —
  are refused on Windows hosts only. `aux.c` is an ordinary Linux file, and
  refusing it where it can be built would break a working workspace to protect
  one that cannot exist. `*` and `?` stay glob syntax.
- **Letter case**: an output may not differ only in case from any other path
  in the graph (another output or a source). On a case-insensitive filesystem,
  the default on Windows and macOS, they are one file and one action would
  overwrite the other's result while the journal recorded two digests. This is
  refused on every host so a workspace loads the same way everywhere; two
  sources that differ in case are left alone, since they can only both exist
  where the filesystem distinguishes them.
- **Long paths**: frost never shortens a path. Rust's standard library uses
  `\\?\` verbatim paths on Windows (the canonical workspace root already is
  one), so hashing, CAS publication, restoration, hermetic trees and cleaning
  accept paths beyond `MAX_PATH`. The E2E
  `a_workspace_path_longer_than_max_path_builds_restores_and_cleans` builds,
  no-ops, restores and hermetically rebuilds a 300+ character output on all
  three hosts. A child tool that is not long-path aware can still fail on such
  a path on Windows; that limit belongs to the tool.
- **The executable bit** is recorded in the CAS and restored on Unix. Windows
  has no such bit — executability is the file extension, resolved through
  `PATHEXT` — so it is neither recorded nor restored there, and a restored
  `.exe` or `.cmd` runs because of its name.

Windows is now an experimental host instead of a compile-time non-goal. The
daemon publishes an ephemeral loopback TCP address in the workspace's
`.frost/frostd.endpoint`, test success stamps are executor-owned rather than
POSIX shell snippets, shell actions use `cmd.exe /C`, and cancellation uses
`taskkill /T` for the complete child process tree. The workspace is
cross-checked for `x86_64-pc-windows-gnu`. Tagged releases publish host-built
macOS and Windows archives alongside static Linux.

## What CI runs where

macOS runs the whole workspace test suite, not a smoke subset: it has the same
C toolchain shape as Linux, and releases ship a macOS archive. It runs every test
that is not explicitly excluded, and the exclusions live in the tests themselves
so they are visible where they apply:

| Gate | Excluded from | Reason |
|---|---|---|
| `cfg(unix)` tests | Windows | POSIX shell command text, `/bin/sh` tool paths, signal semantics |
| MSVC `cl.exe`/`link.exe` | every host | the Windows C/C++ adapter emits GCC/Clang-style flags, so a MinGW or wrapped toolchain is required (#109 covers its dependency report format) |
| `cfg(target_os = "linux")` tests | macOS, Windows | pseudo-terminal cases drive util-linux `script`, whose arguments differ from the BSD tool |
| `--sandbox` E2E | every host without bubblewrap | the test returns early when `/usr/bin/bwrap` is absent, so the Linux-only backend does not fail elsewhere; `--hermetic` covers the same verdict on every host |
| hard-link write-through E2E | Windows | the fixture's command text is POSIX shell; the detection itself is a unit test that runs on all three hosts |
| Windows file-name E2E | Linux, macOS | reserved names and trailing dots are only refused where they cannot be stored |
| MSVC `showincludes` E2E | not yet written | `depfile_format = "showincludes"` is covered by parser unit tests until a Windows MSVC job exists (#109) |

Optional-tool E2E cases (Rust, Go, Java, Python, Node, `zig cc`, `fzf`, Kofun)
skip when the tool is missing rather than failing, so a host without them still
reports a meaningful result. The Java cases also skip when `javac` is newer than
`java` on the host, because a class compiled there cannot be run at all — the
macOS runner image is in that state. Rust similarly uses a compile-and-link
probe; the Windows job omits that optional adapter because putting MinGW first
for native-rule coverage shadows the MSVC `link.exe` required by its Rust
toolchain.

Windows runs the unit tests plus every host-reachable E2E, one at a time with
unbuffered output. The current suite reaches 42 cases there; the difference from
Linux is the explicit gates in the table above rather than a CI-maintained name
list. The full run exposed and now covers these Windows-specific defects:

- tool resolution never appended a name extension, so `gcc` was reported "not
  found in PATH" while `gcc --version` worked in the same shell. `PATH` search
  now tries the host's `PATHEXT` candidates
- the default drivers were `cc`/`c++`, which a MinGW installation does not
  provide; they are now the host's conventional names
- `frost.toml` is TOML, so a `\` in command text is itself escaped; and `cmd`
  binds the remainder of the line to an `if` branch, so
  `if not exist dir mkdir dir & echo x>dir\f` skipped everything once frost had
  created the output's parent. frost creates that parent, so the guard is never
  needed
- built-in binary outputs omitted `.exe`, so MinGW successfully linked a file
  Frost did not consider declared. Native binary graph paths now include the
  host suffix, and the serialized graph version prevents stale suffix-free
  paths from surviving an upgrade
- the bundled sample's generator was a POSIX-only shell command. Genrules now
  expose `${pathsep}`, and the sample declares paired extension-neutral POSIX
  and `cmd.exe` launchers as inputs
- Windows resolves a relative program before applying `current_dir`, so a
  linked `.frost/bin/.../test.exe` could not be started. Direct actions now
  resolve workspace-relative program paths explicitly while bare tool names
  still use `PATH`
- a background daemon inherited the launching client's captured-output
  handles, making the first `build --daemon` wait indefinitely. Windows daemon
  startup now calls `CreateProcessW` detached with handle inheritance disabled
- action environment clearing also removed `LOCALAPPDATA`, which left Go
  without a cache root. Frost passes it through as operational scratch state,
  alongside `TEMP`, without treating its location as output-affecting key
  material

The Windows C/C++ adapter still emits GCC/Clang-style depfile and link flags;
it is suitable for a GNU-like or explicitly wrapped toolchain, not yet a
native MSVC `cl.exe`/`link.exe` contract. Windows genrule command text is
`cmd.exe` syntax, while direct `kind = "command"` and `kind = "test"` argv are
the preferred portable language adapters. Linux-only bubblewrap remains the
only kernel-enforced sandbox backend; `--hermetic` gives macOS and Windows the
same undeclared-input verdict without one.

Target-platform support is distinct from host support:
`[platform.*]` toolchain overlays cross-compile for any device target the
declared toolchain reaches (verified for aarch64-linux via `zig cc`), with
per-platform output trees and cache identities. Genrules and shell tests run
host-side. BSD `ar` on macOS lacks GNU `ar`'s `D` flag, so use an explicit
`arflags` value or `llvm-ar`; `--sandbox` stays Linux-only and `--hermetic`
runs everywhere.
