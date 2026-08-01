# FrostBuild VS Code extension

The editor front end for `frost`, living in the same repo as the build tool it
drives so that a CLI change and the extension change that follows from it land
together. Tracked as issue #180.

## What it does, and what it deliberately does not

It connects the edit loop to the build graph: build a target chosen from the
graph, build the targets that own the file in front of you, run a test target,
and surface compiler diagnostics from a failed build in the Problems panel. It
contributes a `frost` task type so a workspace can wire builds into
`tasks.json` without hand-writing a shell line.

It is not a `frost.toml` language client. Completion, hover and go-to-definition
inside a manifest need a parser that agrees exactly with the one frost itself
uses, and reimplementing that in TypeScript would produce a second grammar to
keep in sync — a guaranteed source of "the editor says this is fine, the build
says it is not". That work belongs to `frost lsp`, an editor-independent
language server tracked as issue #181. When it exists this extension becomes
its client, and gains those features without owning a parser.

## Architecture: one impure module

Everything under `src/frost/` is pure except `cli.ts`:

| Module | Role |
|---|---|
| `src/frost/types.ts` | the shapes read out of frost |
| `src/frost/cli.ts` | spawns `frost` and captures output — the only impure module |
| `src/frost/targets.ts` | parses target listings into structured targets |
| `src/frost/diagnostics.ts` | parses compiler output into diagnostics |
| `src/frost/tests.ts` | parses test targets, shards and result summaries |
| `src/workspace.ts` | settings and workspace resolution — VS Code layer |
| `src/extension.ts` | the VS Code layer: commands, tasks, UI |

Nothing under `src/frost/` other than `cli.ts` imports `vscode`, and the
parsers take already-captured text rather than running anything. That is not
tidiness for its own sake: it is what makes `npm test` a plain `node --test`
with no VS Code download, no display, and no built `frost` binary. A test that
needs to download VS Code is a test CI runs reluctantly and developers skip; a
test that is `node --test` runs everywhere the repo already runs — which is
why the CI job for this directory needs neither a display server nor a
download. The parsers are also where the bugs actually live —
output formats change, command wiring does not — so this is the split that puts
the fast tests on the risky code.

The rule is enforced by consequence rather than by a lint: an accidental
`import * as vscode` in a pure module makes the unit tests fail to resolve the
module under plain Node, immediately.

## What it depends on in the frost CLI

Only surfaces that `docs/28_compatibility_contract.md` marks as contract:

- `frost info --json` — workspace root, config, and the output tree layout, so
  the extension never has to encode the `.frost/out/<config>` rule itself.
- `frost query owners|deps|rdeps|somepath|allpaths --json` — target discovery
  and the file-to-target mapping behind "build the targets owning this file".
- `frost query ... --output label-kind` — the label and kind of each target, for
  splitting build targets from test targets.
- `frost build` / `frost test` exit codes — `0` completed, `1` your code, `2`
  your invocation. The extension reports the second and third differently.

Human-facing progress and diagnostic text is explicitly *not* contract, and the
modules that parse it say so at the point they do it. Everything else here is
covered: renaming a subcommand, an option or a `--json` field breaks this
extension, which is the reason those surfaces are under a contract with a
deprecation procedure rather than under "we will try to remember".

## Build and run

```bash
npm install         # devDependencies only; the extension has no runtime deps
npm run compile     # tsc -p ./  ->  out/
npm test            # compile, then node --test on out/test/
npm run lint        # tsc --noEmit -p ./
```

The test glob is quoted so Node expands it rather than the shell, which keeps
the command identical on POSIX and Windows. Node's own globbing is why: passing
a bare directory to `node --test` stopped walking it in current Node, and
resolves the path as a module instead.

To run the extension itself, open `tools/vscode/` in VS Code and press F5. That
launches an Extension Development Host with the extension loaded; open a
workspace containing a `frost.toml` in it, which is what
`workspaceContains:frost.toml` activates on.

`out/` is generated and git-ignored, as is `node_modules/`.

### Pinned versions

- `typescript` `^5.9.3` — 5.x rather than the current 7.x default: the compiler
  is only build scaffolding here, and tracking a compiler rewrite is not a cost
  worth paying for four commands and a handful of parsers.
- `@types/vscode` `~1.90.0` — tilde, not caret. A caret would resolve to the
  newest 1.x and let code typecheck against API that does not exist in the
  `engines.vscode` floor this extension claims to support.
- `@types/node` `^22.20.1` — the Node the tests run under. The `engines.node`
  floor is `>=20`, which is what VS Code 1.90's extension host ships.

## Publishing

`package.json` sets `"private": true` (JSON has no comments, so the reasoning
is here). Nothing about this landing implies a Marketplace listing: publishing
means a publisher identity, a support commitment to strangers, and a release
cadence tied to VS Code's rather than to frost's. That is a separate decision
to be made deliberately, not a side effect of adding a directory. Until it is
made, this extension is built and run from the repo, and `private: true` makes
an accidental `vsce publish` fail rather than succeed.
