//! Shell completion: the candidate values, and installing the hook.
//!
//! Completers run in a process that was started to complete a word, not to
//! build, so they read the workspace on a best-effort path and return nothing
//! rather than failing when it cannot be read.

use std::ffi::OsStr;
use std::path::Path;
use std::path::PathBuf;

use anyhow::bail;
use anyhow::Context;
use anyhow::Result;
use clap::CommandFactory;
use clap_complete::engine::ValueCompleter;
use clap_complete::CompletionCandidate;
use frostbuild_core::manifest::Manifest;
use frostbuild_core::manifest::TargetKind;

use crate::cli::{Cli, CompletionShell};
use crate::doctor::info_entries;
use crate::frostrc;
use crate::query::AttrFilter;

fn completion_workspace() -> PathBuf {
    let args: Vec<_> = std::env::args_os().collect();
    let mut selected = None;
    for (index, arg) in args.iter().enumerate() {
        if arg == "-C" || arg == "--workspace" {
            selected = args.get(index + 1).map(PathBuf::from);
        } else if let Some(value) = arg
            .to_str()
            .and_then(|arg| arg.strip_prefix("--workspace="))
        {
            selected = Some(PathBuf::from(value));
        } else if let Some(value) = arg
            .to_str()
            .filter(|arg| arg.starts_with("-C") && arg.len() > 2)
            .map(|arg| &arg[2..])
        {
            selected = Some(PathBuf::from(value));
        }
    }
    let current = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    selected
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                current.join(path)
            }
        })
        .unwrap_or(current)
}

pub(crate) fn candidates(
    current: &OsStr,
    values: impl IntoIterator<Item = String>,
) -> Vec<CompletionCandidate> {
    let Some(current) = current.to_str() else {
        return Vec::new();
    };
    values
        .into_iter()
        .filter(|value| value.starts_with(current))
        .map(CompletionCandidate::new)
        .collect()
}

fn completion_manifest() -> Option<Manifest> {
    Manifest::load_reporting(&completion_workspace()).manifest
}

pub(crate) fn complete_fetch(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(
        current,
        Manifest::load_for_fetch(&completion_workspace())
            .into_iter()
            .flat_map(|manifest| manifest.fetches.into_keys()),
    )
}

pub(crate) fn complete_target(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(
        current,
        completion_manifest()
            .into_iter()
            .flat_map(|manifest| manifest.targets.into_keys()),
    )
}

pub(crate) fn complete_test_target(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(
        current,
        completion_manifest().into_iter().flat_map(|manifest| {
            manifest.targets.into_values().filter_map(|target| {
                matches!(target.kind, TargetKind::CcTest | TargetKind::Test).then_some(target.name)
            })
        }),
    )
}

/// The half of `--attr NAME=PATTERN` that comes from a closed set. The value
/// after `=` is a glob or a scalar the author is choosing, so completion stops
/// at the name.
pub(crate) fn complete_attr_filter(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(
        current,
        AttrFilter::NAMES.iter().map(|name| format!("{name}=")),
    )
}

pub(crate) fn complete_target_kind(current: &OsStr) -> Vec<CompletionCandidate> {
    candidates(
        current,
        TargetKind::ALL.iter().map(|kind| kind.as_str().to_string()),
    )
}

/// `[config.NAME]` sections, from whichever `.frostrc` files exist.
///
/// These are enumerable, unlike a test runner's filter grammar, so offering
/// them is a keystroke rather than a guess. A file that does not parse yields
/// nothing rather than an error: completion runs on every Tab, and a broken
/// config should be reported when a command runs, not while typing one.
pub(crate) fn complete_config(current: &OsStr) -> Vec<CompletionCandidate> {
    let mut values = Vec::new();
    let files = frostrc::user_config_path()
        .into_iter()
        .chain(std::iter::once(PathBuf::from(frostrc::WORKSPACE_FILE)));
    for path in files {
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        let Ok(document) = toml::from_str::<toml::Table>(&text) else {
            continue;
        };
        if let Some(toml::Value::Table(named)) = document.get("config") {
            values.extend(named.keys().cloned());
        }
    }
    values.sort();
    values.dedup();
    candidates(current, values)
}

pub(crate) fn complete_profile(current: &OsStr) -> Vec<CompletionCandidate> {
    let mut values = vec![frostbuild_core::manifest::DEFAULT_PROFILE.to_string()];
    if let Some(manifest) = completion_manifest() {
        values.extend(manifest.profiles.into_keys());
    }
    values.sort();
    values.dedup();
    candidates(current, values)
}

/// A shared cache is a directory, a `file://` directory or an HTTP prefix.
/// Offering the schemes turns "what can I even type here?" into a keystroke,
/// and a path-looking value falls through to directory candidates.
pub(crate) fn complete_remote_cache(current: &OsStr) -> Vec<CompletionCandidate> {
    let text = current.to_string_lossy();
    if text.is_empty() || ["f", "h"].iter().any(|start| text.starts_with(start)) {
        let schemes = ["file://", "http://", "https://"]
            .into_iter()
            .map(str::to_string)
            .collect::<Vec<_>>();
        let mut matches = candidates(current, schemes);
        if !matches.is_empty() {
            matches.extend(clap_complete::engine::PathCompleter::dir().complete(current));
            return matches;
        }
    }
    clap_complete::engine::PathCompleter::dir().complete(current)
}

