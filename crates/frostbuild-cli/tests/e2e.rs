//! End-to-end tests driving the real `frost` binary against the sample_c
//! workspace with the host C compiler.

use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(unix)]
use std::process::Stdio;

fn normalized_output(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

/// Can a class `javac` produces be run by `java` on this host?
///
/// A host can have a newer compiler than runtime on `PATH` — the macOS CI image
/// does — and then every class these tests compile fails to load with
/// `UnsupportedClassVersionError`. That is a property of the host, not of frost,
/// so the Java cases skip. The check compiles and runs a class rather than
/// comparing `-version` strings, because those differ per vendor and the
/// property that matters is exactly this one.
fn java_toolchain_is_consistent() -> bool {
    let dir = std::env::temp_dir().join(format!("frost-java-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let source = dir.join("Probe.java");
    let written = std::fs::write(
        &source,
        "public final class Probe { public static void main(String[] a) {} }\n",
    );
    let ok = written.is_ok()
        && Command::new("javac")
            .arg("-d")
            .arg(&dir)
            .arg(&source)
            .output()
            .is_ok_and(|out| out.status.success())
        && Command::new("java")
            .arg("-cp")
            .arg(&dir)
            .arg("Probe")
            .output()
            .is_ok_and(|out| out.status.success());
    let _ = std::fs::remove_dir_all(&dir);
    ok
}

/// Can this host's `rustc` complete a link with the current PATH?
///
/// Windows images can expose both MSVC rustc and a GNU `link.exe`; a version
/// probe succeeds in that state, but the linker selected by rustc cannot.
fn rust_toolchain_is_consistent() -> bool {
    let dir = std::env::temp_dir().join(format!("frost-rust-probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    if std::fs::create_dir_all(&dir).is_err() {
        return false;
    }
    let source = dir.join("probe.rs");
    let output = executable_path(dir.join("probe"));
    let ok = std::fs::write(&source, "fn main() {}\n").is_ok()
        && Command::new("rustc")
            .arg(&source)
            .arg("-o")
            .arg(&output)
            .output()
            .is_ok_and(|result| result.status.success());
    let _ = std::fs::remove_dir_all(&dir);
    ok
}

fn frost_bin() -> &'static str {
    env!("CARGO_BIN_EXE_frost")
}

fn executable_path(path: impl AsRef<Path>) -> PathBuf {
    let mut native = path.as_ref().as_os_str().to_os_string();
    native.push(std::env::consts::EXE_SUFFIX);
    native.into()
}

struct Workspace {
    dir: PathBuf,
}

impl Workspace {
    fn new(name: &str) -> Self {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample_c");
        let workspace = Self::empty(name);
        copy_dir(&src, &workspace.dir).expect("copy sample_c");
        workspace
    }

    /// The multi-package sample: four packages, four target kinds, and a
    /// diamond, which is the shape the graph queries need and `sample_c`'s
    /// single line of dependencies cannot provide.
    fn multi(name: &str) -> Self {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample_multi");
        let workspace = Self::empty(name);
        copy_dir(&src, &workspace.dir).expect("copy sample_multi");
        workspace
    }

    fn empty(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("frost-e2e-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create empty workspace");
        Self { dir }
    }

    fn frost(&self, args: &[&str]) -> (bool, String) {
        let out = Command::new(frost_bin())
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .expect("spawn frost");
        let text = normalized_output(&out.stdout) + &normalized_output(&out.stderr);
        (out.status.success(), text)
    }

    /// The exit code, not just whether it was zero. The contract in docs/28
    /// distinguishes 1 ("your code") from 2 ("your invocation"), and a bool
    /// cannot tell those apart.
    fn frost_code(&self, args: &[&str]) -> (i32, String) {
        let out = Command::new(frost_bin())
            .arg("-C")
            .arg(&self.dir)
            .args(args)
            .output()
            .expect("spawn frost");
        let text = normalized_output(&out.stdout) + &normalized_output(&out.stderr);
        (out.status.code().unwrap_or(-1), text)
    }

    fn frost_env(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
        let mut command = Command::new(frost_bin());
        command.arg("-C").arg(&self.dir).args(args);
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output().expect("spawn frost");
        (
            out.status.success(),
            normalized_output(&out.stdout) + &normalized_output(&out.stderr),
        )
    }

    // `script -q -e -c` is util-linux syntax. macOS ships the BSD tool, whose
    // arguments differ, so the pseudo-terminal cases run on Linux; every other
    // test runs on every host. See docs/09_platform_support.md.
    #[cfg(target_os = "linux")]
    fn frost_pty(&self, args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
        let command_line = pty_command_line(&self.dir, args);
        let mut command = Command::new("script");
        command
            // `-- <command> <args...>` was added to newer util-linux
            // releases. Ubuntu's CI image still requires the long-standing
            // `-c <command>` form.
            .args(["-q", "-e", "-c"])
            .arg(command_line)
            .arg("/dev/null");
        if !env.iter().any(|(key, _)| *key == "CI") {
            // GitHub Actions sets CI for the test harness itself. Positive TTY
            // cases must model an interactive user, while the dedicated CI
            // case below explicitly puts it back.
            command.env_remove("CI");
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let out = command.output().expect("spawn frost in a pseudo-terminal");
        (
            out.status.success(),
            normalized_output(&out.stdout) + &normalized_output(&out.stderr),
        )
    }

    fn build_explain(&self) -> (bool, String) {
        self.frost(&["build", "--explain"])
    }

    fn write(&self, rel: &str, content: &str) {
        std::fs::write(self.dir.join(rel), content).unwrap();
    }

    fn append(&self, rel: &str, content: &str) {
        let path = self.dir.join(rel);
        let mut text = std::fs::read_to_string(&path).unwrap();
        text.push_str(content);
        std::fs::write(&path, text).unwrap();
    }

    fn generator_script(&self) -> &'static str {
        if cfg!(windows) {
            "tools/gen_config.cmd"
        } else {
            "tools/gen_config"
        }
    }

    fn binary(&self, relative_without_suffix: &str) -> PathBuf {
        executable_path(self.dir.join(relative_without_suffix))
    }

    fn run_app(&self) -> String {
        let out = Command::new(self.binary(".frost/bin/debug/app"))
            .output()
            .expect("run built app");
        assert!(out.status.success(), "built app should run");
        normalized_output(&out.stdout)
    }
}

#[test]
fn host_portable_command_target_builds_and_caches() {
    let ws = Workspace::empty("host-command");
    #[cfg(unix)]
    let (shell, shell_arg, command) = ("/bin/sh", "-c", "printf host-ok > ${config}/host.txt");
    #[cfg(windows)]
    let (shell, shell_arg, command) = ("cmd.exe", "/C", "echo host-ok>${config}/host.txt");
    ws.write(
        "frost.toml",
        &format!(
            r#"[workspace]
default_targets = ["smoke"]

[toolchain]
cc = "{shell}"
cxx = "{shell}"
ar = "{shell}"

[toolchain.tools]
host = "{shell}"

[target.smoke]
kind = "command"
tool = "host"
args = ["{shell_arg}", "{command}"]
outputs = ["${{config}}/host.txt"]
"#
        ),
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(
        ok && out.contains("1 built"),
        "portable build failed:\n{out}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.dir.join("debug/host.txt"))
            .unwrap()
            .trim(),
        "host-ok"
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(
        ok && (out.contains("1 cached") || out.contains("up to date")),
        "portable no-op failed:\n{out}"
    );
    let (ok, out) = ws.frost(&["cache", "stats", "--json"]);
    assert!(ok, "cache stats failed:\n{out}");
    let stats: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(stats["object_count"], 1);
    assert!(stats["chunk_reuse_ratio"].is_number());
}

#[test]
fn changing_declared_output_set_invalidates_cache() {
    let ws = Workspace::empty("command-output-set");
    #[cfg(unix)]
    let (shell, shell_arg, command) = (
        "/bin/sh",
        "-c",
        "mkdir -p ${config}; printf one > ${config}/one.txt; printf two > ${config}/two.txt",
    );
    #[cfg(windows)]
    // No `if not exist` guard. cmd binds the rest of the line to the if-branch,
    // so with the output parent already present the whole chain was skipped and
    // the action "succeeded" having written nothing. frost creates the parent of
    // every declared output, so the guard was never needed.
    let (shell, shell_arg, command) = (
        "cmd.exe",
        "/C",
        "echo one>${config}/one.txt & echo two>${config}/two.txt",
    );
    let manifest = |outputs: &str| {
        format!(
            r#"[workspace]
default_targets = ["producer"]

[toolchain]
cc = "{shell}"
cxx = "{shell}"
ar = "{shell}"

[toolchain.tools]
producer = "{shell}"

[target.producer]
kind = "command"
tool = "producer"
args = ["{shell_arg}", "{command}"]
outputs = {outputs}
"#
        )
    };

    ws.write("frost.toml", &manifest(r#"["${config}/one.txt"]"#));
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "initial command build failed:\n{out}");

    let second = ws.dir.join("debug/two.txt");
    assert!(
        second.is_file(),
        "the command should create both test files"
    );
    std::fs::remove_file(&second).unwrap();
    ws.write(
        "frost.toml",
        &manifest(r#"["${config}/one.txt", "${config}/two.txt"]"#),
    );

    let (ok, out) = ws.frost(&["build", "--explain"]);
    assert!(ok, "output-set rebuild failed:\n{out}");
    assert!(
        !out.contains("up to date"),
        "a changed declared output set must not reuse the old action result:\n{out}"
    );
    assert!(
        second.is_file(),
        "the command must rerun and recreate the newly declared output"
    );
}

#[test]
// The command has to name a file after the contents of another file, which is
// what makes the output set unpredictable. Expressing that in `cmd.exe` adds
// nothing to what is being tested; see docs/09_platform_support.md.
#[cfg(unix)]
fn command_target_owns_an_output_directory_it_cannot_name_in_advance() {
    let ws = Workspace::empty("output-dirs");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["report"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.web]
kind = "command"
tool = "sh"
args = ["-c", "mkdir -p dist/${config}/assets; printf built > dist/${config}/$(cat src/name.txt).js; printf shared > dist/${config}/assets/common.css"]
inputs = ["src/name.txt", "src/version.txt"]
output_dirs = ["dist/${config}"]

[target.report]
kind = "command"
tool = "sh"
args = ["-c", "ls dist/${config} | tr '\n' ' ' > ${out}"]
deps = ["web"]
outputs = [".frost/out/${config}/report.txt"]
"#,
    );
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write("src/name.txt", "alpha");
    // Declared, but the command does not read it: changing it reruns the
    // command without changing the tree it produces.
    ws.write("src/version.txt", "1");

    let bundle = ws.dir.join("dist/debug");
    let report = ws.dir.join(".frost/out/debug/report.txt");
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "tree output build failed:\n{out}");
    assert_eq!(read(&bundle.join("alpha.js")), "built");
    assert_eq!(read(&bundle.join("assets/common.css")), "shared");
    assert!(
        read(&report).contains("alpha.js"),
        "the dependent must observe the tree: {}",
        read(&report)
    );
    let stamp = ws.dir.join(".frost/tree/debug/web/contents");
    assert!(
        stamp.is_file(),
        "the tree stamp is the graph node for the dir"
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // A file missing from an owned directory is restored from the CAS, exactly
    // as a missing declared output is.
    std::fs::remove_file(bundle.join("alpha.js")).unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");
    assert_eq!(read(&bundle.join("alpha.js")), "built", "restored from CAS");

    // A rebuild republishes the whole tree, so nothing from the previous run
    // and nothing frost never recorded survives into it.
    std::fs::write(bundle.join("stray.js"), "not mine").unwrap();
    ws.write("src/name.txt", "beta");
    let (ok, out) = ws.frost(&["build", "--explain"]);
    assert!(ok, "tree rebuild failed:\n{out}");
    assert_eq!(read(&bundle.join("beta.js")), "built");
    assert!(
        !bundle.join("alpha.js").exists(),
        "the previous run's file must not survive the republished tree"
    );
    assert!(
        !bundle.join("stray.js").exists(),
        "an undeclared file in an owned directory must not survive it either"
    );
    assert!(
        read(&report).contains("beta.js"),
        "the tree stamp must invalidate the dependent: {}",
        read(&report)
    );

    // A rerun that reproduces the same tree produces the same stamp, so the
    // dependent is cut off rather than rebuilt: early cutoff on a tree works
    // exactly as it does on a single file.
    ws.write("src/version.txt", "2");
    let (ok, out) = ws.frost(&["build", "--explain"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("RUN web"),
        "the changed input must rerun the producer:\n{out}"
    );
    assert!(
        !out.contains("RUN report"),
        "an identical tree must cut the dependent off:\n{out}"
    );
}

#[test]
fn this_repository_and_its_samples_pass_their_own_lint_and_fmt() {
    // A rule that is not run against a real manifest is a rule nobody has
    // checked. Both false positives this rule set shipped with -- a generated
    // include directory reported as missing, and a `cc_test` reported as
    // unreachable -- were found by pointing it at these exact files.
    //
    // A test rather than a stage in `frost.toml`: the gate must not depend on
    // the binary it is gating, which is the same reason `scripts/check.sh`
    // exists.
    let repo = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut offenders = Vec::new();
    for workspace in [
        ".",
        "sample_c",
        "sample_multi",
        "sample_java",
        "sample_spring",
        "sample_maven",
    ] {
        let root = repo.join(workspace);
        if !root.join("frost.toml").is_file() {
            continue;
        }
        let manifest = frostbuild_core::manifest::Manifest::load(&root)
            .unwrap_or_else(|error| panic!("{workspace} failed to load: {error:#}"));
        for finding in frostbuild_core::lint::lint(&manifest, &root) {
            offenders.push(format!(
                "{workspace}: {} {} ({})",
                finding.target, finding.message, finding.rule
            ));
        }
        // And canonically formatted. Kept in one test because the answer to
        // both is the same edit and the same file list.
        let mut manifests = vec![root.join("frost.toml")];
        manifests.extend(frostbuild_core::manifest::package_manifests(&root).unwrap());
        for path in manifests {
            let text = std::fs::read_to_string(&path).unwrap();
            if !frostbuild_core::fmt::is_formatted(&text).unwrap() {
                offenders.push(format!("{workspace}: {} is not canonical", path.display()));
            }
        }
    }
    assert_eq!(
        offenders,
        Vec::<String>::new(),
        "run `frost fmt`, fix the manifest, or record the cost with lint_allow"
    );
}

#[test]
fn query_targets_answers_what_is_in_this_workspace() {
    let ws = Workspace::multi("query-targets");

    // The one query with no starting point. `deps` and `rdeps` both need a
    // target to walk from, which is exactly why this question needed its own
    // primitive rather than a walk from roots derived out of `--output dot`.
    let (ok, out) = ws.frost(&["query", "targets", "--output", "label-kind"]);
    assert!(ok, "{out}");
    let lines: Vec<&str> = out
        .lines()
        .filter(|line| line.contains(" target "))
        .collect();
    assert!(lines.contains(&"cc_binary target //apps/cli:cli"), "{out}");
    assert!(lines.contains(&"cc_test target //core:core_test"), "{out}");
    assert!(lines.contains(&"genrule target gen_version"), "{out}");

    // Sorted, because a listing whose order changes between runs cannot be
    // diffed and cannot back a stable tree view.
    let labels: Vec<&str> = lines
        .iter()
        .filter_map(|line| line.split(" target ").nth(1))
        .collect();
    let mut sorted = labels.clone();
    sorted.sort_unstable();
    assert_eq!(labels, sorted, "target listing must be ordered:\n{out}");

    // It shares QueryOpts, so every filter and format the other functions have
    // works here without a second implementation.
    let (ok, out) = ws.frost(&["query", "targets", "--kind", "cc_test"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.lines().filter(|l| l.starts_with("//")).count(),
        1,
        "{out}"
    );

    let (ok, out) = ws.frost(&["query", "targets", "--json"]);
    assert!(ok, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(&out).expect("json");
    assert_eq!(parsed["query"], "targets()");
    assert!(
        parsed["targets"].as_array().is_some_and(|t| t.len() >= 6),
        "{out}"
    );
}

#[test]
// POSIX shell command text; see docs/09_platform_support.md.
#[cfg(unix)]
fn journal_export_is_stable_and_diff_names_the_cause() {
    let ws = Workspace::empty("journal-forensics");
    let manifest = |extra_env: &str, tool: &str| {
        format!(
            r#"[workspace]
default_targets = ["pack"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
packer = "{tool}"

[target.pack]
kind = "command"
tool = "packer"
args = ["-c", "cat src.txt > ${{out}}"]
inputs = ["src.txt"]
outputs = [".frost/out/${{config}}/packed.txt"]
env = {{ LEVEL = "one"{extra_env} }}
sandbox = false
"#
        )
    };
    ws.write("frost.toml", &manifest("", "/bin/sh"));
    ws.write("src.txt", "contents");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");

    let export = |name: &str| {
        let path = ws.dir.join(name);
        let (ok, out) = ws.frost(&["journal", "export", "--out", path.to_str().unwrap()]);
        assert!(ok, "export failed:\n{out}");
        path
    };
    let first = export("first.json");
    let again = export("again.json");

    // Nothing changed, so the bytes must not either. A HashMap anywhere in the
    // structure would break this at random, which is the failure this pins.
    assert_eq!(
        std::fs::read_to_string(&first).unwrap(),
        std::fs::read_to_string(&again).unwrap(),
        "two exports of one build must be byte-identical"
    );
    let (ok, out) = ws.frost(&[
        "journal",
        "diff",
        first.to_str().unwrap(),
        again.to_str().unwrap(),
    ]);
    assert!(ok && out.contains("identical"), "{out}");

    // An input difference names the file that moved.
    ws.write("src.txt", "different contents");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let input_changed = export("input.json");
    let (ok, out) = ws.frost(&[
        "journal",
        "diff",
        first.to_str().unwrap(),
        input_changed.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("inputs:"), "{out}");
    assert!(out.contains("src.txt"), "the file that moved:\n{out}");

    // An environment difference is reported as env, not as the input and
    // output digests it also changes.
    ws.write("frost.toml", &manifest(r#", EXTRA = "yes""#, "/bin/sh"));
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let env_changed = export("env.json");
    let (ok, out) = ws.frost(&[
        "journal",
        "diff",
        input_changed.to_str().unwrap(),
        env_changed.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("env:"), "{out}");
    assert!(out.contains("EXTRA"), "the variable that appeared:\n{out}");

    // A toolchain difference is a property of the whole build, so it is
    // reported once instead of once per action. A copy of the same shell at a
    // different path is enough: the fingerprint covers which binary frost
    // would invoke, not merely what it does. Copying rather than naming a
    // second interpreter keeps this from silently skipping on a host that
    // happens not to ship one.
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    std::fs::copy("/bin/sh", ws.dir.join("tools/sh")).expect("copy the shell");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    ws.write("frost.toml", &manifest(r#", EXTRA = "yes""#, "tools/sh"));
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let tool_changed = export("toolchain.json");
    let (ok, out) = ws.frost(&[
        "journal",
        "diff",
        env_changed.to_str().unwrap(),
        tool_changed.to_str().unwrap(),
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("toolchain:"), "{out}");
    assert!(
        out.contains("1 difference"),
        "a build-wide change must be reported alone, not per action:\n{out}"
    );

    // A format frost does not know is refused rather than compared field by
    // field against fields that may mean something else.
    let text = std::fs::read_to_string(&first)
        .unwrap()
        .replace("frost-journal-export-v1", "frost-journal-export-v0");
    ws.write("stale.json", &text);
    let (code, out) = ws.frost_code(&[
        "journal",
        "diff",
        first.to_str().unwrap(),
        ws.dir.join("stale.json").to_str().unwrap(),
    ]);
    assert_eq!(
        code, 2,
        "an unreadable input is an invocation error:\n{out}"
    );
    assert!(out.contains("cannot be compared"), "{out}");
}

#[test]
#[cfg(unix)]
fn a_mistyped_target_is_answered_with_candidates() {
    let ws = Workspace::multi("diagnostics-target");
    // A workspace names its targets by label, and the package the author
    // already typed is almost always the one they meant.
    let (ok, out) = ws.frost(&["build", "//apps/cli:cl"]);
    assert!(!ok, "{out}");
    assert!(out.contains("unknown target"), "{out}");
    assert!(
        out.contains("//apps/cli:cli"),
        "the near label must be offered:\n{out}"
    );

    // Nothing similar is not answered with a wrong guess.
    let (ok, out) = ws.frost(&["build", "//apps/cli:zzzzzzzzzz"]);
    assert!(!ok, "{out}");
    assert!(!out.contains("did you mean"), "{out}");
}

#[test]
#[cfg(unix)]
fn a_missing_tool_says_where_it_looked_and_who_needed_it() {
    let ws = Workspace::empty("diagnostics-tool");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["packager"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
mytool = "frost-e2e-definitely-not-installed"

[target.packager]
kind = "command"
tool = "mytool"
inputs = ["a.txt"]
outputs = [".frost/out/${config}/o.txt"]
args = ["x"]
"#,
    );
    ws.write("a.txt", "one");

    let (code, out) = ws.frost_code(&["build"]);
    // The three questions someone actually has, in order.
    assert!(out.contains("frost-e2e-definitely-not-installed"), "{out}");
    assert!(
        out.contains("[toolchain.tools].mytool"),
        "which line to go and look at:\n{out}"
    );
    assert!(out.contains("PATH"), "where it looked:\n{out}");
    assert!(
        out.contains("required by packager"),
        "which target breaks:\n{out}"
    );
    assert!(out.contains("frost doctor"), "what to do next:\n{out}");
    // A workspace frost cannot run as asked, not a build that ran and failed.
    assert_eq!(code, 2, "{out}");
}

#[test]
#[cfg(unix)]
fn exit_codes_separate_your_code_from_your_invocation() {
    // docs/28 promises 0 / 1 / 2 mean three different things. The existing
    // unit test checks the document says so; this checks frost does.
    let ws = Workspace::empty("diagnostics-exit-codes");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["ok"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.ok]
kind = "test"
tool = "sh"
args = ["-c", "true"]
inputs = ["a.txt"]
sandbox = false

[target.broken]
kind = "test"
tool = "sh"
args = ["-c", "exit 1"]
inputs = ["a.txt"]
sandbox = false
"#,
    );
    ws.write("a.txt", "one");

    // 0: the requested work completed.
    let (code, out) = ws.frost_code(&["test", "ok"]);
    assert_eq!(code, 0, "{out}");

    // 1: the work ran and did not succeed. This is an answer about your code.
    let (code, out) = ws.frost_code(&["test", "broken"]);
    assert_eq!(code, 1, "{out}");

    // 2: frost could not run the work as asked. An answer about your
    // invocation, which is the distinction a script needs.
    let (code, out) = ws.frost_code(&["test", "nonexistent-target"]);
    assert_eq!(code, 2, "{out}");
    let (code, out) = ws.frost_code(&["--not-a-flag"]);
    assert_eq!(code, 2, "{out}");

    // A manifest frost cannot read is also an invocation problem, not a
    // failing build.
    ws.write("frost.toml", "[target.app\n");
    let (code, out) = ws.frost_code(&["build"]);
    assert_eq!(code, 2, "{out}");
    assert!(out.contains("frost.toml:1:"), "with the position:\n{out}");
}

#[test]
// POSIX shell command text; see docs/09_platform_support.md.
#[cfg(unix)]
fn runs_per_test_repeats_the_test_and_refuses_a_cached_single_pass() {
    let ws = Workspace::empty("runs-per-test");
    // Appends a line per execution, so the count is observed rather than
    // inferred from what frost says it did.
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["counted"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.counted]
kind = "test"
tool = "sh"
args = ["-c", "echo run >> runs.txt"]
inputs = ["cases.txt"]
sandbox = false
"#,
    );
    ws.write("cases.txt", "one");
    let runs = || {
        std::fs::read_to_string(ws.dir.join("runs.txt"))
            .map(|text| text.lines().count())
            .unwrap_or(0)
    };

    let (ok, out) = ws.frost(&["test"]);
    assert!(ok, "{out}");
    assert_eq!(runs(), 1);

    let (ok, out) = ws.frost(&["test"]);
    assert!(ok && out.contains("1 cached"), "{out}");
    assert_eq!(runs(), 1, "a cached test must not run");

    // The question "does this pass five times" cannot be answered by a
    // recorded single pass, so the cache is not consulted.
    let (ok, out) = ws.frost(&["test", "--runs-per-test", "5"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("1 cached"),
        "must not reuse a single pass:\n{out}"
    );
    assert_eq!(runs(), 6, "five more executions");

    // Failing on a later run is the result worth reporting: which run failed
    // separates a flake from a broken test.
    ws.write(
        "frost.toml",
        &std::fs::read_to_string(ws.dir.join("frost.toml"))
            .unwrap()
            .replace(
                r#"args = ["-c", "echo run >> runs.txt"]"#,
                r#"args = ["-c", "echo run >> runs.txt; test $(wc -l < runs.txt) -lt 9"]"#,
            ),
    );
    let (ok, out) = ws.frost(&["test", "--runs-per-test", "5"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("failed on run 3 of 5"),
        "the failing run must be named:\n{out}"
    );
}

#[test]
// POSIX shell command text; see docs/09_platform_support.md.
#[cfg(unix)]
fn test_output_modes_choose_what_reaches_the_terminal() {
    let ws = Workspace::empty("test-output-modes");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["chatty"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.chatty]
kind = "test"
tool = "sh"
args = ["-c", "echo NOISE_FROM_A_PASSING_TEST"]
inputs = ["cases.txt"]
sandbox = false
"#,
    );
    ws.write("cases.txt", "one");

    // The default hides what a passing test wrote: it is the noise that
    // buries the one failure worth reading.
    let (ok, out) = ws.frost(&["test", "--no-cache"]);
    assert!(ok, "{out}");
    assert!(!out.contains("NOISE_FROM_A_PASSING_TEST"), "{out}");
    assert!(out.contains("1 passed"), "{out}");

    let (ok, out) = ws.frost(&["test", "--no-cache", "--test-output", "all"]);
    assert!(ok && out.contains("NOISE_FROM_A_PASSING_TEST"), "{out}");

    let (ok, out) = ws.frost(&["test", "--no-cache", "--test-output", "summary"]);
    assert!(ok && !out.contains("NOISE_FROM_A_PASSING_TEST"), "{out}");
    assert!(
        out.contains("1 passed"),
        "the counts always survive:\n{out}"
    );

    // A failing test is replayed after the run, so the log that matters is the
    // last thing on screen rather than scrolled away behind later work.
    ws.write(
        "frost.toml",
        &std::fs::read_to_string(ws.dir.join("frost.toml"))
            .unwrap()
            .replace(
                "echo NOISE_FROM_A_PASSING_TEST",
                "echo WHY_IT_BROKE >&2; exit 1",
            ),
    );
    let (ok, out) = ws.frost(&["test"]);
    assert!(!ok, "{out}");
    assert!(out.contains("--- test:chatty ---"), "replay header:\n{out}");
    let replay = out.rfind("--- test:chatty ---").expect("replay");
    assert!(
        out[replay..].contains("WHY_IT_BROKE"),
        "the failing log must be in the replay:\n{out}"
    );

    // `summary` stays quiet even for a failure: the exit code is the answer
    // that mode asked for.
    let (ok, out) = ws.frost(&["test", "--test-output", "summary"]);
    assert!(!ok, "{out}");
    assert!(
        !out.contains("--- test:chatty ---"),
        "summary must not replay:\n{out}"
    );
    assert!(out.contains("1 failed"), "{out}");
}

#[test]
// POSIX shell command text; see docs/09_platform_support.md.
#[cfg(unix)]
fn command_line_test_options_are_separate_results_not_shared_ones() {
    let ws = Workspace::empty("test-options");
    // The test writes what it was told, so the assertions read what actually
    // reached the runner rather than trusting the flags were plumbed.
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["probe"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.probe]
kind = "test"
tool = "sh"
args = ["-c", "printf 'filter=%s level=%s args=%s\n' \"$TESTBRIDGE_TEST_ONLY\" \"$LEVEL\" \"$*\" > seen.txt", "sh"]
inputs = ["cases.txt"]
env = { LEVEL = "manifest" }
sandbox = false
"#,
    );
    ws.write("cases.txt", "one");
    let seen = || std::fs::read_to_string(ws.dir.join("seen.txt")).unwrap();

    let (ok, out) = ws.frost(&["test"]);
    assert!(ok, "plain run failed:\n{out}");
    assert_eq!(seen(), "filter= level=manifest args=\n");

    // Cached, because nothing about the question changed.
    let (ok, out) = ws.frost(&["test"]);
    assert!(ok && out.contains("1 cached"), "{out}");

    // The property #142 asked for: a filtered run is a different question, so
    // the unfiltered result must not answer it.
    let (ok, out) = ws.frost(&["test", "--test-filter", "parse::*"]);
    assert!(ok, "filtered run failed:\n{out}");
    assert!(
        !out.contains("1 cached"),
        "a filtered run must not reuse an unfiltered result:\n{out}"
    );
    assert_eq!(seen(), "filter=parse::* level=manifest args=\n");

    // Going back to unfiltered runs again rather than being answered from
    // cache. The journal keeps one entry per action id, so the filtered run
    // replaced the unfiltered one -- alternating between two questions always
    // re-executes. That is a cost, not a correctness problem: what must never
    // happen is being *served* the other question's answer, and it does not.
    let (ok, out) = ws.frost(&["test"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("1 cached"),
        "one journal entry per action means the filtered run evicted this one:\n{out}"
    );
    assert_eq!(seen(), "filter= level=manifest args=\n");

    // The command line overrides the manifest, and that override is keyed:
    // it runs rather than reusing the manifest-valued result.
    let (ok, out) = ws.frost(&["test", "--test-env", "LEVEL=cli"]);
    assert!(ok && !out.contains("1 cached"), "{out}");
    assert_eq!(seen(), "filter= level=cli args=\n");

    // Extra argv reaches the runner and is keyed the same way.
    let (ok, out) = ws.frost(&["test", "--test-arg", "--extra"]);
    assert!(ok && !out.contains("1 cached"), "{out}");
    assert_eq!(seen(), "filter= level=manifest args=--extra\n");

    // A malformed pair is rejected rather than becoming a variable nothing
    // can read.
    let (ok, out) = ws.frost(&["test", "--test-env", "NOEQUALS"]);
    assert!(!ok, "a KEY=VALUE without '=' must be refused:\n{out}");
    assert!(out.contains("KEY=VALUE"), "{out}");
}

#[test]
// POSIX shell command text; see docs/09_platform_support.md.
#[cfg(unix)]
fn a_flaky_test_passes_on_a_retry_and_is_reported_rather_than_cached() {
    let ws = Workspace::empty("flaky-retries");
    // Fails once per `frost` invocation, then passes: the counter file makes
    // the first attempt of each run fail and the second succeed, which is what
    // a real flake looks like from the outside.
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["sometimes"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.sometimes]
kind = "test"
tool = "sh"
args = ["-c", "if [ -f .attempted ]; then rm -f .attempted; exit 0; else touch .attempted; echo 'first attempt always fails' >&2; exit 1; fi"]
inputs = ["cases.txt"]
flaky_retries = 1
sandbox = false
"#,
    );
    ws.write("cases.txt", "one");

    let (ok, out) = ws.frost(&["test"]);
    assert!(ok, "a retry that passes must leave the build green:\n{out}");
    assert!(
        out.contains("1 flaky"),
        "the summary must name the flake rather than fold it into passed:\n{out}"
    );
    assert!(
        !out.contains("1 passed"),
        "counting a flake as a clean pass erases the only signal:\n{out}"
    );

    // The point of the feature. A cached flake would hide itself from every
    // later build, including the one that would have caught it, so the second
    // run must execute again rather than report `cached`.
    let (ok, out) = ws.frost(&["test"]);
    assert!(ok, "second run failed:\n{out}");
    assert!(
        out.contains("1 flaky") && out.contains("0 cached"),
        "a flaky success must not be cached:\n{out}"
    );
    assert!(
        out.contains("1 built"),
        "nothing was recorded, so the test must run again:\n{out}"
    );

    // And a test that fails every attempt still fails, with the count named
    // so the retries are visible rather than looking like a single run.
    let always = ws.dir.join("frost.toml");
    let text = std::fs::read_to_string(&always).unwrap().replace(
        r#""-c", "if [ -f .attempted ]; then rm -f .attempted; exit 0; else touch .attempted; echo 'first attempt always fails' >&2; exit 1; fi""#,
        r#""-c", "exit 3""#,
    );
    std::fs::write(&always, text).unwrap();
    let (ok, out) = ws.frost(&["test"]);
    assert!(!ok, "a test that never passes must fail:\n{out}");
    assert!(
        out.contains("failed all 2 attempts"),
        "the retries must be visible in the failure:\n{out}"
    );
    assert!(out.contains("1 failed"), "{out}");
}

#[test]
// POSIX shell command text; see docs/09_platform_support.md.
#[cfg(unix)]
fn a_consumer_names_its_dependency_instead_of_that_dependency_s_layout() {
    let ws = Workspace::empty("dep-references");
    // Nothing below writes `gen/`, `.frost/out/` or a profile directory by
    // hand. That is the whole claim: the producer owns where its output goes,
    // and moving it is not a breaking change for the consumers.
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["report"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.greeting]
kind = "genrule"
cmd = "printf hello > ${out}"
inputs = ["seed.txt"]
outputs = ["gen/greeting.txt"]

[target.parts]
kind = "genrule"
cmd = "printf one > gen/a.txt; printf two > gen/b.txt"
inputs = ["seed.txt"]
outputs = ["gen/a.txt", "gen/b.txt"]

# A genrule consuming another genrule, through the shell, with both forms.
[target.bundle]
kind = "genrule"
cmd = "cat ${dep:greeting} ${deps:parts} > ${out}"
deps = ["greeting", "parts"]
outputs = ["gen/bundle.txt"]

# A command consuming the same output through its environment rather than
# through argv, which is the surface a tool configured by env needs.
[target.report]
kind = "command"
tool = "sh"
args = ["-c", "printf '%s' \"$(cat $GREETING) $(cat gen/bundle.txt)\" > ${out}"]
env = { GREETING = "${dep:greeting}" }
deps = ["greeting", "bundle"]
outputs = [".frost/out/${config}/report.txt"]
"#,
    );
    ws.write("seed.txt", "1");

    let report = ws.dir.join(".frost/out/debug/report.txt");
    let read = |path: &Path| std::fs::read_to_string(path).unwrap();

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "dependency-reference build failed:\n{out}");
    assert_eq!(read(&ws.dir.join("gen/bundle.txt")), "helloonetwo");
    assert_eq!(read(&report), "hello helloonetwo");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // The point of the indirection: the producer relocates its output and no
    // consumer is edited. Both the genrule cmd and the env value follow it,
    // and because each expansion is action-key material the consumers rerun
    // rather than replaying a command naming a path that no longer exists.
    let moved = read(&ws.dir.join("frost.toml")).replace("gen/greeting.txt", "gen/text/hello.txt");
    ws.write("frost.toml", &moved);
    let (ok, out) = ws.frost(&["build", "--explain"]);
    assert!(ok, "rebuild after the producer moved failed:\n{out}");
    assert!(
        ws.dir.join("gen/text/hello.txt").is_file(),
        "the producer must write its new path:\n{out}"
    );
    assert_eq!(
        read(&report),
        "hello helloonetwo",
        "consumers still resolve"
    );
    assert!(
        out.contains("RUN report"),
        "the env reference must be action-key material:\n{out}"
    );
}

#[test]
// A shell wrapper stands in for a tool whose dependency protocol is not
// Makefile-shaped; `cmd.exe` adds nothing to what is tested here. The MSVC
// `showincludes` path is covered by unit tests until Windows CI runs the E2E
// suite (#110); see docs/09_platform_support.md.
#[cfg(unix)]
fn a_plain_path_list_depfile_tracks_undeclared_inputs() {
    let ws = Workspace::empty("depfile-lines");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["render"]

[toolchain]
cc = "/bin/sh"
cxx = "/bin/sh"
ar = "/bin/sh"

[toolchain.tools]
sh = "/bin/sh"

[target.render]
kind = "command"
tool = "sh"
args = ["-c", "cat src/page.txt src/partial.txt > ${out}; printf 'src/page.txt\nsrc/partial.txt\n' > ${depfile}"]
inputs = ["src/page.txt"]
outputs = [".frost/out/${config}/page.html"]
depfile = ".frost/out/${config}/page.deps"
depfile_format = "lines"
"#,
    );
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write("src/page.txt", "page\n");
    ws.write("src/partial.txt", "one\n");
    let page = ws.dir.join(".frost/out/debug/page.html");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "lines depfile build failed:\n{out}");
    assert_eq!(std::fs::read_to_string(&page).unwrap(), "page\none\n");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // src/partial.txt is not declared anywhere. Only the reported dependency
    // list can make this rebuild.
    ws.write("src/partial.txt", "two\n");
    let (ok, out) = ws.frost(&["build", "--explain"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("input changed: src/partial.txt"),
        "the reported dependency must be tracked:\n{out}"
    );
    assert_eq!(std::fs::read_to_string(&page).unwrap(), "page\ntwo\n");
}

#[test]
fn a_shared_cache_builds_a_cold_workspace_without_executing_anything() {
    // Two workspaces with identical sources: the second must be able to take
    // everything from the shared cache, including the outputs of actions whose
    // real inputs (headers) are only discovered by running them.
    let shared = std::env::temp_dir().join(format!("frost-remote-shared-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&shared);
    let endpoint = shared.to_str().unwrap().to_string();

    let producer = Workspace::new("remote-producer");
    let (ok, out) = producer.frost(&[
        "build",
        "--remote-cache",
        &endpoint,
        "--remote-upload",
        "--explain",
    ]);
    assert!(ok, "producing build failed:\n{out}");
    assert!(
        out.contains("remote:"),
        "the remote summary must be shown:\n{out}"
    );
    assert!(
        !out.contains("0 up ("),
        "the producing build must publish:\n{out}"
    );

    let consumer = Workspace::new("remote-consumer");
    let (ok, out) = consumer.frost(&["build", "--remote-cache", &endpoint, "--explain"]);
    assert!(ok, "consuming build failed:\n{out}");
    assert!(
        !out.contains(" ran "),
        "a cold workspace must not execute anything with a warm shared cache:\n{out}"
    );
    assert_eq!(
        consumer.run_app(),
        "frost: 42\n",
        "and must produce the binary"
    );

    // An unreachable endpoint costs speed and nothing else.
    let stale = Workspace::new("remote-unreachable");
    let (ok, out) = stale.frost(&["build", "--remote-cache", "http://127.0.0.1:1/frost"]);
    assert!(
        ok,
        "an unreachable remote cache must not fail the build:\n{out}"
    );

    // A blob that no longer hashes to its digest is refused, and the action is
    // executed instead of restoring bytes nobody can vouch for.
    let tampered = Workspace::new("remote-tampered");
    for entry in std::fs::read_dir(shared.join("cas")).unwrap() {
        std::fs::write(entry.unwrap().path(), b"tampered").unwrap();
    }
    let (ok, out) = tampered.frost(&["build", "--remote-cache", &endpoint, "--explain"]);
    assert!(
        ok,
        "a tampered remote cache must not fail the build:\n{out}"
    );
    assert!(
        !out.contains("0 rejected"),
        "the tampered blobs must be reported as rejected:\n{out}"
    );
    assert_eq!(
        tampered.run_app(),
        "frost: 42\n",
        "and the workspace must be built locally instead"
    );

    std::fs::remove_dir_all(shared).ok();
}

#[cfg(target_os = "linux")]
fn pty_command_line(workspace: &Path, args: &[&str]) -> String {
    let command = std::iter::once(frost_bin().to_string())
        .chain(std::iter::once("-C".to_string()))
        .chain(std::iter::once(workspace.to_string_lossy().into_owned()))
        .chain(args.iter().map(|arg| (*arg).to_string()))
        .map(|arg| shell_quote(&arg))
        .collect::<Vec<_>>()
        .join(" ");
    // `script -c` starts a shell on older util-linux. Replacing that shell
    // keeps Frost as the foreground process that receives raw-mode Ctrl-C.
    format!("exec {command}")
}

#[cfg(target_os = "linux")]
fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

fn copy_dir(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        // Never inherit build state from the checked-out sample workspace;
        // every test must start from a genuinely clean tree even if someone
        // ran frost against sample_c manually.
        if entry.file_name() == ".frost" {
            continue;
        }
        let target = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[test]
fn piped_build_uses_stable_plain_progress() {
    let ws = Workspace::new("plain-progress");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("[1/"),
        "plain progress was not printed:\n{out}"
    );
    assert!(
        !out.contains("\u{1b}["),
        "piped output must not contain terminal control codes:\n{out:?}"
    );
}

#[test]
// Drives a real pseudo-terminal through util-linux `script`.
#[cfg(target_os = "linux")]
fn tty_build_shows_live_slots_cache_critical_path_and_logs() {
    let ws = Workspace::new("tui-progress");
    let (ok, out) = ws.frost_pty(&["build"], &[]);
    assert!(ok, "{out}");
    assert!(
        out.contains("\u{1b}[?1049h"),
        "TTY build did not enter the live screen:\n{out:?}"
    );
    for label in ["slots", "cache", "critical path:", "logs ("] {
        assert!(out.contains(label), "TUI omitted {label:?}:\n{out:?}");
    }

    let (ok, cached) = ws.frost_pty(&["build"], &[]);
    assert!(ok, "{cached}");
    assert!(
        cached.contains("cache  5 hit"),
        "live cache-hit state was not updated:\n{cached:?}"
    );
    assert!(cached.contains("up to date"), "{cached}");
}

#[test]
// Drives a real pseudo-terminal through util-linux `script`.
#[cfg(target_os = "linux")]
fn no_tui_and_ci_force_plain_output_even_on_a_tty() {
    for (name, args, env) in [
        ("no-tui", vec!["build", "--no-tui"], vec![]),
        ("ci", vec!["build"], vec![("CI", "1")]),
    ] {
        let ws = Workspace::new(name);
        let (ok, out) = ws.frost_pty(&args, &env);
        assert!(ok, "{out}");
        assert!(
            out.contains("[1/"),
            "plain progress was not printed:\n{out}"
        );
        assert!(
            !out.contains("\u{1b}[?1049h"),
            "{name} unexpectedly enabled the live screen:\n{out:?}"
        );
    }
}

#[test]
// Drives a real pseudo-terminal through util-linux `script`.
#[cfg(target_os = "linux")]
fn tty_failure_is_rendered_before_the_summary() {
    let ws = Workspace::empty("tui-failure");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["broken"]

[target.broken]
kind = "genrule"
cmd = "printf immediate-failure >&2; exit 7"
outputs = ["broken.txt"]
"#,
    );
    let (ok, out) = ws.frost_pty(&["build"], &[]);
    assert!(!ok, "broken action unexpectedly succeeded:\n{out}");
    let immediate = out.find("FAILED:").expect("failure was not rendered");
    let summary = out
        .rfind("failure summary")
        .expect("failure summary was not printed");
    assert!(
        immediate < summary,
        "failure did not appear before the summary:\n{out:?}"
    );
}

#[test]
// Drives a real pseudo-terminal through util-linux `script`.
#[cfg(target_os = "linux")]
fn ctrl_c_in_raw_tui_mode_still_cancels_the_build() {
    use std::io::{Read, Write};
    use std::process::Stdio;
    use std::sync::mpsc;

    let ws = Workspace::empty("tui-cancel");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["slow"]

[target.slow]
kind = "genrule"
cmd = "sleep 3; printf done > ${out}"
outputs = ["slow.txt"]
"#,
    );
    let started = std::time::Instant::now();
    let mut child = Command::new("script")
        .args(["-q", "-e", "-c"])
        .arg(pty_command_line(&ws.dir, &["build"]))
        .arg("/dev/null")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env_remove("CI")
        .spawn()
        .expect("spawn TUI build");
    let mut stdout = child.stdout.take().expect("script stdout");
    let (ready_sender, ready_receiver) = mpsc::channel();
    let output_reader = std::thread::spawn(move || {
        let mut captured = Vec::new();
        let mut buffer = [0u8; 4096];
        let mut announced = false;
        loop {
            let read = stdout.read(&mut buffer).expect("read TUI output");
            if read == 0 {
                break;
            }
            captured.extend_from_slice(&buffer[..read]);
            if !announced
                && captured
                    .windows(b"\x1b[?1049h".len())
                    .any(|window| window == b"\x1b[?1049h")
            {
                let _ = ready_sender.send(());
                announced = true;
            }
        }
        captured
    });
    ready_receiver
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("TUI did not enter raw alternate-screen mode");
    child
        .stdin
        .take()
        .expect("script stdin")
        .write_all(&[3])
        .expect("send Ctrl-C");
    let status = child.wait().expect("wait for cancelled build");
    let captured = output_reader.join().expect("join TUI output reader");
    let mut stderr = Vec::new();
    child
        .stderr
        .take()
        .expect("script stderr")
        .read_to_end(&mut stderr)
        .expect("read script stderr");
    let output = String::from_utf8_lossy(&captured).to_string() + &String::from_utf8_lossy(&stderr);
    assert_eq!(status.code(), Some(130), "{output:?}");
    assert!(
        started.elapsed() < std::time::Duration::from_millis(2_500),
        "Ctrl-C was swallowed by raw terminal mode"
    );
}

#[test]
#[cfg(unix)]
fn kofun_binary_builds_incrementally_and_hits_the_action_cache() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::empty("kofun");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["alpha", "beta"]

[toolchain]
kofunc = "tools/kofunc"

[target.alpha]
kind = "kofun_binary"
srcs = ["src/alpha.kofun"]

[target.beta]
kind = "kofun_binary"
srcs = ["src/beta.kofun"]
"#,
    );
    ws.write("src/alpha.kofun", "alpha-v1\n");
    ws.write("src/beta.kofun", "beta-v1\n");
    ws.write(
        "tools/kofunc",
        r#"#!/bin/sh
set -eu
test "$1" = build
source=$2
test "$3" = -o
output=$4
test "$5" = --emit-c
emitted=$6
printf '%s\n' "$source" >> compiler.log
value=$(sed -n '1p' "$source")
printf '/* generated from %s */\n' "$value" > "$emitted"
printf '#!/bin/sh\nprintf "%%s\\n" "%s"\n' "$value" > "$output"
chmod +x "$output"
"#,
    );
    std::fs::set_permissions(
        ws.dir.join("tools/kofunc"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let (ok, initial) = ws.build_explain();
    assert!(ok, "initial Kofun build failed:\n{initial}");
    assert!(initial.contains("ran kofun:alpha"), "{initial}");
    assert!(initial.contains("ran kofun:beta"), "{initial}");
    assert!(initial.contains("2 built"), "{initial}");
    for target in ["alpha", "beta"] {
        assert!(
            ws.binary(&format!(".frost/bin/debug/{target}")).is_file(),
            "{target} binary was not produced"
        );
        assert!(
            ws.dir
                .join(format!(".frost/obj/debug/{target}/kofun.c"))
                .is_file(),
            "{target} emitted C was not declared and retained"
        );
    }

    let invocations = || std::fs::read_to_string(ws.dir.join("compiler.log")).unwrap();
    let mut initial_invocations = invocations()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    initial_invocations.sort();
    assert_eq!(
        initial_invocations,
        ["src/alpha.kofun", "src/beta.kofun"],
        "independent targets may execute in either scheduler order"
    );

    let (ok, unchanged) = ws.build_explain();
    assert!(ok, "unchanged Kofun rebuild failed:\n{unchanged}");
    assert!(unchanged.contains("up to date"), "{unchanged}");
    assert!(!unchanged.contains("  ran "), "{unchanged}");
    assert_eq!(
        invocations().lines().count(),
        2,
        "cached actions must not invoke kofunc"
    );

    ws.write("src/alpha.kofun", "alpha-v2\n");
    let (ok, incremental) = ws.build_explain();
    assert!(ok, "incremental Kofun build failed:\n{incremental}");
    assert!(
        incremental.contains("ran kofun:alpha :: input changed: src/alpha.kofun"),
        "{incremental}"
    );
    assert!(
        !incremental.contains("ran kofun:beta"),
        "unaffected Kofun target recompiled:\n{incremental}"
    );
    assert!(incremental.contains("1 built, 1 cached"), "{incremental}");
    let after_edit = invocations()
        .lines()
        .map(str::to_string)
        .collect::<Vec<_>>();
    assert_eq!(after_edit.len(), 3);
    assert_eq!(
        after_edit
            .iter()
            .filter(|source| source.as_str() == "src/alpha.kofun")
            .count(),
        2
    );
    assert_eq!(
        after_edit
            .iter()
            .filter(|source| source.as_str() == "src/beta.kofun")
            .count(),
        1
    );

    let alpha = Command::new(ws.binary(".frost/bin/debug/alpha"))
        .output()
        .expect("run Kofun shim output");
    assert!(alpha.status.success());
    assert_eq!(normalized_output(&alpha.stdout), "alpha-v2\n");

    let (ok, final_noop) = ws.build_explain();
    assert!(ok && final_noop.contains("up to date"), "{final_noop}");
    assert_eq!(invocations().lines().count(), 3);
}

#[test]
fn platforms_isolate_outputs_and_caches() {
    let ws = Workspace::new("platforms");
    ws.append(
        "frost.toml",
        "\n[platform.devsim]\ncflags = [\"-DDEVICE=1\"]\n",
    );

    let (ok, out) = ws.build_explain();
    assert!(ok, "host build failed:\n{out}");

    let (ok, out) = ws.frost(&["build", "--platform", "devsim", "--explain"]);
    assert!(ok, "devsim build failed:\n{out}");
    assert!(
        out.contains("5 built"),
        "platform build must not reuse host action results:\n{out}"
    );
    assert!(
        ws.binary(".frost/bin/devsim/debug/app").exists(),
        "platform binary lives in a platform-segmented tree"
    );
    assert!(
        ws.binary(".frost/bin/debug/app").exists(),
        "host binary keeps its historical path"
    );

    // Both configurations stay warm simultaneously: switching back and
    // forth is a cache lookup, never a rebuild (the Bazel analysis-cache
    // wipe pain, avoided by keying every action on its configuration).
    let (ok, out) = ws.frost(&["build", "--platform", "devsim", "--explain"]);
    assert!(ok && out.contains("up to date"), "{out}");
    let (ok, out) = ws.build_explain();
    assert!(ok && out.contains("up to date"), "{out}");

    let (ok, out) = ws.frost(&["build", "--all-platforms", "--explain"]);
    assert!(ok, "multi-platform build failed:\n{out}");
    assert!(out.contains("multi-platform build (2 platforms"), "{out}");
    assert!(
        out.contains("|-- host") && out.contains("`-- devsim"),
        "{out}"
    );
    assert!(out.contains("platform summary"), "{out}");
}

#[test]
#[cfg(unix)]
fn command_adapter_is_platform_aware_keyed_and_language_agnostic() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::empty("command-adapter");
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    ws.write("source.txt", "payload\n");
    for (name, identity) in [("adapter", "host"), ("device-adapter", "device")] {
        let path = ws.dir.join("tools").join(name);
        std::fs::write(
            &path,
            format!(
                "#!/bin/sh\nIFS= read -r content < \"$1\"\n\
                 printf '{}|%s|%s|%s|%s/%s\\n' \"$content\" \"$STATIC_VALUE\" \
                 \"${{LANGUAGE_FLAG-unset}}\" \"$3\" \"$4\" > \"$2\"\n",
                identity
            ),
        )
        .unwrap();
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["artifact"]

[toolchain.tools]
adapter = "tools/adapter"

[platform.device.tools]
adapter = "tools/device-adapter"

[target.artifact]
kind = "command"
tool = "adapter"
args = ["${in}", "${out}", "${profile}", "${platform}"]
inputs = ["source.txt"]
outputs = [".frost/out/${config}/artifact.txt"]
env = { STATIC_VALUE = "manifest" }
pass_env = ["LANGUAGE_FLAG"]
sandbox = false
"#,
    );

    let build = |flag: &str, args: &[&str]| {
        let (ok, out) = ws.frost_env(args, &[("LANGUAGE_FLAG", flag)]);
        assert!(ok, "command adapter build failed:\n{out}");
        out
    };
    build("one", &["build"]);
    let warm = build("one", &["build"]);
    assert!(warm.contains("up to date"), "{warm}");
    assert!(ws.dir.join(".frost/noop-debug.bin").is_file());

    let changed = build("two", &["build"]);
    assert!(
        !changed.contains("up to date"),
        "pass_env must invalidate the action and fast no-op certificate:\n{changed}"
    );
    assert_eq!(
        std::fs::read_to_string(ws.dir.join(".frost/out/debug/artifact.txt")).unwrap(),
        "host|payload|manifest|two|debug/host\n"
    );

    let all = build("two", &["build", "--all-platforms"]);
    assert!(all.contains("platform summary"), "{all}");
    assert_eq!(
        std::fs::read_to_string(ws.dir.join(".frost/out/device/debug/artifact.txt")).unwrap(),
        "device|payload|manifest|two|debug/device\n"
    );

    let host_tool = ws.dir.join("tools/adapter");
    let mut updated = std::fs::read_to_string(&host_tool)
        .unwrap()
        .replace("host|%s", "host-v2|%s");
    updated.push('\n');
    std::fs::write(&host_tool, updated).unwrap();
    std::fs::set_permissions(&host_tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    let changed = build("two", &["build"]);
    assert!(!changed.contains("up to date"), "{changed}");
    assert!(
        std::fs::read_to_string(ws.dir.join(".frost/out/debug/artifact.txt"))
            .unwrap()
            .starts_with("host-v2|"),
        "changing a named tool must invalidate its command action"
    );

    let (ok, out) = ws.frost(&["clean"]);
    assert!(ok, "multi-configuration clean failed:\n{out}");
    assert!(!ws.dir.join(".frost/out/debug/artifact.txt").exists());
    assert!(!ws.dir.join(".frost/out/device/debug/artifact.txt").exists());
}

#[test]
#[cfg(unix)]
fn command_adapter_can_preserve_outputs_for_incremental_compilers() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::empty("preserve-command-outputs");
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    ws.write("source.txt", "one\n");
    ws.write(
        "tools/incremental",
        r#"#!/bin/sh
set -eu
state=$1
stable=$2
changed=$3
input=$4
IFS= read -r value < "$input"
if [ "$value" = one ]; then
  printf 'state\n' > "$state"
  printf 'stable\n' > "$stable"
else
  [ -f "$state" ] && [ -f "$stable" ] || exit 42
fi
printf '%s\n' "$value" > "$changed"
"#,
    );
    std::fs::set_permissions(
        ws.dir.join("tools/incremental"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["incremental"]

[toolchain.tools]
incremental = "tools/incremental"

[target.incremental]
kind = "command"
tool = "incremental"
args = ["${outs}", "${in}"]
inputs = ["source.txt"]
outputs = [
  ".frost/out/${config}/state.txt",
  ".frost/out/${config}/stable.txt",
  ".frost/out/${config}/changed.txt",
]
preserve_outputs = true
sandbox = false
"#,
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "initial incremental build failed:\n{out}");
    ws.write("source.txt", "two\n");
    let (ok, out) = ws.frost(&["build", "--explain"]);
    assert!(ok, "incremental rerun lost its prior outputs:\n{out}");
    assert!(out.contains("ran command:incremental"), "{out}");
    assert_eq!(
        std::fs::read_to_string(ws.dir.join(".frost/out/debug/stable.txt")).unwrap(),
        "stable\n"
    );
    assert_eq!(
        std::fs::read_to_string(ws.dir.join(".frost/out/debug/changed.txt")).unwrap(),
        "two\n"
    );
}

#[test]
fn command_adapter_builds_real_rust_go_java_python_and_typescript_tools() {
    let ws = Workspace::empty("language-tools");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();

    let available = |tool: &str| Command::new(tool).arg("--version").output().is_ok();
    let mut tools = Vec::new();
    let mut targets = Vec::new();
    let mut defaults = Vec::new();

    if !cfg!(windows) && available("rustc") && rust_toolchain_is_consistent() {
        ws.write("src/main.rs", "fn main() { println!(\"rust-ok\"); }\n");
        tools.push("rustc = \"rustc\"".to_string());
        targets.push(
            r#"[target.rust]
kind = "command"
tool = "rustc"
args = ["${in}", "-o", "${out}"]
inputs = ["src/main.rs"]
outputs = [".frost/out/${config}/rust-app"]
sandbox = false
"#
            .to_string(),
        );
        defaults.push("rust");
    }
    if available("go") {
        ws.write(
            "src/main.go",
            "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"go-ok\") }\n",
        );
        tools.push("go = \"go\"".to_string());
        targets.push(
            r#"[target.go]
kind = "command"
tool = "go"
args = ["build", "-o", "${out}", "${in}"]
inputs = ["src/main.go"]
outputs = [".frost/out/${config}/go-app"]
sandbox = false
"#
            .to_string(),
        );
        defaults.push("go");
    }
    if available("javac") && java_toolchain_is_consistent() {
        ws.write(
            "src/Hello.java",
            "public final class Hello { static final class Nested {} \
             public static void main(String[] a) { System.out.println(\"java-ok\"); } }\n",
        );
        tools.push("javac = \"javac\"".to_string());
        tools.push(format!(
            "pack_jar = {}",
            serde_json::to_string(frost_bin()).unwrap()
        ));
        targets.push(
            r#"[target.java]
kind = "command"
tool = "javac"
args = ["-d", "${clean_dir}", "${in}"]
inputs = ["src/Hello.java"]
outputs = [".frost/out/${config}/java.jar"]
clean_dirs = [".frost/tmp/${config}/java"]
steps = [{ tool = "pack_jar", args = ["pack-jar", "--input", "${clean_dir}",
                                       "--output", "${out}",
                                       "--main-class", "Hello"] }]
# On macOS `javac` is a stub that selects the JDK from JAVA_HOME, so a build
# that cleared it would target a different JDK than the `java` below.
pass_env = ["JAVA_HOME"]
sandbox = false
"#
            .to_string(),
        );
        defaults.push("java");
    }
    if available("python3") {
        std::fs::create_dir_all(ws.dir.join("src/frost_language_demo")).unwrap();
        ws.write(
            "src/frost_language_demo/__init__.py",
            "def message():\n    return 'python-ok'\n",
        );
        tools.push(format!(
            "pack_wheel = {}",
            serde_json::to_string(frost_bin()).unwrap()
        ));
        targets.push(
            r#"[target.python]
kind = "command"
tool = "pack_wheel"
args = ["pack-wheel", "--input", "src", "--distribution", "frost-language-demo",
        "--version", "1.0.0", "--output", "${out}"]
inputs = ["src/frost_language_demo/__init__.py"]
outputs = [".frost/out/${config}/frost_language_demo-1.0.0-py3-none-any.whl"]
sandbox = false
"#
            .to_string(),
        );
        defaults.push("python");
    }

    // Modern Node runs erasable TypeScript syntax directly. Probe first so
    // older CI images simply exercise the other real adapters.
    if available("node") {
        ws.write(
            "src/write.ts",
            "import { writeFileSync } from 'node:fs';\n\
             const message: string = 'typescript-ok\\n';\n\
             writeFileSync(process.argv[2], message);\n",
        );
        let probe = Command::new("node")
            .arg(ws.dir.join("src/write.ts"))
            .arg(ws.dir.join("typescript-probe.txt"))
            .output();
        if probe.is_ok_and(|output| output.status.success()) {
            tools.push("node = \"node\"".to_string());
            targets.push(
                r#"[target.typescript]
kind = "command"
tool = "node"
args = ["src/write.ts", "${out}"]
inputs = ["src/write.ts"]
outputs = [".frost/out/${config}/typescript.txt"]
sandbox = false
"#
                .to_string(),
            );
            defaults.push("typescript");
        }
    }

    assert!(
        !defaults.is_empty(),
        "the test host has no supported language tool"
    );
    ws.write(
        "frost.toml",
        &format!(
            "[workspace]\ndefault_targets = [{}]\n\n[toolchain.tools]\n{}\n\n{}",
            defaults
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(", "),
            tools.join("\n"),
            targets.join("\n")
        ),
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "real language adapter build failed:\n{out}");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");
    if defaults.contains(&"rust") {
        assert!(ws.dir.join(".frost/out/debug/rust-app").is_file());
    }
    if defaults.contains(&"go") {
        assert!(ws.dir.join(".frost/out/debug/go-app").is_file());
    }
    if defaults.contains(&"java") {
        let archive = ws.dir.join(".frost/out/debug/java.jar");
        assert!(archive.is_file());
        let listing = || {
            let file = std::fs::File::open(&archive).unwrap();
            let mut jar = zip::ZipArchive::new(file).unwrap();
            (0..jar.len())
                .map(|index| jar.by_index(index).unwrap().name().to_string())
                .collect::<Vec<_>>()
        };
        let entries = listing();
        assert!(
            entries.iter().any(|name| name == "Hello.class"),
            "{entries:?}"
        );
        assert!(
            entries.iter().any(|name| name == "Hello$Nested.class"),
            "{entries:?}"
        );
        if available("java") {
            let output = Command::new("java")
                .arg("-jar")
                .arg(&archive)
                .output()
                .expect("run Frost-packed Java archive");
            assert!(output.status.success(), "{output:?}");
            assert_eq!(normalized_output(&output.stdout), "java-ok\n");
        }
        #[cfg(unix)]
        {
            let (ok, out) = ws.frost(&[
                "debug",
                "java",
                "--debugger",
                "/bin/echo",
                "--print",
                "--",
                "argument",
            ]);
            assert!(ok, "Java debug argv generation failed:\n{out}");
            assert!(out.contains("Java/jdb"), "{out}");
            assert!(out.contains("-classpath"), "{out}");
            assert!(out.contains("Hello"), "{out}");
        }

        // The intermediate tree is reset before the next multi-step action.
        // Removing a nested class must not leave stale bytecode in the jar.
        ws.write(
            "src/Hello.java",
            "public final class Hello { public static void main(String[] a) { \
             System.out.println(\"java-ok-v2\"); } }\n",
        );
        let (ok, out) = ws.frost(&["build"]);
        assert!(ok, "Java multi-step rebuild failed:\n{out}");
        let entries = listing();
        assert!(
            entries.iter().any(|name| name == "Hello.class"),
            "{entries:?}"
        );
        assert!(
            entries.iter().all(|name| name != "Hello$Nested.class"),
            "{entries:?}"
        );
    }
    if defaults.contains(&"python") {
        let wheel = ws
            .dir
            .join(".frost/out/debug/frost_language_demo-1.0.0-py3-none-any.whl");
        let output = Command::new("python3")
            .args([
                "-c",
                "import sys; sys.path.insert(0, sys.argv[1]); \
                 import frost_language_demo; print(frost_language_demo.message())",
            ])
            .arg(&wheel)
            .output()
            .expect("run Frost-packed Python wheel");
        assert!(output.status.success(), "{output:?}");
        assert_eq!(normalized_output(&output.stdout), "python-ok\n");
    }
    if defaults.contains(&"typescript") {
        assert_eq!(
            std::fs::read_to_string(ws.dir.join(".frost/out/debug/typescript.txt")).unwrap(),
            "typescript-ok\n"
        );
    }
}

#[test]
fn unknown_platform_fails_with_diagnostic() {
    let ws = Workspace::new("unknown-platform");
    let (ok, out) = ws.frost(&["build", "--platform", "nope"]);
    assert!(!ok, "build with undeclared platform must fail");
    assert!(out.contains("unknown platform"), "{out}");
}

/// Real device cross-compilation: build the sample workspace for
/// aarch64-linux-musl via `zig cc` and verify the produced ELF machine.
/// Skips (with a note) when zig is not installed.
#[test]
#[cfg(unix)]
fn cross_compile_aarch64_device_build() {
    if Command::new("zig").arg("version").output().is_err() {
        eprintln!("skipping cross_compile_aarch64_device_build: zig not in PATH");
        return;
    }
    let ws = Workspace::new("cross-aarch64");
    ws.write(
        "tools/zig-cc",
        "#!/bin/sh\nexec zig cc -target aarch64-linux-musl \"$@\"\n",
    );
    ws.write("tools/zig-ar", "#!/bin/sh\nexec zig ar \"$@\"\n");
    for tool in ["tools/zig-cc", "tools/zig-ar"] {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(ws.dir.join(tool), std::fs::Permissions::from_mode(0o755))
            .unwrap();
    }
    ws.append(
        "frost.toml",
        "\n[platform.aarch64]\ncc = \"tools/zig-cc\"\nar = \"tools/zig-ar\"\n",
    );

    let (ok, out) = ws.frost(&["build", "--platform", "aarch64", "--explain"]);
    assert!(ok, "cross build failed:\n{out}");

    let bin = std::fs::read(ws.binary(".frost/bin/aarch64/debug/app")).unwrap();
    assert_eq!(&bin[..4], b"\x7fELF", "output must be an ELF binary");
    let machine = u16::from_le_bytes([bin[18], bin[19]]);
    assert_eq!(machine, 183, "e_machine must be EM_AARCH64 (183)");

    // Cross results are cached independently of the host tree.
    let (ok, out) = ws.frost(&["build", "--platform", "aarch64", "--explain"]);
    assert!(ok && out.contains("up to date"), "{out}");
}

#[test]
fn query_deps_rdeps_somepath() {
    let ws = Workspace::new("query");

    let (ok, out) = ws.frost(&["query", "deps", "app"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["app", "gen_config", "util"]
    );

    let (ok, out) = ws.frost(&["query", "rdeps", "util"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim().lines().collect::<Vec<_>>(), ["app", "util"]);

    let (ok, out) = ws.frost(&["query", "somepath", "app", "gen_config", "--json"]);
    assert!(ok, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(parsed["query"], "somepath(app, gen_config)");
    assert_eq!(parsed["targets"][0], "app");

    let (ok, out) = ws.frost(&["query", "somepath", "util", "gen_config"]);
    assert!(!ok, "no-path case exits nonzero");
    assert!(out.contains("no path"), "{out}");
}

/// Every shard together covers exactly what one unsharded run covers.
///
/// Frost does not divide the cases — it tells the runner which slice is its
/// own — so the property to check is that a runner honouring the protocol
/// partitions the set: each case in exactly one shard, none lost, none twice.
/// The partition is expressed with `awk`, which is why this is a Unix case;
/// `docs/09_platform_support.md` tabulates host exclusions.
#[cfg(unix)]
#[test]
fn every_shard_together_covers_one_unsharded_run() {
    let ws = Workspace::empty("shard-coverage");
    ws.write("cases.txt", "alpha\nbravo\ncharlie\ndelta\necho\n");
    ws.write(
        "frost.toml",
        r#"
[workspace]
default_targets = []

[target.split]
kind = "test"
cmd = "awk \"NR % $TEST_TOTAL_SHARDS == $TEST_SHARD_INDEX\" cases.txt > ran-$TEST_SHARD_INDEX.txt"
shard_count = 3
inputs = ["cases.txt"]
sandbox = false
"#,
    );

    let (ok, out) = ws.frost(&["test", "--all"]);
    assert!(ok, "{out}");
    assert!(out.contains("3 passed"), "{out}");

    let mut covered: Vec<String> = Vec::new();
    for index in 0..3 {
        let slice = std::fs::read_to_string(ws.dir.join(format!("ran-{index}.txt")))
            .unwrap_or_else(|_| panic!("shard {index} should have written its slice"));
        covered.extend(slice.lines().map(str::to_string));
    }
    covered.sort();
    assert_eq!(
        covered,
        ["alpha", "bravo", "charlie", "delta", "echo"],
        "the shards must partition the cases: none lost, none run twice"
    );
}

/// A failing shard reruns; the shards that passed stay cached.
///
/// Which shard fails has to be decided by the test command, so this is POSIX
/// shell command text and runs where that is the shell — the `cfg(unix)` row of
/// `docs/09_platform_support.md`. What sharding does that is not shell-shaped —
/// action identity, per-shard stamps, the environment, and both rejections —
/// is covered by `frostbuild-core` unit tests, which run on every host.
#[cfg(unix)]
#[test]
fn one_shard_failing_leaves_the_other_shards_cached() {
    let ws = Workspace::empty("shard-caching");
    ws.write("failing.txt", "1\n");
    // Shard 1 fails; the others pass. Nothing about the sources differs
    // between them, so this isolates the shard identity itself.
    ws.write(
        "frost.toml",
        r#"
[workspace]
default_targets = []

[target.split]
kind = "test"
cmd = "grep -qx \"$TEST_SHARD_INDEX\" failing.txt && exit 1; exit 0"
shard_count = 3
inputs = ["failing.txt"]
sandbox = false
"#,
    );

    let (ok, out) = ws.frost(&["test", "--all"]);
    assert!(!ok, "one shard fails, so the run fails: {out}");
    assert!(out.contains("2 passed, 1 failed"), "{out}");
    // Each shard is its own action, so the failure is attributable.
    assert!(out.contains("test:split#1/3"), "{out}");

    // Nothing changed. The two that passed are restored from cache and only
    // the failure runs again — a failed shard cannot invalidate a passing one.
    let (ok, out) = ws.frost(&["test", "--all"]);
    assert!(!ok, "{out}");
    assert!(out.contains("0 passed, 1 failed, 2 cached"), "{out}");

    // Fixing the cause reruns only what was not already proven.
    ws.write("failing.txt", "\n");
    let (ok, out) = ws.frost(&["test", "--all"]);
    assert!(ok, "{out}");
    assert!(out.contains("3 passed"), "{out}");

    let (ok, out) = ws.frost(&["test", "--all"]);
    assert!(ok, "{out}");
    assert!(out.contains("0 passed, 0 failed, 3 cached"), "{out}");
}

#[test]
fn multi_module_java_sample_builds_across_module_boundaries() {
    let javac_present = Command::new("javac").arg("--version").output().is_ok();
    if !(javac_present && java_toolchain_is_consistent()) {
        eprintln!("skipping Java sample E2E: javac and java must be present and from the same JDK");
        return;
    }
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../sample_java");
    let ws = Workspace::empty("java-sample");
    copy_dir(&src, &ws.dir).expect("copy sample_java");

    // The sample names its jar step `frost`, because a checked-in manifest
    // cannot know this build's target directory. Put it on PATH rather than
    // rewriting the manifest, so the test exercises what a reader would run.
    let frost_dir = Path::new(frost_bin()).parent().unwrap();
    let path = std::env::var("PATH").unwrap_or_default();
    let separator = if cfg!(windows) { ";" } else { ":" };
    let with_frost = format!("{}{separator}{path}", frost_dir.display());

    let (ok, out) = ws.frost_env(&["build"], &[("PATH", with_frost.as_str())]);
    assert!(ok, "{out}");

    let greeting = ws.dir.join("greeting/.frost/out/debug/greeting.jar");
    let app = ws.dir.join("app/.frost/out/debug/app.jar");
    assert!(greeting.is_file(), "greeting jar: {out}");
    assert!(app.is_file(), "app jar: {out}");

    // App compiles against Greeting, so a class that loads and runs is the
    // proof that ${deps} put the dependency's jar on javac's classpath without
    // the manifest naming its path.
    let classpath = format!("{}{separator}{}", app.display(), greeting.display());
    let run = Command::new("java")
        .args(["-cp", &classpath, "com.example.app.App"])
        .output()
        .expect("run the packaged app");
    assert!(run.status.success(), "packaged app should run: {out}");
    assert_eq!(normalized_output(&run.stdout), "frost: 42\n");

    let (ok, out) = ws.frost_env(&["build"], &[("PATH", with_frost.as_str())]);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    // Editing the library has to reach the application module.
    let source = ws
        .dir
        .join("greeting/src/main/java/com/example/greeting/Greeting.java");
    let edited = std::fs::read_to_string(&source)
        .unwrap()
        .replace("\"frost\"", "\"frost-edited\"");
    std::fs::write(&source, edited).unwrap();
    let (ok, out) = ws.frost_env(&["build"], &[("PATH", with_frost.as_str())]);
    assert!(ok, "{out}");
    let run = Command::new("java")
        .args(["-cp", &classpath, "com.example.app.App"])
        .output()
        .expect("run the rebuilt app");
    assert!(run.status.success(), "rebuilt app should run");
    assert_eq!(normalized_output(&run.stdout), "frost-edited: 42\n");
}

#[test]
fn multi_package_sample_builds_runs_and_caches() {
    let ws = Workspace::multi("multi-sample");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    // The genrule writes a header one package consumes and three more inherit
    // through their dependencies, so a cold build of the diamond is a real
    // cross-package ordering test, not four independent compiles.
    assert!(out.contains("GEN gen_version"), "{out}");
    assert!(out.contains("LINK //apps/cli:cli"), "{out}");

    let binary = ws.binary(".frost/bin/debug/apps_cli_cli");
    let run = Command::new(&binary).output().expect("run built cli");
    assert!(run.status.success(), "built cli should run");
    assert_eq!(normalized_output(&run.stdout), "frost 1: 42\n");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "{out}");

    let (ok, out) = ws.frost(&["test", "--all"]);
    assert!(ok, "{out}");
    assert!(out.contains("1 passed"), "{out}");

    // Editing the shared bottom of the diamond has to reach the top through
    // both middles; a stale `render` would still link and print the old answer.
    let core = ws.dir.join("core/src/core.c");
    let edited = std::fs::read_to_string(&core)
        .unwrap()
        .replace("return a + b;", "return a + b + 1;");
    std::fs::write(&core, edited).unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let run = Command::new(&binary).output().expect("run rebuilt cli");
    assert!(run.status.success(), "rebuilt cli should run");
    assert_eq!(normalized_output(&run.stdout), "frost 1: 43\n");
}

#[test]
fn query_owners_reports_declared_and_generated_inputs() {
    let ws = Workspace::multi("query-owners");

    // A source belongs to exactly the target that compiles it. `*` stops at
    // `/`, so this pattern reaches the three packages one level down and not
    // apps/cli, which is two.
    let (ok, out) = ws.frost(&["query", "owners", "*/src/*.c"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["//core:core", "//render:render", "//text:text"]
    );

    // `**` crosses `/`, so the same query reaches the whole workspace.
    let (ok, out) = ws.frost(&["query", "owners", "**/src/*.c"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        [
            "//apps/cli:cli",
            "//core:core",
            "//render:render",
            "//text:text"
        ]
    );

    // The generated header is the case the configuration-free graph can still
    // answer completely: every target that transitively depends on the genrule
    // carries it as an order-only input, and the genrule that writes it does
    // not consume it.
    let (ok, out) = ws.frost(&["query", "owners", "gen/version.h"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        [
            "//apps/cli:cli",
            "//core:core",
            "//core:core_test",
            "//render:render",
            "//text:text"
        ]
    );

    // Several patterns union rather than intersect.
    let (ok, out) = ws.frost(&["query", "owners", "core/src/core.c", "text/src/text.c"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["//core:core", "//text:text"]
    );

    // A path nobody declares is empty and says why, rather than looking like a
    // target with no owners.
    let (ok, out) = ws.frost(&["query", "owners", "core/include/core.h"]);
    assert!(!ok, "an undeclared header exits nonzero: {out}");
    assert!(out.contains("frost explain"), "{out}");
}

#[test]
fn query_allpaths_returns_every_route() {
    let ws = Workspace::multi("query-allpaths");

    // The diamond: somepath commits to one route, allpaths owes both.
    let (ok, out) = ws.frost(&["query", "somepath", "//apps/cli:cli", "//core:core"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim().lines().count(), 3);

    let (ok, out) = ws.frost(&[
        "query",
        "allpaths",
        "//apps/cli:cli",
        "//core:core",
        "--json",
    ]);
    assert!(ok, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(parsed["query"], "allpaths(//apps/cli:cli, //core:core)");
    assert_eq!(parsed["truncated"], false);
    assert_eq!(
        parsed["paths"],
        serde_json::json!([
            ["//apps/cli:cli", "//render:render", "//core:core"],
            ["//apps/cli:cli", "//text:text", "//core:core"],
        ])
    );

    // Text output separates the routes by a blank line and keeps the
    // one-target-per-line shape every other query function prints.
    let (ok, out) = ws.frost(&["query", "allpaths", "//apps/cli:cli", "//core:core"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim(),
        "//apps/cli:cli\n//render:render\n//core:core\n\n//apps/cli:cli\n//text:text\n//core:core"
    );

    // Direction matters, and no route is stated rather than implied by silence.
    let (ok, out) = ws.frost(&["query", "allpaths", "//core:core", "//apps/cli:cli"]);
    assert!(!ok, "no-path case exits nonzero");
    assert!(out.contains("no path"), "{out}");

    // The bound is reported, not applied quietly.
    let (ok, out) = ws.frost(&[
        "query",
        "allpaths",
        "//apps/cli:cli",
        "//core:core",
        "--limit",
        "1",
    ]);
    assert!(ok, "{out}");
    assert!(out.contains("not complete"), "{out}");

    let (ok, out) = ws.frost(&[
        "query",
        "allpaths",
        "//apps/cli:cli",
        "//core:core",
        "--limit",
        "1",
        "--json",
    ]);
    assert!(ok, "{out}");
    let parsed: serde_json::Value = serde_json::from_str(out.trim()).unwrap();
    assert_eq!(parsed["truncated"], true);
    assert_eq!(parsed["paths"].as_array().unwrap().len(), 1);
}

#[test]
fn query_filters_by_kind_and_attr() {
    let ws = Workspace::multi("query-filters");

    let (ok, out) = ws.frost(&["query", "rdeps", "//core:core", "--kind", "cc_library"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["//core:core", "//render:render", "//text:text"]
    );

    let (ok, out) = ws.frost(&["query", "rdeps", "//core:core", "--kind", "cc_test"]);
    assert!(ok, "{out}");
    assert_eq!(out.trim().lines().collect::<Vec<_>>(), ["//core:core_test"]);

    // Direct deps, not the transitive closure the query walked.
    let (ok, out) = ws.frost(&[
        "query",
        "deps",
        "//apps/cli:cli",
        "--attr",
        "deps=//core:core",
    ]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["//render:render", "//text:text"]
    );

    let (ok, out) = ws.frost(&["query", "deps", "//apps/cli:cli", "--attr", "srcs=**/*.c"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("gen_version"),
        "a genrule has no sources: {out}"
    );

    // Filters compose as AND.
    let (ok, out) = ws.frost(&[
        "query",
        "rdeps",
        "//core:core",
        "--kind",
        "cc_library",
        "--attr",
        "deps=//core:core",
    ]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim().lines().collect::<Vec<_>>(),
        ["//render:render", "//text:text"]
    );

    // A filter that removes everything is an empty result with a reason, not a
    // silent success.
    let (ok, out) = ws.frost(&["query", "deps", "//apps/cli:cli", "--kind", "command"]);
    assert!(!ok, "{out}");
    assert!(out.contains("filters removed everything"), "{out}");

    // The accepted names are closed sets, so a typo fails instead of widening
    // the answer.
    let (ok, out) = ws.frost(&["query", "deps", "//apps/cli:cli", "--kind", "cc_lib"]);
    assert!(!ok, "{out}");
    assert!(out.contains("unknown --kind"), "{out}");

    let (ok, out) = ws.frost(&["query", "deps", "//apps/cli:cli", "--attr", "source=x"]);
    assert!(!ok, "{out}");
    assert!(out.contains("unknown --attr name"), "{out}");

    let (ok, out) = ws.frost(&["query", "deps", "//apps/cli:cli", "--attr", "deps"]);
    assert!(!ok, "{out}");
    assert!(out.contains("not NAME=PATTERN"), "{out}");
}

#[test]
fn query_output_formats_and_json_compatibility() {
    let ws = Workspace::multi("query-output");

    let (ok, text) = ws.frost(&["query", "deps", "//text:text"]);
    assert!(ok, "{text}");
    assert_eq!(text.trim(), "//core:core\n//text:text\ngen_version");

    // --output text is the default spelled out, so it must agree exactly.
    let (ok, explicit) = ws.frost(&["query", "deps", "//text:text", "--output", "text"]);
    assert!(ok, "{explicit}");
    assert_eq!(explicit, text);

    let (ok, out) = ws.frost(&["query", "deps", "//text:text", "--output", "label-kind"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim(),
        "cc_library target //core:core\ncc_library target //text:text\ngenrule target gen_version"
    );

    let (ok, out) = ws.frost(&["query", "deps", "//text:text", "--output", "dot"]);
    assert!(ok, "{out}");
    assert_eq!(
        out.trim(),
        concat!(
            "digraph frost_query {\n",
            "  rankdir=LR;\n",
            "  \"//core:core\";\n",
            "  \"//text:text\";\n",
            "  \"gen_version\";\n",
            "  \"//core:core\" -> \"gen_version\";\n",
            "  \"//text:text\" -> \"//core:core\";\n",
            "}"
        )
    );

    // --json predates --output and keeps its exact payload: adding `paths` to
    // the path queries must not have added anything here.
    let (ok, older) = ws.frost(&["query", "deps", "//text:text", "--json"]);
    assert!(ok, "{older}");
    let parsed: serde_json::Value = serde_json::from_str(older.trim()).unwrap();
    assert_eq!(parsed["query"], "deps(//text:text)");
    assert_eq!(
        parsed["targets"],
        serde_json::json!(["//core:core", "//text:text", "gen_version"])
    );
    assert_eq!(
        parsed.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["query", "targets"]
    );

    let (ok, newer) = ws.frost(&["query", "deps", "//text:text", "--output", "json"]);
    assert!(ok, "{newer}");
    assert_eq!(newer, older, "--json and --output json are one format");

    // Two spellings that disagree are a mistake worth naming.
    let (ok, out) = ws.frost(&["query", "deps", "//text:text", "--json", "--output", "dot"]);
    assert!(!ok, "{out}");
    assert!(out.contains("disagree"), "{out}");
}

#[test]
fn completion_scripts_and_fzf_selection_are_available() {
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::new("completion");
    for (shell, marker) in [
        ("bash", "_frost"),
        ("zsh", "#compdef frost"),
        ("fish", "__fish_frost"),
        ("powershell", "Register-ArgumentCompleter"),
        ("elvish", "arg-completer[frost]"),
        ("nushell", "export extern frost"),
    ] {
        let (ok, out) = ws.frost(&["completions", shell]);
        assert!(ok && out.contains(marker), "{shell} completion:\n{out}");
        assert!(
            out.contains("pack-jar"),
            "{shell} completion omitted pack-jar:\n{out}"
        );
        assert!(
            out.contains("pack-wheel"),
            "{shell} completion omitted pack-wheel:\n{out}"
        );
        assert!(
            out.contains("bazel-dev"),
            "{shell} completion omitted bazel-dev:\n{out}"
        );
        for command in ["dev", "debug", "ide", "doctor", "cache", "init", "language"] {
            assert!(
                out.contains(command),
                "{shell} completion omitted {command}:\n{out}"
            );
        }
    }

    let dynamic = |words: &[&str], index: usize| {
        let out = Command::new(frost_bin())
            .arg("--")
            .args(words)
            .env("COMPLETE", "bash")
            .env("_CLAP_IFS", "\u{b}")
            .env("_CLAP_COMPLETE_INDEX", index.to_string())
            .env("_CLAP_COMPLETE_COMP_TYPE", "9")
            .env("_CLAP_COMPLETE_SPACE", "true")
            .output()
            .expect("query dynamic completion");
        assert!(out.status.success(), "dynamic completion failed: {out:?}");
        String::from_utf8(out.stdout)
            .unwrap()
            .split('\u{b}')
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    let root = ws.dir.to_str().unwrap();
    let targets = dynamic(&["frost", "-C", root, "build", ""], 4);
    for target in ["app", "gen_config", "util"] {
        assert!(targets.contains(&target.to_string()), "{targets:?}");
    }
    assert_eq!(
        dynamic(&["frost", "-C", root, "build", "--profile", ""], 5),
        ["debug"]
    );
    assert_eq!(
        dynamic(&["frost", "-C", root, "build", "--platform", ""], 5),
        ["host"]
    );
    assert_eq!(
        dynamic(&["frost", "init", "--language", ""], 3),
        ["native", "java", "rust", "go", "typescript", "python"]
    );
    let keys = dynamic(&["frost", "-C", root, "info", ""], 4);
    for key in ["workspace_root", "bin_dir", "action_key_schema"] {
        assert!(keys.contains(&key.to_string()), "{keys:?}");
    }
    let endpoints = dynamic(&["frost", "-C", root, "build", "--remote-cache", ""], 5);
    assert!(endpoints.contains(&"file://".to_string()), "{endpoints:?}");

    #[cfg(unix)]
    {
        let tools = ws.dir.join("completion-tools");
        std::fs::create_dir_all(&tools).unwrap();
        let fzf = tools.join("fzf");
        std::fs::write(
            &fzf,
            "#!/bin/sh\nIFS= read -r selected\nprintf '%s\\n' \"$selected\"\n",
        )
        .unwrap();
        std::fs::set_permissions(&fzf, std::fs::Permissions::from_mode(0o755)).unwrap();
        let (ok, out) = ws.frost_env(&["pick", "--print"], &[("PATH", tools.to_str().unwrap())]);
        assert!(ok, "fzf-backed selection failed:\n{out}");
        assert_eq!(out.trim(), "app");
    }
}

#[test]
fn clean_build_then_noop_is_fully_cached() {
    let ws = Workspace::new("noop");

    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");
    assert!(out.contains("5 built"), "unexpected summary:\n{out}");
    assert_eq!(ws.run_app(), "frost: 42\n");

    let (ok, out) = ws.build_explain();
    assert!(ok, "no-op build failed:\n{out}");
    assert!(
        out.contains("up to date"),
        "no-op should be fully cached:\n{out}"
    );
    assert!(
        !out.contains("  ran "),
        "no actions should have run:\n{out}"
    );
}

#[test]
fn plain_default_build_uses_and_invalidates_the_fast_noop_certificate() {
    let ws = Workspace::new("fast-noop");

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "clean build failed:\n{out}");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");
    assert!(
        ws.dir.join(".frost/noop-debug.bin").is_file(),
        "the fully checked no-op did not persist its certificate"
    );

    // A fast hit does not need to reconstruct the per-action journal.
    std::fs::write(ws.dir.join(".frost/journal.bin"), b"corrupt journal").unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // Corrupting the shortcut itself is a cache miss, never a build failure
    // and never evidence that stale outputs are current.
    let certificate = ws.dir.join(".frost/noop-debug.bin");
    let mut corrupt = std::fs::read(&certificate).unwrap();
    let last = corrupt.len() - 1;
    corrupt[last] ^= 0x80;
    std::fs::write(&certificate, corrupt).unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "corrupt certificate did not fall back safely:\n{out}");

    // The certificate is only a shortcut: any input mismatch falls back to
    // the normal path. With the journal deliberately unusable, that path
    // rebuilds the closure rather than accepting the stale certificate.
    ws.write(
        "src/util_internal.h",
        "#ifndef FROST_SAMPLE_UTIL_INTERNAL_H\n\
         #define FROST_SAMPLE_UTIL_INTERNAL_H\n\
         #define FROST_INTERNAL_BIAS 1\n\
         #endif\n",
    );
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "fallback build failed:\n{out}");
    assert!(!out.contains("up to date"), "{out}");
    assert_eq!(ws.run_app(), "frost: 43\n");
}

#[test]
fn fast_noop_certificate_is_bound_to_the_default_target_graph() {
    let ws = Workspace::new("fast-noop-graph");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok && out.contains("up to date"), "{out}");
    let certificate = ws.dir.join(".frost/noop-debug.bin");
    let old_certificate = std::fs::read(&certificate).unwrap();

    let manifest = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    ws.write(
        "frost.toml",
        &format!(
            "{}\n\
             [target.extra]\n\
             kind = \"genrule\"\n\
             cmd = \"printf extra > ${{out}}\"\n\
             outputs = [\".frost/extra/result.txt\"]\n",
            manifest.replace(
                "default_targets = [\"app\"]",
                "default_targets = [\"app\", \"extra\"]"
            )
        ),
    );
    let extra = ws.dir.join(".frost/extra/result.txt");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "new default target failed:\n{out}");
    assert!(extra.is_file(), "{out}");
    assert_eq!(
        std::fs::read(&certificate).unwrap(),
        old_certificate,
        "a build that executed work should leave the prior certificate in place"
    );

    std::fs::remove_file(&extra).unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "stale graph certificate blocked fallback:\n{out}");
    assert!(
        extra.is_file(),
        "the old default-target certificate skipped the new target:\n{out}"
    );
}

#[test]
fn internal_header_change_recompiles_only_util() {
    let ws = Workspace::new("header");
    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");

    // util_internal.h is only included by util.c; discovered via the depfile.
    ws.write(
        "src/util_internal.h",
        "#ifndef FROST_SAMPLE_UTIL_INTERNAL_H\n\
         #define FROST_SAMPLE_UTIL_INTERNAL_H\n\
         #define FROST_INTERNAL_BIAS 1\n\
         #endif\n",
    );

    let (ok, out) = ws.build_explain();
    assert!(ok, "incremental build failed:\n{out}");
    assert!(
        out.contains("ran compile:util:src/util.c :: input changed: src/util_internal.h"),
        "util.c should recompile due to the header:\n{out}"
    );
    assert!(
        !out.contains("ran compile:app:src/main.c"),
        "main.c must NOT recompile for an internal header change:\n{out}"
    );
    assert!(out.contains("ran archive:util"), "{out}");
    assert!(out.contains("ran link:app"), "{out}");
    assert_eq!(ws.run_app(), "frost: 43\n");
}

#[test]
fn genrule_rerun_with_identical_output_cuts_off_downstream() {
    let ws = Workspace::new("cutoff");
    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");

    // Touching the script changes the genrule's key, but the regenerated
    // header is byte-identical, so downstream compiles must stay cached.
    let harmless_tweak = if cfg!(windows) {
        "rem harmless tweak\r\n"
    } else {
        "# harmless tweak\n"
    };
    ws.append(ws.generator_script(), harmless_tweak);

    let (ok, out) = ws.build_explain();
    assert!(ok, "incremental build failed:\n{out}");
    assert!(out.contains("ran genrule:gen_config"), "{out}");
    assert!(
        out.contains("1 built, 4 cached"),
        "early cutoff should keep downstream cached:\n{out}"
    );
}

#[test]
fn cflags_change_recompiles_translation_units() {
    let ws = Workspace::new("flags");
    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");

    let manifest = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    ws.write(
        "frost.toml",
        &manifest.replace(
            "cflags = [\"-O2\", \"-Wall\"]",
            "cflags = [\"-O2\", \"-Wall\", \"-DFROST_EXTRA=1\"]",
        ),
    );

    let (ok, out) = ws.build_explain();
    assert!(ok, "incremental build failed:\n{out}");
    assert!(
        out.contains("ran compile:util:src/util.c :: command or toolchain changed"),
        "{out}"
    );
    assert!(
        out.contains("ran compile:app:src/main.c :: command or toolchain changed"),
        "{out}"
    );
}

#[test]
fn deleted_output_is_rebuilt() {
    let ws = Workspace::new("tamper");
    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");

    std::fs::remove_file(ws.binary(".frost/bin/debug/app")).unwrap();

    let (ok, out) = ws.build_explain();
    assert!(ok, "rebuild failed:\n{out}");
    assert!(out.contains("up to date"), "CAS restore expected:\n{out}");
    assert_eq!(ws.run_app(), "frost: 42\n");
}

#[test]
fn plan_predicts_and_build_settles() {
    let ws = Workspace::new("plan");

    let (ok, out) = ws.frost(&["plan"]);
    assert!(ok, "plan failed:\n{out}");
    assert!(out.contains("would run genrule:gen_config"), "{out}");
    assert!(
        out.contains("may run"),
        "downstream should be may-run:\n{out}"
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "build failed:\n{out}");

    let (ok, out) = ws.frost(&["plan"]);
    assert!(ok, "plan failed:\n{out}");
    assert!(
        out.contains("plan: 0 would run, 0 may run, 5 cached"),
        "{out}"
    );
}

#[test]
fn compile_failure_reports_and_skips_downstream() {
    let ws = Workspace::new("fail");
    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");

    ws.write("src/util.c", "#include \"util.h\"\nthis is not C\n");

    let (ok, out) = ws.build_explain();
    assert!(!ok, "build must fail on a compile error");
    assert!(out.contains("FAILED: CC src/util.c"), "{out}");
    assert!(out.contains("failed"), "{out}");
    assert!(
        out.contains("skipped link:app") || out.contains("skipped archive:util"),
        "downstream must be skipped:\n{out}"
    );
}

#[test]
fn clean_removes_outputs_and_full_rebuild_works() {
    let ws = Workspace::new("clean");
    let (ok, out) = ws.build_explain();
    assert!(ok, "clean build failed:\n{out}");

    let (ok, out) = ws.frost(&["clean"]);
    assert!(ok, "clean failed:\n{out}");
    assert!(!ws.binary(".frost/bin/debug/app").exists());
    assert!(!ws.dir.join("gen/config.h").exists());

    let (ok, out) = ws.build_explain();
    assert!(ok, "rebuild after clean failed:\n{out}");
    assert!(
        out.contains("up to date") && out.contains("5 actions"),
        "the CAS should restore the outputs rather than rebuild them:\n{out}"
    );
}

#[test]
fn graph_dot_lists_target_edges() {
    let ws = Workspace::new("graph");
    let (ok, out) = ws.frost(&["graph", "--dot"]);
    assert!(ok, "graph failed:\n{out}");
    assert!(out.contains("\"app\" -> \"util\""), "{out}");
    assert!(out.contains("digraph frost"), "{out}");
}

#[test]
fn profiles_coexist_and_switch_back_is_cached() {
    let ws = Workspace::new("profiles");
    let mut manifest = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    manifest.push_str(
        "\n[profile.debug]\ncflags = [\"-g\"]\n\n[profile.release]\ncflags = [\"-O3\"]\n",
    );
    ws.write("frost.toml", &manifest);
    let (ok, out) = ws.frost(&["build", "--profile", "debug"]);
    assert!(ok, "{out}");
    let (ok, out) = ws.frost(&["build", "--profile", "release"]);
    assert!(ok, "{out}");
    assert!(ws.binary(".frost/bin/debug/app").exists());
    assert!(ws.binary(".frost/bin/release/app").exists());
    let (ok, out) = ws.frost(&["build", "--profile", "debug"]);
    assert!(ok && out.contains("up to date"), "{out}");
}

#[test]
fn cxx_glob_test_and_compdb_are_usable() {
    let ws = Workspace::new("cxx-test");
    ws.write("src/math.cpp", "int answer() { return 42; }\n");
    ws.write(
        "src/math_test.cpp",
        "extern int answer(); int main() { return answer() == 42 ? 0 : 1; }\n",
    );
    let mut manifest = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    manifest.push_str("\n[target.math_test]\nkind = \"cc_test\"\nsrcs = [\"src/math*.cpp\"]\n");
    ws.write("frost.toml", &manifest);
    let (ok, out) = ws.frost(&["test", "math_test", "--explain"]);
    assert!(ok && out.contains("tests: 1 passed"), "{out}");
    let (ok, out) = ws.frost(&["test", "math_test"]);
    assert!(ok && out.contains("1 cached"), "{out}");
    let (ok, out) = ws.frost(&["compdb"]);
    assert!(ok, "{out}");
    let db: serde_json::Value =
        serde_json::from_slice(&std::fs::read(ws.dir.join("compile_commands.json")).unwrap())
            .unwrap();
    assert!(db
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["file"] == "src/math.cpp"));
}

#[test]
fn direct_argv_language_test_is_cached_and_cleans_a_failed_stamp() {
    if Command::new("python3").arg("--version").output().is_err() {
        eprintln!("skipping direct_argv_language_test: python3 not in PATH");
        return;
    }
    let ws = Workspace::new("direct-test");
    std::fs::create_dir_all(ws.dir.join("tests")).unwrap();
    ws.write("tests/value.txt", "pass\n");
    ws.write(
        "tests/check.py",
        concat!(
            "import os, pathlib, sys\n",
            "actual = pathlib.Path(sys.argv[1]).read_text().strip()\n",
            "expected = os.environ['EXPECTED']\n",
            "if actual != expected:\n",
            "    raise SystemExit(f'expected {expected!r}, got {actual!r}')\n",
        ),
    );
    ws.append(
        "frost.toml",
        r#"
[toolchain.tools]
python = "python3"

[target.python_test]
kind = "test"
tool = "python"
args = ["tests/check.py", "tests/value.txt"]
inputs = ["tests/check.py", "tests/value.txt"]
env = { EXPECTED = "pass" }
sandbox = false
"#,
    );

    let (ok, out) = ws.frost(&["test", "python_test", "--explain"]);
    assert!(ok && out.contains("tests: 1 passed"), "{out}");
    let stamp = ws.dir.join(".frost/test/debug/python_test/passed");
    assert!(stamp.is_file());
    let (ok, out) = ws.frost(&["test", "python_test"]);
    assert!(ok && out.contains("1 cached"), "{out}");

    ws.write("tests/value.txt", "fail\n");
    let (ok, out) = ws.frost(&["test", "python_test", "--explain"]);
    assert!(!ok, "changed failing test must run and fail");
    assert!(out.contains("expected 'pass', got 'fail'"), "{out}");
    assert!(
        !stamp.exists(),
        "a failed test must not retain its success stamp"
    );
}

#[test]
fn test_all_selects_every_test_target() {
    let ws = Workspace::new("test-all");
    ws.append(
        "frost.toml",
        "\n[target.first]\nkind = \"test\"\ncmd = \"true\"\n\
         \n[target.second]\nkind = \"test\"\ncmd = \"true\"\n",
    );

    // An explicit target would normally select only that target. `--all`
    // intentionally expands the selection to every declared test target.
    let (ok, out) = ws.frost(&["test", "first", "--all", "--no-cache"]);
    assert!(ok && out.contains("tests: 2 passed"), "{out}");

    let (ok, out) = ws.frost(&["test", "--all", "--predictive"]);
    assert!(!ok && out.contains("cannot be used with"), "{out}");
}

#[test]
fn multi_package_labels_build_across_packages() {
    let ws = Workspace::new("packages");
    std::fs::create_dir_all(ws.dir.join("lib")).unwrap();
    std::fs::create_dir_all(ws.dir.join("app")).unwrap();
    ws.write(
        "frost.toml",
        "[workspace]\ndefault_targets = [\"//app:app\"]\n",
    );
    ws.write("lib/lib.c", "int package_value(void) { return 7; }\n");
    ws.write(
        "lib/frost.toml",
        "[target.lib]\nkind = \"cc_library\"\nsrcs = [\"lib.c\"]\n",
    );
    ws.write(
        "app/main.c",
        "int package_value(void); int main(void) { return package_value() == 7 ? 0 : 1; }\n",
    );
    ws.write(
        "app/frost.toml",
        "[target.app]\nkind = \"cc_binary\"\nsrcs = [\"main.c\"]\ndeps = [\"//lib:lib\"]\n",
    );
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let status = Command::new(ws.binary(".frost/bin/debug/app_app"))
        .status()
        .unwrap();
    assert!(status.success());
}

#[test]
fn generated_header_is_order_only_for_unrelated_translation_units() {
    let ws = Workspace::new("order-only");
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    let script = std::fs::read_to_string(ws.dir.join(ws.generator_script())).unwrap();
    ws.write(ws.generator_script(), &script.replace("frost:", "ice:"));
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    assert!(out.contains("ran compile:app:src/main.c"), "{out}");
    assert!(
        !out.contains("ran compile:util:src/util.c"),
        "unrelated TU rebuilt:\n{out}"
    );
    assert_eq!(ws.run_app(), "ice: 42\n");
}

#[test]
fn determinism_check_names_macro_and_output() {
    let ws = Workspace::new("determinism");
    ws.write(
        "src/nondeterministic.c",
        "const char *stamp = __TIME__; int main(void) { return stamp[0] == 0; }\n",
    );
    let mut manifest = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    manifest.push_str(
        "\n[target.nondeterministic]\nkind = \"cc_binary\"\nsrcs = [\"src/nondeterministic.c\"]\n",
    );
    ws.write("frost.toml", &manifest);
    let (ok, out) = ws.frost(&["build", "nondeterministic", "--check-determinism"]);
    assert!(!ok, "nondeterministic action must fail the check");
    assert!(
        out.contains("non-deterministic action compile:nondeterministic"),
        "{out}"
    );
    assert!(out.contains(".frost/obj/debug/nondeterministic"), "{out}");
}

#[test]
fn daemon_build_status_and_stop() {
    let ws = Workspace::new("daemon");
    let (ok, out) = ws.frost(&["build", "--daemon"]);
    assert!(ok, "{out}");
    let (ok, out) = ws.frost(&["build", "--daemon"]);
    assert!(ok && out.contains("up to date"), "{out}");

    #[cfg(unix)]
    {
        // A valid certificate must be answered inside frostd. The deliberately
        // nonexistent fallback program proves that no second frost process was
        // needed for this hit.
        let in_process = frostbuild_daemon::request(
            &ws.dir,
            &frostbuild_daemon::Request::Run {
                version: frostbuild_daemon::PROTOCOL_VERSION,
                program: ws.dir.join("definitely-missing-frost"),
                args: Vec::new(),
                env: Vec::new(),
                fast_noop: Some(frostbuild_daemon::FastNoopRequest {
                    profile: "debug".into(),
                    platform: frostbuild_core::manifest::HOST_PLATFORM.into(),
                    key_env: frostbuild_exec::key_environment_snapshot(),
                }),
            },
        )
        .unwrap();
        assert_eq!(in_process.code, 0, "{in_process:?}");
        assert!(in_process.stdout.contains("up to date"), "{in_process:?}");

        // A watcher barrier must observe output changes under .frost before a
        // cached proof can be accepted. Deleting an artifact immediately before
        // the request must take the full path and restore it from CAS.
        std::fs::remove_file(ws.binary(".frost/bin/debug/app")).unwrap();
        let (ok, out) = ws.frost(&["build", "--daemon", "--explain"]);
        assert!(ok && out.contains("up to date"), "{out}");
        assert_eq!(ws.run_app(), "frost: 42\n");

        ws.append("src/util.c", "\n/* daemon watcher change */\n");
        let miss = frostbuild_daemon::request(
            &ws.dir,
            &frostbuild_daemon::Request::Run {
                version: frostbuild_daemon::PROTOCOL_VERSION,
                program: ws.dir.join("definitely-missing-frost"),
                args: Vec::new(),
                env: Vec::new(),
                fast_noop: Some(frostbuild_daemon::FastNoopRequest {
                    profile: "debug".into(),
                    platform: frostbuild_core::manifest::HOST_PLATFORM.into(),
                    key_env: frostbuild_exec::key_environment_snapshot(),
                }),
            },
        )
        .unwrap();
        assert_ne!(miss.code, 0, "a changed input must reject the certificate");
        std::thread::sleep(std::time::Duration::from_millis(100));
        let (ok, out) = ws.frost(&["build", "--daemon"]);
        assert!(
            ok && out.contains("1 built"),
            "source change missed:\n{out}"
        );
    }
    let (ok, out) = ws.frost(&["daemon", "status"]);
    assert!(ok && out.contains("running"), "{out}");
    let (ok, out) = ws.frost(&["daemon", "stop"]);
    assert!(ok && out.contains("stopped"), "{out}");
}

#[test]
#[cfg(unix)]
fn dev_infers_the_artifact_rebuilds_and_restarts_its_process() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::new("watch-restart");
    ws.write(
        "tools/dev-probe",
        "#!/bin/sh\nset -eu\n\"$1\" >> .frost/dev-runs.txt\n",
    );
    std::fs::set_permissions(
        ws.dir.join("tools/dev-probe"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let mut watch = Command::new(frost_bin())
        .arg("-C")
        .arg(&ws.dir)
        .args([
            "dev",
            "app",
            "--debounce-ms",
            "20",
            "--runner",
            "tools/dev-probe",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let runs = ws.dir.join(".frost/dev-runs.txt");
    let wait_for_runs = |minimum: usize| {
        for _ in 0..250 {
            let count = std::fs::read_to_string(&runs)
                .unwrap_or_default()
                .lines()
                .count();
            if count >= minimum {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    };

    let initial = wait_for_runs(1);
    ws.append("src/util.c", "\n/* trigger watch rebuild */\n");
    let restarted = wait_for_runs(2);
    let _ = watch.kill();
    let _ = watch.wait();

    assert!(initial, "dev did not infer and run the initial artifact");
    assert!(restarted, "dev did not restart after a source change");
    let observed = std::fs::read_to_string(runs).unwrap();
    assert!(
        observed.lines().all(|line| line == "frost: 42"),
        "unexpected dev process output: {observed:?}"
    );
}

#[test]
#[cfg(unix)]
fn bazel_query_import_creates_buildable_package_manifests_without_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::empty("import-bazel");
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    std::fs::create_dir_all(ws.dir.join("lib")).unwrap();
    std::fs::create_dir_all(ws.dir.join("app")).unwrap();
    ws.write("lib/math.cc", "int add(int a, int b) { return a + b; }\n");
    ws.write(
        "app/main.cc",
        "int add(int, int); int main() { return add(20, 22) == 42 ? 0 : 1; }\n",
    );
    ws.write(
        "tools/bazel",
        r#"#!/bin/sh
set -eu
case "$*" in
  *--version*) printf 'bazel 9.1.0\n' ;;
  *--output=build*) printf '# expanded BUILD without configurable attributes\n' ;;
  *--output=xml*)
    /bin/cat <<'XML'
<?xml version="1.0" encoding="UTF-8"?>
<query version="2">
  <rule class="cc_library rule" name="//lib:math">
    <list name="srcs"><label value="//lib:math.cc"/></list>
  </rule>
  <rule class="cc_binary rule" name="//app:runner">
    <list name="srcs"><label value="//app:main.cc"/></list>
    <list name="deps"><label value="//lib:math"/></list>
  </rule>
</query>
XML
    ;;
  *) exit 2 ;;
esac
"#,
    );
    std::fs::set_permissions(
        ws.dir.join("tools/bazel"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let (ok, out) = ws.frost(&["import-bazel", "--bazel", "tools/bazel"]);
    assert!(ok, "Bazel import failed:\n{out}");
    assert!(out.contains("2 rules"), "{out}");
    assert!(ws.dir.join("frost.toml").is_file());
    assert!(ws.dir.join("lib/frost.toml").is_file());
    assert!(ws.dir.join("app/frost.toml").is_file());

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "imported manifests did not build:\n{out}");

    let (ok, out) = ws.frost(&["import-bazel", "--bazel", "tools/bazel"]);
    assert!(!ok, "a second import overwrote manifests:\n{out}");
    assert!(out.contains("refusing to overwrite"), "{out}");
}

#[test]
#[cfg(unix)]
fn npm_workspace_import_tracks_transitive_gate_inputs_without_overwrite() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::empty("import-npm");
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    std::fs::create_dir_all(ws.dir.join("packages/core/src")).unwrap();
    std::fs::create_dir_all(ws.dir.join("apps/web/src")).unwrap();
    ws.write(
        "package.json",
        r#"{"name":"demo","private":true,"workspaces":["packages/*","apps/*"]}"#,
    );
    ws.write("package-lock.json", "{}");
    ws.write(
        "packages/core/package.json",
        r#"{"name":"@demo/core","scripts":{"typecheck":"tsc --noEmit"}}"#,
    );
    ws.write(
        "packages/core/src/index.ts",
        "export const answer: number = 42;\n",
    );
    ws.write(
        "apps/web/package.json",
        r#"{"name":"@demo/web","scripts":{"typecheck":"tsc --noEmit"},"dependencies":{"@demo/core":"*"}}"#,
    );
    ws.write(
        "apps/web/src/main.ts",
        "import { answer } from '@demo/core'; void answer;\n",
    );
    ws.write(
        "tools/npm",
        r#"#!/bin/sh
set -eu
mkdir -p .frost
printf '%s\n' "$*" >> .frost/npm-runs.txt
"#,
    );
    std::fs::set_permissions(
        ws.dir.join("tools/npm"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let (ok, preview) = ws.frost(&[
        "import-npm",
        "--script",
        "typecheck",
        "--npm",
        "tools/npm",
        "--node",
        "/bin/sh",
        "--dry-run",
    ]);
    assert!(ok, "npm import preview failed:\n{preview}");
    assert!(
        preview.contains("[target.demo-core-typecheck]"),
        "{preview}"
    );
    assert!(preview.contains("[target.demo-web-typecheck]"), "{preview}");
    assert!(!ws.dir.join("frost.toml").exists());

    let (ok, out) = ws.frost(&[
        "import-npm",
        "--script",
        "typecheck",
        "--npm",
        "tools/npm",
        "--node",
        "/bin/sh",
    ]);
    assert!(ok && out.contains("2 test gates"), "{out}");
    let (ok, out) = ws.frost(&["test", "--all", "--no-tui"]);
    assert!(ok && out.contains("2 built"), "{out}");
    let runs = ws.dir.join(".frost/npm-runs.txt");
    assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 2);

    let (ok, out) = ws.frost(&["test", "--all", "--no-tui"]);
    assert!(ok && out.contains("2 cached"), "{out}");
    assert_eq!(std::fs::read_to_string(&runs).unwrap().lines().count(), 2);

    ws.write(
        "packages/core/src/index.ts",
        "export const answer: number = 43;\n",
    );
    let (ok, out) = ws.frost(&["test", "--all", "--no-tui"]);
    assert!(
        ok && out.contains("2 built"),
        "dependency source change did not rerun both gates:\n{out}"
    );
    let observed = std::fs::read_to_string(&runs).unwrap();
    assert_eq!(observed.lines().count(), 4);
    assert!(observed.contains("run typecheck --workspace @demo/core"));
    assert!(observed.contains("run typecheck --workspace @demo/web"));

    let (ok, out) = ws.frost(&[
        "import-npm",
        "--script",
        "typecheck",
        "--npm",
        "tools/npm",
        "--node",
        "/bin/sh",
    ]);
    assert!(!ok && out.contains("already exists"), "{out}");
    assert!(
        std::fs::read_dir(&ws.dir).unwrap().all(|entry| !entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .starts_with(".frost.toml.import-")),
        "failed import left a temporary manifest behind"
    );
}

#[test]
#[cfg(unix)]
fn bazel_dev_rebuilds_and_restarts_only_after_success() {
    use std::os::unix::fs::PermissionsExt;

    let ws = Workspace::empty("bazel-dev");
    std::fs::create_dir_all(ws.dir.join("tools")).unwrap();
    ws.write("app.txt", "healthy one\n");
    ws.write(
        "tools/bazel",
        r#"#!/bin/sh
set -eu
mkdir -p .frost
case "$1" in
  build)
    printf '%s\n' "$*" >> .frost/bazel-builds.txt
    if grep -q broken app.txt; then
      exit 7
    fi
    ;;
  run)
    printf '%s\n' "$*" >> .frost/bazel-runs.txt
    trap 'exit 0' INT TERM
    while :; do
      printf tick >> .frost/bazel-heartbeat.txt
      sleep 0.02
    done
    ;;
  *) exit 2 ;;
esac
"#,
    );
    std::fs::set_permissions(
        ws.dir.join("tools/bazel"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();

    let mut dev = Command::new(frost_bin())
        .arg("-C")
        .arg(&ws.dir)
        .args([
            "bazel-dev",
            "//app:server",
            "--bazel",
            "tools/bazel",
            "--debounce-ms",
            "20",
            "--bazel-arg=--config=dev",
            "--",
            "--port",
            "3000",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let builds = ws.dir.join(".frost/bazel-builds.txt");
    let runs = ws.dir.join(".frost/bazel-runs.txt");
    let heartbeats = ws.dir.join(".frost/bazel-heartbeat.txt");
    let wait_for_lines = |path: &Path, minimum: usize| {
        for _ in 0..250 {
            if std::fs::read_to_string(path)
                .unwrap_or_default()
                .lines()
                .count()
                >= minimum
            {
                return true;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        false
    };
    let wait_for_stable_counts = |minimum_runs: usize| {
        let mut previous = (0, 0);
        let mut stable_checks = 0;
        for _ in 0..500 {
            let current = (
                std::fs::read_to_string(&builds)
                    .unwrap_or_default()
                    .lines()
                    .count(),
                std::fs::read_to_string(&runs)
                    .unwrap_or_default()
                    .lines()
                    .count(),
            );
            if current.1 >= minimum_runs && current == previous {
                stable_checks += 1;
                if stable_checks >= 25 {
                    return Some(current);
                }
            } else {
                stable_checks = 0;
            }
            previous = current;
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        None
    };

    assert!(
        wait_for_lines(&runs, 1),
        "initial Bazel target did not start"
    );
    ws.write("app.txt", "healthy two\n");
    assert!(
        wait_for_lines(&runs, 2),
        "successful rebuild did not restart"
    );
    // Native watchers may deliver a second event for the same editor write
    // after the first debounce window, especially on a heavily loaded host.
    // Establish the healthy baseline only after all such successful restarts
    // settle; the assertion below is specifically about the later failed
    // build, not about the backend's event coalescing behavior.
    let settled = wait_for_stable_counts(2);
    assert!(
        settled.is_some(),
        "successful Bazel rebuild/restart stream did not settle: {} builds / {} runs",
        std::fs::read_to_string(&builds)
            .unwrap_or_default()
            .lines()
            .count(),
        std::fs::read_to_string(&runs)
            .unwrap_or_default()
            .lines()
            .count()
    );
    let (healthy_builds, healthy_runs) = settled.unwrap();

    ws.write("app.txt", "broken\n");
    assert!(
        wait_for_lines(&builds, healthy_builds + 1),
        "broken change was not rebuilt"
    );
    let before = std::fs::metadata(&heartbeats).unwrap().len();
    std::thread::sleep(std::time::Duration::from_millis(150));
    let after = std::fs::metadata(&heartbeats).unwrap().len();
    assert!(
        after > before,
        "failed build stopped the last healthy process"
    );
    assert_eq!(
        std::fs::read_to_string(&runs).unwrap().lines().count(),
        healthy_runs,
        "failed build launched a replacement process"
    );

    unsafe {
        libc::kill(dev.id() as i32, libc::SIGINT);
    }
    let status = dev.wait().unwrap();
    assert_eq!(status.code(), Some(130));
    let run_log = std::fs::read_to_string(runs).unwrap();
    assert!(run_log.contains("--config=dev //app:server -- --port 3000"));
}

#[test]
fn completed_action_survives_killed_build() {
    let ws = Workspace::new("journal-kill");
    ws.write(
        "frost.toml",
        "[workspace]\ndefault_targets = [\"slow\"]\n\n[target.fast]\nkind = \"genrule\"\ncmd = \"printf done > ${out}\"\noutputs = [\"gen/fast.txt\"]\n\n[target.slow]\nkind = \"genrule\"\ncmd = \"sleep 10; printf done > ${out}\"\noutputs = [\"gen/slow.txt\"]\ndeps = [\"fast\"]\n",
    );
    let mut child = Command::new(frost_bin())
        .arg("-C")
        .arg(&ws.dir)
        .arg("build")
        .spawn()
        .unwrap();
    for _ in 0..200 {
        if frostbuild_core::journal::Journal::load(&ws.dir)
            .actions
            .contains_key("genrule:fast@debug")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(frostbuild_core::journal::Journal::load(&ws.dir)
        .actions
        .contains_key("genrule:fast@debug"));
    child.kill().unwrap();
    let _ = child.wait();
    let (ok, out) = ws.frost(&["plan"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("would run genrule:fast"),
        "completed action was lost:\n{out}"
    );
    assert!(out.contains("would run genrule:slow"), "{out}");
}

#[test]
fn sandbox_rejects_undeclared_workspace_header() {
    if !Path::new("/usr/bin/bwrap").exists() {
        return;
    }
    let ws = Workspace::new("sandbox");
    ws.write("secret.h", "#define SECRET 0\n");
    ws.write(
        "src/sandbox.c",
        "#include \"../secret.h\"\nint main(void) { return SECRET; }\n",
    );
    let mut manifest = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    manifest.push_str("\n[target.sandbox_app]\nkind = \"cc_binary\"\nsrcs = [\"src/sandbox.c\"]\n");
    ws.write("frost.toml", &manifest);
    let (ok, out) = ws.frost(&["build", "sandbox_app"]);
    assert!(ok, "non-sandbox control build failed:\n{out}");
    let (ok, out) = ws.frost(&["clean", "--cache"]);
    assert!(ok, "{out}");
    let (ok, out) = ws.frost(&["build", "sandbox_app", "--sandbox"]);
    assert!(
        !ok && out.contains("secret.h"),
        "undeclared header was not diagnosed:\n{out}"
    );
}

#[test]
fn strategies_are_selectable_and_measured() {
    let ws = Workspace::new("strategies");
    for (scheduler, estimator) in [
        ("critical-path", "journal"),
        ("critical-path", "learned"),
        ("fifo", "static"),
        ("fifo", "heuristic"),
    ] {
        let dir = ws.dir.join(".frost");
        let _ = std::fs::remove_dir_all(&dir);
        let (ok, out) = ws.frost(&[
            "build",
            "--scheduler",
            scheduler,
            "--estimator",
            estimator,
            "--stats",
        ]);
        assert!(ok, "{scheduler}/{estimator} failed:\n{out}");
        // Every strategy runs the same actions and reports what it cost, so a
        // comparison never depends on rerunning with a stopwatch.
        assert!(out.contains("5 built"), "{out}");
        assert!(
            out.contains(&format!("strategy    {scheduler} / {estimator}")),
            "stats must name the strategy in effect:\n{out}"
        );
        assert!(out.contains("utilization"), "{out}");
        assert!(out.contains("critical"), "{out}");
    }
}

#[test]
fn action_reading_stdin_does_not_hang_the_build() {
    let ws = Workspace::new("stdin");
    // `cat` with no operand reads stdin. If actions inherit the terminal this
    // blocks forever and the build looks slow rather than broken.
    ws.append(
        "frost.toml",
        "\n[target.reads_stdin]\nkind = \"genrule\"\n\
         cmd = \"cat > ${out}\"\noutputs = [\"gen/stdin.txt\"]\n",
    );
    let (ok, out) = ws.frost(&["build", "reads_stdin"]);
    assert!(ok, "build must finish rather than block on stdin:\n{out}");
    assert_eq!(
        std::fs::read_to_string(ws.dir.join("gen/stdin.txt")).unwrap(),
        "",
        "stdin is empty, so the action produces an empty file"
    );
}

#[test]
fn simulate_compares_strategies_without_building() {
    let ws = Workspace::new("simulate");
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    let before = std::fs::read(ws.dir.join(".frost/journal.bin")).unwrap();

    let (ok, out) = ws.frost(&["simulate", "--jobs", "1,4"]);
    assert!(ok, "{out}");
    assert!(out.contains("critical path"), "{out}");
    assert!(out.contains("critical-path / journal"), "{out}");
    assert!(out.contains("fifo / journal"), "{out}");
    assert!(out.contains("-j 4"), "{out}");
    assert!(out.contains("fastest:"), "{out}");

    // Simulation must not touch build state: it is safe to run mid-session.
    assert_eq!(
        std::fs::read(ws.dir.join(".frost/journal.bin")).unwrap(),
        before,
        "simulate must not write to the journal"
    );

    let (ok, json) = ws.frost(&["simulate", "--json"]);
    assert!(ok, "{json}");
    let parsed: serde_json::Value = serde_json::from_str(json.trim()).unwrap();
    assert_eq!(parsed["actions"], 5);
    let points = parsed["points"].as_array().unwrap();
    assert!(!points.is_empty());
    let cp = parsed["critical_path_ms"].as_u64().unwrap();
    for p in points {
        assert!(
            p["makespan_ms"].as_u64().unwrap() >= cp,
            "no schedule beats the critical path: {p}"
        );
    }
}

#[test]
fn a_path_is_stat_checked_once_per_build() {
    let ws = Workspace::new("stat-once");
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");

    // The generated header is gen_config's output and app's order-only input.
    // Both checks run in the same build; the second must reuse the first's
    // result rather than stat the file again.
    let (ok, out) = ws.build_explain();
    assert!(ok && out.contains("up to date"), "{out}");

    // The saving must not cost correctness: a change between builds is still
    // seen, because each build starts from a fresh cache.
    #[cfg(unix)]
    let changed_generator = "#!/bin/sh\nset -eu\ncat > \"$1\" <<'EOF'\n\
                             #ifndef FROST_SAMPLE_CONFIG_H\n\
                             #define FROST_SAMPLE_CONFIG_H\n\
                             #define FROST_GREETING \"frosty:\"\n\
                             #endif\nEOF\n";
    #[cfg(windows)]
    let changed_generator = "@echo off\n\
                             > \"%~1\" echo #ifndef FROST_SAMPLE_CONFIG_H\n\
                             >> \"%~1\" echo #define FROST_SAMPLE_CONFIG_H\n\
                             >> \"%~1\" echo #define FROST_GREETING \"frosty:\"\n\
                             >> \"%~1\" echo #endif\n";
    ws.write(ws.generator_script(), changed_generator);
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    assert!(out.contains("ran genrule:gen_config"), "{out}");
}

#[test]
#[cfg(unix)]
fn daemon_works_from_a_deeply_nested_workspace() {
    // A Unix socket address is capped near 100 bytes. Keeping the socket in
    // the workspace made the daemon unusable a few directories deep, and the
    // failure surfaced as `SUN_LEN` with no mention of paths.
    let ws = Workspace::new("deep");
    // Nest outside the source workspace, or the copy recurses into itself.
    let deep = std::env::temp_dir()
        .join(format!("frost-nested-root-{}", std::process::id()))
        .join("a-directory-with-a-fairly-long-name")
        .join("and-another-level-here-as-well")
        .join("plus-a-third-level-for-good-measure")
        .join("and-a-fourth-one-to-be-quite-sure")
        .join("finally-the-workspace-itself");
    let _ = std::fs::remove_dir_all(deep.ancestors().nth(5).unwrap());
    std::fs::create_dir_all(&deep).unwrap();
    copy_dir(&ws.dir, &deep).unwrap();
    assert!(
        deep.to_string_lossy().len() > 100,
        "the test is pointless unless the path exceeds the socket limit"
    );

    let frost = |args: &[&str]| {
        let out = Command::new(frost_bin())
            .arg("-C")
            .arg(&deep)
            .args(args)
            .output()
            .expect("spawn frost");
        (
            out.status.success(),
            normalized_output(&out.stdout) + &normalized_output(&out.stderr),
        )
    };

    let (ok, out) = frost(&["daemon", "start"]);
    assert!(ok, "daemon must start from a nested workspace:\n{out}");
    let (ok, out) = frost(&["build", "--daemon"]);
    assert!(ok, "build through the daemon failed:\n{out}");
    assert!(out.contains("5 built"), "{out}");
    let (ok, out) = frost(&["daemon", "stop"]);
    assert!(ok, "{out}");
    let _ = std::fs::remove_dir_all(deep.ancestors().nth(5).unwrap());
}

#[test]
fn include_path_environment_selects_a_different_header_and_is_keyed() {
    // CPATH changes which header the compiler finds without touching the
    // command line or any declared input. The depfile records the header that
    // was resolved *last* time, so re-digesting it proves nothing: unless the
    // environment is part of the action key, frost reports everything cached
    // and hands back a binary built against the other header.
    let ws = Workspace::new("cpath");
    let one = ws.dir.join("inc-one");
    let two = ws.dir.join("inc-two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(one.join("tuning.h"), "#define TUNING 1\n").unwrap();
    std::fs::write(two.join("tuning.h"), "#define TUNING 99\n").unwrap();

    let util = std::fs::read_to_string(ws.dir.join("src/util.c")).unwrap();
    ws.write(
        "src/util.c",
        &format!(
            "#include <tuning.h>\n{}",
            util.replace(
                "return a + b + FROST_INTERNAL_BIAS;",
                "return a + b + FROST_INTERNAL_BIAS + TUNING;"
            )
        ),
    );

    let run_with = |dir: &std::path::Path| {
        let (ok, out) = ws.frost_env(&["build"], &[("CPATH", dir.to_str().unwrap())]);
        assert!(ok, "build failed:\n{out}");
        let app = Command::new(ws.binary(".frost/bin/debug/app"))
            .output()
            .expect("run built app");
        (out, normalized_output(&app.stdout))
    };

    let (_, first) = run_with(&one);
    assert_eq!(first, "frost: 43\n");
    let (out, first_warm) = run_with(&one);
    assert_eq!(first_warm, "frost: 43\n");
    assert!(out.contains("up to date"), "{out}");
    assert!(
        ws.dir.join(".frost/noop-debug.bin").is_file(),
        "the environment regression must exercise the fast no-op path"
    );

    let (out, second) = run_with(&two);
    assert_eq!(
        second, "frost: 141\n",
        "a different header must produce a different binary:\n{out}"
    );
    assert!(
        !out.contains("up to date"),
        "the environment change must invalidate, not report everything cached:\n{out}"
    );

    let (_, back) = run_with(&one);
    assert_eq!(back, "frost: 43\n", "switching back is equally observable");
}

#[test]
#[cfg(unix)]
fn daemon_builds_with_the_client_environment_not_the_daemons() {
    // A daemon outlives the shells that talk to it. It used to spawn the child
    // build with its own inherited environment, so a daemon started without
    // CPATH answered `CPATH=<dir> frost build --daemon` with a binary built
    // against the daemon's headers, reported it as built, and then reported
    // "up to date" for a binary matching neither request. The certificate
    // check already used the client's environment; the build did not.
    let ws = Workspace::new("daemon-env");
    let one = ws.dir.join("inc-one");
    let two = ws.dir.join("inc-two");
    std::fs::create_dir_all(&one).unwrap();
    std::fs::create_dir_all(&two).unwrap();
    std::fs::write(one.join("tuning.h"), "#define TUNING 1\n").unwrap();
    std::fs::write(two.join("tuning.h"), "#define TUNING 99\n").unwrap();

    let util = std::fs::read_to_string(ws.dir.join("src/util.c")).unwrap();
    ws.write(
        "src/util.c",
        &format!(
            "#include <tuning.h>\n{}",
            util.replace(
                "return a + b + FROST_INTERNAL_BIAS;",
                "return a + b + FROST_INTERNAL_BIAS + TUNING;"
            )
        ),
    );

    // The daemon deliberately inherits the header directory the client will
    // not ask for.
    let (ok, out) = ws.frost_env(&["daemon", "start"], &[("CPATH", one.to_str().unwrap())]);
    assert!(ok, "{out}");

    let build_with = |dir: &std::path::Path| {
        let (ok, out) = ws.frost_env(&["build", "--daemon"], &[("CPATH", dir.to_str().unwrap())]);
        assert!(ok, "daemon build failed:\n{out}");
        (out, ws.run_app())
    };

    let (out, first) = build_with(&two);
    assert_eq!(
        first, "frost: 141\n",
        "the daemon must build what the client asked for:\n{out}"
    );
    let (out, warm) = build_with(&two);
    assert!(out.contains("up to date"), "{out}");
    assert_eq!(warm, "frost: 141\n", "a cached hit must keep that binary");

    let (out, switched) = build_with(&one);
    assert_eq!(
        switched, "frost: 43\n",
        "switching the client environment must invalidate through the daemon:\n{out}"
    );

    let (ok, out) = ws.frost(&["daemon", "stop"]);
    assert!(ok && out.contains("stopped"), "{out}");
}

#[test]
fn a_glob_that_matches_nothing_is_reported_where_it_is_written() {
    // A typo in a srcs glob used to produce an empty archive that built
    // happily, and the build then failed at the link with a message about
    // symbols — nowhere near the cause.
    let ws = Workspace::new("empty-glob");
    let good = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(
        ok,
        "the workspace builds before the typo is introduced:\n{out}"
    );

    ws.append(
        "frost.toml",
        "\n[target.typo]\nkind = \"cc_library\"\nsrcs = [\"srcs/**/*.c\"]\n",
    );
    let (ok, out) = ws.frost(&["build", "typo"]);
    assert!(!ok, "an empty glob must not build:\n{out}");
    assert!(out.contains("matched no files"), "{out}");
    assert!(out.contains("typo"), "the target must be named:\n{out}");

    // The manifest is rejected as a whole, so an unrelated target cannot be
    // built around it either — a broken manifest is broken for everyone.
    let (ok, out) = ws.frost(&["build", "util"]);
    assert!(!ok, "{out}");
    assert!(out.contains("matched no files"), "{out}");

    ws.write("frost.toml", &good);
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "removing the typo restores the build:\n{out}");
}

#[test]
fn init_writes_a_manifest_that_actually_builds() {
    // Running frost in a directory with sources but no manifest used to end
    // at an error with no next step. The scaffold has to be good enough to
    // build as written, or it is just a different dead end.
    let dir = std::env::temp_dir().join(format!("frost-init-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("include")).unwrap();
    std::fs::write(
        dir.join("src/main.c"),
        "#include <stdio.h>\n#include \"util.h\"\n\
         int main(void) { printf(\"%d\\n\", add(20, 22)); return 0; }\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("src/util.c"),
        "#include \"util.h\"\nint add(int a, int b) { return a + b; }\n",
    )
    .unwrap();
    std::fs::write(dir.join("include/util.h"), "int add(int, int);\n").unwrap();

    let frost = |args: &[&str]| {
        let out = Command::new(frost_bin())
            .arg("-C")
            .arg(&dir)
            .args(args)
            .output()
            .expect("spawn frost");
        (
            out.status.success(),
            normalized_output(&out.stdout) + &normalized_output(&out.stderr),
        )
    };

    let (ok, out) = frost(&["build"]);
    assert!(!ok);
    assert!(
        out.contains("frost init"),
        "the error must name a next step:\n{out}"
    );

    let (ok, out) = frost(&["init"]);
    assert!(ok, "{out}");
    assert!(
        out.contains("src/main.c"),
        "the summary names the entry point:\n{out}"
    );
    let manifest = std::fs::read_to_string(dir.join("frost.toml")).unwrap();
    assert!(manifest.contains("[profile.debug]"), "{manifest}");
    assert!(
        manifest.contains("cflags = [\"-O0\", \"-g\"]"),
        "{manifest}"
    );

    let (ok, out) = frost(&["build"]);
    assert!(ok, "the scaffold must build as written:\n{out}");
    let run = Command::new(executable_path(
        dir.join(".frost/bin/debug").join(dir.file_name().unwrap()),
    ))
    .output()
    .expect("run built binary");
    assert_eq!(normalized_output(&run.stdout), "42\n");

    let target = dir.file_name().unwrap().to_str().unwrap();
    let (ok, out) = frost(&["run", target]);
    assert!(ok, "run must build and execute the target:\n{out}");
    assert!(out.contains("frost: run"), "{out}");
    assert!(out.ends_with("42\n"), "{out}");

    std::fs::create_dir_all(dir.join("tools")).unwrap();
    std::fs::write(
        dir.join("tools/fake-gdb"),
        "#!/bin/sh\nprintf '%s\\n' \"$@\" > debug-argv.txt\n",
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(
            dir.join("tools/fake-gdb"),
            std::fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        let (ok, out) = frost(&[
            "debug",
            target,
            "--debugger",
            "tools/fake-gdb",
            "--",
            "hello",
        ]);
        assert!(ok, "debug launch failed:\n{out}");
        assert!(out.contains("frost: debug"), "{out}");
        let argv = std::fs::read_to_string(dir.join("debug-argv.txt")).unwrap();
        assert!(argv.lines().next() == Some("--args"), "{argv}");
        assert!(argv.contains(".frost/bin/debug"), "{argv}");
        assert!(argv.lines().last() == Some("hello"), "{argv}");
    }

    let (ok, out) = frost(&["ide", target, "--dry-run"]);
    assert!(ok, "IDE preview failed:\n{out}");
    assert!(out.contains("\"type\": \"cppdbg\""), "{out}");
    assert!(out.contains(&format!("frost: build {target}")), "{out}");
    let (ok, out) = frost(&["ide", target]);
    assert!(ok, "IDE generation failed:\n{out}");
    let launch: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(".vscode/launch.json")).unwrap()).unwrap();
    let tasks: serde_json::Value =
        serde_json::from_slice(&std::fs::read(dir.join(".vscode/tasks.json")).unwrap()).unwrap();
    assert_eq!(launch["configurations"][0]["type"], "cppdbg");
    assert_eq!(
        launch["configurations"][0]["preLaunchTask"],
        format!("frost: build {target}")
    );
    assert_eq!(tasks["tasks"][0]["command"], "frost");
    let (ok, out) = frost(&["ide", target]);
    assert!(!ok, "IDE generation must not overwrite user files");
    assert!(out.contains("--dry-run"), "{out}");

    let (ok, out) = frost(&["doctor", "--json"]);
    assert!(ok, "doctor rejected a buildable scaffold:\n{out}");
    let diagnosis: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(diagnosis["status"], "ready");
    assert!(diagnosis["required_tools"].as_array().unwrap().len() >= 4);

    // init refuses to clobber an existing manifest, and says how to look
    // without writing.
    let (ok, out) = frost(&["init"]);
    assert!(!ok, "{out}");
    assert!(out.contains("--dry-run"), "{out}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn init_java_writes_a_runnable_deterministic_jar_manifest() {
    if !java_toolchain_is_consistent() {
        eprintln!("skipping Java init E2E: javac and java must be present and from the same JDK");
        return;
    }

    let ws = Workspace::empty("init-java");
    std::fs::create_dir_all(ws.dir.join("src/main/java/com/example")).unwrap();
    ws.write(
        "src/main/java/com/example/App.java",
        "package com.example;\n\
         public final class App {\n\
           public static void main(String[] args) {\n\
             System.out.println(\"java-init-ok\");\n\
           }\n\
         }\n",
    );

    // The generated manifest intentionally names `frost`, just as a user's
    // installed manifest will. Cargo exposes the test binary by absolute path,
    // so put its directory on PATH to exercise that installed-command shape.
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let frost_parent = Path::new(frost_bin()).parent().unwrap().to_path_buf();
    let path = std::env::join_paths(
        std::iter::once(frost_parent).chain(std::env::split_paths(&current_path)),
    )
    .unwrap();
    let frost = |args: &[&str]| {
        let output = Command::new(frost_bin())
            .arg("-C")
            .arg(&ws.dir)
            .args(args)
            .env("PATH", &path)
            .output()
            .expect("spawn frost");
        (
            output.status.success(),
            normalized_output(&output.stdout) + &normalized_output(&output.stderr),
        )
    };

    let (ok, out) = frost(&["init"]);
    assert!(ok, "Java auto-detection failed:\n{out}");
    assert!(out.contains("1 Java source file(s)"), "{out}");
    assert!(out.contains("entry point: com.example.App"), "{out}");

    let manifest_text = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    let manifest = frostbuild_core::manifest::Manifest::parse_str(&manifest_text).unwrap();
    let target = ws.dir.file_name().unwrap().to_str().unwrap();
    let spec = &manifest.targets[target];
    assert_eq!(spec.tool.as_deref(), Some("javac"));
    assert_eq!(spec.steps.len(), 1);
    assert_eq!(spec.steps[0].tool, "frost");
    assert!(
        spec.steps[0]
            .args
            .windows(2)
            .any(|args| args == ["--main-class", "com.example.App"]),
        "{:?}",
        spec.steps[0].args
    );

    let (ok, out) = frost(&["build"]);
    assert!(ok, "generated Java manifest did not build:\n{out}");
    let jar = ws
        .dir
        .join(".frost/out/debug")
        .join(format!("{target}.jar"));
    let direct = Command::new("java")
        .arg("-jar")
        .arg(&jar)
        .output()
        .expect("run generated Java JAR");
    assert!(direct.status.success(), "{direct:?}");
    assert_eq!(normalized_output(&direct.stdout), "java-init-ok\n");

    let (ok, out) = frost(&["run", target]);
    assert!(ok, "frost run rejected generated Java target:\n{out}");
    assert!(out.contains("`-- runtime   Java"), "{out}");
    assert!(out.ends_with("java-init-ok\n"), "{out}");

    let (ok, out) = frost(&[
        "debug",
        target,
        "--debugger",
        frost_bin(),
        "--print",
        "--",
        "argument",
    ]);
    assert!(ok, "Java debug preview rejected generated JAR:\n{out}");
    assert!(out.contains("Java/jdb"), "{out}");
    assert!(out.contains("-classpath"), "{out}");
    assert!(out.contains("com.example.App"), "{out}");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn init_mixed_workspace_requires_an_explicit_language() {
    let ws = Workspace::empty("init-mixed");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write("src/main.c", "int main(void) { return 0; }\n");
    ws.write(
        "src/App.java",
        "public final class App { public static void main(String[] args) {} }\n",
    );

    let (ok, out) = ws.frost(&["init"]);
    assert!(!ok, "mixed source families must not be guessed:\n{out}");
    assert!(out.contains("--language native"), "{out}");
    assert!(out.contains("--language java"), "{out}");
    assert!(!ws.dir.join("frost.toml").exists());

    let (ok, out) = ws.frost(&["init", "--language", "java", "--dry-run"]);
    assert!(ok, "explicit Java preview failed:\n{out}");
    assert!(out.contains("inputs = [\"src/App.java\"]"), "{out}");
    assert!(!out.contains("src/main.c"), "{out}");
    assert!(!ws.dir.join("frost.toml").exists());
}

/// The generated manifests name `frost` and the language driver the way an
/// installed workspace does, so the test runs them through PATH rather than
/// Cargo's absolute test-binary path.
fn frost_in(dir: &Path, args: &[&str]) -> (bool, String) {
    let current_path = std::env::var_os("PATH").unwrap_or_default();
    let frost_parent = Path::new(frost_bin()).parent().unwrap().to_path_buf();
    let path = std::env::join_paths(
        std::iter::once(frost_parent).chain(std::env::split_paths(&current_path)),
    )
    .unwrap();
    let output = Command::new(frost_bin())
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("PATH", &path)
        .output()
        .expect("spawn frost");
    (
        output.status.success(),
        normalized_output(&output.stdout) + &normalized_output(&output.stderr),
    )
}

fn language_tool_available(tool: &str) -> bool {
    Command::new(tool)
        .arg("--version")
        .output()
        .is_ok_and(|output| output.status.success())
}

fn init_dry_run_snapshot(ws: &Workspace, language: &str) -> String {
    let (ok, snapshot) = frost_in(&ws.dir, &["init", "--language", language, "--dry-run"]);
    assert!(ok, "{language} dry run failed:\n{snapshot}");
    assert!(
        snapshot.starts_with("# Generated by `frost init`"),
        "{snapshot}"
    );
    assert!(snapshot.contains("# Next: frost build"), "{snapshot}");
    assert!(snapshot.contains("# TODO:"), "{snapshot}");
    assert!(
        !ws.dir.join("frost.toml").exists(),
        "dry run wrote frost.toml"
    );
    snapshot
}

fn assert_init_matches_snapshot(ws: &Workspace, language: &str, snapshot: &str) -> String {
    let (ok, out) = frost_in(&ws.dir, &["init"]);
    assert!(ok, "{language} auto-detection failed:\n{out}");
    assert_eq!(
        std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap(),
        snapshot,
        "{language} dry-run output diverged from the written manifest"
    );
    out
}

#[test]
fn init_rust_builds_directly_and_reruns_only_on_a_real_change() {
    if cfg!(windows) || !language_tool_available("rustc") || !rust_toolchain_is_consistent() {
        eprintln!("skipping Rust init E2E: a consistent rustc is required");
        return;
    }

    let ws = Workspace::empty("init-rust");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write(
        "src/main.rs",
        "mod helper;\nfn main() { println!(\"rust-init-ok {}\", helper::value()); }\n",
    );
    ws.write("src/helper.rs", "pub fn value() -> i32 { 42 }\n");

    let snapshot = init_dry_run_snapshot(&ws, "rust");
    let out = assert_init_matches_snapshot(&ws, "rust", &snapshot);
    assert!(out.contains("entry point: src/main.rs"), "{out}");

    let target = ws.dir.file_name().unwrap().to_str().unwrap().to_string();
    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "generated Rust manifest did not build:\n{out}");
    let binary = ws.dir.join(".frost/out/debug").join(&target);
    let run = Command::new(&binary).output().expect("run rust artifact");
    assert_eq!(normalized_output(&run.stdout), "rust-init-ok 42\n");

    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // A module the crate root pulls in is a declared input, so editing it
    // must rebuild even though rustc — not Frost — reads it.
    ws.write("src/helper.rs", "pub fn value() -> i32 { 7 }\n");
    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "editing a declared module did not rebuild:\n{out}");
    let run = Command::new(&binary).output().expect("run rust artifact");
    assert_eq!(normalized_output(&run.stdout), "rust-init-ok 7\n");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn init_go_builds_the_package_main_of_a_module() {
    if !language_tool_available("go") {
        eprintln!("skipping Go init E2E: go is required");
        return;
    }

    let ws = Workspace::empty("init-go");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write("go.mod", "module initdemo\n\ngo 1.21\n");
    ws.write(
        "src/main.go",
        "package main\nimport \"fmt\"\nfunc main() { fmt.Println(\"go-init-ok\") }\n",
    );

    let snapshot = init_dry_run_snapshot(&ws, "go");
    let out = assert_init_matches_snapshot(&ws, "go", &snapshot);
    assert!(out.contains("building package ./src"), "{out}");

    let target = ws.dir.file_name().unwrap().to_str().unwrap().to_string();
    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "generated Go manifest did not build:\n{out}");
    let run = Command::new(ws.dir.join(".frost/out/debug").join(&target))
        .output()
        .expect("run go artifact");
    assert_eq!(normalized_output(&run.stdout), "go-init-ok\n");

    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn init_typescript_owns_the_directory_tsc_names() {
    if !language_tool_available("tsc") {
        eprintln!("skipping TypeScript init E2E: tsc is required");
        return;
    }

    let ws = Workspace::empty("init-typescript");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write(
        "src/index.ts",
        "export const message: string = 'ts-init-ok';\nconsole.log(message);\n",
    );

    let snapshot = init_dry_run_snapshot(&ws, "typescript");
    let out = assert_init_matches_snapshot(&ws, "typescript", &snapshot);
    assert!(out.contains("1 TypeScript source file(s)"), "{out}");

    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "generated TypeScript manifest did not build:\n{out}");
    let emitted = ws.dir.join("dist/debug/index.js");
    assert!(emitted.is_file(), "tsc emitted nothing into the owned tree");

    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // Frost owns the directory, so a stale file the compiler never wrote does
    // not survive the next run.
    std::fs::write(ws.dir.join("dist/debug/stale.js"), "// stale\n").unwrap();
    ws.write(
        "src/index.ts",
        "export const message: string = 'ts-init-changed';\nconsole.log(message);\n",
    );
    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "changed TypeScript source did not rebuild:\n{out}");
    assert!(!ws.dir.join("dist/debug/stale.js").exists(), "{out}");
    assert!(emitted.is_file());

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn init_python_packs_a_byte_identical_wheel() {
    let ws = Workspace::empty("init-python");
    std::fs::create_dir_all(ws.dir.join("src/init_demo")).unwrap();
    ws.write(
        "src/init_demo/__init__.py",
        "def message():\n    return 'python-init-ok'\n",
    );
    ws.write(
        "pyproject.toml",
        "[project]\nname = \"init-demo\"\nversion = \"1.2.3\"\n",
    );

    let snapshot = init_dry_run_snapshot(&ws, "python");
    let out = assert_init_matches_snapshot(&ws, "python", &snapshot);
    assert!(out.contains("distribution init-demo 1.2.3"), "{out}");

    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "generated Python manifest did not build:\n{out}");
    let wheel = ws
        .dir
        .join(".frost/out/debug/init_demo-1.2.3-py3-none-any.whl");
    let first = std::fs::read(&wheel).expect("packed wheel");

    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok && out.contains("up to date"), "{out}");

    // The wheel is the artifact frost promises to reproduce, so a clean
    // rebuild must return the same bytes rather than a new timestamp.
    let (ok, out) = frost_in(&ws.dir, &["clean"]);
    assert!(ok, "clean failed:\n{out}");
    let (ok, out) = frost_in(&ws.dir, &["build"]);
    assert!(ok, "rebuild after clean failed:\n{out}");
    assert_eq!(first, std::fs::read(&wheel).expect("repacked wheel"));

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn completions_install_is_idempotent_and_never_clobbers_a_hand_written_hook() {
    let ws = Workspace::empty("completions-install");
    let home = ws.dir.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let rc = home.join(".bashrc");
    std::fs::write(&rc, "# user rc\nexport EDITOR=vim\n").unwrap();
    let env = [("HOME", home.to_str().unwrap())];

    let (ok, out) = ws.frost_env(&["completions", "bash", "--install", "--dry-run"], &env);
    assert!(ok, "{out}");
    assert!(out.contains("would write"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&rc).unwrap(),
        "# user rc\nexport EDITOR=vim\n",
        "--dry-run must not touch the file"
    );

    let (ok, out) = ws.frost_env(&["completions", "bash", "--install"], &env);
    assert!(ok, "{out}");
    let installed = std::fs::read_to_string(&rc).unwrap();
    assert!(
        installed.contains("source <(COMPLETE=bash frost)"),
        "{installed}"
    );
    assert!(installed.starts_with("# user rc\n"), "{installed}");

    // Running it twice is the common case — a second hook would redefine the
    // completion function in every new shell.
    let (ok, out) = ws.frost_env(&["completions", "bash", "--install"], &env);
    assert!(ok, "{out}");
    assert!(out.contains("already installed"), "{out}");
    assert_eq!(std::fs::read_to_string(&rc).unwrap(), installed);

    // A hook the user wrote by hand is theirs, not ours to duplicate.
    std::fs::write(&rc, "source <(COMPLETE=bash frost)\n").unwrap();
    let (ok, out) = ws.frost_env(&["completions", "bash", "--install"], &env);
    assert!(ok, "{out}");
    assert!(out.contains("leaving it alone"), "{out}");
    assert_eq!(
        std::fs::read_to_string(&rc).unwrap(),
        "source <(COMPLETE=bash frost)\n"
    );

    // Without an argument the shell has to be recognizable.
    let (ok, out) = ws.frost_env(
        &["completions", "--install"],
        &[("HOME", home.to_str().unwrap()), ("SHELL", "/bin/tcsh")],
    );
    assert!(!ok, "an unrecognized shell must not be guessed:\n{out}");
    assert!(out.contains("name it"), "{out}");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
#[cfg(unix)]
fn a_hanging_action_is_stopped_and_never_recorded_as_done() {
    let ws = Workspace::empty("timeout");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["hang"]

[toolchain.tools]
sleeper = "sleep"

[target.hang]
kind = "command"
tool = "sleeper"
args = ["120"]
inputs = []
outputs = [".frost/out/${config}/hang.txt"]
timeout = 2
sandbox = false
"#,
    );

    let started = std::time::Instant::now();
    let (ok, out) = ws.frost(&["build"]);
    let elapsed = started.elapsed();
    assert!(!ok, "a timed-out action must fail the build:\n{out}");
    assert!(
        elapsed < std::time::Duration::from_secs(60),
        "the limit did not stop the action: {elapsed:?}"
    );
    // The message has to name the limit and where it came from, or the reader
    // has to go looking for which of three places set it.
    assert!(out.contains("timed out after 2s"), "{out}");
    assert!(out.contains("target hang"), "{out}");

    // A limit says nothing about the result, so nothing may be recorded: the
    // next build must run the action again rather than replay a failure.
    let (ok, out) = ws.frost(&["build"]);
    assert!(!ok, "{out}");
    assert!(out.contains("timed out after 2s"), "{out}");
    assert!(!out.contains("cached"), "a timeout was cached:\n{out}");

    // The invocation can impose a limit where the manifest declares none.
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["hang"]

[toolchain.tools]
sleeper = "sleep"

[target.hang]
kind = "command"
tool = "sleeper"
args = ["120"]
inputs = []
outputs = [".frost/out/${config}/hang.txt"]
sandbox = false
"#,
    );
    let (ok, out) = ws.frost(&["build", "--timeout", "1"]);
    assert!(!ok, "{out}");
    assert!(out.contains("timed out after 1s (--timeout)"), "{out}");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn stale_on_disk_state_is_rebuilt_rather_than_misread() {
    // docs/28_compatibility_contract.md promises that `.frost/` written by
    // another version costs time, never correctness. This walks every stored
    // format at once: each keeps its structure and gets a version marker this
    // build cannot claim, which is exactly what a downgrade or an upgrade
    // across a format bump leaves behind.
    let ws = Workspace::new("stale-state");
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "initial build failed:\n{out}");
    assert_eq!(ws.run_app(), "frost: 42\n");

    let mut stamped = Vec::new();
    let mut stamp = |relative: &str, marker: &[u8]| {
        let path = ws.dir.join(relative);
        let Ok(mut bytes) = std::fs::read(&path) else {
            return;
        };
        if bytes.len() < marker.len() {
            return;
        }
        bytes[..marker.len()].copy_from_slice(marker);
        std::fs::write(&path, &bytes).unwrap();
        stamped.push(relative.to_string());
    };
    stamp(".frost/journal.bin", b"FRSTJR99");
    stamp(".frost/hashcache.bin", b"FRSTHC99");
    stamp(".frost/noop-debug.bin", b"FRSTNO99");
    // The graph store keeps its magic and carries the version in the four
    // bytes after it, so this is a version bump rather than a corrupt file.
    let graph = ws.dir.join(".frost/graph-debug.bin");
    if let Ok(mut bytes) = std::fs::read(&graph) {
        if bytes.len() >= 12 {
            bytes[8..12].copy_from_slice(&u32::MAX.to_le_bytes());
            std::fs::write(&graph, &bytes).unwrap();
            stamped.push(".frost/graph-debug.bin".to_string());
        }
    }
    assert!(
        stamped.len() >= 3,
        "expected the build to leave several stored formats behind, saw {stamped:?}"
    );

    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "a foreign .frost/ must not fail the build:\n{out}");
    assert_eq!(
        ws.run_app(),
        "frost: 42\n",
        "rebuild produced a wrong binary"
    );

    // Rebuilding from the manifest is the whole point: the state that could
    // not be read must not be reported as an up-to-date answer either.
    assert!(
        !out.contains("up to date"),
        "unreadable state was treated as a warm cache:\n{out}"
    );

    // And the workspace converges: the next build is warm again.
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    assert!(out.contains("up to date"), "state did not converge:\n{out}");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn info_answers_path_questions_without_a_graph() {
    // A fresh directory has no manifest and no graph. `info` still has to
    // answer, because that is when a wrapper needs the paths most.
    let ws = Workspace::empty("info");

    let (ok, out) = ws.frost(&["info", "--json"]);
    assert!(ok, "info failed on a workspace without a manifest:\n{out}");
    let table: serde_json::Value = serde_json::from_str(&out).unwrap();
    // Compare resolved paths: macOS reaches its temporary directory through a
    // symlinked `/var`, so the spelling frost reports and the one this test
    // holds can differ while naming the same directory.
    let reported_root = table["workspace_root"].as_str().unwrap().to_string();
    assert_eq!(
        std::fs::canonicalize(&reported_root).unwrap(),
        std::fs::canonicalize(&ws.dir).unwrap()
    );
    assert_eq!(table["config"], "debug");
    assert_eq!(table["action_key_schema"], "frost-action-key-v4");
    assert_eq!(table["version"], env!("CARGO_PKG_VERSION"));
    for key in ["output_dir", "bin_dir", "cas_dir", "journal", "graph_store"] {
        assert!(table[key].is_string(), "{key} missing from {table}");
    }

    // One key prints its bare value so a shell can substitute it directly.
    // The suffixes are matched by path component so the separator the host
    // uses is not part of the contract.
    let (ok, out) = ws.frost(&["info", "bin_dir"]);
    assert!(ok, "{out}");
    let bin_dir = Path::new(out.trim_end()).to_path_buf();
    assert!(bin_dir.starts_with(&reported_root), "{out}");
    assert!(bin_dir.ends_with(".frost/bin/debug"), "{out}");

    let (ok, out) = ws.frost(&["info", "output_dir", "--profile", "release"]);
    assert!(ok, "{out}");
    assert!(
        Path::new(out.trim_end()).ends_with(".frost/out/release"),
        "{out}"
    );

    // A cross configuration is a nested tree, and info must say so rather
    // than leaving callers to rebuild the rule.
    let (ok, out) = ws.frost(&["info", "output_dir", "--platform", "device"]);
    assert!(ok, "{out}");
    assert!(
        Path::new(out.trim_end()).ends_with(".frost/out/device/debug"),
        "{out}"
    );

    let (ok, out) = ws.frost(&["info", "not_a_key"]);
    assert!(!ok, "an unknown key must not be reported as empty:\n{out}");
    assert!(out.contains("known keys"), "{out}");

    let _ = std::fs::remove_dir_all(&ws.dir);
}

#[test]
fn doctor_separates_missing_required_tools_from_optional_integrations() {
    let ws = Workspace::empty("doctor-missing");
    ws.write("input.txt", "input\n");
    ws.write(
        "frost.toml",
        r#"[workspace]
default_targets = ["artifact"]

[toolchain.tools]
missing = "definitely-not-a-real-frost-tool"

[target.artifact]
kind = "command"
tool = "missing"
args = ["${in}", "${out}"]
inputs = ["input.txt"]
outputs = [".frost/out/${config}/artifact.txt"]
sandbox = false
"#,
    );
    let (ok, out) = ws.frost(&["doctor", "--json"]);
    assert!(!ok, "missing required tool must make doctor nonzero");
    let diagnosis: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(diagnosis["status"], "blocked");
    let tools = diagnosis["required_tools"].as_array().unwrap();
    assert!(tools.iter().any(|tool| {
        tool["configured"] == "definitely-not-a-real-frost-tool"
            && tool["available"] == false
            && tool["required"] == true
    }));
    assert!(diagnosis["optional_integrations"].is_array());
}

#[test]
fn init_refuses_a_directory_it_cannot_describe() {
    let dir = std::env::temp_dir().join(format!("frost-init-empty-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    // Shell and Make are real build inputs that belong to no family init can
    // name an artifact for; it must say so instead of inventing a target.
    std::fs::write(dir.join("Makefile"), "all:\n\t@echo built\n").unwrap();
    std::fs::write(dir.join("run.sh"), "#!/bin/sh\necho run\n").unwrap();

    let out = Command::new(frost_bin())
        .arg("-C")
        .arg(&dir)
        .arg("init")
        .output()
        .expect("spawn frost");
    assert!(
        !out.status.success(),
        "init only auto-detects artifact-safe source families"
    );
    let text = String::from_utf8_lossy(&out.stderr);
    assert!(text.contains("no safely scaffoldable"), "{text}");
    assert!(text.contains("kind = \"command\""), "{text}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn a_mode_change_invalidates_and_a_restored_output_keeps_its_mode() {
    use std::os::unix::fs::PermissionsExt;

    // `chmod -x` on a script a genrule runs leaves every byte in place. With
    // a content-only digest frost reported the build as current while a clean
    // build of the same tree failed — the cache disagreeing with the source.
    let ws = Workspace::new("mode");
    ws.write("tools/run.sh", "#!/bin/sh\nprintf 'ran\\n' > \"$1\"\n");
    std::fs::set_permissions(
        ws.dir.join("tools/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    ws.append(
        "frost.toml",
        "\n[target.viashell]\nkind = \"genrule\"\ncmd = \"./tools/run.sh ${out}\"\n\
         inputs = [\"tools/run.sh\"]\noutputs = [\"gen/ran.txt\"]\n",
    );

    let (ok, out) = ws.frost(&["build", "viashell"]);
    assert!(ok, "{out}");

    std::fs::set_permissions(
        ws.dir.join("tools/run.sh"),
        std::fs::Permissions::from_mode(0o644),
    )
    .unwrap();
    let (ok, out) = ws.frost(&["build", "viashell"]);
    assert!(
        !ok,
        "a build that a clean tree cannot reproduce must not report success:\n{out}"
    );
    assert!(!out.contains("up to date"), "{out}");

    // Restoring the bit restores the build.
    std::fs::set_permissions(
        ws.dir.join("tools/run.sh"),
        std::fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    let (ok, out) = ws.frost(&["build", "viashell"]);
    assert!(ok, "{out}");

    // An executable output restored from the CAS has to come back executable,
    // or the next action that runs it fails.
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    let app = ws.binary(".frost/bin/debug/app");
    let before = std::fs::metadata(&app).unwrap().permissions().mode();
    assert!(before & 0o111 != 0, "the built binary is executable");
    std::fs::remove_file(&app).unwrap();
    let (ok, out) = ws.frost(&["build"]);
    assert!(ok, "{out}");
    assert_eq!(
        std::fs::metadata(&app).unwrap().permissions().mode() & 0o111,
        before & 0o111,
        "a binary restored from the CAS must still be executable"
    );
    assert_eq!(ws.run_app(), "frost: 42\n");
}

#[test]
#[cfg(unix)]
fn a_different_toolchain_binary_invalidates_the_workspace() {
    // The fingerprint covers the resolved driver binaries, so pointing the
    // manifest at a different one has to invalidate even though no source
    // changed. (That the shell is in the same set is asserted in
    // frostbuild-exec, where the stamp can be read directly — swapping the
    // machine's /bin/sh is not something a test should do.)
    let ws = Workspace::new("shell");
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    let (ok, out) = ws.build_explain();
    assert!(ok && out.contains("up to date"), "{out}");

    // Point the workspace at a private copy of the shell, so the fingerprint
    // has something to notice without touching the machine's /bin/sh.
    let fake = ws.dir.join("fake-cc");
    std::fs::copy("/bin/sh", &fake).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    // The sample leaves the driver to the host default, so this declares one.
    ws.write(
        "frost.toml",
        &std::fs::read_to_string(ws.dir.join("frost.toml"))
            .unwrap()
            .replace(
                "[toolchain]\n",
                &format!("[toolchain]\ncc = {:?}\n", fake.to_str().unwrap()),
            ),
    );
    let (_, out) = ws.build_explain();
    assert!(
        !out.contains("up to date"),
        "a different C driver must invalidate:\n{out}"
    );
}

#[test]
fn a_corrupt_cas_object_is_rebuilt_rather_than_handed_back() {
    // The CAS is content-addressed: an object's name is its digest. An object
    // that no longer hashes to its own name is corrupt, and restoring it
    // would deliver an artifact that never existed while reporting the build
    // as current — the worst failure a build system has.
    let ws = Workspace::new("cas-corrupt");
    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    let app = ws.binary(".frost/bin/debug/app");
    let correct = std::fs::read(&app).unwrap();

    // Find the object backing the built binary and flip one byte, keeping the
    // size identical so nothing but a content check can notice.
    let mut objects = Vec::new();
    let mut stack = vec![ws.dir.join(".frost/cas")];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else {
                objects.push(path);
            }
        }
    }
    let object = objects
        .iter()
        .find(|p| std::fs::read(p).is_ok_and(|b| b == correct))
        .expect("the built binary is in the CAS");
    let mut bytes = std::fs::read(object).unwrap();
    let middle = bytes.len() / 2;
    bytes[middle] ^= 0xFF;
    std::fs::write(object, &bytes).unwrap();
    std::fs::remove_file(&app).unwrap();

    let (ok, out) = ws.build_explain();
    assert!(ok, "{out}");
    assert!(
        !out.contains("up to date"),
        "a corrupt object must not be restored as if current:\n{out}"
    );
    let rebuilt = std::fs::read(&app).unwrap();
    assert_ne!(
        rebuilt, bytes,
        "the corrupt CAS bytes must never be materialized as the rebuilt output"
    );
    // Re-execution is the correctness boundary here. Toolchains are allowed
    // to produce semantically equivalent but byte-different artifacts (for
    // example PE link timestamps), so do not turn this corruption test into
    // an undeclared determinism test.
    assert_eq!(ws.run_app(), "frost: 42\n");
}

// ---------------------------------------------------------------------------
// frostw: the version this repository requires, not the one this machine has.
//
// The download path is the interesting one and it must not need the network,
// so these serve the GitHub release layout — `v<version>/SHA256SUMS` and
// `v<version>/frostbuild-v<version>-<triple>.<ext>` — from a loopback socket
// and point the wrapper at it.
// ---------------------------------------------------------------------------

/// The release triple the wrappers derive for this host, or `None` where no
/// release is published — in which case the wrapper's job is to say so, and
/// there is no download to test.
fn release_triple() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => Some("x86_64-unknown-linux-musl"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("macos", "x86_64") => Some("x86_64-apple-darwin"),
        ("windows", "x86_64") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// The wrapper bootstraps with what the host already ships. Where one of those
/// is missing the wrapper reports it and stops, which is correct behavior and
/// not what these cases are about.
fn wrapper_prerequisites_present() -> bool {
    let has = |tool: &str| {
        Command::new(tool)
            .arg("--version")
            .output()
            .is_ok_and(|out| out.status.success())
    };
    if cfg!(windows) {
        has("curl.exe") && has("tar.exe")
    } else {
        (has("curl") || has("wget")) && (has("sha256sum") || has("shasum")) && has("tar")
    }
}

/// Serves a fixed set of paths over HTTP on loopback, counting requests so a
/// test can assert that no download happened at all.
struct ReleaseServer {
    base_url: String,
    requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl ReleaseServer {
    fn start(files: Vec<(String, Vec<u8>)>) -> Self {
        use std::io::{BufRead, BufReader, Write};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind loopback");
        let port = listener.local_addr().expect("local addr").port();
        let requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = std::sync::Arc::clone(&requests);
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                counter.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                let Ok(peer) = stream.try_clone() else {
                    continue;
                };
                let mut reader = BufReader::new(peer);
                let mut request = String::new();
                if reader.read_line(&mut request).is_err() {
                    continue;
                }
                // Headers, to the blank line. Nothing here reads a body.
                loop {
                    let mut header = String::new();
                    match reader.read_line(&mut header) {
                        Ok(n) if n > 2 => continue,
                        _ => break,
                    }
                }
                let path = request.split_whitespace().nth(1).unwrap_or_default();
                let body = files
                    .iter()
                    .find(|(served, _)| served == path)
                    .map(|(_, bytes)| bytes.clone());
                let response = match body {
                    Some(bytes) => {
                        let mut out = format!(
                            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\
                             Content-Type: application/octet-stream\r\nConnection: close\r\n\r\n",
                            bytes.len()
                        )
                        .into_bytes();
                        out.extend_from_slice(&bytes);
                        out
                    }
                    None => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\
                              Connection: close\r\n\r\n"
                        .to_vec(),
                };
                let _ = stream.write_all(&response);
                let _ = stream.flush();
            }
        });
        Self {
            base_url: format!("http://127.0.0.1:{port}"),
            requests,
        }
    }

    fn requests(&self) -> usize {
        self.requests.load(std::sync::atomic::Ordering::SeqCst)
    }
}

/// Pack the real `frost` binary into the archive layout release.yml publishes,
/// and return `(asset name, bytes)`.
fn release_archive(scratch: &Path, version: &str, triple: &str) -> (String, Vec<u8>) {
    let name = format!("frostbuild-v{version}-{triple}");
    let stage = scratch.join("stage");
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(stage.join(&name)).expect("stage the archive");
    std::fs::copy(
        frost_bin(),
        executable_path(stage.join(&name).join("frost")),
    )
    .expect("copy frost into the archive");

    let asset = if cfg!(windows) {
        format!("{name}.zip")
    } else {
        format!("{name}.tar.gz")
    };
    let archive = scratch.join(&asset);
    let mut tar = Command::new("tar");
    if cfg!(windows) {
        tar.args(["-c", "--format=zip", "-f"]);
    } else {
        tar.args(["-c", "-z", "-f"]);
    }
    let packed = tar
        .arg(&archive)
        .arg("-C")
        .arg(&stage)
        .arg(&name)
        .output()
        .expect("spawn tar");
    assert!(
        packed.status.success(),
        "packing the release archive failed: {}",
        normalized_output(&packed.stderr)
    );
    let bytes = std::fs::read(&archive).expect("read the packed archive");
    (asset, bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::Digest;
    let mut hasher = sha2::Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// A workspace with the wrapper `frost init --wrapper` writes, pinned to
/// `version`.
fn wrapper_workspace(name: &str, version: &str) -> Workspace {
    let workspace = Workspace::empty(name);
    let (ok, out) = workspace.frost(&["init", "--wrapper"]);
    assert!(ok, "init --wrapper failed:\n{out}");
    std::fs::write(workspace.dir.join(".frost-version"), format!("{version}\n"))
        .expect("pin the declared version");
    workspace
}

fn run_wrapper(workspace: &Workspace, args: &[&str], env: &[(&str, &str)]) -> (bool, String) {
    let mut command = if cfg!(windows) {
        let mut command = Command::new("cmd");
        command.arg("/C").arg(workspace.dir.join("frostw.cmd"));
        command
    } else {
        Command::new(workspace.dir.join("frostw"))
    };
    command.args(args).current_dir(&workspace.dir);
    // Loopback must not be handed to whatever proxy the host has configured.
    for name in ["NO_PROXY", "no_proxy"] {
        command.env(name, "*");
    }
    for (key, value) in env {
        command.env(key, value);
    }
    let out = command.output().expect("spawn frostw");
    (
        out.status.success(),
        normalized_output(&out.stdout) + &normalized_output(&out.stderr),
    )
}

#[test]
fn frostw_fetches_verifies_and_runs_the_version_the_workspace_declares() {
    let Some(triple) = release_triple() else {
        eprintln!("skipping frostw download E2E: no release is published for this host");
        return;
    };
    if !wrapper_prerequisites_present() {
        eprintln!("skipping frostw download E2E: curl/tar/sha256 are not all present");
        return;
    }

    // Deliberately not this binary's own version: a frost already on PATH must
    // not be able to satisfy the pin and hide the download path.
    let version = "9.9.9";
    let workspace = wrapper_workspace("frostw-download", version);
    let (asset, archive) = release_archive(&workspace.dir, version, triple);
    let sums = format!("{}  {asset}\n", sha256_hex(&archive));
    let server = ReleaseServer::start(vec![
        (format!("/v{version}/SHA256SUMS"), sums.into_bytes()),
        (format!("/v{version}/{asset}"), archive),
    ]);

    let home = workspace.dir.join("frost-home");
    let home = home.to_str().unwrap();
    let env = [
        ("FROST_HOME", home),
        ("FROSTW_RELEASE_BASE_URL", server.base_url.as_str()),
    ];

    let (ok, out) = run_wrapper(&workspace, &["--version"], &env);
    assert!(ok, "frostw could not install the declared version:\n{out}");
    assert!(
        out.contains("downloading frost 9.9.9"),
        "the wait has to be explained while it happens:\n{out}"
    );
    assert!(
        out.contains("frost 0.9.0")
            || out.contains(&format!("frost {}", env!("CARGO_PKG_VERSION"))),
        "the downloaded binary is what ran:\n{out}"
    );
    let installed = executable_path(Path::new(home).join("versions").join(version).join("frost"));
    assert!(
        installed.exists(),
        "the verified release was not cached at {}",
        installed.display()
    );
    let after_install = server.requests();
    assert!(
        after_install >= 2,
        "the checksums and the archive are both fetched, saw {after_install}"
    );

    // Second run: the cache answers, and nothing is fetched again.
    let (ok, out) = run_wrapper(&workspace, &["--version"], &env);
    assert!(ok, "{out}");
    assert!(!out.contains("downloading"), "{out}");
    assert_eq!(
        server.requests(),
        after_install,
        "a cached version must not be re-fetched"
    );
}

#[test]
fn a_tampered_release_archive_is_rejected_and_installs_nothing() {
    let Some(triple) = release_triple() else {
        eprintln!("skipping frostw checksum E2E: no release is published for this host");
        return;
    };
    if !wrapper_prerequisites_present() {
        eprintln!("skipping frostw checksum E2E: curl/tar/sha256 are not all present");
        return;
    }

    let version = "9.9.9";
    let workspace = wrapper_workspace("frostw-tampered", version);
    let (asset, archive) = release_archive(&workspace.dir, version, triple);
    // The checksums describe the real archive; the served bytes are not it.
    let sums = format!("{}  {asset}\n", sha256_hex(&archive));
    let mut tampered = archive.clone();
    let middle = tampered.len() / 2;
    tampered[middle] ^= 0xFF;
    let server = ReleaseServer::start(vec![
        (format!("/v{version}/SHA256SUMS"), sums.into_bytes()),
        (format!("/v{version}/{asset}"), tampered),
    ]);

    let home = workspace.dir.join("frost-home");
    let home_str = home.to_str().unwrap();
    let (ok, out) = run_wrapper(
        &workspace,
        &["--version"],
        &[
            ("FROST_HOME", home_str),
            ("FROSTW_RELEASE_BASE_URL", server.base_url.as_str()),
        ],
    );

    assert!(!ok, "a modified archive must not be executed:\n{out}");
    assert!(out.contains("checksum mismatch"), "{out}");
    // Naming the recovery is the point of the message: a rejected download is
    // otherwise a dead end.
    assert!(out.contains("put it on PATH"), "{out}");
    assert!(
        !home.join("versions").join(version).exists(),
        "a rejected archive must leave no partially installed version behind"
    );
    let leftovers: Vec<_> = std::fs::read_dir(home.join("versions"))
        .map(|entries| entries.flatten().map(|e| e.file_name()).collect())
        .unwrap_or_default();
    assert!(
        leftovers.is_empty(),
        "staging must be cleaned up, found {leftovers:?}"
    );
}

#[test]
fn a_matching_frost_on_path_is_used_without_downloading() {
    if !wrapper_prerequisites_present() {
        eprintln!("skipping frostw PATH E2E: curl/tar/sha256 are not all present");
        return;
    }

    let version = env!("CARGO_PKG_VERSION");
    let workspace = wrapper_workspace("frostw-on-path", version);
    // Serves nothing: reaching it at all is the failure this asserts against.
    let server = ReleaseServer::start(Vec::new());

    let bin_dir = Path::new(frost_bin()).parent().unwrap().to_path_buf();
    let path = std::env::join_paths(
        std::iter::once(bin_dir).chain(std::env::split_paths(&std::env::var_os("PATH").unwrap())),
    )
    .expect("join PATH");
    let home = workspace.dir.join("frost-home");

    let (ok, out) = run_wrapper(
        &workspace,
        &["--version"],
        &[
            ("FROST_HOME", home.to_str().unwrap()),
            ("FROSTW_RELEASE_BASE_URL", server.base_url.as_str()),
            ("PATH", path.to_str().unwrap()),
        ],
    );

    assert!(ok, "{out}");
    assert!(out.contains(&format!("frost {version}")), "{out}");
    assert_eq!(
        server.requests(),
        0,
        "an installed frost that already matches the pin must not be replaced"
    );
    assert!(
        !home.exists(),
        "nothing is cached when nothing was downloaded"
    );
}

#[test]
fn frost_warns_when_it_is_not_the_version_the_workspace_declares() {
    let ws = Workspace::new("version-mismatch");

    // The pin the wrapper reads is also readable by frost itself, which is how
    // a direct invocation that bypassed the wrapper still names the difference
    // rather than leaving it to be discovered through its consequences.
    std::fs::write(ws.dir.join(".frost-version"), "0.0.1\n").unwrap();
    let (ok, out) = ws.frost(&["info", "version"]);
    assert!(
        ok,
        "a version difference is a warning, not a failure:\n{out}"
    );
    assert!(out.contains("requires frost 0.0.1"), "{out}");
    assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    assert!(
        out.contains("frostw"),
        "the warning has to name the way out:\n{out}"
    );

    std::fs::write(
        ws.dir.join(".frost-version"),
        format!("{}\n", env!("CARGO_PKG_VERSION")),
    )
    .unwrap();
    let (ok, out) = ws.frost(&["info", "version"]);
    assert!(ok, "{out}");
    assert!(
        !out.contains("warning"),
        "a matching pin says nothing:\n{out}"
    );
}

#[test]
fn this_repository_checks_in_the_wrapper_frost_writes() {
    // The wrapper only pins anything if it is committed, and this repository
    // builds itself with it (see frost.toml). Drift between the shipped asset
    // and the checked-in copy would mean `frost init --wrapper` hands new
    // workspaces something this one does not actually run.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let assets = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets");

    let shipped = std::fs::read_to_string(assets.join("frostw")).unwrap();
    let checked_in = std::fs::read_to_string(repo.join("frostw")).unwrap();
    assert_eq!(
        checked_in, shipped,
        "frostw at the repository root is not the frostw frost writes"
    );

    let shipped_cmd = std::fs::read_to_string(assets.join("frostw.cmd")).unwrap();
    let checked_in_cmd = std::fs::read_to_string(repo.join("frostw.cmd")).unwrap();
    assert_eq!(
        checked_in_cmd.replace("\r\n", "\n"),
        shipped_cmd.replace("\r\n", "\n"),
        "frostw.cmd at the repository root is not the one frost writes"
    );

    let pinned = std::fs::read_to_string(repo.join(".frost-version")).unwrap();
    assert_eq!(
        pinned.trim(),
        env!("CARGO_PKG_VERSION"),
        ".frost-version and the workspace version have to move together; \
         scripts/release.sh bumps both"
    );
}

#[test]
fn this_repository_describes_its_own_build() {
    // frost.toml at the repository root is not a sample: `task check` runs
    // these targets, and this repository's release binaries come out of them.
    //
    // Configuring it in-process costs no build and writes nothing, and it is
    // the check that actually rots — an input glob that stopped matching after
    // a rename does not fail a build, it silently narrows what a gate watches.
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repository root");
    let manifest = frostbuild_core::manifest::Manifest::load(&repo)
        .expect("the repository's own manifest must configure");

    let mut targets: Vec<&str> = manifest.targets.keys().map(String::as_str).collect();
    targets.sort_unstable();
    assert_eq!(
        targets,
        [
            "binaries",
            "clippy",
            "fmt",
            "python_test",
            "rust_test",
            "vscode_test"
        ],
        "the stages are the gate in CONTRIBUTING.md, plus the binaries"
    );
    // The manifest has no `[workspace]`, so the sample workspaces below this
    // directory are not packages of it. Were one ever added they still would
    // not be: a subdirectory declaring `[workspace]` is a workspace root, and
    // discovery stops there. Either way, absorbing them would build them with
    // this toolchain rather than the one they declared for themselves.
    assert!(
        !targets.iter().any(|name| name.starts_with("//sample")),
        "a sample workspace was absorbed as a package: {targets:?}"
    );

    let gate = &manifest.targets["rust_test"];
    for expected in [
        "crates/frostbuild-cli/src/main.rs",
        "crates/frostbuild-cli/tests/e2e.rs",
        "crates/frostbuild-cli/tests/cli-surface.txt",
        "sample_multi/core/src/core.c",
        "Cargo.lock",
    ] {
        assert!(
            gate.inputs.iter().any(|input| input == expected),
            "the test gate stopped watching {expected}; it would be cached \
             across a change to it"
        );
    }
    assert!(
        gate.inputs
            .iter()
            .all(|input| !input.starts_with("target/")),
        "build output is not input"
    );

    // The wrapper scripts are shipped bytes embedded in the binary, so the
    // stage that produces it has to rerun when they change.
    let binaries = &manifest.targets["binaries"];
    assert!(
        binaries
            .inputs
            .iter()
            .any(|input| input == "crates/frostbuild-cli/assets/frostw"),
        "the binaries stage stopped watching the wrappers it embeds"
    );

    // What `frost fmt` and `frost lint` say about this manifest is asserted in
    // `this_repository_and_its_samples_pass_their_own_lint_and_fmt`, against the
    // files in the tree and across every workspace this repository ships.
}

// ---------------------------------------------------------------------------
// --report: one build, explained in one file.
// ---------------------------------------------------------------------------

/// A report is only useful if it can be handed to someone and opened. Anything
/// fetched from elsewhere makes it a page that needs the network to be read,
/// so the property is asserted against the bytes frost actually wrote.
fn assert_self_contained(html: &str) {
    for reference in [
        "http://",
        "https://",
        "src=\"//",
        "href=\"//",
        "@import",
        "<script",
        "<iframe",
        "<img",
    ] {
        assert!(
            !html.contains(reference),
            "the report reaches outside itself with {reference:?}"
        );
    }
    assert!(html.starts_with("<!doctype html>"), "not a whole document");
    assert!(html.contains("<style>"), "styling has to be inline");
}

/// The `N` in `--stats`' "critical    N ms estimated" line.
fn stats_critical_path_ms(stats_output: &str) -> &str {
    stats_output
        .lines()
        .find_map(|line| line.trim().strip_prefix("critical"))
        .and_then(|rest| rest.split_whitespace().next())
        .expect("--stats prints an estimated critical path when something ran")
}

#[test]
fn a_report_shows_the_same_build_stats_printed() {
    let ws = Workspace::multi("report-stats");

    let (ok, out) = ws.frost(&[
        "build", "--no-tui", "--stats", "--report", "--trace", "t.json",
    ]);
    assert!(ok, "{out}");
    assert!(
        out.contains("frost: report "),
        "the report has to say where it landed:\n{out}"
    );

    let report = ws.dir.join(".frost/report/host-debug.html");
    let html = std::fs::read_to_string(&report).expect("the default report path");
    assert_self_contained(&html);

    // Two renderings of one run. Where they overlap they have to agree, or one
    // of them is describing a build that did not happen.
    let critical = stats_critical_path_ms(&out);
    assert!(
        html.contains(&format!("{critical} ms estimated before the run")),
        "the report's critical path disagrees with --stats ({critical} ms):\n{html}"
    );
    for fragment in [
        "utilization",
        "<h2>Critical path</h2>",
        "<h2>Slowest actions that ran</h2>",
        "<h2>Cache, by kind of work</h2>",
        "<h2>Why work ran</h2>",
        "not built before",
        "compile",
        "link",
    ] {
        assert!(
            html.contains(fragment),
            "the report omits {fragment}:\n{html}"
        );
    }
    // The trace is the timeline and the report is the summary; the report
    // points at it with a relative link, so copying the pair keeps it working.
    assert!(html.contains("href=\"../../t.json\""), "{html}");
    assert!(ws.dir.join("t.json").exists());

    // A warm build reports being warm rather than reporting zeroes.
    let (ok, out) = ws.frost(&["build", "--no-tui", "--report"]);
    assert!(ok, "{out}");
    let warm = std::fs::read_to_string(&report).expect("the report is rewritten");
    assert_self_contained(&warm);
    assert!(warm.contains("Nothing ran"), "{warm}");
    assert!(
        !warm.contains("<h2>Slowest actions that ran</h2>"),
        "nothing ran, so nothing was slowest:\n{warm}"
    );
    assert!(
        warm.contains("100% of the closure"),
        "a fully cached closure is worth saying plainly:\n{warm}"
    );
}

#[test]
fn a_failing_build_still_writes_a_report_naming_the_failure() {
    // This is the build whose report someone actually wants, so it is written
    // before the nonzero exit rather than skipped along with the success path.
    let ws = Workspace::new("report-failure");
    ws.write(
        "src/util.c",
        "#include \"util.h\"\nint util(void) { return \"deliberate type error\"; }\n",
    );

    let (ok, out) = ws.frost(&["build", "--no-tui", "-k", "--report=fail.html"]);
    assert!(!ok, "the build was supposed to fail:\n{out}");
    assert!(out.contains("frost: report "), "{out}");

    let html = std::fs::read_to_string(ws.dir.join("fail.html")).expect("the report");
    assert_self_contained(&html);
    assert!(html.contains("<h2>Failures</h2>"), "{html}");
    assert!(html.contains("src/util.c"), "{html}");
    // The compiler's own words, escaped rather than dropped: a report that
    // says "it failed" without them sends the reader back to the terminal.
    assert!(
        html.contains("deliberate type error") || html.contains("error"),
        "the failure output tail is missing:\n{html}"
    );
}

#[test]
fn a_test_report_counts_shards_as_slices_of_their_test() {
    let ws = Workspace::multi("report-tests");
    let manifest = ws.dir.join("core/frost.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    assert!(text.contains("core_test"), "{text}");
    std::fs::write(&manifest, format!("{text}shard_count = 3\n")).unwrap();

    let (ok, out) = ws.frost(&["test", "--all", "--no-tui", "--report=tests.html"]);
    assert!(ok, "{out}");

    let html = std::fs::read_to_string(ws.dir.join("tests.html")).expect("the report");
    assert_self_contained(&html);
    assert!(html.contains("<h2>Tests</h2>"), "{html}");
    for shard in ["0/3", "1/3", "2/3"] {
        assert!(
            html.contains(&format!("<td class=\"dim\">{shard}</td>")),
            "shard {shard} is missing from the report:\n{html}"
        );
    }
}

// ---------------------------------------------------------------------------
// frost lsp: the workspace frost already has, spoken as Language Server
// Protocol. Driven the way an editor drives it — framed messages on a pipe —
// because the framing and the dispatch are as much of the feature as the
// answers are.
// ---------------------------------------------------------------------------

fn lsp_wire(messages: &[serde_json::Value]) -> Vec<u8> {
    let mut wire = Vec::new();
    for message in messages {
        let body = serde_json::to_vec(message).expect("encode a request");
        wire.extend_from_slice(format!("Content-Length: {}\r\n\r\n", body.len()).as_bytes());
        wire.extend_from_slice(&body);
    }
    wire
}

fn lsp_replies(bytes: &[u8]) -> Vec<serde_json::Value> {
    let mut replies = Vec::new();
    let mut rest = bytes;
    while let Some(at) = rest.windows(4).position(|window| window == b"\r\n\r\n") {
        let headers = String::from_utf8_lossy(&rest[..at]).into_owned();
        let length: usize = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.trim()
                    .eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse().ok())?
            })
            .expect("every frame carries a Content-Length");
        let body = &rest[at + 4..at + 4 + length];
        replies.push(serde_json::from_slice(body).expect("a reply is JSON"));
        rest = &rest[at + 4 + length..];
    }
    replies
}

/// Run one whole session and return everything the server sent.
fn lsp_session(dir: &Path, messages: &[serde_json::Value]) -> Vec<serde_json::Value> {
    use std::io::Write as _;

    let mut child = Command::new(frost_bin())
        .arg("-C")
        .arg(dir)
        .arg("lsp")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("spawn frost lsp");
    child
        .stdin
        .take()
        .expect("stdin")
        .write_all(&lsp_wire(messages))
        .expect("write the session");
    let out = child.wait_with_output().expect("frost lsp exits");
    assert!(
        out.status.success(),
        "frost lsp failed: {}",
        normalized_output(&out.stderr)
    );
    lsp_replies(&out.stdout)
}

fn lsp_initialize() -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": { "processId": serde_json::Value::Null, "capabilities": {} },
    })
}

fn lsp_did_open(uri: &str, text: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "method": "textDocument/didOpen",
        "params": { "textDocument": {
            "uri": uri, "languageId": "toml", "version": 1, "text": text,
        }},
    })
}

fn lsp_at(id: u32, method: &str, uri: &str, line: u32, character: u32) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0", "id": id, "method": method,
        "params": {
            "textDocument": { "uri": uri },
            "position": { "line": line, "character": character },
            "context": { "includeDeclaration": true },
        },
    })
}

/// `shutdown` then `exit`, which is how a client is supposed to leave. The
/// protocol asks a server to exit nonzero when it is told to exit without
/// having been told to shut down, so sending both is part of the test.
fn lsp_exit() -> [serde_json::Value; 2] {
    [
        serde_json::json!({ "jsonrpc": "2.0", "id": 99, "method": "shutdown" }),
        serde_json::json!({ "jsonrpc": "2.0", "method": "exit" }),
    ]
}

/// A `file:` URI for a path, spelled the way the server spells one.
///
/// Two host details make the naive `format!("file://{path}")` wrong, and both
/// were found by CI rather than by reading. Windows: that produces
/// `file://C:/…`, where `C:` sits in the authority position, so the server
/// reads no path at all and answers null to everything; and `canonicalize`
/// there returns a `\\?\` verbatim prefix that no editor would ever send.
/// macOS: every temp directory is reached through `/var` while its real path
/// is `/private/var`, so an expectation built from an unresolved path compares
/// two spellings of one file.
fn lsp_uri(path: &Path) -> String {
    let resolved = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let text = resolved.display().to_string().replace('\\', "/");
    let text = match (text.strip_prefix("//?/UNC/"), text.strip_prefix("//?/")) {
        (Some(share), _) => format!("//{share}"),
        (None, Some(rest)) => rest.to_string(),
        (None, None) => text,
    };
    let absolute = if text.starts_with('/') {
        text
    } else {
        format!("/{text}")
    };
    let mut encoded = String::with_capacity(absolute.len());
    for byte in absolute.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' | b':' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    format!("file://{encoded}")
}

fn reply_to(replies: &[serde_json::Value], id: u32) -> serde_json::Value {
    replies
        .iter()
        .find(|reply| reply["id"] == id)
        .unwrap_or_else(|| panic!("no reply to request {id}:\n{replies:#?}"))["result"]
        .clone()
}

fn diagnostics_for(replies: &[serde_json::Value], uri: &str) -> Vec<serde_json::Value> {
    replies
        .iter()
        .filter(|reply| {
            reply["method"] == "textDocument/publishDiagnostics" && reply["params"]["uri"] == uri
        })
        .flat_map(|reply| {
            reply["params"]["diagnostics"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

/// The line of a manifest containing `needle`, 0-based.
fn line_of(text: &str, needle: &str) -> u64 {
    text.lines()
        .position(|line| line.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} is not in this manifest")) as u64
}

#[test]
fn frost_lsp_reports_an_undefined_label_where_it_is_written() {
    let ws = Workspace::multi("lsp-diagnostics");
    let manifest = ws.dir.join("apps/cli/frost.toml");
    let broken = std::fs::read_to_string(&manifest)
        .unwrap()
        .replace("\"//text:text\"", "\"//text:absent\"");
    std::fs::write(&manifest, &broken).unwrap();
    let uri = lsp_uri(&manifest);

    let replies = lsp_session(
        &ws.dir,
        &[lsp_initialize(), lsp_did_open(&uri, &broken)]
            .into_iter()
            .chain(lsp_exit())
            .collect::<Vec<_>>(),
    );

    let capabilities = &reply_to(&replies, 1)["capabilities"];
    for provider in ["definitionProvider", "referencesProvider", "hoverProvider"] {
        assert_eq!(capabilities[provider], true, "{capabilities:#?}");
    }

    let diagnostics = diagnostics_for(&replies, &uri);
    assert_eq!(diagnostics.len(), 1, "{diagnostics:#?}");
    let diagnostic = &diagnostics[0];
    assert_eq!(
        diagnostic["range"]["start"]["line"],
        line_of(&broken, "//text:absent"),
        "the squiggle belongs on the line that wrote the label"
    );

    // Byte for byte the sentence a build prints. Two wordings for one mistake
    // is a second source of truth, and the editor's is the untested one.
    let (ok, out) = ws.frost(&["build", "--no-tui"]);
    assert!(!ok, "the workspace was supposed to be broken:\n{out}");
    let from_cli = out
        .lines()
        .find_map(|line| line.strip_prefix("frost: error: "))
        .expect("the build says what is wrong");
    assert_eq!(diagnostic["message"], from_cli);
    assert_eq!(diagnostic["source"], "frost");
}

#[test]
fn frost_lsp_completes_labels_across_packages() {
    let ws = Workspace::multi("lsp-completion");
    let manifest = ws.dir.join("apps/cli/frost.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let uri = lsp_uri(&manifest);
    let deps = line_of(&text, "deps = [") as u32;
    let srcs = line_of(&text, "srcs = [") as u32;

    let replies = lsp_session(
        &ws.dir,
        &[
            lsp_initialize(),
            lsp_did_open(&uri, &text),
            // Inside the first string of the deps array.
            lsp_at(2, "textDocument/completion", &uri, deps, 10),
            // A key position: the start of a line that holds a key.
            lsp_at(3, "textDocument/completion", &uri, srcs, 0),
        ]
        .into_iter()
        .chain(lsp_exit())
        .collect::<Vec<_>>(),
    );

    let labels: Vec<String> = reply_to(&replies, 2)
        .as_array()
        .expect("a completion list")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or_default().to_string())
        .collect();
    // The point of a build-aware server: a package the open file has never
    // mentioned is still offered, spelled the way it would have to be written.
    assert!(labels.contains(&"//core:core".to_string()), "{labels:?}");
    assert!(
        labels.contains(&"//render:render".to_string()),
        "{labels:?}"
    );
    assert!(labels.contains(&"//:gen_version".to_string()), "{labels:?}");

    let keys: Vec<String> = reply_to(&replies, 3)
        .as_array()
        .expect("a completion list")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or_default().to_string())
        .collect();
    // `cc_binary`, so `srcs` and `ldflags` are offered and `cmd` is not.
    assert!(keys.contains(&"srcs".to_string()), "{keys:?}");
    assert!(keys.contains(&"ldflags".to_string()), "{keys:?}");
    assert!(!keys.contains(&"cmd".to_string()), "{keys:?}");
}

#[test]
fn frost_lsp_jumps_from_a_label_to_the_line_that_declares_it() {
    let ws = Workspace::multi("lsp-definition");
    let manifest = ws.dir.join("text/frost.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let uri = lsp_uri(&manifest);
    // By the assignment, not by the label: this manifest's opening comment
    // names //core:core too, and a cursor in a comment is not on a label.
    let deps = line_of(&text, "deps = [") as u32;
    let column = text
        .lines()
        .nth(deps as usize)
        .unwrap()
        .find("//core")
        .unwrap() as u32
        + 2;

    let replies = lsp_session(
        &ws.dir,
        &[
            lsp_initialize(),
            lsp_did_open(&uri, &text),
            lsp_at(2, "textDocument/definition", &uri, deps, column),
        ]
        .into_iter()
        .chain(lsp_exit())
        .collect::<Vec<_>>(),
    );

    let location = reply_to(&replies, 2);
    let target = ws.dir.join("core/frost.toml");
    assert_eq!(location["uri"], lsp_uri(&target), "{location:#?}");
    let core = std::fs::read_to_string(&target).unwrap();
    assert_eq!(
        location["range"]["start"]["line"],
        line_of(&core, "[target.core]"),
        "the jump lands on the declaration, not the top of the file"
    );
}

#[test]
fn frost_lsp_hover_and_references_are_the_answers_query_gives() {
    // The rule this enforces is "no second implementation": both features call
    // the functions `frost query` calls, so a disagreement here would mean the
    // editor had grown its own idea of the graph.
    let ws = Workspace::multi("lsp-query-agreement");
    let manifest = ws.dir.join("core/frost.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let uri = lsp_uri(&manifest);
    let header = line_of(&text, "[target.core]") as u32;

    let replies = lsp_session(
        &ws.dir,
        &[
            lsp_initialize(),
            lsp_did_open(&uri, &text),
            lsp_at(2, "textDocument/hover", &uri, header, 9),
            lsp_at(3, "textDocument/references", &uri, header, 9),
        ]
        .into_iter()
        .chain(lsp_exit())
        .collect::<Vec<_>>(),
    );

    let (ok, out) = ws.frost(&["query", "rdeps", "//core:core", "--json"]);
    assert!(ok, "{out}");
    let rdeps: serde_json::Value = serde_json::from_str(&out).unwrap();
    let expected = rdeps["targets"].as_array().unwrap();

    // One location per target `rdeps` names, each at that target's own
    // declaration. `includeDeclaration` is set, so the sets match exactly.
    let references = reply_to(&replies, 3);
    let references = references.as_array().expect("a location list");
    assert_eq!(
        references.len(),
        expected.len(),
        "references and `query rdeps` disagree:\n{references:#?}\n{expected:#?}"
    );
    for target in expected {
        let label = target.as_str().unwrap();
        let (package, name) = label
            .trim_start_matches("//")
            .split_once(':')
            .expect("a workspace label");
        let declaring = if package.is_empty() {
            ws.dir.join("frost.toml")
        } else {
            ws.dir.join(package).join("frost.toml")
        };
        let declaration = std::fs::read_to_string(&declaring).unwrap();
        let uri = lsp_uri(&declaring);
        assert!(
            references.iter().any(|location| {
                location["uri"] == uri
                    && location["range"]["start"]["line"]
                        == line_of(&declaration, &format!("[target.{name}]"))
            }),
            "no reference at {label}'s declaration:\n{references:#?}"
        );
    }

    let (ok, out) = ws.frost(&["query", "deps", "//core:core", "--json"]);
    assert!(ok, "{out}");
    let deps: serde_json::Value = serde_json::from_str(&out).unwrap();
    let closure = deps["targets"].as_array().unwrap().len();

    let hover = reply_to(&replies, 2);
    let markdown = hover["contents"]["value"].as_str().expect("markdown");
    assert!(markdown.contains("**//core:core**"), "{markdown}");
    assert!(markdown.contains("`cc_library`"), "{markdown}");
    assert!(
        markdown.contains(&format!(
            "{closure} targets in `frost query deps //core:core`"
        )),
        "the hover's closure size is not the one query prints ({closure}):\n{markdown}"
    );
    // The declared output, which only the configured graph knows.
    assert!(markdown.contains(".frost/lib/debug/"), "{markdown}");
}

#[test]
fn frost_lsp_keeps_answering_while_a_manifest_does_not_parse() {
    // The state a manifest is in for most of the time it is being edited. A
    // server that went quiet here would be useless exactly when it is wanted.
    let ws = Workspace::multi("lsp-mid-edit");
    let manifest = ws.dir.join("apps/cli/frost.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    let uri = lsp_uri(&manifest);
    let deps = line_of(&text, "deps = [") as u32;
    let half_typed = format!(
        "{}\ndeps = [\n  \"//co",
        &text[..text.find("deps = [").unwrap()]
    );

    let replies = lsp_session(
        &ws.dir,
        &[
            lsp_initialize(),
            lsp_did_open(&uri, &half_typed),
            // Inside the unterminated string on the last line.
            lsp_at(
                2,
                "textDocument/completion",
                &uri,
                half_typed.lines().count() as u32 - 1,
                7,
            ),
        ]
        .into_iter()
        .chain(lsp_exit())
        .collect::<Vec<_>>(),
    );
    let _ = deps;

    let diagnostics = diagnostics_for(&replies, &uri);
    assert_eq!(diagnostics.len(), 1, "the syntax error is reported once");
    let labels: Vec<String> = reply_to(&replies, 2)
        .as_array()
        .expect("a completion list")
        .iter()
        .map(|item| item["label"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        labels.contains(&"//core:core".to_string()),
        "labels are still offered while the document does not parse: {labels:?}"
    );
}

#[cfg(unix)]
#[test]
fn frost_lsp_answers_about_a_document_opened_through_a_symlink() {
    // The case macOS CI found: `frost -C` resolves the workspace root, an
    // editor sends whatever path the user opened, and on macOS every temp
    // directory is reached through `/var` while its real path is
    // `/private/var`. Comparing those literally puts every file in the root
    // package, so every local label resolves to a target that does not exist
    // and the server answers nothing, anywhere.
    //
    // A symlink reproduces it on any Unix host, which is the point: the
    // earlier E2E only failed on macOS because only macOS supplied one.
    let ws = Workspace::multi("lsp-symlink");
    let link = ws.dir.with_extension("link");
    let _ = std::fs::remove_file(&link);
    std::os::unix::fs::symlink(&ws.dir, &link).expect("symlink the workspace");

    let manifest = link.join("text/frost.toml");
    let text = std::fs::read_to_string(&manifest).unwrap();
    // Deliberately unresolved: this is the spelling an editor sends.
    let uri = format!("file://{}", manifest.display());
    // The target's own header, so the question is what package this document
    // is in. An absolute label like `//core:core` resolves the same whatever
    // the answer, and would pass with the bug still there.
    let header = line_of(&text, "[target.text]") as u32;

    let replies = lsp_session(
        &link,
        &[
            lsp_initialize(),
            lsp_did_open(&uri, &text),
            lsp_at(2, "textDocument/hover", &uri, header, 9),
            lsp_at(3, "textDocument/references", &uri, header, 9),
        ]
        .into_iter()
        .chain(lsp_exit())
        .collect::<Vec<_>>(),
    );

    let hover = reply_to(&replies, 2);
    assert!(
        hover["contents"]["value"]
            .as_str()
            .is_some_and(|markdown| markdown.contains("**//text:text**")),
        "the document's own target went unrecognized through a symlink: {hover:#?}"
    );
    let references = reply_to(&replies, 3);
    assert!(
        references.as_array().is_some_and(|locations| locations
            .iter()
            .any(|location| { location["uri"] == lsp_uri(&ws.dir.join("apps/cli/frost.toml")) })),
        "references went silent through a symlink: {references:#?}"
    );

    let _ = std::fs::remove_file(&link);
}

// ---------------------------------------------------------------------------
// Diagnostics: where the mistake is, what was probably meant, what to do next.
//
// The failure path is the output read most often and the one a competitor is
// judged against, so these hold its shape rather than only its exit code.
// ---------------------------------------------------------------------------

#[test]
fn an_unknown_target_offers_the_targets_it_might_have_been() {
    let ws = Workspace::multi("diagnostic-target");

    // A bare name in a multi-package workspace: the label is what has to come
    // back, because that is what would actually have worked.
    let (ok, out) = ws.frost(&["build", "cli"]);
    assert!(!ok, "{out}");
    assert!(out.contains("unknown target \"cli\""), "{out}");
    assert!(out.contains("did you mean \"//apps/cli:cli\"?"), "{out}");

    // A typo inside a label stays inside its package: `//core:core` is what
    // was meant, so it leads the list even when other names are near enough to
    // be offered after it.
    let (ok, out) = ws.frost(&["build", "//core:cor"]);
    assert!(!ok, "{out}");
    assert!(out.contains("did you mean \"//core:core\""), "{out}");

    // Nothing close: the known set, since this workspace is small enough to
    // print. A suggestion that is not actually similar is worse than none.
    let (ok, out) = ws.frost(&["build", "qqqqqqqq"]);
    assert!(!ok, "{out}");
    assert!(!out.contains("did you mean"), "{out}");
    assert!(out.contains("//core:core"), "{out}");
}

#[test]
fn a_broken_manifest_says_which_line_and_what_was_meant() {
    let ws = Workspace::empty("diagnostic-manifest");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write("src/main.c", "int main(void) { return 0; }\n");

    // A mistyped key. The suggestion is the whole point: `expected one of` on
    // its own is a list of twenty-two names to read.
    ws.write(
        "frost.toml",
        "[target.app]\nkind = \"cc_binary\"\nsrc = [\"src/main.c\"]\n",
    );
    let (ok, out) = ws.frost(&["build"]);
    assert!(!ok, "{out}");
    // Workspace-relative and `path:line:column`, which is the form an editor
    // and a `grep` both already know how to jump to — and which is identical
    // on every machine, so it is a shape a test can hold.
    assert!(
        out.contains("frost.toml:3:1: unknown field `src`"),
        "the position and the problem come first:\n{out}"
    );
    assert!(out.contains("3 | src = [\"src/main.c\"]"), "{out}");
    assert!(
        out.contains("^^^"),
        "the caret covers the offending span:\n{out}"
    );
    assert!(out.contains("= did you mean `srcs`?"), "{out}");
    assert!(out.contains("= expected one of `kind`, `srcs`"), "{out}");

    // A mistyped value of a closed set gets the same treatment.
    ws.write(
        "frost.toml",
        "[target.app]\nkind = \"cc_binry\"\nsrcs = [\"src/main.c\"]\n",
    );
    let (ok, out) = ws.frost(&["build"]);
    assert!(!ok, "{out}");
    assert!(
        out.contains("frost.toml:2:8: unknown variant `cc_binry`"),
        "{out}"
    );
    assert!(out.contains("= did you mean `cc_binary`?"), "{out}");

    // Syntax, where the parser's own span is the authority.
    ws.write(
        "frost.toml",
        "[target.app]\nkind = \"cc_binary\"\nsrcs = [\n",
    );
    let (ok, out) = ws.frost(&["build"]);
    assert!(!ok, "{out}");
    assert!(out.contains("frost.toml:3:9:"), "{out}");
    assert!(out.contains("unclosed array"), "{out}");

    // A package manifest names itself, not the root.
    ws.write(
        "frost.toml",
        "[workspace]\ndefault_targets = [\"//core:core\"]\n",
    );
    std::fs::create_dir_all(ws.dir.join("core/src")).unwrap();
    ws.write("core/src/core.c", "int core(void) { return 1; }\n");
    ws.write(
        "core/frost.toml",
        "[target.core]\nkind = \"cc_library\"\nsrc = [\"src/core.c\"]\n",
    );
    let (ok, out) = ws.frost(&["build"]);
    assert!(!ok, "{out}");
    assert!(out.contains("core/frost.toml:3:1:"), "{out}");
}

#[test]
fn a_missing_tool_says_where_it_looked_and_what_it_blocks() {
    let ws = Workspace::empty("diagnostic-tool");
    std::fs::create_dir_all(ws.dir.join("src")).unwrap();
    ws.write("src/a.in", "input\n");
    ws.write(
        "frost.toml",
        "[toolchain.tools]\n\
         absent = \"frost-e2e-definitely-absent-tool\"\n\
         \n\
         [target.first]\n\
         kind = \"command\"\n\
         tool = \"absent\"\n\
         args = [\"${in}\"]\n\
         inputs = [\"src/a.in\"]\n\
         outputs = [\".frost/out/${config}/first.out\"]\n\
         \n\
         [target.second]\n\
         kind = \"command\"\n\
         tool = \"absent\"\n\
         args = [\"${in}\"]\n\
         inputs = [\"src/a.in\"]\n\
         outputs = [\".frost/out/${config}/second.out\"]\n",
    );

    let (ok, out) = ws.frost(&["build", "first", "second"]);
    assert!(!ok, "{out}");
    // One message carrying all four things a reader needs: which key asked for
    // it, what it named, where frost looked, and what stops working until it
    // is there. Any one of them alone leaves the next step a guess.
    //
    // `a_missing_tool_says_where_it_looked_and_who_needed_it` asks the same of
    // the same message and is not a duplicate of this: it names one target and
    // runs only on unix, so it cannot see whether the attribution sorts and
    // joins more than one, or whether any of this survives on Windows.
    assert!(out.contains("[toolchain.tools].absent"), "{out}");
    assert!(out.contains("frost-e2e-definitely-absent-tool"), "{out}");
    assert!(out.contains("PATH entries"), "{out}");
    assert!(out.contains("required by first, second"), "{out}");
    assert!(out.contains("frost doctor"), "{out}");
}

#[test]
fn exit_codes_separate_a_bad_invocation_from_a_bad_build() {
    // The distinction a script acts on: 1 is an answer about your code, 2 is
    // an answer about your command line or your environment. Both are in
    // docs/28; this is the part that holds them to it.
    let ws = Workspace::new("diagnostic-exit-codes");

    let code = |args: &[&str]| -> i32 {
        Command::new(frost_bin())
            .arg("-C")
            .arg(&ws.dir)
            .args(args)
            .output()
            .expect("spawn frost")
            .status
            .code()
            .expect("frost exits rather than being signalled")
    };

    // A workspace with no `[profile.*]` sections gives any profile a bare
    // tree on purpose, so declaring one is what makes an undeclared profile a
    // mistake rather than a choice.
    ws.append("frost.toml", "\n[profile.release]\ncflags = [\"-O2\"]\n");

    assert_eq!(code(&["build"]), 0, "a build that succeeds");
    assert_eq!(code(&["build"]), 0, "and again, from the cache");

    // Frost could not run the work as asked: each of these asks for something
    // that does not exist, and none of them is a result about the code. They
    // run against a healthy tree so a compile failure cannot stand in for the
    // exit code being tested.
    for invocation in [
        vec!["build", "no-such-target"],
        vec!["build", "--profile", "no-such-profile"],
        vec!["build", "--platform", "no-such-platform"],
        vec!["query", "deps", "no-such-target"],
    ] {
        assert_eq!(
            code(&invocation),
            2,
            "`frost {}` is a question frost cannot act on",
            invocation.join(" ")
        );
    }

    // The work ran and did not succeed.
    ws.write(
        "src/util.c",
        "#include \"util.h\"\nint util(void) { return \"not an int\"; }\n",
    );
    assert_eq!(code(&["build"]), 1, "a compile that fails");

    // A workspace that is not one at all.
    let empty = Workspace::empty("diagnostic-exit-codes-empty");
    let out = Command::new(frost_bin())
        .arg("-C")
        .arg(&empty.dir)
        .arg("build")
        .output()
        .expect("spawn frost");
    assert_eq!(out.status.code(), Some(2), "a directory with no manifest");
    assert!(
        normalized_output(&out.stderr).contains("frost init"),
        "and it says what to do about it"
    );
}

/// The exit code of one `frost` invocation in `workspace`.
fn exit_code(workspace: &Path, args: &[&str]) -> i32 {
    Command::new(frost_bin())
        .arg("-C")
        .arg(workspace)
        .args(args)
        .output()
        .expect("spawn frost")
        .status
        .code()
        .expect("frost exits rather than being signalled")
}

#[test]
fn fmt_rewrites_a_manifest_once_and_then_leaves_it_alone() {
    let ws = Workspace::new("fmt-idempotent");

    // Keys out of canonical order, an array past the wrap width, and a comment
    // that has to survive both. Appended to the real manifest rather than
    // replacing it, so the workspace still builds and the last assertion here
    // means something.
    ws.append(
        "frost.toml",
        "\n[target.extra]\n\
         # why this target exists\n\
         srcs = [\"src/util.c\"]\n\
         kind = \"cc_library\"\n\
         cflags = [\"-Wall\", \"-Wextra\", \"-Wpedantic\", \"-Wshadow\", \"-Wconversion\", \
         \"-Wsign-conversion\", \"-Wdouble-promotion\"]\n\
         includes = [\"include\"]\n",
    );

    assert_eq!(
        exit_code(&ws.dir, &["fmt", "--check"]),
        1,
        "starts unformatted"
    );
    assert_eq!(exit_code(&ws.dir, &["fmt"]), 0);
    let once = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();

    assert_eq!(
        exit_code(&ws.dir, &["fmt", "--check"]),
        0,
        "now it is clean"
    );
    assert_eq!(exit_code(&ws.dir, &["fmt"]), 0);
    let twice = std::fs::read_to_string(ws.dir.join("frost.toml")).unwrap();
    assert_eq!(once, twice, "formatting is a fixed point");

    // The three things a formatter must not lose.
    assert!(once.contains("# why this target exists"), "{once}");
    assert!(once.contains("src/util.c"), "{once}");
    assert!(once.contains("-Wsign-conversion"), "{once}");
    // `kind` is what a reader looks for first, so canonical order puts it there.
    let extra = once
        .split("[target.extra]")
        .nth(1)
        .expect("the target survived");
    assert!(
        extra.trim_start().starts_with("kind = \"cc_library\""),
        "keys are canonically ordered: {extra}"
    );
    // Which line ending the file has is the checkout's business — a Windows
    // one is CRLF — and is covered by `fmt::tests::
    // a_file_keeps_the_line_ending_it_arrived_with`. This test is about
    // layout, so it reads the layout without asserting on the ending.
    let layout = once.replace("\r\n", "\n");
    // The long array wrapped; the short one did not.
    assert!(layout.contains("cflags = [\n"), "{layout}");
    assert!(layout.contains("includes = [\"include\"]"), "{layout}");

    // And it is still the same workspace afterwards.
    assert_eq!(exit_code(&ws.dir, &["build"]), 0);
}

#[test]
fn fmt_reaches_every_package_of_a_workspace() {
    let ws = Workspace::multi("fmt-packages");
    ws.append(
        "core/frost.toml",
        "\n[target.extra]\nsrcs = [\"src/core.c\"]\nkind = \"cc_library\"\nincludes = [\"include\"]\n",
    );

    let (ok, out) = ws.frost(&["fmt", "--check"]);
    assert!(!ok, "{out}");
    // The point of the test: `--check` from the root reached into a package
    // rather than stopping at the root manifest. The wording around the name
    // is not contract, so only the name is asserted.
    assert!(out.contains("core/frost.toml"), "{out}");

    assert_eq!(exit_code(&ws.dir, &["fmt"]), 0);
    assert_eq!(exit_code(&ws.dir, &["fmt", "--check"]), 0);
    assert_eq!(exit_code(&ws.dir, &["build"]), 0, "and it still builds");
}

#[test]
fn fmt_works_on_a_manifest_that_does_not_load() {
    // Formatting is most wanted mid-edit. A manifest that parses as TOML but
    // names an unknown target is exactly that moment, and a formatter that
    // needs the workspace to be valid first is no use there.
    let ws = Workspace::new("fmt-invalid");
    ws.write(
        "frost.toml",
        "[workspace]\ndefault_targets = [\"app\"]\n\n\
         [target.app]\nsrcs = [\"src/main.c\"]\nkind = \"cc_binary\"\ndeps = [\"nope\"]\n",
    );

    assert_eq!(exit_code(&ws.dir, &["build"]), 2, "the workspace is broken");
    assert_eq!(
        exit_code(&ws.dir, &["fmt"]),
        0,
        "and formatting still works"
    );
    assert_eq!(exit_code(&ws.dir, &["fmt", "--check"]), 0);
}

#[test]
fn the_shipped_samples_are_already_formatted() {
    // `frost fmt --check` has to be true of what the repository ships, or the
    // first thing a reader copies is a workspace their own CI would reject.
    //
    // Lint is not asserted here: `this_repository_and_its_samples_pass_their_own_lint`
    // covers it against the files in the tree rather than against copies, and
    // over more samples than this loop has fixtures for.
    for sample in ["fmt-clean-c", "fmt-clean-multi"] {
        let ws = if sample.ends_with("multi") {
            Workspace::multi(sample)
        } else {
            Workspace::new(sample)
        };
        assert_eq!(
            exit_code(&ws.dir, &["fmt", "--check"]),
            0,
            "{sample} is formatted"
        );
    }
}
