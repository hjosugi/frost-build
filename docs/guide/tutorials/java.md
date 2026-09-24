# Tutorial: Java

<!-- guide-test: requires=javac,java dir=hello -->

From an empty directory to a runnable JAR and a passing test, with Frost
running `javac` itself — no Gradle or Maven. This is the right shape when your
code has no third-party dependencies to resolve, or when you vendor them; when
a build tool must resolve dependencies, wrap it instead (see
[docs/29_sample_workspaces.md](../../29_sample_workspaces.md)).

You need `frost` and a JDK (`javac` and `java`) on `PATH`. The tutorial starts
in an empty directory named `hello`, because `frost init` names the target
after its directory:

```sh skip
mkdir hello && cd hello
```

## 1. The sources

The standard Maven/Gradle layout works unchanged; Frost does not care where
sources live, only that the manifest names them.

```java file=src/main/java/com/example/hello/Greeting.java
package com.example.hello;

public final class Greeting {
    private Greeting() {}

    public static String of(String name) {
        return "hello, " + name;
    }
}
```

```java file=src/main/java/com/example/hello/App.java
package com.example.hello;

public final class App {
    public static void main(String[] args) {
        System.out.println(Greeting.of(args.length > 0 ? args[0] : "frost"));
    }
}
```

## 2. Let `frost init` write the manifest

`frost init` looks at what is in the directory and writes the smallest build it
can describe without guessing. Preview it first:

```sh
frost init --dry-run
```

```text output
kind = "command"
tool = "javac"
"--main-class", "com.example.hello.App"
```

Then write it. `init` also pins this workspace to the frost version that wrote
it, with a `./frostw` wrapper anyone can use without installing frost:

```sh
frost init
```

```text output
2 Java source file(s)
entry point: com.example.hello.App
frost: pinned this workspace to frost
```

The manifest it wrote is one `command` target: `javac` compiles every source
into a directory Frost empties before each run (`clean_dirs`, so a deleted
class cannot survive into the JAR), and a second step packs that directory
into a deterministic JAR with `frost pack-jar`. Identical inputs give
byte-identical JARs, which is what lets everything downstream stay cached.

## 3. Build and run

```sh
frost build
java -jar .frost/out/debug/hello.jar
```

```text output
RUN hello [javac]
frost: packed 2 files -> .frost/out/debug/hello.jar
hello, frost
```

`frost run` finds the JAR itself and runs it with `java`:

```sh
frost run hello -- tutorial
```

```text output
runtime   Java
hello, tutorial
```

A second build does nothing:

```sh
frost build
```

```text output
frost: up to date
```

## 4. A test

Frost does not ship a Java test framework, and does not need one: a test is
any program whose exit status is its verdict. This one uses a plain `main`, so
the tutorial needs nothing but the JDK; a JUnit console launcher works the
same way once you have its JAR.

```java file=src/test/java/com/example/hello/GreetingTest.java
package com.example.hello;

public final class GreetingTest {
    public static void main(String[] args) {
        String actual = Greeting.of("test");
        if (!actual.equals("hello, test")) {
            System.err.println("unexpected greeting: " + actual);
            System.exit(1);
        }
        System.out.println("GreetingTest passed");
    }
}
```

Now the manifest, with two targets added: `hello_tests` compiles the main and
test sources into a test JAR whose entry point is the test, and `hello_test`
runs it. The `hello` target is exactly what `init` wrote; the comment is
trimmed.

