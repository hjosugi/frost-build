//! Manifest patterns that parse, build, and are still a mistake.
//!
//! Every rule here catches something nothing else does. That is the entry
//! requirement, and it removed several obvious-looking candidates: duplicate
//! outputs are already a hard error in `push_action`, an undeclared profile is
//! already rejected in `from_manifest_configured`, and an absolute path in
//! `srcs`, `inputs`, `outputs` or `includes` is already refused by
//! `validate_rel_path`, and a glob matching nothing is already refused by
//! `expand_paths`. A lint that restates an error teaches people that lints
//! are noise.
//!
//! What is left is the class of thing that is legal, does exactly what it
//! says, and costs you later: a target nothing can reach, an `-I` pointing at
//! a directory that is not there, an environment opt-in that quietly makes
//! your cache per-machine.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::Serialize;

use crate::manifest::{Manifest, TargetKind};

/// One finding. `rule` is stable: it is what a `# frost-lint: allow` comment
/// or a CI filter would name, so renaming one is a breaking change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub rule: &'static str,
    /// The target it was found in, or the workspace when it belongs to no one.
    pub target: String,
    pub message: String,
    /// One line on why this costs something. A finding the reader cannot act
    /// on is a finding they learn to skip.
    pub why: &'static str,
}

/// Host variables that are deliberately *outside* the action key, per
/// docs/16. Naming one in `pass_env` puts it back in.
const VOLATILE: [&str; 5] = ["PATH", "HOME", "TMPDIR", "TMP", "TEMP"];

/// Metacharacters whose meaning differs between `/bin/sh` and `cmd.exe`, which
/// are the two shells frost runs a genrule through.
const SHELL_METACHARACTERS: [&str; 6] = ["&&", "||", "|", ">", "<", ";"];

/// Lint a manifest. `root` answers the questions the text alone cannot: a
/// directory either exists in this tree or it does not.
pub fn lint(manifest: &Manifest, root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let generated = generated_directories(manifest);
    findings.extend(unreachable_targets(manifest));
    for (name, target) in &manifest.targets {
        findings.extend(missing_include_dirs(name, target, root, &generated));
        findings.extend(volatile_pass_env(name, target));
        findings.extend(absolute_paths_in_text(name, target));
        findings.extend(shell_dependent_cmd(name, target));
    }
    // A target may declare it can live with a rule. Applied here rather than
    // inside each rule so every rule gets it for free and none can forget.
    findings.retain(|finding| {
        manifest
            .targets
            .get(&finding.target)
            .is_none_or(|target| !target.lint_allow.iter().any(|rule| rule == finding.rule))
    });
    // Stable order, so a CI diff of two runs shows what changed rather than
    // how the map happened to iterate.
    findings.sort_by(|a, b| (a.target.as_str(), a.rule).cmp(&(b.target.as_str(), b.rule)));
    findings
}

/// Targets nothing reaches: not a default, not a test, and not a dependency
/// of anything.
///
/// Test kinds are roots. `frost test` selects them by kind rather than by
/// dependency, so a `cc_test` that nothing depends on is the normal shape of a
/// test and not a finding — reporting it would fire on almost every workspace,
/// which is how a rule teaches people to pass `--no-verify`.
fn unreachable_targets(manifest: &Manifest) -> Vec<Finding> {
    let mut reachable: BTreeSet<&str> = manifest
        .default_targets
        .iter()
        .map(String::as_str)
        .chain(
            manifest
                .targets
                .iter()
                .filter(|(_, target)| matches!(target.kind, TargetKind::Test | TargetKind::CcTest))
                .map(|(name, _)| name.as_str()),
        )
        .collect();
    // Transitive: a target reachable only through another unreachable one is
    // still unreachable, so this walks rather than checking direct edges.
    let mut frontier: Vec<&str> = reachable.iter().copied().collect();
    while let Some(name) = frontier.pop() {
        let Some(target) = manifest.targets.get(name) else {
            continue;
        };
        for dep in &target.deps {
            if reachable.insert(dep.as_str()) {
                frontier.push(dep.as_str());
            }
        }
    }
    manifest
        .targets
        .iter()
        .filter(|(name, _)| !reachable.contains(name.as_str()))
        .map(|(name, _)| Finding {
            rule: "unreachable-target",
            target: name.clone(),
            message: format!("{name:?} is not a default target and nothing depends on it"),
            why: "it is never built unless named explicitly, so it rots without anyone noticing",
        })
        .collect()
}