/// npm owns the script names; reading them back is the whole point of
/// `import-npm`, so the flag should not make the author retype them.
pub(crate) fn complete_npm_script(current: &OsStr) -> Vec<CompletionCandidate> {
    let Ok(text) = std::fs::read_to_string(completion_workspace().join("package.json")) else {
        return Vec::new();
    };
    let Ok(package) = serde_json::from_str::<serde_json::Value>(&text) else {
        return Vec::new();
    };
    let names = package["scripts"]
        .as_object()
        .map(|scripts| scripts.keys().cloned().collect::<Vec<_>>())
        .unwrap_or_default();
    candidates(current, names)
}

pub(crate) fn complete_info_key(current: &OsStr) -> Vec<CompletionCandidate> {
    let values: Vec<String> = info_entries(&completion_workspace(), "debug", "host")
        .into_iter()
        .map(|(name, _)| name.to_string())
        .collect();
    candidates(current, values)
}

pub(crate) fn complete_platform(current: &OsStr) -> Vec<CompletionCandidate> {
    let mut values = vec![frostbuild_core::manifest::HOST_PLATFORM.to_string()];
    if let Some(manifest) = completion_manifest() {
        values.extend(manifest.platforms.into_keys());
    }
    values.sort();
    values.dedup();
    candidates(current, values)
}

/// The startup file each shell reads, and the line that turns on *dynamic*
/// completion — the one that asks this binary for candidates, so targets and
/// profiles follow the workspace instead of a snapshot taken at install time.
fn completion_hook(shell: CompletionShell) -> Result<(PathBuf, &'static str)> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("neither HOME nor USERPROFILE is set, so there is no startup file to edit")?;
    let config = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".config"));
    Ok(match shell {
        CompletionShell::Bash => (home.join(".bashrc"), "source <(COMPLETE=bash frost)"),
        CompletionShell::Zsh => (home.join(".zshrc"), "source <(COMPLETE=zsh frost)"),
        CompletionShell::Fish => (
            config.join("fish/config.fish"),
            "COMPLETE=fish frost | source",
        ),
        CompletionShell::Elvish => (
            config.join("elvish/rc.elv"),
            "eval (E:COMPLETE=elvish frost | slurp)",
        ),
        // Both profile locations depend on the host and the shell's own
        // configuration, and Nushell has no dynamic callback protocol at all.
        // Guessing a path here would write a file nothing reads.
        CompletionShell::Powershell | CompletionShell::Nushell => bail!(
            "--install cannot locate this shell's profile reliably. add the line from \
             `frost completions {}` to your profile instead",
            match shell {
                CompletionShell::Nushell => "nushell",
                _ => "powershell",
            }
        ),
    })
}

fn detect_shell() -> Result<CompletionShell> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = Path::new(&shell)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or_default();
    match name {
        "bash" => Ok(CompletionShell::Bash),
        "zsh" => Ok(CompletionShell::Zsh),
        "fish" => Ok(CompletionShell::Fish),
        "elvish" => Ok(CompletionShell::Elvish),
        other => bail!(
            "cannot tell which shell to install for (SHELL={other:?}). name it: \
             `frost completions bash --install`"
        ),
    }
}

const COMPLETION_BEGIN: &str = "# >>> frost completions >>>";
const COMPLETION_END: &str = "# <<< frost completions <<<";

pub(crate) fn install_completions(shell: Option<CompletionShell>, dry_run: bool) -> Result<i32> {
    let shell = match shell {
        Some(shell) => shell,
        None => detect_shell()?,
    };
    let (path, hook) = completion_hook(shell)?;
    let block = format!("{COMPLETION_BEGIN}\n{hook}\n{COMPLETION_END}\n");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();

    // Someone who already wired this up by hand gets left alone: a second
    // hook is not additive, it is a duplicate definition on every new shell.
    if !existing.contains(COMPLETION_BEGIN) && existing.contains("COMPLETE=") {
        println!(
            "frost: {} already sources a completion hook; leaving it alone",
            path.display()
        );
        return Ok(0);
    }

    let updated = match (
        existing.find(COMPLETION_BEGIN),
        existing.find(COMPLETION_END),
    ) {
        (Some(start), Some(end)) if end > start => {
            let end = end + COMPLETION_END.len();
            let mut updated = String::with_capacity(existing.len() + block.len());
            updated.push_str(&existing[..start]);
            updated.push_str(block.trim_end());
            updated.push_str(&existing[end..]);
            updated
        }
        _ => {
            let mut updated = existing.clone();
            if !updated.is_empty() && !updated.ends_with('\n') {
                updated.push('\n');
            }
            if !updated.is_empty() {
                updated.push('\n');
            }
            updated.push_str(&block);
            updated
        }
    };

    if updated == existing {
        println!(
            "frost: completion hook already installed in {}",
            path.display()
        );
        return Ok(0);
    }
    if dry_run {
        println!("frost: would write {}", path.display());
        for line in block.lines() {
            println!("  + {line}");
        }
        return Ok(0);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&path, updated)
        .with_context(|| format!("failed to write {}", path.display()))?;
    println!("frost: installed the completion hook in {}", path.display());
    println!("  restart the shell, or run: {hook}");
    Ok(0)
}

