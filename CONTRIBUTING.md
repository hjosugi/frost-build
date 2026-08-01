# Contributing

Issues use Background / Scope / Acceptance Criteria / Dependencies. Feature,
correctness and research forms are provided. Research is complete only when a
checked-in decision memo records evidence, adoption/rejection and follow-up.

Labels: `area:*` names ownership; `kind:feature`, `kind:test`, `kind:infra`, and
`kind:research` name work type; `perf` requires harness evidence; `correctness`
requires a regression scenario. Apply both when a speed optimization changes a
correctness boundary.

Before a PR:

```bash
frost test --all          # the gate, incrementally, if you have a frost built
scripts/check.sh          # the same stages, unconditionally, with no frost
```

Both run `cargo test --workspace --all-targets --locked`,
`cargo clippy --workspace --all-targets --all-features -- -D warnings`,
`cargo fmt --all -- --check`, `python3 -m unittest discover -s tests` and the
VS Code extension's suite.

The root `frost.toml` is this repository building itself. Each stage declares
what it reads, so a stage that already passed on these exact inputs is a cached
success: editing a `.rs` file reruns the three Rust stages and leaves the Python
and extension suites alone, which `scripts/check.sh` cannot do. Measured here,
a second run of an unchanged tree is 1.6 s against 103 s. `frost test --all
--explain` says which input changed and why a stage reran.

`scripts/check.sh` stays because bootstrapping cannot depend on the thing being
bootstrapped — a contributor with no frost, or one whose frost does not build,
still needs the gate.

## Releasing

Releases are cut by CI, not from a terminal. Nothing here needs push access to
a tag.

```bash
scripts/release.sh 0.7.1        # bump the workspace version, stamp the CHANGELOG, commit
```

Open a PR for that commit and merge it. Then publish, from the Actions tab or
with:

```bash
gh workflow run release.yml -f version=0.7.1
```

The workflow refuses a version that does not match `Cargo.toml`, has no
CHANGELOG section, or already has a tag; it builds the Linux/macOS/Windows
archives, then creates the tag and the GitHub release together, so a failed
build cannot leave a tag behind. Pushing a `vX.Y.Z` tag by hand still works and
takes the same path.

Merged branches are removed weekly by `.github/workflows/branch-cleanup.yml`,
which only deletes branches whose every commit is already in the default
branch. Run it early with `gh workflow run branch-cleanup.yml -f dry_run=false`,
or use `scripts/cleanup-branches.sh` from a clone.

Changing a CLI name, a manifest key, a JSON field or an exit code touches the
compatibility contract in [docs/28_compatibility_contract.md](docs/28_compatibility_contract.md).
Additive changes only need the CLI snapshot refreshed with
`UPDATE_CLI_SURFACE=1 cargo test -p frostbuild-cli --bin frost`; renames and
removals follow the deprecation procedure in that document.

Performance claims must include `frost-bench` JSON, host metadata, medians and
dispersion. Do not use a one-off stopwatch result. Design changes update
`DESIGN.md`; manifest/storage changes add compatibility and corruption tests.
Use conventional commit subjects. M1 covers correctness/table stakes, M2 local
performance/tooling, and M3 daemon/distribution/v2 research.
