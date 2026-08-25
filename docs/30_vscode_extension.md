# The VS Code extension

`tools/vscode/` is unpublished (`private: true`) and built and tested in CI. It
is a client of the `frost` binary, not a second implementation of anything: it
runs commands, parses contract surfaces, and puts the answers where an editor
expects them.

## The split that makes it testable

Everything under `src/frost/` is pure — it never imports `vscode` — except
`cli.ts`, which spawns the binary. Everything that *does* import `vscode` is
kept thin enough to hold in your head: `extension.ts`, `tasks.ts`,
`workspace.ts`, and the providers.

That is not a stylistic preference. The alternative, an extension test harness,
downloads VS Code and starts a display server on every CI run. A job slow
enough that nobody waits for it is a job nobody trusts. Instead `node --test`
runs the whole suite against a stubbed `vscode` module in well under a minute,
and `test/architecture.test.ts` enforces the rule — with a guard against
vacuously passing if its own path resolution breaks, which caught a real
mistake while it was being written.

The stub is in `test/vscode-stub.ts` and records what the extension did:
commands registered, diagnostics published, context keys set, quick picks
shown. Tests assert against those recordings, so a command registered under the
wrong id fails a test rather than producing "command not found" in someone's
editor.

## What it asks frost for

Listed in [28_compatibility_contract.md](28_compatibility_contract.md). The
short version: `frost info --json` for paths, `frost query targets` for the
tree and the pickers, `frost query owners` for "build what owns this file", and
`--no-tui` build output for diagnostics. `frost daemon status --json` is polled
every five seconds for the status bar; stopped, running, and protocol-mismatch
are data states, while an unavailable binary stays a quiet unknown indicator.

The status bar keeps two independent pieces of state: daemon health and the
last build/test/debug result. Refreshing daemon health therefore cannot erase a
failure, and a successful build cannot make a stopped daemon look healthy.

Diagnostics keep the target frost attributed them to (`frost (//core:core)`),
which is the part a declarative problem matcher cannot do — a matcher sees the
compiler's line and never frost's `FAILED: ... (//core:core)` framing.

## Debugging

There is no debug adapter here. `frost.debugTarget` builds the target and hands
off to whatever adapter the language already has — cpptools, vscode-java-debug,
Node, debugpy. Writing a fifth adapter to launch a binary that the four
existing ones already launch would be work with no payoff, and it would rot
independently of all of them.

## Things deliberately not done

- **A dependency graph view.** `frost query` answers graph questions in a
  terminal already, and a rendered graph of a real workspace is unreadable.
- **Parsing `--output dot`.** Node shapes encode target kind, which is a
  rendering choice rather than a contract. The extension did this once, to
  enumerate targets before `frost query targets` existed; both the workaround
  and its parser were deleted when the primitive landed.
- **A language server.** Tracked separately as #181; the extension does not
  pretend to be one in the meantime.