pub(crate) fn print_completions(shell: CompletionShell) {
    write_completions(shell, &mut std::io::stdout());
}

pub(crate) fn write_completions(shell: CompletionShell, output: &mut dyn std::io::Write) {
    let mut command = Cli::command();
    match shell {
        CompletionShell::Bash => {
            clap_complete::generate(clap_complete::Shell::Bash, &mut command, "frost", output)
        }
        CompletionShell::Zsh => {
            clap_complete::generate(clap_complete::Shell::Zsh, &mut command, "frost", output)
        }
        CompletionShell::Fish => {
            clap_complete::generate(clap_complete::Shell::Fish, &mut command, "frost", output)
        }
        CompletionShell::Powershell => clap_complete::generate(
            clap_complete::Shell::PowerShell,
            &mut command,
            "frost",
            output,
        ),
        CompletionShell::Elvish => {
            clap_complete::generate(clap_complete::Shell::Elvish, &mut command, "frost", output)
        }
        CompletionShell::Nushell => clap_complete::generate(
            clap_complete_nushell::Nushell,
            &mut command,
            "frost",
            output,
        ),
    }
}

#[cfg(test)]
mod completion_contract_tests {
    use super::*;
    use clap::builder::ValueHint;
    use clap::CommandFactory;
    use clap_complete::ArgValueCompleter;

    /// Values no shell can usefully guess: numbers, free identifiers and
    /// tool-specific expressions. Listing them here is the point of the test —
    /// a new argument has to make this choice deliberately instead of falling
    /// back to whatever the shell does by default.
    const FREE_TEXT: [&str; 41] = [
        // Seconds.
        "frost build::timeout",
        "frost test::timeout",
        // Names another ecosystem owns and Frost cannot enumerate cheaply: a
        // Bazel label needs a Bazel query, and the wheel/JAR metadata is what
        // the author is deciding at that moment.
        "frost bazel-dev::target",
        "frost pack-jar::main_class",
        "frost pack-wheel::distribution",
        "frost pack-wheel::version",
        // Numbers.
        "frost build::jobs",
        "frost build::local_cpu_resources",
        "frost build::local_ram_resources",
        "frost build::local_test_jobs",
        "frost build::remote_timeout",
        "frost build::check_determinism",
        "frost run::jobs",
        "frost watch::jobs",
        "frost watch::debounce_ms",
        "frost dev::jobs",
        "frost dev::debounce_ms",
        "frost debug::jobs",
        "frost ide::jobs",
        "frost test::jobs",
        "frost test::local_cpu_resources",
        "frost test::local_ram_resources",
        "frost test::local_test_jobs",
        "frost test::remote_timeout",
        "frost simulate::jobs",
        "frost simulate::local_cpu_resources",
        "frost simulate::local_ram_resources",
        "frost simulate::local_test_jobs",
        "frost bazel-dev::debounce_ms",
        "frost query allpaths::limit",
        // Argv handed to another program, which owns its own grammar.
        "frost run::program_args",
        "frost watch::run",
        "frost dev::program_args",
        "frost debug::program_args",
        "frost bazel-dev::bazel_args",
        "frost bazel-dev::args",
        // A Bazel query expression; answering it means running Bazel.
        "frost import-bazel::query",
        // A test runner's own filter grammar, the environment, and argv handed
        // to that runner. Frost does not know any runner's case names, which is
        // exactly why the filter travels as an environment variable rather than
        // as a flag Frost would have to spell per language.
        "frost test::test_filter",
        "frost test::test_env",
        "frost test::test_arg",
        "frost test::runs_per_test",
    ];

    fn walk(command: &clap::Command, path: &str, undeclared: &mut Vec<String>) {
        for arg in command.get_arguments() {
            if !arg.get_action().takes_values() {
                continue;
            }
            let declared = arg.get_value_hint() != ValueHint::Unknown
                || arg.get::<ArgValueCompleter>().is_some()
                || !arg.get_possible_values().is_empty();
            let id = format!("{path}::{}", arg.get_id());
            if !declared && !FREE_TEXT.contains(&id.as_str()) {
                undeclared.push(id);
            }
        }
        for sub in command.get_subcommands() {
            walk(sub, &format!("{path} {}", sub.get_name()), undeclared);
        }
    }

    #[test]
    fn every_value_taking_argument_declares_how_it_completes() {
        let command = Cli::command();
        let mut undeclared = Vec::new();
        walk(&command, "frost", &mut undeclared);
        assert!(
            undeclared.is_empty(),
            "these arguments complete as nothing: {undeclared:#?}"
        );
    }
}
