# npm/Vite production adoption

Decision: production adoption is accepted for bounded validation and Vite
build trees. Persistent browser HMR remains framework-owned, and
`node_modules` remains package-manager-owned rather than being presented as a
hermetic Frost input.

The checked certificate is
[`2026-07-28-iroha-pdf-adoption.json`](../bench/baselines/2026-07-28-iroha-pdf-adoption.json).
It records a read-only clone of
[`hjosugi/iroha-pdf`](https://github.com/hjosugi/iroha-pdf) at
`10bf0acb10d18f374f6d33d4f69fcf847afe1c4b`; no change was pushed to that
repository.

## Production proof

Environment: FrostBuild 0.6.1, Node 26.5.0, npm 11.17.0, TypeScript 6.0.3 and
Vite 8.1.4 on Linux x86-64. Dependencies were materialized with
`npm ci --ignore-scripts`.

The repository's checked `frost.toml` already contains seven imported test and
typecheck gates plus a hand-written `desktop-web` command target whose owned
tree is `apps/desktop/dist/${config}`.

```bash
frost test --all --no-tui
frost build desktop-web --no-tui
frost build desktop-web --no-tui
```

Results:

- all seven graph gates passed: 51 JavaScript/TypeScript tests plus the
  workspace type checks, in 6569 ms;
- the first production Vite build completed in 2917 ms;
- the immediate no-op completed in 0 ms;
- the output tree contained seven files and 6,260,425 bytes;
- after moving the complete `dist/debug` tree out of the workspace, Frost
  restored it from CAS without executing Vite in 10 ms;
- normalized SHA-256 manifests before and after restoration were identical:
  `88b3753c4fd734847cb8321e2f58c1bb6b6907ea7fdf5de04afb35462b3b719b`.

Vite reported bundle-size and browser-crypto compatibility warnings, but the
production build succeeded. Those warnings concern the adopted application's
bundle and do not invalidate Frost's output ownership proof.

## Importer boundary

`frost import-npm --vite-builds` now recognizes the conservative script shape
`vite build` (including a preceding step such as `tsc -b && vite build`) and
emits an explicit command target:

```toml
[target.iroha-pdf-desktop-build]
kind = "command"
tool = "npm"
args = [
  "--prefix", "apps/desktop", "run", "build", "--",
  "--outDir", "dist/${config}",
]
output_dirs = ["apps/desktop/dist/${config}"]
sandbox = false
```

The generated target includes the package and transitive in-repository
workspace sources, lock/config files, Node/npm toolchain closure, a narrow
environment, and any recognized dependency build edges. The option is
explicit because output ownership is a stronger promise than importing a
validation gate.

The importer refuses a Vite build that already supplies a custom `--outDir` or
`--watch`; it cannot know whether overriding that contract is safe. The
conventional `build` name remains invalid as `--script build`, so a build can
never be mislabeled and cached as a test.

## Resolved policy decisions

### Persistent compiler and browser HMR

Vite/Expo/Tauri development processes remain outside the cached action graph.
They own their protocol, browser state and module replacement. Frost can wrap
their stable production build and can use generic success-only `watch`/`dev`
restart, but does not claim module-level HMR.

### `node_modules`

The npm lockfile and Node/npm executable closure are fingerprinted.
`node_modules` is materialized by `npm ci`, excluded from broad input globs,
and imported npm targets set `sandbox = false`. That is an explicit
non-hermetic boundary, not a silent sandbox hole. A future hermetic policy
would need an immutable package-manager store or declared install action; it
is not required to make the current cache claim.

### Source maps and debugger handoff

Source-map generation belongs to the producer. The adopted Vite configuration
enables maps for its Tauri debug environment. For runnable JavaScript
artifacts, `frost debug` launches Node inspect and `frost ide` emits a Node
launch configuration whose `sourceMaps` value is true only when a `.map` file
is declared in the target closure. Browser DevTools/DAP session ownership
stays with Vite/Tauri; Frost does not imply maps that the build did not emit.

This resolves the original “cannot adopt” record: the generic executor,
toolchain/environment model, workspace discovery, owned output directories and
real production proof now exist. The remaining boundaries are documented
product policies rather than unimplemented prerequisites.
