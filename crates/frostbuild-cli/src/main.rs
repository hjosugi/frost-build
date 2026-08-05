//! The `frost` binary: parse a command line, then hand the work to the module
//! that owns the command.
//!
//! Nothing here implements a command. [`run`] is a dispatch table, and the rule
//! it follows is that an arm either delegates in one call or is small enough to
//! read at a glance — `build` and `test` are the two long arms, and they are
//! long because they name every field of a [`build::BuildRequest`], not because
//! they do anything.
//!
//! The command surface itself is in [`cli`], apart from the behaviour, because
//! it is a compatibility promise checked against a snapshot. Everything shared
//! by more than one command — loading the graph, resolving target names — is in
//! [`graph`].

use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{CommandFactory, Parser};

mod bazel;
mod build;
mod cache;
mod clean;
mod cli;
mod compdb;
mod completions;
mod coverage;
mod daemon;
mod doctor;
mod events;
mod explain;
mod frostrc;
mod graph;
mod ide;
mod init;
mod jar;
mod journal;
mod launch;
mod lsp;
mod ninja;
mod npm;
mod progress;
mod query;
mod report;
mod simulate;
mod style;
mod watch;
mod wheel;
mod wrapper;

use crate::build::{parse_test_options, run_build_selected, run_pick, BuildRequest};
use crate::cli::{Cli, Cmd, JournalCmd, TestOutputArg};
use crate::completions::{install_completions, print_completions};
use crate::daemon::daemon_command;
use crate::doctor::{run_doctor, run_info};
use crate::ide::run_ide;
use crate::init::run_init;
use crate::journal::{run_journal_diff, run_journal_export};
use crate::launch::{run_debug, run_target};
use crate::ninja::import_ninja;
use crate::query::run_query;
use crate::simulate::run_simulate;
use crate::style::{run_fmt, run_lint};
use crate::watch::{run_dev, run_watch, WatchRequest};

#[cfg(windows)]
fn main() {
    // Windows executables default to a 1 MiB main-thread stack.  Constructing
    // the full clap command tree can exceed that in debug builds as the CLI
    // grows, so run the actual entry point on an explicitly sized stack.  Keep
    // this Windows-only to avoid adding thread startup to Unix no-op latency.
    match std::thread::Builder::new()
        .name("frost-main".into())
        .stack_size(8 * 1024 * 1024)
        .spawn(frost_main)
    {
        Ok(worker) => {
            if worker.join().is_err() {
                eprintln!("frost: main thread panicked");
                std::process::exit(2);
            }
        }
        Err(error) => {
            eprintln!("frost: failed to start main thread: {error}");
            std::process::exit(2);
        }
    }
}

#[cfg(not(windows))]
fn main() {
    frost_main();
}

fn frost_main() {
    // Dynamic completion scripts call back into this binary, allowing target,
    // profile and platform candidates to reflect the current frost.toml.
    clap_complete::CompleteEnv::with_factory(Cli::command)
        .bin("frost")
        .complete();
    if let Err(error) = frostbuild_exec::install_signal_handler() {
        eprintln!("frost: failed to install signal handler: {error:#}");
        std::process::exit(2);
    }
    let cli = match resolve_command_line() {
        Ok(cli) => cli,
        Err(error) => {
            eprintln!("frost: error: {error:#}");
            std::process::exit(2);
        }
    };
    match run(cli) {
        Ok(code) => std::process::exit(code),
        Err(err) => {
            eprintln!("frost: error: {err:#}");
            std::process::exit(2);
        }
    }
}

