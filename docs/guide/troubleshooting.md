# Troubleshooting

<!-- guide-test: requires=sh -->

Most surprises in an incremental build are one of two questions: *why did this
rebuild?* and *why didn't it?* Frost can answer both, and the second one is
almost always a missing declaration. This page uses a tiny workspace to show
each tool; the commands run in CI exactly as written.

```toml file=frost.toml
[workspace]
default_targets = ["banner"]

[target.banner]
kind = "genrule"
cmd = "cat ${in} > ${out}"
inputs = ["banner.txt"]
outputs = ["gen/banner.txt"]
```

```text file=banner.txt
hello
```

```sh
frost build
```

## Is my setup right? `frost doctor`

`frost doctor` loads the manifest, resolves every tool it names — compilers,
named `[toolchain.tools]`, optional debuggers, `fzf`, `bwrap`, Graphviz — and
says which are missing and whether that blocks a build. It also lists every
`.frostrc` setting in effect with the file and line it came from, which is
where "but I never passed that flag" usually ends.

```sh
frost doctor
```

```text output
frost: doctor · ready
result     build prerequisites are ready
```

`frost doctor --json` is the same report for scripts. `frost info` prints the
paths Frost derives (output directories, the journal, the cache), so a script
asks instead of hard-coding the layout:

```sh
frost info journal
frost info --json
```

## Why did this rebuild? `--explain`

`--explain` prints, for every action that ran, what made it run: the input
whose content changed, a changed command line, a changed tool, or a missing
output.

```sh
echo 'hello, again' > banner.txt
frost build --explain
```

```text output
ran genrule:banner :: input changed: banner.txt
```

`frost explain TARGET` answers the same question for one target without
building anything, including for actions that would *not* run:

```sh
frost explain banner
```

```text output
no execution required for banner (debug); all actions cached
```

A touched file with identical content does not rebuild: the cache key is the
content, not the timestamp.

```sh
touch banner.txt
frost build
```

```text output
frost: up to date
```

## Why did CI rebuild what my machine had cached? `frost journal`

`frost journal export` writes every recorded action's key material — argv,
environment, input digests, the toolchain fingerprint, profile and platform —
in a stable order. Export on both sides and diff:

```sh
frost journal export --out before.json
echo 'hello, CI' > banner.txt
frost build
frost journal export --out after.json
frost journal diff before.json after.json
```

```text output
journal: 1 difference
genrule:banner
  inputs: banner.txt
```

The diff reports the *first* field that differs per action, in the order argv,
environment, `pass_env`, inputs, outputs — decisions before their effects —
and a build-wide difference such as a different compiler is reported once,
not once per action. The usual findings are a different compiler build on the
CI image, an environment variable a target declared in `pass_env`, or an
absolute path that reached a command line.

## Why *didn't* this rebuild? Undeclared inputs

Frost rebuilds when a declared input changes. A command that reads a file the
manifest does not mention is invisible to it, and the build is then silently
stale. Here the genrule reads `greeting.txt` without declaring it:

```sh
cat >> frost.toml <<'EOF'

[target.stale]
kind = "genrule"
cmd = "cat greeting.txt > ${out}"
outputs = ["gen/stale.txt"]
EOF
echo 'first' > greeting.txt
frost build stale
echo 'second' > greeting.txt
frost build stale
cat gen/stale.txt
```

```text output
frost: up to date
first
```

The fix is to declare it — `inputs = ["greeting.txt"]` and `${in}` in the
command — after which the edit rebuilds. Finding these before they bite is
what `--hermetic` is for: each action runs in a private tree holding only the
files it declared, so the undeclared read fails loudly instead of reading
stale data. A checking mode is not part of the cache key — a result that
passed once is not re-run just to be checked — so start from an empty cache
to put every action through it:

```sh fails
frost clean --cache
frost build stale --hermetic
```

```text output
cat: greeting.txt: No such file or directory
```

The tools that catch undeclared inputs:

- **`frost build --hermetic`** works on every host with no extra tool; `frost
  doctor` shows how it fills the private trees (reflink, hardlink or copy).
  Run CI with it.
- **`frost build --sandbox`** (Linux, needs `bwrap`) reaches the same verdict
  with a bubblewrap sandbox instead of a copied tree.
- **Depfiles** make compilers report what they read. Native C/C++ rules do
  this automatically; a `command` target opts in with `depfile`.
- **`frost build --check-determinism`** reruns actions and compares outputs,
  which catches the neighboring problem: an action whose output changes when
  its inputs did not.

## Common non-hermetic patterns

| Pattern | Symptom | Fix |
|---|---|---|
| a script reads a file not in `inputs` | edits to that file do not rebuild | declare it; run CI with `--hermetic` or `--sandbox` |
| a genrule calls a tool from `PATH` (`python3`, `protoc`) | a tool upgrade does not rebuild | make it a `[toolchain.tools]` entry and a `command` target, so it is fingerprinted |
| an action reads `$HOME`, a cache directory or the network | different results on different machines | vendor the data (`[fetch.NAME]`), or declare the boundary and accept it, visibly |
| `pass_env = ["PATH"]` or `["HOME"]` | nothing is shared between machines, every CI run rebuilds | pass the specific variable the tool needs; `frost lint` reports this as `volatile-pass-env` |
| a timestamp, git SHA or random value in an output | every build changes the output, and everything downstream rebuilds | use `[stamp]` with a `STABLE_` key for values that should rebuild, a volatile key for ones that should not |
| an absolute path in `args` or `cmd` | works on one machine only; `frost lint` reports `absolute-path` | use workspace-relative paths and substitutions |
| two targets writing one directory | outputs overwrite each other | give each its own `${config}` path, or one target `output_dirs` |

`frost lint` checks the manifest for several of these statically, and exits 1
when it finds anything, so it can gate CI. On this page's manifest it finds
three: both genrules redirect with `>`, which means different things to
`/bin/sh` and `cmd.exe`, and `stale` is neither a default target nor a
dependency of one, so nothing would ever build it unless asked by name:

```sh fails
frost lint
```

```text output
(shell-dependent-cmd)
"stale" is not a default target and nothing depends on it
lint: 3 findings
```

## Starting over

`frost clean` removes this workspace's outputs for one configuration; `frost
clean --cache` also drops the cached decisions (journal, graph store, local
CAS). Nothing outside `.frost/` and the declared outputs is touched. If a
build ever behaves differently after `frost clean --cache`, that difference is
a bug worth reporting with the output of `frost journal export` from before
and after.

```sh
frost clean --cache
frost build
```

## Where the answers come from

- [docs/16_action_key_audit.md](../16_action_key_audit.md) lists everything
  that is and is not in an action's cache key, and why.
- [docs/22_developer_loop.md](../22_developer_loop.md) covers `--report`,
  `--trace` and the watch loop.
- The [command reference](reference/cli.md) has every flag mentioned here.