/// Every directory some target writes an output into, and their parents.
///
/// A genrule declaring `gen/config.h` makes `gen` exist, so an `includes` entry
/// naming it is correct even on a tree that has never been built.
fn generated_directories(manifest: &Manifest) -> BTreeSet<String> {
    let mut directories = BTreeSet::new();
    for target in manifest.targets.values() {
        for output in target.outputs.iter().chain(target.output_dirs.iter()) {
            let mut path = output.as_str();
            while let Some((parent, _)) = path.rsplit_once('/') {
                directories.insert(parent.to_string());
                path = parent;
            }
        }
        for directory in &target.output_dirs {
            directories.insert(directory.clone());
        }
    }
    directories
}

fn missing_include_dirs(
    name: &str,
    target: &crate::manifest::Target,
    root: &Path,
    generated: &BTreeSet<String>,
) -> Vec<Finding> {
    target
        .includes
        .iter()
        .filter(|directory| !root.join(directory).is_dir())
        // A directory some target generates into does not exist before the
        // first build, which is not the same thing as not existing. Reporting
        // it would make the rule fire on any workspace with a generated
        // header -- and be silent after one build, which is worse than either.
        .filter(|directory| !generated.contains(directory.as_str()))
        .map(|directory| Finding {
            rule: "missing-include-dir",
            target: name.to_string(),
            message: format!("include directory {directory:?} does not exist"),
            why: "the compiler is handed a -I that cannot resolve anything, so a missing header \
                  fails later and further away",
        })
        .collect()
}

fn volatile_pass_env(name: &str, target: &crate::manifest::Target) -> Vec<Finding> {
    target
        .pass_env
        .iter()
        .filter(|variable| VOLATILE.contains(&variable.as_str()))
        .map(|variable| Finding {
            rule: "volatile-pass-env",
            target: name.to_string(),
            message: format!("pass_env names {variable:?}"),
            why: "its value differs per machine and per CI step, and pass_env puts it in the \
                  action key, so nothing this target builds is ever shared between them",
        })
        .collect()
}

/// Absolute paths where nothing validates them.
///
/// `srcs`, `inputs`, `outputs` and `includes` are already refused by
/// `validate_rel_path`. `args`, `cmd` and `env` values are free text, which is
/// exactly where an absolute path survives to break on someone else's machine.
fn absolute_paths_in_text(name: &str, target: &crate::manifest::Target) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut check = |field: &str, text: &str| {
        for token in text.split_whitespace() {
            let candidate = token.trim_start_matches(['"', '\'', '=']);
            let absolute = candidate.starts_with('/')
                || (candidate.len() > 2
                    && candidate.as_bytes()[1] == b':'
                    && candidate.as_bytes()[0].is_ascii_alphabetic()
                    && matches!(candidate.as_bytes()[2], b'/' | b'\\'));
            // A leading `//` is a frost label, not a filesystem path.
            if absolute && !candidate.starts_with("//") {
                findings.push(Finding {
                    rule: "absolute-path",
                    target: name.to_string(),
                    message: format!("{field} contains the absolute path {candidate:?}"),
                    why: "it names a location on the machine that wrote it, so the build works \
                          there and nowhere else",
                });
            }
        }
    };
    for arg in &target.args {
        check("args", arg);
    }
    if let Some(cmd) = &target.cmd {
        check("cmd", cmd);
    }
    for (key, value) in &target.env {
        check(&format!("env {key}"), value);
    }
    findings
}

