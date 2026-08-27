//! Deterministic documentation assets embedded in every release archive.
//!
//! This module is feature-gated and reached only by the `release-assets`
//! example. Keeping it out of the normal feature set means manpage generation
//! does not add code or dependencies to the build tool users execute.

use std::path::Path;

use anyhow::{bail, Context, Result};
use clap::CommandFactory;

use crate::cli::{Cli, CompletionShell};
use crate::completions::write_completions;

const COMPLETIONS: [(CompletionShell, &str); 6] = [
    (CompletionShell::Bash, "frost.bash"),
    (CompletionShell::Zsh, "_frost"),
    (CompletionShell::Fish, "frost.fish"),
    (CompletionShell::Powershell, "_frost.ps1"),
    (CompletionShell::Elvish, "frost.elv"),
    (CompletionShell::Nushell, "frost.nu"),
];

pub(crate) fn generate(output: &Path, date: &str) -> Result<()> {
    validate_date(date)?;
    let man_dir = output.join("share/man/man1");
    let completion_dir = output.join("share/completions");
    std::fs::create_dir_all(&man_dir).with_context(|| format!("creating {}", man_dir.display()))?;
    std::fs::create_dir_all(&completion_dir)
        .with_context(|| format!("creating {}", completion_dir.display()))?;

    let mut command = Cli::command().disable_help_subcommand(true);
    command.build();
    write_manpages(command, &man_dir, date)?;

    for (shell, filename) in COMPLETIONS {
        let mut rendered = Vec::new();
        write_completions(shell, &mut rendered);
        if rendered.is_empty() {
            bail!("{filename} completion generator wrote no bytes");
        }
        std::fs::write(completion_dir.join(filename), rendered)
            .with_context(|| format!("writing {filename}"))?;
    }
    Ok(())
}

fn write_manpages(command: clap::Command, output: &Path, date: &str) -> Result<()> {
    for subcommand in command
        .get_subcommands()
        .filter(|subcommand| !subcommand.is_hide_set())
        .cloned()
    {
        write_manpages(subcommand, output, date)?;
    }
    let man = clap_mangen::Man::new(command)
        .date(date)
        .manual("FrostBuild Manual")
        .source(format!("frost {}", env!("CARGO_PKG_VERSION")));
    let path = output.join(man.get_filename());
    let mut rendered = Vec::new();
    man.render(&mut rendered)
        .with_context(|| format!("rendering {}", path.display()))?;
    std::fs::write(&path, rendered).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn validate_date(date: &str) -> Result<()> {
    let bytes = date.as_bytes();
    if bytes.len() != 10
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes
            .iter()
            .enumerate()
            .any(|(index, byte)| !matches!(index, 4 | 7) && !byte.is_ascii_digit())
    {
        bail!("release asset date {date:?} is not YYYY-MM-DD");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_dates_are_closed_and_machine_readable() {
        assert!(validate_date("2026-08-27").is_ok());
        assert!(validate_date("2026-8-27").is_err());
        assert!(validate_date("today-----").is_err());
    }
}
