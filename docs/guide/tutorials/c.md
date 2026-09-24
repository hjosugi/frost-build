# Tutorial: C and C++

<!-- guide-test: requires=cc -->

From an empty directory to a library, a program and a passing test, then the
loop you will run every day: rebuild, no-op, change one file, ask why.

You need `frost` on `PATH` and a C compiler reachable as `cc` (on Windows,
`gcc` from MinGW works; see the note at the end). Every command below is run
from the directory you start in.

## 1. The sources

A small library with a header, and a program that uses it.

```c file=include/greet.h
#ifndef GREET_H
#define GREET_H

/* Writes "hello, NAME" into out, which holds size bytes. */
int greet(char *out, unsigned long size, const char *name);

#endif
```

```c file=src/greet.c
#include "greet.h"

#include <stdio.h>

int greet(char *out, unsigned long size, const char *name) {
    return snprintf(out, size, "hello, %s", name);
}
```

```c file=src/main.c
#include "greet.h"

#include <stdio.h>

int main(int argc, char **argv) {
    char line[64];
    greet(line, sizeof line, argc > 1 ? argv[1] : "frost");
    puts(line);
    return 0;
}
```

## 2. The manifest

`frost.toml` declares what to build. Paths are relative to the manifest and use
`/` on every platform.

```toml file=frost.toml
[workspace]
default_targets = ["hello"]

[toolchain]
cc = "cc"
cflags = ["-Wall"]

[profile.debug]
cflags = ["-O0", "-g"]

[profile.release]
cflags = ["-O2", "-DNDEBUG"]

[target.greet]
kind = "cc_library"
srcs = ["src/greet.c"]
includes = ["include"]

[target.hello]
kind = "cc_binary"
srcs = ["src/main.c"]
deps = ["greet"]
```

What each part says:

- `[workspace]` makes this directory a workspace root, and `default_targets`
  is what `frost build` builds when you name nothing.
- `[toolchain]` names the compiler and flags every compile gets. The driver's
  identity is part of every compile's cache key, so switching compilers
  rebuilds rather than reusing objects from the other one.
- `[profile.*]` are named flag sets. `debug` is the default.
- `greet` is a static library. `includes` is *exported*: anything that depends
  on `greet` gets `-Iinclude` too, which is why `hello` never mentions it.
- `hello` links `greet` because it depends on it.

`frost lint` reviews a manifest for patterns that load fine and cost you later,
and `frost fmt --check` confirms it is in canonical form:

```sh
frost lint
frost fmt --check
```

## 3. Build and run

```sh
frost build
```

```text output
CC src/greet.c (greet)
CC src/main.c (hello)
AR libgreet.a
LINK hello
frost: 4 built · 4 actions
```

Compiles run in parallel, so the first two lines may appear in either order.
Outputs live under `.frost/`, split by configuration. Ask for the directory
instead of memorizing the layout:

```sh
frost info bin_dir
"$(frost info bin_dir)/hello"
```

```text output
/.frost/bin/debug
hello, frost
```

`frost run` builds first if needed, finds the artifact, and passes everything
after `--` to it:

```sh
frost run hello -- tutorial
```

```text output
hello, tutorial
```

## 4. The everyday loop

Building again with nothing changed does nothing, and says so:

```sh
frost build
```

```text output
frost: up to date
```

Change the library's source and rebuild with `--explain`, which says what ran
and why. Only `greet.c` recompiles; `main.c` does not, because nothing it
includes changed. The library is re-archived because an object changed, and
`hello` relinked because the library did.

```sh
sed -i.bak 's/hello, %s/hello again, %s/' src/greet.c && rm src/greet.c.bak
frost build --explain
"$(frost info bin_dir)/hello"
```

```text output
ran compile:greet:src/greet.c :: input changed: src/greet.c
cached compile:hello:src/main.c
ran link:hello :: input changed: .frost/lib/debug/libgreet.a
hello again, frost
```

`frost explain TARGET` answers the same question without building, and `frost
plan` says what a build would do:

```sh
frost explain hello
frost plan
```

```text output
all actions cached
plan: 0 would run, 0 may run, 4 cached (4 actions)
```

## 5. A test

A `cc_test` is a binary that Frost also runs; exit status 0 is a pass. The
result is cached like any other output, so an unchanged test does not run
again.

```c file=tests/greet_test.c
#include "greet.h"

#include <stdio.h>
#include <string.h>

int main(void) {
    char line[64];
    greet(line, sizeof line, "test");
    if (strcmp(line, "hello again, test") != 0) {
        fprintf(stderr, "unexpected greeting: %s\n", line);
        return 1;
    }
    return 0;
}
```

Append the target to the manifest:

```sh
cat >> frost.toml <<'EOF'

[target.greet_test]
kind = "cc_test"
srcs = ["tests/greet_test.c"]
deps = ["greet"]
EOF
frost test --all
```

```text output
TEST greet_test
tests: 1 passed, 0 failed, 0 cached
```

Run it again and the verdict comes from the cache:

```sh
frost test --all
```

```text output
tests: 0 passed, 0 failed, 1 cached
```

A failing test fails the command with exit status 1 and replays the test's
output after the run:

```sh fails
sed -i.bak 's/hello again, test/goodbye, test/' tests/greet_test.c && rm tests/greet_test.c.bak
frost test --all
```

```text output
unexpected greeting: hello again, test
```

Put it back. The test recompiles and relinks, but the relinked binary is
byte-identical to the one that already passed, so its verdict is reused
instead of running it again — early cutoff, applied to a test:

```sh
sed -i.bak 's/goodbye, test/hello again, test/' tests/greet_test.c && rm tests/greet_test.c.bak
frost test --all
```

```text output
tests: 0 passed, 0 failed, 1 cached
```

## 6. Release builds

A profile has its own output tree and cache, so switching back and forth never
rebuilds what the other one already built:

```sh
frost build --profile release
frost info bin_dir --profile release
frost build
```

```text output
/.frost/bin/release
frost: up to date
```

The last line shows the debug tree was left untouched by the release build.

## 7. Where to go next

- C++ works the same way: `.cc`, `.cpp` and `.cxx` sources compile with `cxx`
  (default `c++`), and any C++ source makes the link use it. Set `cxxflags` in
  `[toolchain]` or a profile.
- Headers are discovered from the compiler's own dependency output, so you
  never list them. Generated headers come from a `genrule` in `deps`; see
  [Genrules and tests](../../06_manifest_spec.md#genrules-and-tests).
- Cross compiling is a `[platform.NAME]` overlay and `--platform NAME`; see
  the [manifest reference](../reference/manifest.md#platformname).
- `frost doctor` checks the toolchain and optional integrations;
  [Troubleshooting](../troubleshooting.md) covers unexpected rebuilds.
- A directory that already has C sources can start with `frost init`, which
  writes a manifest like the one above for you to review.

On Windows, set `cc = "gcc"` (MinGW) and the binary is `hello.exe`; the rest
of the tutorial is the same, with `copy`/PowerShell edits in place of `sed`.
