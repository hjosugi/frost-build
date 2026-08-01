//! Manifest patterns that load fine and go wrong later.
//!
//! The parser rejects what cannot mean anything: an absolute `srcs` path, a
//! glob matching no files, two targets claiming one output. What it cannot
//! reject is a manifest that is valid and unwise — one that will build here and
//! not on a colleague's machine, or that quietly stops caching. Those are what
//! this reports.
//!
//! Every rule here is a lint rather than an error for the same reason: each has
//! a legitimate exception, and the ones that do not are already errors. So each
//! carries a stable identifier to name it by and one line saying what it costs,
//! and none of them stops a build.

use std::collections::BTreeSet;
use std::path::Path;

use crate::manifest::{Manifest, TargetKind};

/// Something worth changing about a manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    /// Stable across releases, so CI can name one rule to require or ignore.
    pub rule: &'static str,
    /// The target it is about, when it is about one.
    pub target: Option<String>,
    /// What was found, naming the specific thing that triggered it.
    pub detail: String,
    /// What it costs. The same sentence for every finding of a rule, because
    /// the reason is a property of the rule and not of the occurrence.
    pub why: &'static str,
}

/// Every rule this version can report, sorted.
///
/// `--allow` validates against it, so a misspelled rule name is refused rather
/// than silently allowing nothing.
pub const RULES: &[&str] = &[
    "absolute-path",
    "host-shell-syntax",
    "missing-include-dir",
    "redundant-pass-env",
    "unreachable-target",
];

/// Shell operators whose meaning differs between `/bin/sh` and `cmd.exe`.
const HOST_SHELL_OPERATORS: &[&str] = &["&&", "||", "|", ">>", ">", "<", ";", "$(", "`"];

/// Everything wrong with `manifest`, in a deterministic order.
///
/// `root` is read from: whether an `includes` directory exists is a fact about
/// the workspace, not about the text.
pub fn lint(root: &Path, manifest: &Manifest) -> Vec<Finding> {
    let mut findings = Vec::new();
    unreachable_targets(manifest, &mut findings);
    let produced = produced_directories(manifest);
    for (name, target) in &manifest.targets {
        redundant_pass_env(name, target, &mut findings);
        absolute_paths(name, target, &mut findings);
        host_shell_syntax(name, target, &mut findings);
        missing_include_dirs(root, &produced, name, target, &mut findings);
    }
    findings.sort_by(|a, b| {
        a.rule
            .cmp(b.rule)
            .then_with(|| a.target.cmp(&b.target))
            .then_with(|| a.detail.cmp(&b.detail))
    });
    // A rule missing from `RULES` cannot be named in `--allow` or in the
    // documentation, which makes it unusable rather than merely undocumented.
    debug_assert!(
        findings.iter().all(|f| RULES.contains(&f.rule)),
        "a rule is missing from lint::RULES"
    );
    findings
}

fn unreachable_targets(manifest: &Manifest, findings: &mut Vec<Finding>) {
    let mut reached: BTreeSet<&str> = manifest
        .default_targets
        .iter()
        .map(String::as_str)
        .collect();
    for target in manifest.targets.values() {
        reached.extend(target.deps.iter().map(String::as_str));
    }
    for (name, target) in &manifest.targets {
        // A test is an entry point of its own: `frost test` selects them
        // directly, so nothing depending on one means nothing.
        if matches!(target.kind, TargetKind::CcTest | TargetKind::Test) {
            continue;
        }
        if !reached.contains(name.as_str()) {
            findings.push(Finding {
                rule: "unreachable-target",
                target: Some(name.clone()),
                detail: format!("nothing depends on {name:?} and it is not a default target"),
                why: "it is never built unless someone names it, so it is not covered by \
                      `frost build` and nothing notices when it breaks",
            });
        }
    }
}

fn redundant_pass_env(name: &str, target: &crate::manifest::Target, findings: &mut Vec<Finding>) {
    for variable in &target.pass_env {
        if crate::ENV_PASSTHROUGH.contains(&variable.as_str()) {
            findings.push(Finding {
                rule: "redundant-pass-env",
                target: Some(name.to_string()),
                detail: format!("{variable:?} is passed to every action already"),
                why: "naming it does not make it available, it makes its value action-key \
                      material, so two machines whose values differ stop sharing cache \
                      entries; what PATH selects here is the target's tool, and the resolved \
                      tool is in the toolchain fingerprint already",
            });
        }
    }
}

