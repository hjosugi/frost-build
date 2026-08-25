//! Turning an action into processes: argv, environment, stamps and timeouts.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::collections::BTreeSet;
use std::process::Command;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::Result;

use crate::process::{resolve_timeout, terminate_process_tree, wait_for_child, was_cancelled};
use crate::sandbox::sandbox_command;
use crate::{describe_exit, resolve_action_program, CommandBatch, Engine, RUNNING_PROCESS_GROUPS};

impl<'a> Engine<'a> {
    /// The limit for one action: what the target declared, else what the
    /// invocation asked for, else the default that only tests carry.
    fn timeout_for(&self, action: &frostbuild_core::graph::ActionNode) -> Option<Duration> {
        resolve_timeout(
            self.graph
                .targets
                .get(&action.target)
                .and_then(|target| target.timeout_secs),
            self.opts.timeout,
            action.kind,
        )
    }

    /// Naming the source is the difference between a limit a reader can change
    /// and one they have to go looking for.
    fn describe_timeout_source(&self, action: &frostbuild_core::graph::ActionNode) -> String {
        if self
            .graph
            .targets
            .get(&action.target)
            .is_some_and(|target| target.timeout_secs.is_some())
        {
            return format!("timeout declared by target {}", action.target);
        }
        if self.opts.timeout.is_some() {
            return "--timeout".to_string();
        }
        "the default test timeout; declare `timeout` on the target to change it".to_string()
    }

    pub(crate) fn run_action_commands(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
    ) -> std::result::Result<CommandBatch, String> {
        let mut captured = String::new();
        if let Some(spec) = &action.coverage {
            return Ok(self.merge_coverage(action, spec));
        }
        for argv in std::iter::once(&action.argv).chain(&action.followup_argv) {
            // The graph carries the reference; the value arrives here, once
            // per build. Doing it at execution rather than at graph
            // construction is what lets the graph stay a pure function of the
            // manifest, and therefore stay cacheable.
            let argv = &self
                .stamped_argv(action, argv)
                .map_err(|error| format!("{error:#}"))?;
            let mut command = self
                .command_for_argv(action, inputs, argv)
                .map_err(|error| format!("{error:#}"))?;
            #[cfg(unix)]
            {
                use std::os::unix::process::CommandExt;
                command.process_group(0);
            }
            let child = command
                .spawn()
                .map_err(|error| format!("failed to spawn {:?}: {error}", argv[0]))?;
            let pid = child.id();
            {
                let mut groups = RUNNING_PROCESS_GROUPS
                    .get_or_init(|| Mutex::new(BTreeSet::new()))
                    .lock()
                    .unwrap();
                groups.insert(pid);
                // Close the spawn/registration race: request_cancellation may
                // have run after `spawn` but before this process group became
                // visible. It sets the flag before taking the same mutex, so
                // either that call kills us or this check does.
                if was_cancelled() {
                    terminate_process_tree(pid);
                }
            }
            let limit = self.timeout_for(action);
            let waited = wait_for_child(child, pid, limit);
            RUNNING_PROCESS_GROUPS
                .get()
                .unwrap()
                .lock()
                .unwrap()
                .remove(&pid);
            let (output, timed_out) = match waited {
                Ok(waited) => waited,
                Err(error) => {
                    return Err(format!("failed waiting for {}: {error}", action.id));
                }
            };
            if timed_out {
                let limit = limit.unwrap_or_default();
                captured.push_str(&String::from_utf8_lossy(&output.stdout));
                captured.push_str(&String::from_utf8_lossy(&output.stderr));
                return Ok(CommandBatch {
                    captured,
                    failure: Some((
                        argv.to_vec(),
                        format!(
                            "timed out after {}s ({})",
                            limit.as_secs(),
                            self.describe_timeout_source(action)
                        ),
                    )),
                });
            }
            captured.push_str(&String::from_utf8_lossy(&output.stdout));
            captured.push_str(&String::from_utf8_lossy(&output.stderr));
            if !output.status.success() {
                return Ok(CommandBatch {
                    captured,
                    failure: Some((argv.to_vec(), describe_exit(&output.status))),
                });
            }
        }
        Ok(CommandBatch {
            captured,
            failure: None,
        })
    }

