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
macOS runner image is in that state.

Windows runs the same whole suite. The image ships MinGW, and the default driver
names are the host's (`gcc`/`g++` where `cc`/`c++` do not exist), so the native
C/C++ cases run there too. Tests run one at a time with unbuffered output, so a
case that hangs is named in the log rather than leaving the job to expire.

Two Windows-only defects surfaced when those cases first ran. `frost.toml` is
TOML, so a `\` in command text is itself escaped; and `cmd` binds the remainder
of the line to an `if` branch, so `if not exist dir mkdir dir & echo x>dir\f`
skipped everything once frost had already created the output's parent — the
action "succeeded" having written nothing. frost creates the parent of every
declared output, so such a guard is never needed in command text.

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
