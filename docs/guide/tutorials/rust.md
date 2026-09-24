# Tutorial: any compiler, through the command adapter (Rust)

<!-- guide-test: requires=rustc,env -->

Frost has native rules only for C and C++. Every other language goes through
one language-neutral rule, `kind = "command"`: a named tool, run directly with
an argument list — no shell in between — with declared inputs and outputs.
This tutorial uses `rustc` because it exercises every part of that rule, but
nothing here is specific to Rust: the same shape drives `javac`, `go tool
compile`, `tsc`, `protoc` or your own code generator.

You need `frost` and `rustc` on `PATH`. Cargo is not used: here Frost owns the
compiler invocations. (When a project needs crates.io dependencies or build
scripts, keep Cargo in charge behind a single `command` target instead; see
[docs/19_rust_cargo_comparison.md](../../19_rust_cargo_comparison.md).)

## 1. The sources

A library crate split across two files, and a program that uses it.

```rust file=src/greeting/lib.rs
mod style;

pub fn greeting(name: &str) -> String {
    format!("{}, {}", style::salutation(), name)
}

#[cfg(test)]
mod tests {
    #[test]
    fn greets_by_name() {
        let text = super::greeting("test");
        assert!(text.ends_with(", test"), "unexpected greeting: {text}");
    }
}
```

```rust file=src/greeting/style.rs
pub fn salutation() -> &'static str {
    "hello"
}
```

```rust file=src/main.rs
fn main() {
    let name = std::env::args().nth(1).unwrap_or_else(|| "frost".to_string());
    println!("{}", greeting::greeting(&name));
}
```

## 2. The manifest

```toml file=frost.toml
[workspace]
default_targets = ["hello"]

[toolchain.tools]
rustc = "rustc"

[target.greeting]
kind = "command"
tool = "rustc"
inputs = ["src/greeting/**/*.rs"]
outputs = [".frost/out/${config}/libgreeting.rlib"]
args = [
  "--edition=2021", "--crate-type=rlib", "--crate-name=greeting",
  "-o", "${out}",
  "src/greeting/lib.rs",
]

[target.hello]
kind = "command"
tool = "rustc"
inputs = ["src/main.rs"]
deps = ["greeting"]
outputs = [".frost/out/${config}/hello"]
args = [
  "--edition=2021", "--crate-type=bin", "--crate-name=hello",
  "--extern", "greeting=${dep:greeting}",
  "-o", "${out}",
  "${in}",
]
```

The parts of a command target:

- `tool` names an entry in `[toolchain.tools]`. Frost resolves it on `PATH`
  once, fingerprints the executable, and puts that fingerprint in every
  action's cache key — upgrading `rustc` rebuilds everything it compiled.