fn absolute_paths(name: &str, target: &crate::manifest::Target, findings: &mut Vec<Finding>) {
    // Declared paths are already rejected by the parser. Arguments are not:
    // they are opaque to Frost, which is what makes an absolute one able to
    // hide there.
    let arguments = target
        .args
        .iter()
        .chain(target.steps.iter().flat_map(|step| step.args.iter()))
        .chain(target.cmd.iter());
    for argument in arguments {
        if let Some(absolute) = absolute_component(argument) {
            findings.push(Finding {
                rule: "absolute-path",
                target: Some(name.to_string()),
                detail: format!("{absolute:?} is an absolute path"),
                why: "it names a location on this machine, so the action does the wrong thing \
                      or nothing at all on another one",
            });
        }
    }
}

/// The first absolute-looking path in a command argument, if any.
fn absolute_component(argument: &str) -> Option<&str> {
    argument.split_whitespace().find(|word| {
        // A flag's value can carry one too: `-I/opt/include`.
        let path = word.split_once('=').map_or(*word, |(_, value)| value);
        let path = path.trim_start_matches(['-', 'I', 'L']).trim_matches('"');
        path.starts_with('/') && path.len() > 1
            || path.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
                && path
                    .get(1..3)
                    .is_some_and(|rest| rest == ":\\" || rest == ":/")
    })
}

fn host_shell_syntax(name: &str, target: &crate::manifest::Target, findings: &mut Vec<Finding>) {
    // A genrule runs through the host shell on purpose. The risk is not that
    // it uses one, it is that the two hosts disagree about what it means.
    let Some(command) = &target.cmd else {
        return;
    };
    if !matches!(target.kind, TargetKind::Genrule | TargetKind::Test) {
        return;
    }
    for operator in HOST_SHELL_OPERATORS {
        if command.contains(operator) {
            findings.push(Finding {
                rule: "host-shell-syntax",
                target: Some(name.to_string()),
                detail: format!("{operator:?} in a command run through the host shell"),
                why: "`/bin/sh` and `cmd.exe` disagree about it, so the workspace builds on one \
                      host and not the other; a `command` target with direct argv has no shell \
                      to disagree with",
            });
            break;
        }
    }
}

/// Every directory some target builds into, and their parents.
///
/// A genrule that writes `gen/config.h` and declares `includes = ["gen"]` so
/// its dependents can find the header is the ordinary way to generate one. The
/// directory does not exist before the build, which is not the same thing as
/// not existing.
fn produced_directories(manifest: &Manifest) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for target in manifest.targets.values() {
        // A declared output names a file, so its parents are the directories.
        // An `output_dirs` entry names a directory, so it counts itself.
        let outputs = target.outputs.iter().map(|out| ancestors_of(out));
        let owned = target
            .output_dirs
            .iter()
            .map(|dir| ancestors_of(dir).chain(std::iter::once(dir.as_str())));
        for path in outputs.flatten() {
            directories.insert(path.to_string());
        }
        for path in owned.flatten() {
            directories.insert(path.to_string());
        }
    }
    directories
}

/// The proper directory prefixes of a manifest path, longest first. Manifest
/// paths are always `/`-separated, so this does not depend on the host.
fn ancestors_of(path: &str) -> impl Iterator<Item = &str> {
    path.char_indices()
        .filter(|(_, c)| *c == '/')
        .map(move |(at, _)| &path[..at])
        .filter(|prefix| !prefix.is_empty())
}

