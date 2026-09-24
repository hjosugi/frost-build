# FrostBuild user guide

How to build *your* repository with Frost. This is the entrance for using the
tool; the numbered documents one level up are the other entrance — the
decision records and measurements that explain why Frost behaves the way it
does (see [the documentation index](../README.md)).

The tutorials, the Make, Ninja and npm migration guides and the
troubleshooting page are executed in CI exactly as written, each from an empty
directory: the files, the commands and the output snippets shown. (The Bazel
guide needs a Bazel installation and is not.) If a page here disagrees with the
`frost` you are running, the page is for a different version.

## Tutorials

Start here. Each one goes from an empty directory to a passing build and test,
and ends with the everyday loop — rebuild, no-op, change one file, explain.

| Tutorial | You will build | Needs |
|---|---|---|
| [C and C++](tutorials/c.md) | a static library, a binary and a `cc_test`, with debug and release profiles | a C compiler |
| [Java](tutorials/java.md) | a runnable JAR and a test, with Frost running `javac` directly | a JDK |
| [Rust through the command adapter](tutorials/rust.md) | a library crate and a binary with `rustc`, using a depfile | `rustc` |

The third tutorial is the template for every other language: any compiler with
a real command line is a `kind = "command"` target, run without a shell.

## Moving an existing build

| From | Guide | Frost's tool |
|---|---|---|
| Make | [Migrating from Make](migrate/make.md) | `frost init`, then hand-written targets |
| Ninja (and CMake) | [Migrating from Ninja](migrate/ninja.md) | `frost import-ninja` |
| Bazel | [Migrating from Bazel](migrate/bazel.md) | `frost import-bazel`, `frost bazel-dev` |
| npm scripts | [Migrating from npm scripts](migrate/npm.md) | `frost import-npm` |

Each guide lists what the importer deliberately refuses to translate. Those
stops are the important part: an importer that guessed would produce a build
that is wrong in ways nobody notices for months.

## Reference

- [`frost.toml` reference](reference/manifest.md) — every table and key, with
  types and defaults. A test keeps it identical to what the loader accepts.
- [Command reference](reference/cli.md) — every subcommand's `--help`,
  generated from the binary and checked against it.
- [Troubleshooting](troubleshooting.md) — why something rebuilt, why it did
  not, and the non-hermetic patterns that cause both.

## Beyond the guide

- [docs/06_manifest_spec.md](../06_manifest_spec.md) is the normative manifest
  specification: the semantics behind each key in the reference.
- [docs/28_compatibility_contract.md](../28_compatibility_contract.md) says
  which surfaces a release promises not to break.
- [docs/29_sample_workspaces.md](../29_sample_workspaces.md) walks through the
  five checked-in sample workspaces.
- [docs/30_distribution.md](../30_distribution.md) covers installation,
  `frostw` and self-update.

## Running the pages yourself

The pages are ordinary Markdown with a little meaning in their code-fence info
strings (`sh`, `c file=src/main.c`, `text output`), which
[`scripts/run_guides.py`](../../scripts/run_guides.py) executes:

```sh skip
cargo build -p frostbuild-cli
python3 scripts/run_guides.py --frost target/debug/frost docs/guide
```

A page whose tools are missing on your machine is skipped and says which tool
it needed.
