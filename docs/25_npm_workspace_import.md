# npm workspace gate and Vite build import

`frost import-npm` discovers npm workspaces from the root `package.json` and
turns selected non-interactive package scripts into Frost `test` targets. With
an explicit option, it also creates owned output-tree targets for conservatively
recognized Vite production builds.

Preview the generated root manifest:

```bash
frost -C my-monorepo import-npm --dry-run
```

The default script set is `test` and `typecheck`. Select another
non-interactive gate by repeating or comma-separating `--script`:

```bash
frost -C my-monorepo import-npm \
  --script test,typecheck,e2e \
  --npm /absolute/path/to/npm \
  --node /absolute/path/to/node
frost -C my-monorepo test --all
```

Import validation gates plus recognized `vite build` scripts:

```bash
frost -C my-monorepo import-npm --vite-builds --dry-run
frost -C my-monorepo import-npm --vite-builds
frost -C my-monorepo test --all
frost -C my-monorepo build
```

The build option recognizes `vite build` as adjacent command tokens, including
a preceding step such as `tsc -b && vite build`. It emits
`npm --prefix PACKAGE run build -- --outDir dist/${config}` and owns
`PACKAGE/dist/${config}` through `output_dirs`. Builds that already provide a
custom `--outDir` or `--watch` are not inferred; overriding either would be an
unsafe guess.

`lint` is opt-in because some framework-provided lint scripts bootstrap or
rewrite project configuration on their first run. Import it only after the
workspace's lint command is known to be non-interactive and non-mutating:

```bash
frost -C my-monorepo import-npm --script test,typecheck,lint
```

The importer supports both npm workspace forms:

```json
{ "workspaces": ["apps/*", "packages/*"] }
```

```json
{ "workspaces": { "packages": ["apps/*", "packages/*"] } }
```

For every matching package script, the generated target:

- invokes `npm run SCRIPT --workspace PACKAGE` as direct argv through a
  fingerprinted `[toolchain.tools].npm`, with the Node runtime included in the
  same toolchain closure;
- is a first-class Frost test gate with success-only result caching;
- tracks the root package metadata, npm lock/config files, the package tree,
  and transitive in-repository workspace dependency trees;
- links same-script runtime workspace dependencies in the Frost target graph;
- forces `CI=true` so imported gates cannot silently enter watch mode, and
  forwards only an explicit small Node/npm environment set;
- disables the workspace sandbox because `node_modules` remains npm-owned.

The generated broad package patterns rely on the repository `.gitignore` and
`.frostignore` to exclude `node_modules`, `dist`, coverage, compiler caches,
and other generated state. Review the manifest before running it.

## Deliberate limits

The importer refuses the conventional output-producing or persistent script
names `build`, `dev`, `start`, `serve`, `watch`, and `preview`, even when they
are passed explicitly. A custom script name can still be interactive, start a
persistent process, or write a variable output tree, so it must be reviewed
before import; treating any of those behaviors as a cached test would be
incorrect. `--vite-builds` creates `kind = "command"` targets and does not
relax that test-gate rule. Other frameworks and custom Vite output contracts
still need hand-written command targets.

Persistent development processes stay outside the cached graph. Vite, Expo or
Tauri owns browser HMR and state; Frost can provide success-only generic
restart but does not synthesize module updates. `node_modules` remains
npm-owned, excluded from inputs and outside the sandbox. The lockfile and
Node/npm executables are fingerprinted, making this an explicit non-hermetic
boundary rather than an implied one.

The importer never overwrites an existing `frost.toml`. The full real-repository
production build, no-op and byte-identical output-tree restoration proof is in
[27_npm_production_adoption.md](27_npm_production_adoption.md).