- `args` is the argument list, passed exactly as written: no shell, no word
  splitting, no quoting to get wrong. `${out}`, `${in}` and `${dep:greeting}`
  are substituted by Frost; the
  [reference](../reference/manifest.md#substitutions) lists all of them.
- `inputs` is what the action reads. A glob covers every module of the crate,
  so a new `mod` file is picked up without editing the manifest; the crate
  root is still named explicitly in `args`, because `${in}` would pass every
  module to `rustc` as a crate root.
- `outputs` must contain `${config}` (the profile, or platform/profile), so a
  debug build and a release build can never overwrite each other's files.
- `${dep:greeting}` is the one output of a target this one depends on, so
  `hello` never repeats where the library is written.

Frost clears the environment of every action except a small baseline (`PATH`,
`HOME` and the temporary-directory variables), so a variable that changes what
the compiler produces must be declared with `env` or `pass_env` — where it
also becomes part of the cache key.

## 3. Build and run

```sh
frost build
"$(frost info output_dir)/hello"
```

```text output
RUN greeting [rustc]
RUN hello [rustc]
frost: 2 built · 2 actions
hello, frost
```

A second build does nothing:

```sh
frost build
```

```text output
frost: up to date
```

## 4. What a change reruns

Edit a module of the library. `greeting` reruns because one of its inputs
changed; `hello` reruns because its input — the library's output — changed.
`--explain` says exactly that:

```sh
sed -i.bak 's/"hello"/"hi"/' src/greeting/style.rs && rm src/greeting/style.rs.bak
frost build --explain
"$(frost info output_dir)/hello"
```

```text output
ran command:greeting :: input changed: src/greeting/style.rs
ran command:hello :: input changed: .frost/out/debug/libgreeting.rlib
hi, frost
```

Edit only the program, and the library is not touched:

```sh
echo '// the entry point' >> src/main.rs
frost build --explain
```

```text output
cached command:greeting
ran command:hello :: input changed: src/main.rs
```

## 5. Tests through the same rule

`rustc --test` builds the crate's unit tests into a program, and a `test`
target runs it. That program is an output of the build, not an installed tool,
so the test's `tool` is `env`, which starts the program named by its first
argument; `${dep:greeting_tests}` supplies the path.

The complete manifest, with the two new targets and the `run` tool:

```toml file=frost.toml
[workspace]
default_targets = ["hello"]

[toolchain.tools]
rustc = "rustc"
run = "env"

[target.greeting]
kind = "command"
tool = "rustc"
inputs = ["src/greeting/**/*.rs"]
outputs = [".frost/out/${config}/libgreeting.rlib"]
args = [
  "--edition=2021", "--crate-type=rlib", "--crate-name=greeting",
  "-o", "${out}",
  "src/greeting/lib.rs",
]

[target.hello]
kind = "command"
tool = "rustc"
inputs = ["src/main.rs"]
deps = ["greeting"]
outputs = [".frost/out/${config}/hello"]
args = [
  "--edition=2021", "--crate-type=bin", "--crate-name=hello",
  "--extern", "greeting=${dep:greeting}",
  "-o", "${out}",
  "${in}",
]

[target.greeting_tests]
kind = "command"
tool = "rustc"
inputs = ["src/greeting/**/*.rs"]
outputs = [".frost/out/${config}/greeting-tests"]
args = [
  "--edition=2021", "--test", "--crate-name=greeting",
  "-o", "${out}",
  "src/greeting/lib.rs",
]

[target.greeting_test]
kind = "test"
tool = "run"
args = ["${dep:greeting_tests}"]
deps = ["greeting_tests"]
```

```sh
frost test --all
```

```text output
RUN greeting_tests [rustc]
TEST greeting_test
tests: 1 passed, 0 failed, 0 cached
```

Adding the `run` tool changed the toolchain fingerprint, which is part of
every action's key, so the next `frost build` recompiles `greeting` and
`hello` once even though their sources did not change.

Break the greeting's format and the test fails, replaying the harness's own
message:

```sh fails
sed -i.bak 's/"{}, {}"/"{} {}"/' src/greeting/lib.rs && rm src/greeting/lib.rs.bak
frost test --all
```

```text output
unexpected greeting: hi test
tests: 0 passed, 1 failed, 0 cached
```

Put it back:

```sh
sed -i.bak 's/"{} {}"/"{}, {}"/' src/greeting/lib.rs && rm src/greeting/lib.rs.bak
frost test --all
```

## Where to go next

- The same rule shape is exercised end to end in CI with `go`, `javac`, `tsc`
  and Python; see [docs/10_language_adapters.md](../../10_language_adapters.md).
- `depfile` lets a compiler report the files it actually read, so they join
  the action's key without being declared; `output_dirs` handles tools that
  name their outputs by content (bundlers, `tsc --outDir`); `steps` runs
  several tools as one atomic action; `clean_dirs` gives a tool a scratch
  directory emptied before every run. All of them are in the
  [manifest reference](../reference/manifest.md).
- `frost doctor` shows which tools resolved, and from where.