```toml file=frost.toml
[workspace]
default_targets = ["hello"]

[toolchain.tools]
javac = "javac"
frost = "frost"
java = "java"

[target.hello]
kind = "command"
tool = "javac"
args = ["-encoding", "UTF-8", "-g", "-d", "${clean_dir}", "${in}"]
inputs = ["src/main/java/com/example/hello/App.java", "src/main/java/com/example/hello/Greeting.java"]
outputs = [".frost/out/${config}/hello.jar"]
clean_dirs = [".frost/tmp/${config}/java/hello"]
steps = [{ tool = "frost", args = ["pack-jar", "--input", "${clean_dir}", "--output", "${out}", "--main-class", "com.example.hello.App"] }]
pass_env = ["JAVA_HOME"]
sandbox = false

[target.hello_tests]
kind = "command"
tool = "javac"
args = ["-encoding", "UTF-8", "-g", "-d", "${clean_dir}", "${in}"]
inputs = ["src/main/java/**/*.java", "src/test/java/**/*.java"]
outputs = [".frost/out/${config}/hello-tests.jar"]
clean_dirs = [".frost/tmp/${config}/java/hello-tests"]
steps = [{ tool = "frost", args = ["pack-jar", "--input", "${clean_dir}", "--output", "${out}", "--main-class", "com.example.hello.GreetingTest"] }]
pass_env = ["JAVA_HOME"]
sandbox = false

[target.hello_test]
kind = "test"
tool = "java"
args = ["-jar", "${dep:hello_tests}"]
deps = ["hello_tests"]
pass_env = ["JAVA_HOME"]
sandbox = false
```

A few things worth noticing:

- `inputs` accepts globs, so a new test class is picked up without editing the
  manifest. `init` wrote an explicit list; either is fine.
- `${dep:hello_tests}` is the one output of a target this one depends on. The
  test never spells out where that JAR lives, so the layout can change without
  breaking it.
- `pass_env = ["JAVA_HOME"]` passes that one variable through (Frost clears
  the rest) and makes its value part of the cache key, so switching JDKs
  rebuilds instead of mixing class files from two compilers.
- `sandbox = false` because `javac` reads a JDK outside the workspace.

Build, then run every test. `hello` rebuilds as well, although none of its
inputs changed: the whole toolchain — every `[toolchain]` driver and every
`[toolchain.tools]` entry — is one fingerprint in every action's cache key, so
adding `java` counts as a toolchain change.

```sh
frost build --explain
frost test --all
```

```text output
ran command:hello :: command or toolchain changed
RUN hello_tests [javac]
TEST hello_test
tests: 1 passed, 0 failed, 0 cached
```

The test result is cached. Change the code it tests and it runs again; a
failure fails the command and replays what the test printed:

```sh fails
sed -i.bak 's/"hello, "/"goodbye, "/' src/main/java/com/example/hello/Greeting.java
rm src/main/java/com/example/hello/Greeting.java.bak
frost test --all
```

```text output
unexpected greeting: goodbye, test
```

Restoring the source recompiles the test JAR, and because `pack-jar` is
deterministic the JAR comes out byte-identical to the one that already passed,
so the verdict is reused rather than run again:

```sh
sed -i.bak 's/"goodbye, "/"hello, "/' src/main/java/com/example/hello/Greeting.java
rm src/main/java/com/example/hello/Greeting.java.bak
frost test --all
```

```text output
tests: 0 passed, 0 failed, 1 cached
```

## 5. Why did that rebuild?

`--explain` names every action that ran and the input that made it run:

```sh
touch src/main/java/com/example/hello/App.java
frost build --explain
```

```text output
frost: up to date
```

Touching a file without changing it rebuilds nothing — the cache key is the
content, not the timestamp.

```sh
echo '// a comment' >> src/main/java/com/example/hello/App.java
frost build --explain
```

```text output
ran command:hello :: input changed: src/main/java/com/example/hello/App.java
```

## Where to go next

- A multi-module layout, where one module's JAR is on the next one's
  classpath through `${dep:LABEL}`, is the checked-in
  [`sample_java`](../../../sample_java/frost.toml).
- `frost debug hello` starts `jdb` on the JAR; `frost ide hello` writes VS Code
  launch configuration.
- Every key used above is in the [manifest reference](../reference/manifest.md),
  and every command in the [command reference](../reference/cli.md).
- When Gradle or Maven must stay in charge, one `command` target runs it with
  declared inputs and an owned output directory; see
  [docs/17_java_gradle_maven_comparison.md](../../17_java_gradle_maven_comparison.md)
  for what that costs and saves.