    /// Run a coverage merge in process rather than spawning it.
    ///
    /// The work is one `gcov` call per counter file plus a merge into lcov, and
    /// the shell pipeline that did the same would need `lcov` or `gcovr` —
    /// neither of which ships with a toolchain, while `gcov` does. Doing it here
    /// keeps a Perl dependency out of every CI image that wants coverage, and
    /// keeps the merge one cacheable action rather than a script per test.
    ///
    /// Emptiness is a *failure*, not an empty tracefile: 0% is a number someone
    /// would act on, and "not measured" is a different statement from "nothing
    /// covered". A missing `.gcno` is a warning on the captured output, which is
    /// where the rest of an action's diagnostics go.
    fn merge_coverage(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        spec: &frostbuild_core::graph::CoverageSpec,
    ) -> CommandBatch {
        let mut captured = String::new();
        let gcda_dirs: Vec<std::path::PathBuf> = spec
            .gcda_dirs
            .iter()
            .map(|dir| self.root.join(dir))
            .collect();
        let notes: Vec<std::path::PathBuf> =
            spec.notes.iter().map(|note| self.root.join(note)).collect();
        let gcov = &action.argv[0];
        let mut warnings = Vec::new();
        let lcov =
            match frostbuild_core::coverage::merge(self.root, &gcda_dirs, &notes, gcov, |warning| {
                warnings.push(warning)
            }) {
                Ok(lcov) => lcov,
                Err(error) => {
                    return CommandBatch {
                        captured,
                        failure: Some((action.argv.clone(), format!("{error:#}"))),
                    }
                }
            };
        for warning in warnings {
            captured.push_str(&format!("coverage: {warning}\n"));
        }
        if lcov.is_empty() {
            return CommandBatch {
                captured,
                failure: Some((
                    action.argv.clone(),
                    format!(
                        "no coverage data for {}: the test produced no counters, \
                         or none of the files it covered are in this workspace",
                        action.target
                    ),
                )),
            };
        }
        let destination = self.root.join(&spec.output);
        let written = destination
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&destination, &lcov));
        if let Err(error) = written {
            return CommandBatch {
                captured,
                failure: Some((
                    action.argv.clone(),
                    format!("writing {}: {error}", spec.output),
                )),
            };
        }
        CommandBatch {
            captured,
            failure: None,
        }
    }

    fn command_for_argv(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        inputs: &BTreeMap<String, String>,
        argv: &[String],
    ) -> Result<Command> {
        let mut command = if self.opts.sandbox && action.sandbox {
            sandbox_command(self.root, self.graph, action, inputs, argv)?
        } else {
            let mut command = Command::new(resolve_action_program(self.root, &argv[0]));
            command.args(&argv[1..]).current_dir(self.root);
            command
        };
        command
            .env_clear()
            .envs(self.command_env.iter().map(|(key, value)| (key, value)))
            .envs(self.stamped_env(action)?.iter())
            .env("LC_ALL", "C")
            .env("LANG", "C")
            // Actions never read from the terminal. Inheriting stdin lets a
            // command that expects input (`cat > out` when ${in} expanded to
            // nothing, an accidental interactive prompt) block forever with no
            // output and no diagnostic, which looks exactly like a slow build.
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        for name in &action.pass_env {
            if let Some(value) = std::env::var_os(name) {
                command.env(name, value);
            }
        }
        Ok(command)
    }

    /// This action's argv with every `${stamp.KEY}` replaced by its value.
    ///
    /// Borrowed unchanged for the overwhelming majority of actions, which
    /// reference no stamp at all.
    fn stamped_argv<'argv>(
        &self,
        action: &frostbuild_core::graph::ActionNode,
        argv: &'argv [String],
    ) -> Result<Cow<'argv, [String]>> {
        if !self.action_reads_a_stamp(action) {
            return Ok(Cow::Borrowed(argv));
        }
        let context = format!("action {:?}", action.id);
        argv.iter()
            .map(|arg| self.expand_stamps(arg, &context))
            .collect::<Result<Vec<_>>>()
            .map(Cow::Owned)
    }

    fn stamped_env<'env>(
        &self,
        action: &'env frostbuild_core::graph::ActionNode,
    ) -> Result<Cow<'env, BTreeMap<String, String>>> {
        if !self.action_reads_a_stamp(action) {
            return Ok(Cow::Borrowed(&action.env));
        }
        let context = format!("action {:?} environment", action.id);
        action
            .env
            .iter()
            .map(|(name, value)| Ok((name.clone(), self.expand_stamps(value, &context)?)))
            .collect::<Result<BTreeMap<_, _>>>()
            .map(Cow::Owned)
    }

    /// With stamping off, a reference expands to nothing rather than failing:
    /// `--no-stamp` exists to build without the values, and a workspace that
    /// has a `[stamp]` section is one that references it.
    fn expand_stamps(&self, text: &str, context: &str) -> Result<String> {
        match &self.opts.stamps {
            Some(stamps) => frostbuild_core::stamp::expand(text, stamps, context),
            None => Ok(frostbuild_core::stamp::blank(text, context)?),
        }
    }

    fn action_reads_a_stamp(&self, action: &frostbuild_core::graph::ActionNode) -> bool {
        !action.stable_stamps.is_empty() || !action.volatile_stamps.is_empty()
    }
}
