# Migrating from npm scripts

<!-- guide-test: requires=npm,node dir=shop -->

In an npm workspace, `npm` owns dependency resolution and `node_modules`, and
it should keep owning them. What Frost adds is a graph and a cache *around*
the scripts: `frost test --all` runs every package's checks in dependency
order, in parallel, and skips a package whose files — and whose workspace
dependencies' files — have not changed since it last passed.

`frost import-npm` writes that graph from `package.json` metadata. This guide
runs it on a two-package workspace, in an empty directory named `shop`.

## 0. The starting point

An npm workspace with two packages, where `@shop/web` depends on
`@shop/prices`:

```json file=package.json
{
  "name": "shop",
  "private": true,
  "workspaces": ["packages/*"]
}
```

```json file=packages/prices/package.json
{
  "name": "@shop/prices",
  "version": "1.0.0",
  "main": "index.js",
  "scripts": {
    "test": "node --test",
    "build": "node -e \"console.log('bundling')\""
  }
}
```

```js file=packages/prices/index.js
exports.withTax = (cents) => Math.round(cents * 1.1);
```

```js file=packages/prices/test/prices.test.js
const assert = require("node:assert");
const test = require("node:test");
const { withTax } = require("..");

test("adds ten percent", () => assert.strictEqual(withTax(100), 110));
```

```json file=packages/web/package.json
{
  "name": "@shop/web",
  "version": "1.0.0",
  "dependencies": { "@shop/prices": "1.0.0" },
  "scripts": {
    "test": "node --test",
    "typecheck": "node --check index.js"
  }
}
```

```js file=packages/web/index.js
const { withTax } = require("@shop/prices");
exports.label = (cents) => `$${(withTax(cents) / 100).toFixed(2)}`;
```

```js file=packages/web/test/web.test.js
const assert = require("node:assert");
const test = require("node:test");
const { label } = require("..");

test("labels with tax", () => assert.strictEqual(label(1000), "$11.00"));
```

`npm install` links the workspace packages into `node_modules` and writes the
lockfile; nothing is downloaded, because nothing outside the workspace is
depended on.

```sh
npm install --no-audit --no-fund --offline
npm run test --workspaces
```

## 1. Preview the import

```sh
frost import-npm --dry-run
```

```text output
[target.shop-prices-test]
args = ["run","test","--workspace","@shop/prices"]
[target.shop-web-test]
deps = ["shop-prices-test"]
[target.shop-web-typecheck]
env = { CI = "true" }
```

What the importer decided, per package script:

- **`test` and `typecheck` are imported by default**, as `kind = "test"`
  targets: a script that checks something and exits 0 or not. Their verdict is
  cached. More gates are opt-in with `--script test,typecheck,lint`.
- **`build` was not imported.** `build`, `dev`, `start`, `serve`, `watch` and
  `preview` are refused even when named explicitly: they write output trees or
  start long-running processes, and caching one as a pass/fail test would be
  wrong. A build becomes a `kind = "command"` target with declared
  `output_dirs`; for Vite, `--vite-builds` writes that target for you when the
  script is recognizably `vite build`.
- **`@shop/web`'s test depends on `@shop/prices`'s test**, because web lists
  prices as a dependency; and web's inputs include prices' files, so changing
  prices reruns both.
- Each target runs `npm run SCRIPT --workspace PACKAGE` directly (no shell),
  with `npm` and `node` fingerprinted as tools, `CI=true` forced so no script
  can fall into watch mode, and only a small list of Node/npm variables passed
  through.
- `sandbox = false`, because `node_modules` stays npm's — outside Frost's
  inputs, and outside the sandbox.

## 2. Import and run

```sh
frost import-npm
frost test --all
```

```text output
3 targets (3 test gates, 0 Vite builds; test, typecheck)
tests: 3 passed, 0 failed, 0 cached
```

Nothing changed, so nothing runs:

```sh
frost test --all
```

```text output
tests: 0 passed, 0 failed, 3 cached
```

A change to `@shop/web` reruns only web's gates; a change to `@shop/prices`
reruns both packages' tests, because web depends on it:

```sh
echo '// comment' >> packages/web/index.js
frost test --all --explain
```

```text output
cached test:shop-prices-test
ran test:shop-web-test :: input changed: packages/web/index.js
tests: 2 passed, 0 failed, 1 cached
```

## 3. Review what you imported

The generated `frost.toml` is meant to be read. Check, in particular:

- **The broad input globs.** Each package's inputs are its files, excluding
  `node_modules`, `dist`, `build`, `coverage` and other generated directories
  by name, plus the root `package.json`, lockfile, `.npmrc` and
  `tsconfig*.json`. Add anything else your scripts read to `.frostignore` or
  `.gitignore` if it is generated, or to `inputs` if it is not.
- **Scripts that are not really checks.** A custom script name can still
  rewrite files or start a server. Remove it from the manifest, or never pass
  it to `--script`. `lint` is opt-in for this reason: some lint setups rewrite
  configuration on first run.
- **The `npm`/`node` paths.** `--npm` and `--node` pin absolute paths, so CI
  and laptops agree on which Node ran.

The importer never overwrites an existing `frost.toml`.

## 4. What stays npm's

- Dependency installation and `node_modules`: run `npm ci` before `frost test`
  in CI. The lockfile is an input, so a dependency change reruns every gate.
- Development servers and HMR (`dev`, `start`, `serve`): run them directly.
  `frost watch --run` / `frost dev` can restart a process after a successful
  build, but browser hot reload stays the framework's.
- Publishing.

The real-repository adoption record — a production Vite build, its no-op and
its byte-identical restoration from the cache — is
[docs/27_npm_production_adoption.md](../../27_npm_production_adoption.md);
the importer's design is
[docs/25_npm_workspace_import.md](../../25_npm_workspace_import.md).