/// Parse the command line, with `.frostrc` spliced in ahead of it.
///
/// Two passes: the first learns the workspace, the subcommand and which
/// `--config` sections were asked for, because all three are needed to know
/// what the file contributes. The second parses the real thing.
///
/// Config arguments go *before* the user's, so clap's last-occurrence-wins
/// rule gives the documented precedence without this module reimplementing it.
fn resolve_command_line() -> Result<Cli> {
    let argv: Vec<std::ffi::OsString> = std::env::args_os().collect();
    let first = Cli::try_parse_from(&argv);
    // A command line clap rejects is reported by clap, not by this: a config
    // file cannot fix a typo, and a parse error here would replace a good
    // message with a worse one.
    let Ok(first) = first else {
        return Ok(Cli::parse_from(&argv));
    };
    if first.no_frostrc {
        return Ok(first);
    }
    // From clap's own parse, not from argv[1]: a global flag may come first,
    // as in `frost -C dir build`, and guessing the position meant the file was
    // silently ignored for exactly the invocation `-C` exists for.
    let command = Cli::command();
    let Some(subcommand) = command
        .clone()
        .try_get_matches_from(&argv)
        .ok()
        .and_then(|matches| matches.subcommand_name().map(str::to_string))
    else {
        return Ok(first);
    };
    let accepts = |key: &str| {
        let long = key.replace('_', "-");
        let has = |cmd: &clap::Command| {
            cmd.get_arguments()
                .any(|arg| arg.get_long() == Some(long.as_str()))
        };
        has(&command)
            || command
                .get_subcommands()
                .any(|sub| sub.get_name() == subcommand && has(sub))
    };
    let resolved = frostrc::resolve(&first.workspace, &subcommand, &first.config, &accepts)?;
    frostrc::validate(&command, &subcommand, &resolved)?;
    if resolved.args.is_empty() {
        return Ok(first);
    }

    // Rebuilt as [program, subcommand, <from file>, <everything the user
    // typed>]. Splicing after the subcommand keeps a global flag typed before
    // it — `frost -C dir build` — in front of the file's arguments, which is
    // where it has to be for clap to see it at all.
    let mut spliced: Vec<std::ffi::OsString> = Vec::with_capacity(argv.len() + resolved.args.len());
    let position = argv
        .iter()
        .position(|arg| arg == std::ffi::OsStr::new(&subcommand))
        .unwrap_or(1);
    spliced.extend(argv[..=position].iter().cloned());
    spliced.extend(resolved.args.iter().map(std::ffi::OsString::from));
    spliced.extend(argv[position + 1..].iter().cloned());
    Ok(Cli::parse_from(spliced))
}

fn human_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0usize;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{bytes} B")
    } else {
        format!("{value:.2} {}", UNITS[unit])
    }
}

