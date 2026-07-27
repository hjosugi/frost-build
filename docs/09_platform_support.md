# Host and target platform support

Linux remains the reference host: inotify via `notify`, hardlink/copy CAS,
Unix-domain daemon sockets and bubblewrap sandboxing. macOS uses the native
`notify` backend and the same Unix-domain daemon transport. Native `clonefile`
and Seatbelt are not implemented; `--sandbox` reports unavailable when
bubblewrap is absent.

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
| `--sandbox` E2E | every host without bubblewrap | the test returns early when `/usr/bin/bwrap` is absent, so the Linux-only backend does not fail elsewhere |
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
only strict sandbox backend.

Target-platform support is distinct from host support:
`[platform.*]` toolchain overlays cross-compile for any device target the
declared toolchain reaches (verified for aarch64-linux via `zig cc`), with
per-platform output trees and cache identities. Genrules and shell tests run
host-side. BSD `ar` on macOS lacks GNU `ar`'s `D` flag, so use an explicit
`arflags` value or `llvm-ar`; `--sandbox` stays Linux-only.