fn missing_include_dirs(
    root: &Path,
    produced: &BTreeSet<String>,
    name: &str,
    target: &crate::manifest::Target,
    findings: &mut Vec<Finding>,
) {
    for include in &target.includes {
        if produced.contains(include.as_str()) {
            continue;
        }
        if !root.join(include).is_dir() {
            findings.push(Finding {
                rule: "missing-include-dir",
                target: Some(name.to_string()),
                detail: format!("{include:?} is not a directory"),
                why: "it is still put on the compiler's search path, where it finds nothing, so \
                      a header this target was meant to see is resolved from somewhere else \
                      or not at all",
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A workspace on disk that removes itself, so a failing assertion leaves
    /// nothing behind for the next run to inherit.
    struct Fixture {
        root: std::path::PathBuf,
        manifest: Manifest,
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.root);
        }
    }

    impl Fixture {
        fn findings(&self) -> Vec<Finding> {
            lint(&self.root, &self.manifest)
        }

        fn rules(&self) -> Vec<&'static str> {
            self.findings().into_iter().map(|f| f.rule).collect()
        }

        fn of(&self, rule: &str) -> Vec<Finding> {
            self.findings()
                .into_iter()
                .filter(|f| f.rule == rule)
                .collect()
        }
    }

    fn workspace(manifest: &str) -> Fixture {
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "frost-lint-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::create_dir_all(root.join("include")).unwrap();
        std::fs::write(root.join("src/a.c"), "int a(void) { return 1; }\n").unwrap();
        std::fs::write(root.join("src/b.c"), "int b(void) { return 2; }\n").unwrap();
        std::fs::write(root.join("include/a.h"), "int a(void);\n").unwrap();
        std::fs::write(root.join(crate::manifest::MANIFEST_FILE), manifest).unwrap();
        let loaded = Manifest::load(&root).expect("the fixture must load");
        Fixture {
            root,
            manifest: loaded,
        }
    }

    #[test]
    fn a_target_nothing_reaches_is_reported_and_a_reached_one_is_not() {
        let fixture = workspace(
            "[workspace]\ndefault_targets = [\"app\"]\n\n\
             [target.app]\nkind = \"cc_binary\"\nsrcs = [\"src/a.c\"]\ndeps = [\"used\"]\n\n\
             [target.used]\nkind = \"cc_library\"\nsrcs = [\"src/b.c\"]\n\n\
             [target.orphan]\nkind = \"cc_library\"\nsrcs = [\"src/b.c\"]\n",
        );
        let unreachable = fixture.of("unreachable-target");
        assert_eq!(unreachable.len(), 1, "{:#?}", fixture.findings());
        assert_eq!(unreachable[0].target.as_deref(), Some("orphan"));
    }

    #[test]
    fn a_test_is_its_own_entry_point() {
        // The negative case that matters: `frost test` selects tests directly,
        // so a rule that called every test unreachable would fire on every
        // workspace and be turned off immediately.
        let fixture = workspace(
            "[workspace]\ndefault_targets = [\"app\"]\n\n\
             [target.app]\nkind = \"cc_binary\"\nsrcs = [\"src/a.c\"]\n\n\
             [target.check]\nkind = \"test\"\ncmd = \"true\"\ninputs = [\"src/b.c\"]\n",
        );
        assert!(!fixture.rules().contains(&"unreachable-target"));
    }

    #[test]
    fn passing_a_variable_frost_already_passes_is_reported() {
        let fixture = workspace(
            "[toolchain.tools]\ncopy = \"cp\"\n\n\
             [workspace]\ndefault_targets = [\"gen\"]\n\n\
             [target.gen]\nkind = \"command\"\ntool = \"copy\"\n\
             args = [\"src/a.c\", \"${out}\"]\n\
             inputs = [\"src/a.c\"]\noutputs = [\".frost/out/${config}/a.c\"]\n\
             pass_env = [\"HOME\", \"PATH\", \"JAVA_HOME\"]\n",
        );
        let redundant = fixture.of("redundant-pass-env");
        let reported: Vec<&str> = redundant.iter().map(|f| f.detail.as_str()).collect();
        assert_eq!(redundant.len(), 2, "{reported:#?}");
        assert!(
            reported.iter().any(|d| d.contains("\"HOME\"")),
            "{reported:#?}"
        );
        assert!(
            reported.iter().any(|d| d.contains("\"PATH\"")),
            "{reported:#?}"
        );
        // JAVA_HOME is exactly what `pass_env` is for: Frost clears the
        // environment, so without naming it the action would not see it.
        assert!(!reported.iter().any(|d| d.contains("JAVA_HOME")));
    }

    #[test]
    fn pass_env_only_reaches_targets_whose_tool_is_declared() {
        // `redundant-pass-env` rests on this. PATH's remaining blind spot is a
        // command that finds a tool nothing declared, which is a genrule or a
        // shell test — and neither accepts `pass_env` at all. If that ever
        // changes, keying on PATH starts buying something and the rule needs
        // to say so instead of calling it redundant.
        for kind in ["genrule", "cc_binary"] {
            let root = std::env::temp_dir().join(format!("frost-lint-premise-{kind}"));
            let _ = std::fs::remove_dir_all(&root);
            std::fs::create_dir_all(root.join("src")).unwrap();
            std::fs::write(root.join("src/a.c"), "int a(void) { return 1; }\n").unwrap();
            let body = if kind == "genrule" {
                "cmd = \"cp src/a.c gen/a.c\"\ninputs = [\"src/a.c\"]\noutputs = [\"gen/a.c\"]\n"
            } else {
                "srcs = [\"src/a.c\"]\n"
            };
            std::fs::write(
                root.join(crate::manifest::MANIFEST_FILE),
                format!(
                    "[workspace]\ndefault_targets = [\"t\"]\n\n\
                     [target.t]\nkind = \"{kind}\"\n{body}pass_env = [\"PATH\"]\n"
                ),
            )
            .unwrap();
            let error = Manifest::load(&root)
                .expect_err("{kind} must not accept pass_env")
                .to_string();
            assert!(error.contains("invalid target"), "{kind}: {error}");
            let _ = std::fs::remove_dir_all(&root);
        }
    }

    #[test]
    fn an_absolute_path_in_an_argument_is_reported_and_a_relative_one_is_not() {
        let absolute = workspace(
            "[toolchain.tools]\ncc = \"cc\"\n\n\
             [workspace]\ndefault_targets = [\"obj\"]\n\n\
             [target.obj]\nkind = \"command\"\ntool = \"cc\"\n\
             args = [\"-I/opt/vendor/include\", \"-c\", \"${in}\"]\n\
             inputs = [\"src/a.c\"]\noutputs = [\".frost/out/${config}/a.o\"]\n",
        );
        assert!(
            absolute.rules().contains(&"absolute-path"),
            "{:#?}",
            absolute.findings()
        );

        let relative = workspace(
            "[toolchain.tools]\ncc = \"cc\"\n\n\
             [workspace]\ndefault_targets = [\"obj\"]\n\n\
             [target.obj]\nkind = \"command\"\ntool = \"cc\"\n\
             args = [\"-Iinclude\", \"-c\", \"${in}\"]\n\
             inputs = [\"src/a.c\"]\noutputs = [\".frost/out/${config}/a.o\"]\n",
        );
        assert!(!relative.rules().contains(&"absolute-path"));
    }

    #[test]
    fn shell_syntax_that_two_hosts_read_differently_is_reported() {
        let shell = workspace(
            "[workspace]\ndefault_targets = [\"gen\"]\n\n\
             [target.gen]\nkind = \"genrule\"\n\
             cmd = \"cp src/a.c gen/a.c && echo done\"\n\
             inputs = [\"src/a.c\"]\noutputs = [\"gen/a.c\"]\n",
        );
        assert!(shell.rules().contains(&"host-shell-syntax"));

        let plain = workspace(
            "[workspace]\ndefault_targets = [\"gen\"]\n\n\
             [target.gen]\nkind = \"genrule\"\ncmd = \"cp src/a.c gen/a.c\"\n\
             inputs = [\"src/a.c\"]\noutputs = [\"gen/a.c\"]\n",
        );
        assert!(!plain.rules().contains(&"host-shell-syntax"));
    }

    #[test]
    fn an_include_path_that_is_not_a_directory_is_reported() {
        let fixture = workspace(
            "[workspace]\ndefault_targets = [\"lib\"]\n\n\
             [target.lib]\nkind = \"cc_library\"\nsrcs = [\"src/a.c\"]\n\
             includes = [\"include\", \"headers\"]\n",
        );
        let missing = fixture.of("missing-include-dir");
        assert_eq!(missing.len(), 1, "{:#?}", fixture.findings());
        assert!(missing[0].detail.contains("headers"), "{missing:#?}");
    }

    #[test]
    fn a_directory_the_build_generates_is_not_missing() {
        // The sample workspaces do exactly this: a genrule writes a header and
        // declares the directory as an include so its dependents find it. It
        // does not exist before the build, which is not the same as not
        // existing, and reporting it would fire on the ordinary way to
        // generate a header.
        let fixture = workspace(
            "[workspace]\ndefault_targets = [\"app\"]\n\n\
             [target.gen]\nkind = \"genrule\"\ncmd = \"touch ${out}\"\n\
             inputs = [\"src/a.c\"]\noutputs = [\"gen/config.h\"]\n\
             includes = [\"gen\"]\n\n\
             [target.app]\nkind = \"cc_binary\"\nsrcs = [\"src/a.c\"]\n\
             deps = [\"gen\"]\nincludes = [\"gen\"]\n",
        );
        assert!(
            !fixture.rules().contains(&"missing-include-dir"),
            "{:#?}",
            fixture.findings()
        );
    }

    #[test]
    fn every_rule_says_what_it_costs() {
        // A finding a reader cannot act on is noise, and the identifier is
        // what makes one nameable in CI, so both are required of every rule
        // rather than remembered per rule.
        let fixture = workspace(
            "[toolchain.tools]\ncc = \"cc\"\n\n\
             [workspace]\ndefault_targets = [\"app\"]\n\n\
             [target.app]\nkind = \"cc_binary\"\nsrcs = [\"src/a.c\"]\n\
             includes = [\"nope\"]\n\n\
             [target.orphan]\nkind = \"command\"\ntool = \"cc\"\n\
             args = [\"-c\", \"${in}\"]\npass_env = [\"HOME\"]\n\
             inputs = [\"src/b.c\"]\noutputs = [\".frost/out/${config}/b.o\"]\n",
        );
        let findings = fixture.findings();
        let rules: BTreeSet<&str> = findings.iter().map(|f| f.rule).collect();
        assert_eq!(
            rules,
            BTreeSet::from([
                "missing-include-dir",
                "redundant-pass-env",
                "unreachable-target"
            ]),
            "{findings:#?}"
        );
        for finding in &findings {
            assert!(!finding.rule.is_empty());
            assert!(finding
                .rule
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == '-'));
            assert!(finding.why.len() > 20, "{finding:#?}");
            assert!(!finding.detail.is_empty());
        }
    }
}