fn run(cli: Cli) -> Result<i32> {
    let root = cli
        .workspace
        .canonicalize()
        .with_context(|| format!("workspace {} not found", cli.workspace.display()))?;
    // Said before the work, not after: the point is to name the version
    // difference before the reader starts debugging whatever it caused.
    wrapper::warn_on_version_mismatch(&root);

    match cli.command {
        Cmd::Build {
            targets,
            jobs,
            keep_going,
            explain,
            verbose,
            profile,
            platform,
            all_platforms,
            no_cache,
            no_stamp,
            stamp_optional,
            remote_cache,
            remote_upload,
            remote_timeout,
            sandbox,
            check_determinism,
            timeout,
            trace,
            report,
            stats,
            no_tui,
            daemon,
            scheduler,
            estimator,
        } => run_build_selected(
            &root,
            BuildRequest {
                targets,
                jobs,
                keep_going,
                explain,
                verbose,
                profile,
                platform,
                no_cache,
                sandbox,
                check_determinism: check_determinism.is_some(),
                trace,
                report,
                stats,
                remote_cache,
                remote_upload,
                remote_timeout,
                no_tui,
                timeout,
                test_mode: false,
                test_options: Default::default(),
                runs_per_test: 1,
                test_output: TestOutputArg::Errors,
                build_event_json: cli.build_event_json.clone(),
                no_stamp,
                stamp_optional,
                daemon,
                affected: false,
                predictive: false,
                all: false,
                scheduler,
                estimator,
            },
            all_platforms,
        ),
        Cmd::Run {
            target,
            jobs,
            profile,
            platform,
            runner,
            print,
            program_args,
        } => run_target(
            &root,
            target,
            jobs,
            profile,
            platform,
            runner,
            print,
            program_args,
        ),
        Cmd::Watch {
            targets,
            jobs,
            profile,
            platform,
            debounce_ms,
            run,
        } => run_watch(
            &root,
            WatchRequest {
                targets,
                jobs,
                profile,
                platform,
                debounce: Duration::from_millis(debounce_ms),
                run,
                auto_run: None,
            },
        ),
        Cmd::Dev {
            target,
            jobs,
            profile,
            platform,
            debounce_ms,
            runner,
            program_args,
        } => run_dev(
            &root,
            target,
            jobs,
            profile,
            platform,
            Duration::from_millis(debounce_ms),
            runner,
            program_args,
        ),
        Cmd::Debug {
            target,
            jobs,
            profile,
            platform,
            debugger,
            print,
            program_args,
        } => run_debug(
            &root,
            target,
            jobs,
            profile,
            platform,
            debugger,
            print,
            program_args,
        ),
        Cmd::Ide {
            target,
            jobs,
            profile,
            platform,
            output,
            dry_run,
        } => run_ide(&root, target, jobs, profile, platform, output, dry_run),
        Cmd::Doctor {
            profile,
            platform,
            json,
        } => {
            // Resolved again here rather than threaded through every command:
            // `doctor` is the only one that reports it, and the read is a
            // couple of small files.
            let frostrc = (!cli.no_frostrc)
                .then(|| frostrc::resolve(&root, "build", &cli.config, &|_| true))
                .transpose()?
                .unwrap_or_default();
            run_doctor(&root, &profile, &platform, json, &frostrc)
        }
        Cmd::Test {
            targets,
            jobs,
            keep_going,
            timeout,
            affected,
            predictive,
            all,
            no_cache,
            no_stamp,
            stamp_optional,
            test_filter,
            test_env,
            test_arg,
            runs_per_test,
            test_output,
            remote_cache,
            remote_upload,
            remote_timeout,
            explain,
            report,
            profile,
            platform,
            all_platforms,
            sandbox,
            no_tui,
            daemon,
            scheduler,
            estimator,
        } => run_build_selected(
            &root,
            BuildRequest {
                targets,
                jobs,
                keep_going,
                explain,
                verbose: false,
                profile,
                platform,
                no_cache,
                sandbox,
                check_determinism: false,
                trace: None,
                report,
                stats: false,
                remote_cache,
                remote_upload,
                remote_timeout,
                no_tui,
                timeout,
                test_mode: true,
                test_options: parse_test_options(test_filter, test_env, test_arg)?,
                runs_per_test,
                test_output,
                build_event_json: cli.build_event_json.clone(),
                no_stamp,
                stamp_optional,
                daemon,
                affected,
                predictive,
                all,
                scheduler,
                estimator,
            },
            all_platforms,
        ),
        Cmd::Plan {
            targets,
            profile,
            platform,
        } => build::run_plan(&root, targets, &profile, &platform),
        Cmd::Clean {
            cache,
            profile,
            platform,
        } => clean::run_clean(&root, cache, profile, platform),
        Cmd::Graph {
            dot,
            profile,
            platform,
        } => graph::run_graph(&root, dot, &profile, &platform),
        Cmd::Compdb {
            output,
            profile,
            platform,
        } => compdb::run_compdb(&root, output, &profile, &platform),
        Cmd::CoverageLcov {
            gcda,
            objects,
            output,
            gcov,
        } => coverage::run_coverage_lcov(&root, &gcda, &objects, &output, &gcov),
        Cmd::Explain {
            target,
            profile,
            platform,
        } => explain::run_explain(&root, target, &profile, &platform),
        Cmd::Lsp => lsp::serve(&root),
        Cmd::Init {
            dry_run,
            language,
            wrapper,
        } => run_init(&root, dry_run, language, wrapper),
        Cmd::Simulate {
            targets,
            jobs,
            profile,
            platform,
            json,
        } => run_simulate(&root, targets, jobs, &profile, &platform, json),
        Cmd::Query { function } => run_query(&root, &function),
        Cmd::Fmt { check } => run_fmt(&root, check),
        Cmd::Lint { json } => run_lint(&root, json),
        Cmd::Journal { command } => match command {
            JournalCmd::Export {
                out,
                profile,
                platform,
            } => run_journal_export(&root, &profile, &platform, out.as_deref()),
            JournalCmd::Diff { first, second } => run_journal_diff(&first, &second),
        },
        Cmd::Cache { command } => cache::run_cache(&root, command),
        Cmd::Daemon { command } => daemon_command(&root, command),
        Cmd::ImportNinja { ninja, output } => import_ninja(&root, ninja, output),
        Cmd::ImportBazel {
            query,
            bazel,
            dry_run,
        } => bazel::run_import(&root, &query, bazel.as_deref(), dry_run),
        Cmd::ImportNpm {
            scripts,
            vite_builds,
            npm,
            node,
            dry_run,
        } => npm::run_import(&root, &scripts, vite_builds, &npm, &node, dry_run),
        Cmd::BazelDev {
            target,
            bazel,
            debounce_ms,
            bazel_args,
            args,
        } => bazel::run_dev(
            &root,
            &target,
            bazel.as_deref(),
            Duration::from_millis(debounce_ms),
            &bazel_args,
            &args,
        ),
        Cmd::PackJar {
            input,
            output,
            main_class,
        } => {
            let entries = jar::pack(&root, &input, &output, main_class.as_deref())?;
            println!(
                "frost: packed {entries} files -> {}",
                output.to_string_lossy()
            );
            Ok(0)
        }
        Cmd::PackWheel {
            input,
            distribution,
            version,
            output,
        } => {
            let entries = wheel::pack(&root, &input, &distribution, &version, &output)?;
            println!(
                "frost: packed {entries} files -> {}",
                output.to_string_lossy()
            );
            Ok(0)
        }
        Cmd::Info {
            key,
            profile,
            platform,
            json,
        } => run_info(&root, key.as_deref(), &profile, &platform, json),
        Cmd::Completions {
            shell,
            install,
            dry_run,
        } => {
            if install {
                return install_completions(shell, dry_run);
            }
            let Some(shell) = shell else {
                bail!("name a shell, or pass --install to detect it from $SHELL");
            };
            print_completions(shell);
            Ok(0)
        }
        Cmd::Pick {
            tests,
            print,
            profile,
            platform,
        } => run_pick(&root, tests, print, profile, platform),
    }
}

