# Migrating from Make

<!-- guide-test: requires=make,cc dir=calc -->

There is no `frost import-make`, and there will not be a general one: a
Makefile is a program — `$(shell ...)`, recursive `make`, conditionals,
variables from the environment — and the only faithful interpreter of it is
Make. What can be translated mechanically is the part Make is usually used
for in C and C++ projects: compile, archive, link, test. This guide does that
by hand, with `frost init` writing the first draft, and keeps the Makefile
until the two builds agree.

The walk-through uses a small but typical Makefile. Run it in an empty
directory named `calc`, or follow along in your own project.

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

```makefile file=Makefile
CC ?= cc
CFLAGS ?= -O2 -Wall
OBJS = src/main.o src/calc.o

calc: $(OBJS)
	$(CC) $(CFLAGS) -o $@ $(OBJS)

src/%.o: src/%.c include/calc.h
	$(CC) $(CFLAGS) -Iinclude -c $< -o $@

test: calc
	./calc | grep -qx 'calc: 42'

clean:
	rm -f calc $(OBJS)

.PHONY: test clean
```

```sh
make
./calc
```

```text output
calc: 42
```

## 1. Inventory what Make actually runs

Read the commands, not the rules: `make -n -B` prints every command a clean
build would run without running it. This is the list your Frost build has to
reproduce.

```sh
make -n -B
```

```text output
-Iinclude -c src/main.c -o src/main.o
```

Sort each command into one of four buckets:

| What Make runs | Becomes in Frost |
|---|---|
| a compile of one `.c`/`.cpp` | a source in a `cc_library` or `cc_binary` target |
| `ar` of objects | a `cc_library` |
| a link | a `cc_binary` (or `cc_test` for a test program) |
| anything else that writes files | a `genrule` (shell) or `kind = "command"` (direct argv) with declared `inputs` and `outputs` |
| a `.PHONY` check such as `test` | a `kind = "test"` target |
| `clean`, `install`, `deploy` | not a build target — `frost clean`, or keep it in a script |

## 2. Let `frost init` draft the manifest

`frost init` does not read the Makefile. It looks at the sources, recognizes
`main()` and `include/`, and writes native rules:

```sh
frost init --dry-run
```

```text output
[target.calc_lib]
kind = "cc_library"
srcs = ["src/calc.c"]
includes = ["include"]
[target.calc]
kind = "cc_binary"
```

That draft is close to the finished manifest. Write the one you actually want
— here: the draft, with the release profile matching the Makefile's `-O2`, and
the Makefile's `test` target added — and compare it with the Makefile line by
line:

```toml file=frost.toml
[workspace]
default_targets = ["calc"]

[toolchain]
cc = "cc"
cflags = ["-Wall"]

[toolchain.tools]
sh = "sh"

[profile.debug]
cflags = ["-O0", "-g"]

[profile.release]
cflags = ["-O2"]

[target.calc_lib]
kind = "cc_library"
srcs = ["src/calc.c"]
includes = ["include"]

[target.calc]
kind = "cc_binary"
srcs = ["src/main.c"]
deps = ["calc_lib"]

[target.calc_test]
kind = "test"
tool = "sh"
args = ["-c", '"$0" | grep -qx "calc: 42"', "${dep:calc}"]
deps = ["calc"]
```

What changed, and why:

- **`CFLAGS ?= -O2 -Wall` became a toolchain flag plus profiles.** Make's `?=`
  lets the environment override the flags silently; Frost clears the
  environment of every action, and flags that differ per build type are
  profiles (`--profile release`), each with its own output tree and cache.
- **The header dependency list is gone.** `src/%.o: src/%.c include/calc.h`
  is a hand-maintained guess that goes stale the day someone adds a header.
  Frost reads the compiler's own dependency output (`-MD`) and rebuilds
  exactly the translation units whose headers changed.
- **`-Iinclude` moved to `includes` on the library**, which exports it to
  every dependent — `calc` does not repeat it.
- **`test` became a target.** It depends on `calc`, runs its output through
  `${dep:calc}`, and its verdict is cached like any other result. The `sh -c`
  keeps the Makefile's pipeline; a test runner with a real command line would
  not need the shell.
- **Objects live under `.frost/`**, not next to the sources, so there is no
  `clean` target to write — `frost clean` removes only what Frost owns.

## 3. Build both ways and compare

```sh
frost build
frost test --all
"$(frost info bin_dir)/calc"
```

```text output
TEST calc_test
tests: 1 passed, 0 failed, 0 cached
calc: 42
```

Keep comparing until you trust the result: the program's behavior, the test
verdicts, and — for a release build — the flags each compile received
(`frost compdb` writes a `compile_commands.json` you can diff against `make -n`
output). Then run `frost build` again: nothing runs, where `make` would have
re-checked every timestamp.

```sh
frost build
make clean
```

```text output
frost: up to date
```

## 4. What does not translate

Stop and decide by hand when the Makefile does any of these; do not
approximate them:

- **`$(shell ...)`, `$(wildcard ...)` computed at parse time.** Frost's graph
  is a function of the manifest. A value that must come from outside the build
  (a version, a git SHA) belongs in `[stamp]`; a file list belongs in a glob.
- **Recursive `make -C subdir`.** Each subdirectory becomes a package with its
  own `frost.toml` under one root `[workspace]`, and cross-directory
  dependencies become labels such as `//lib:lib`.
- **Environment-driven behavior** (`CC`, `CFLAGS`, `DESTDIR` from the caller).
  Put toolchains in `[toolchain]`/`[platform.NAME]` and flags in profiles; a
  variable an action genuinely needs is declared with `env` or `pass_env`,
  which also puts it in the cache key.
- **Timestamp tricks** (touch stamps, `.PHONY` used to force reruns). Frost
  rebuilds on content, not time, so a stamp file carries no meaning.
- **Side effects** — install, deploy, publish, docker. They are not build
  outputs; keep them in scripts that run after `frost build`.

If a part of the build is too Make-shaped to translate yet, it can stay Make's:
a `kind = "command"` target with `tool = "make"` runs it as one cached action,
provided its outputs are declared (under `${config}`) and its inputs listed.
Frost then treats that subtree as a single boundary and does not look inside.