fn shell_dependent_cmd(name: &str, target: &crate::manifest::Target) -> Vec<Finding> {
    if target.kind != TargetKind::Genrule {
        return Vec::new();
    }
    let Some(cmd) = &target.cmd else {
        return Vec::new();
    };
    let found: Vec<&str> = SHELL_METACHARACTERS
        .iter()
        .copied()
        .filter(|token| cmd.contains(token))
        .collect();
    if found.is_empty() {
        return Vec::new();
    }
    vec![Finding {
        rule: "shell-dependent-cmd",
        target: name.to_string(),
        message: format!("cmd uses {}", found.join(", ")),
        why: "a genrule runs through /bin/sh on Unix and cmd.exe on Windows, where these do not \
              mean the same thing",
    }]
}

/// Findings grouped for `--json`, so a consumer reads a shape rather than
/// parsing lines.
#[derive(Debug, Serialize)]
pub struct LintReport<'a> {
    pub findings: &'a [Finding],
    pub count: usize,
    /// Rule -> how many, so a CI job can threshold one rule without parsing.
    pub by_rule: BTreeMap<&'static str, usize>,
}

impl<'a> LintReport<'a> {
    pub fn new(findings: &'a [Finding]) -> Self {
        let mut by_rule = BTreeMap::new();
        for finding in findings {
            *by_rule.entry(finding.rule).or_insert(0) += 1;
        }
        Self {
            findings,
            count: findings.len(),
            by_rule,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lint_str(text: &str, root: &Path) -> Vec<Finding> {
        lint(&Manifest::parse_str(text).unwrap(), root)
    }

    fn rules(findings: &[Finding]) -> Vec<&str> {
        findings.iter().map(|f| f.rule).collect()
    }

    #[test]
    fn an_unreachable_target_is_found_and_a_reachable_one_is_not() {
        let text = r#"
            [workspace]
            default_targets = ["app"]

            [target.app]
            kind = "cc_binary"
            srcs = ["main.c"]
            deps = ["used"]

            [target.used]
            kind = "cc_library"
            srcs = ["used.c"]

            [target.orphan]
            kind = "cc_library"
            srcs = ["orphan.c"]
            "#;
        let findings = lint_str(text, Path::new("/nonexistent"));
        let unreachable: Vec<&str> = findings
            .iter()
            .filter(|f| f.rule == "unreachable-target")
            .map(|f| f.target.as_str())
            .collect();
        // `used` is reached through `app`, so only `orphan` is reported.
        assert_eq!(unreachable, ["orphan"]);
    }

    #[test]
    fn reachability_is_transitive() {
        // `deep` is reachable only through `mid`, which is reachable only
        // through the default. Checking direct edges alone would call it
        // unreachable, which would be a false positive on a normal library.
        let text = r#"
            [workspace]
            default_targets = ["app"]

            [target.app]
            kind = "cc_binary"
            srcs = ["main.c"]
            deps = ["mid"]

            [target.mid]
            kind = "cc_library"
            srcs = ["mid.c"]
            deps = ["deep"]

            [target.deep]
            kind = "cc_library"
            srcs = ["deep.c"]
            "#;
        let findings = lint_str(text, Path::new("/nonexistent"));
        assert!(
            !rules(&findings).contains(&"unreachable-target"),
            "{findings:?}"
        );
    }

    #[test]
    fn pass_env_of_a_volatile_variable_is_found() {
        let positive = r#"
            [toolchain.tools]
            runner = "runner"

            [workspace]
            default_targets = ["t"]

            [target.t]
            kind = "command"
            tool = "runner"
            inputs = ["a.txt"]
            outputs = [".frost/out/${config}/o"]
            args = ["x"]
            pass_env = ["PATH"]
            "#;
        let findings = lint_str(positive, Path::new("/nonexistent"));
        assert!(
            rules(&findings).contains(&"volatile-pass-env"),
            "{findings:?}"
        );

        // A variable that genuinely selects what the compiler finds is keyed
        // on purpose (docs/16) and is not a finding.
        let negative = positive.replace(r#"pass_env = ["PATH"]"#, r#"pass_env = ["CPATH"]"#);
        let findings = lint_str(&negative, Path::new("/nonexistent"));
        assert!(
            !rules(&findings).contains(&"volatile-pass-env"),
            "{findings:?}"
        );
    }

    #[test]
    fn an_absolute_path_in_free_text_is_found_but_a_label_is_not() {
        let manifest = |args: &str| {
            format!(
                r#"
                [toolchain.tools]
                runner = "runner"

                [workspace]
                default_targets = ["t"]

                [target.t]
                kind = "command"
                tool = "runner"
                inputs = ["a.txt"]
                outputs = [".frost/out/${{config}}/o"]
                args = {args}
                "#
            )
        };
        let findings = lint_str(&manifest(r#"["--sdk", "/opt/local/sdk"]"#), Path::new("/x"));
        assert!(rules(&findings).contains(&"absolute-path"), "{findings:?}");

        // `//pkg:target` is a label, not a path from the filesystem root.
        let findings = lint_str(&manifest(r#"["--dep", "//core:core"]"#), Path::new("/x"));
        assert!(!rules(&findings).contains(&"absolute-path"), "{findings:?}");

        // A relative path is the correct spelling and is not a finding.
        let findings = lint_str(&manifest(r#"["--sdk", "vendor/sdk"]"#), Path::new("/x"));
        assert!(!rules(&findings).contains(&"absolute-path"), "{findings:?}");
    }

    #[test]
    fn a_shell_dependent_genrule_is_found_and_a_plain_one_is_not() {
        let manifest = |cmd: &str| {
            format!(
                r#"
                [workspace]
                default_targets = ["gen"]

                [target.gen]
                kind = "genrule"
                cmd = "{cmd}"
                inputs = ["in.txt"]
                outputs = ["out.txt"]
                "#
            )
        };
        let findings = lint_str(
            &manifest("cp ${in} ${out} && touch ${out}"),
            Path::new("/x"),
        );
        assert!(
            rules(&findings).contains(&"shell-dependent-cmd"),
            "{findings:?}"
        );

        let findings = lint_str(&manifest("cp ${in} ${out}"), Path::new("/x"));
        assert!(
            !rules(&findings).contains(&"shell-dependent-cmd"),
            "{findings:?}"
        );
    }

    #[test]
    fn a_target_can_declare_it_can_live_with_a_rule() {
        // The finding is true -- a Maven build really does need $HOME/.m2 --
        // and the workspace still has to pass HOME. Suppression keeps the cost
        // written next to the thing that pays it.
        let manifest = |allow: &str| {
            format!(
                r#"
                [toolchain.tools]
                runner = "runner"

                [workspace]
                default_targets = ["t"]

                [target.t]
                kind = "command"
                tool = "runner"
                inputs = ["a.txt"]
                outputs = [".frost/out/${{config}}/o"]
                args = ["x"]
                pass_env = ["HOME"]
                {allow}
                "#
            )
        };
        let findings = lint_str(&manifest(""), Path::new("/nonexistent"));
        assert!(
            rules(&findings).contains(&"volatile-pass-env"),
            "{findings:?}"
        );

        let findings = lint_str(
            &manifest(r#"lint_allow = ["volatile-pass-env"]"#),
            Path::new("/nonexistent"),
        );
        assert!(
            !rules(&findings).contains(&"volatile-pass-env"),
            "{findings:?}"
        );

        // Allowing one rule does not silence the others.
        let findings = lint_str(
            &manifest(r#"lint_allow = ["absolute-path"]"#),
            Path::new("/nonexistent"),
        );
        assert!(
            rules(&findings).contains(&"volatile-pass-env"),
            "{findings:?}"
        );
    }
    #[test]
    fn findings_are_ordered_so_two_runs_can_be_diffed() {
        let text = r#"
            [workspace]
            default_targets = []

            [target.zebra]
            kind = "cc_library"
            srcs = ["z.c"]

            [target.alpha]
            kind = "cc_library"
            srcs = ["a.c"]
            "#;
        let findings = lint_str(text, Path::new("/nonexistent"));
        let targets: Vec<&str> = findings.iter().map(|f| f.target.as_str()).collect();
        let mut sorted = targets.clone();
        sorted.sort_unstable();
        assert_eq!(targets, sorted);
    }
}