#[cfg(test)]
mod cli_surface_tests {
    use super::*;

    use clap::CommandFactory;

    const SNAPSHOT: &str = include_str!("../tests/cli-surface.txt");

    /// The rendered command tree: every subcommand with its flags and
    /// positionals, in a stable order.
    ///
    /// `docs/28_compatibility_contract.md` makes this surface part of what a
    /// release promises, so it is checked in rather than merely described. A
    /// diff here is not a failure to fix by regenerating — it is the moment to
    /// decide whether the change is additive, and if it is not, to follow the
    /// deprecation procedure in that document.
    fn render(command: &clap::Command, path: &str, out: &mut String) {
        let mut flags: Vec<String> = Vec::new();
        let mut positionals: Vec<String> = Vec::new();
        for arg in command.get_arguments() {
            if arg.is_positional() {
                let repeated = arg
                    .get_num_args()
                    .is_some_and(|count| count.max_values() > 1);
                positionals.push(format!(
                    "<{}{}>",
                    arg.get_id(),
                    if repeated { "..." } else { "" }
                ));
                continue;
            }
            let mut spelling = String::new();
            if let Some(short) = arg.get_short() {
                spelling.push_str(&format!("-{short}/"));
            }
            spelling.push_str(&format!(
                "--{}",
                arg.get_long().unwrap_or(arg.get_id().as_str())
            ));
            if arg.get_action().takes_values() {
                spelling.push_str("=VALUE");
            }
            flags.push(spelling);
        }
        flags.sort();
        let mut line = path.to_string();
        for item in positionals.iter().chain(flags.iter()) {
            line.push(' ');
            line.push_str(item);
        }
        out.push_str(&line);
        out.push('\n');
        let mut subcommands: Vec<&clap::Command> = command.get_subcommands().collect();
        subcommands.sort_by_key(|sub| sub.get_name());
        for sub in subcommands {
            render(sub, &format!("{path} {}", sub.get_name()), out);
        }
    }

    fn current_surface() -> String {
        let command = Cli::command();
        let mut out = String::new();
        render(&command, "frost", &mut out);
        out
    }

    #[test]
    fn the_command_surface_matches_the_checked_in_contract() {
        let current = current_surface();
        // A Windows checkout may hand `include_str!` CRLF regardless of what
        // was committed, and the line ending is not what this test is about.
        let snapshot = SNAPSHOT.replace("\r\n", "\n");
        if current != snapshot {
            let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/cli-surface.txt");
            if std::env::var_os("UPDATE_CLI_SURFACE").is_some() {
                std::fs::write(path, &current).expect("write CLI surface snapshot");
                return;
            }
            panic!(
                "the CLI surface changed.\n\nexpected:\n{snapshot}\nactual:\n{current}\n\
                 Adding a subcommand or option is additive and only needs the snapshot \
                 refreshed with UPDATE_CLI_SURFACE=1. Renaming or removing one is a \
                 breaking change: follow docs/28_compatibility_contract.md first."
            );
        }
    }

    /// The three outcomes a caller is allowed to distinguish. Scripts branch on
    /// these, so they are contract, not implementation.
    #[test]
    fn exit_codes_keep_their_documented_meanings() {
        let contract = [
            (0, "the requested work completed"),
            (1, "the work ran and did not succeed"),
            (2, "frost could not run the work as asked"),
        ];
        let documented = include_str!("../../../docs/28_compatibility_contract.md");
        for (code, meaning) in contract {
            assert!(
                documented.contains(&format!("| `{code}` |")),
                "exit code {code} ({meaning}) is not in the contract document"
            );
        }
    }
}
