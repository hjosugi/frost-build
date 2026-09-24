# Migrating from Ninja

<!-- guide-test: requires=ninja,cc dir=calc -->

`frost import-ninja` translates a `build.ninja` into a `frost.toml` in which
every Ninja edge is a `genrule` running the same command. It is a starting
point for a migration, not a way to run Ninja files: the value comes from the
second half of this guide, where the imported edges are replaced with Frost's
native rules one by one.

The importer accepts a small subset of Ninja and refuses the rest by line
number, rather than dropping what it cannot translate. The subset and its
design notes are in [docs/06_ninja_importer.md](../../06_ninja_importer.md).
This walk-through starts in an empty directory named `calc`.

## 0. The starting point

```c file=include/calc.h
#ifndef CALC_H
#define CALC_H
int add(int a, int b);
#endif
```

```c file=src/calc.c
#include "calc.h"

int add(int a, int b) { return a + b; }
```

```c file=src/main.c
#include <stdio.h>

#include "calc.h"

int main(void) {
    printf("calc: %d\n", add(40, 2));
    return 0;
}
```

A hand-written `build.ninja`, as small projects and generators commonly write
it:

```ninja file=build.ninja
cflags = -O2 -Wall -Iinclude

rule cc
  command = cc $cflags -c $in -o $out
  description = CC $out

rule link
  command = cc $in -o $out

build out/main.o: cc src/main.c | include/calc.h
build out/calc.o: cc src/calc.c | include/calc.h
build out/calc: link out/main.o out/calc.o

build all: phony out/calc
default all
```

```sh
ninja
./out/calc
```

```text output
calc: 42
```

## 1. Import

Write the manifest next to `build.ninja` — Ninja paths are relative to that
directory, and so are a manifest's:

```sh
rm -rf out .ninja_log .ninja_deps
frost import-ninja build.ninja --output frost.toml
cat frost.toml
```

```text output
[target.out_main_o]
kind = "genrule"
cmd = "cc -O2 -Wall -Iinclude -c src/main.c -o out/main.o"
inputs = ["src/main.c","include/calc.h"]
cmd = "cc out/main.o out/calc.o -o out/calc"
deps = ["out_main_o","out_calc_o"]
default_targets = ["out_calc"]
```

What the translation did:

- **Variables are evaluated.** `$cflags` is written out in each command, so
  the manifest shows the exact command line that will run.
- **`$in` and `$out` become the edge's own paths**, in Ninja's order.
- **Inputs are split** into files (`inputs`) and edges that produce them
  (`deps`), so the link depends on both compiles.
- **Implicit (`|`) inputs are inputs; order-only (`||`) inputs are
  dependencies.** Treating order-only inputs as real inputs can only rebuild
  more than Ninja would, never less.
- **`phony` edges are aliases, not targets**, and `default all` became
  `default_targets` through the alias.

## 2. Build and compare

```sh
frost build
./out/calc
frost build
```

```text output
frost: 3 built · 3 actions
calc: 42
frost: up to date
```

The binary does what the Ninja-built one did. Frost's no-op check is content
based: touching a source file without changing it does not rebuild, where
Ninja would.

## 3. What the importer refuses

Each of these stops the import with the file and line; nothing is written.

| Ninja feature | Why it is refused |
|---|---|
| `depfile`, `deps = gcc`/`msvc` on a rule | a genrule cannot read the header list a compiler reports, so a header edit would not rebuild |
| per-edge bindings (indented `name = value` under `build`) | the edge would run a different command than its rule says |
| `include`, `subninja` | one self-contained file only |
| `pool`, and rule attributes other than `command` and `description` | scheduling and restat semantics the genrule does not have |
| validations (`\|@`) and `$in_newline` | not translated |
| a path with `..` or an absolute path | the manifest could not own it; an out-of-source build directory (`cmake -B build`) cannot be imported as is |
| an existing `frost.toml` at `--output` | importers never overwrite a manifest |

A CMake- or Meson-generated `build.ninja` uses most of these — `include`,
depfiles, `phony` re-run rules, absolute paths — so it will be refused. For
those projects the useful input is the source tree, not the generated file:
start from `frost init` (see [Migrating from Make](make.md)) or keep CMake
behind a single `command` target.

Here is the refusal for the most common one, a rule with a depfile:

```sh fails
cat > with-depfile.ninja <<'EOF'
rule cc
  command = cc -MD -MF $out.d -c $in -o $out
  depfile = $out.d
  deps = gcc
build out/main.o: cc src/main.c
EOF
frost import-ninja with-depfile.ninja --output with-depfile.toml
```

```text output
with-depfile.ninja:3: rule "cc" sets `depfile`
```

## 4. Replace edges with native rules

An imported compile edge is correct only as long as its declared inputs are
complete — here, as long as someone keeps `include/calc.h` in the edge. Frost's
native C/C++ rules discover headers from the compiler itself, keep objects in a
per-configuration tree, and support profiles and platforms. Once the imported
build matches Ninja's, replace the compile and link edges:

```toml file=frost.toml
[workspace]
default_targets = ["calc"]

[toolchain]
cc = "cc"
cflags = ["-O2", "-Wall"]

[target.calc_lib]
kind = "cc_library"
srcs = ["src/calc.c"]
includes = ["include"]

[target.calc]
kind = "cc_binary"
srcs = ["src/main.c"]
deps = ["calc_lib"]
```

```sh
frost build
"$(frost info bin_dir)/calc"
```

```text output
calc: 42
```

Edges that are not compiles or links — code generators, asset steps — are
worth keeping as genrules, or turning into `kind = "command"` targets when the
tool has a real command line, so it runs without a shell and is
fingerprinted as a tool. See the
[command-adapter tutorial](../tutorials/rust.md).
