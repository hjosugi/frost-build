//! `frostw`: the frost version a repository requires, rather than the version
//! the machine running the build happens to have.
//!
//! Distribution (`docs/13`, issue #134) answers "how does frost get onto this
//! machine". That leaves the other half unanswered: before 1.0 a minor release
//! may break a manifest, so a workspace written against 0.9 and built with 0.8
//! fails with an error that is correct and says nothing about the version
//! difference that caused it. gradlew, mvnw and bazelisk all answer that by
//! checking the requirement into the repository, and so does this.
//!
//! The wrapper scripts are checked-in assets rather than generated text: what
//! a workspace commits is exactly what ships here, so reading one file tells
//! you what every workspace runs.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// One line naming the exact frost version a workspace requires.
///
/// Deliberately its own file rather than a `frost.toml` key: reading the
/// manifest requires a frost, and choosing which frost to run is the question.
pub const VERSION_FILE: &str = ".frost-version";

/// The POSIX wrapper, checked in at the workspace root.
pub const WRAPPER_SH: &str = "frostw";

/// The Windows wrapper. Same contract, same cache, same `.frost-version`.
pub const WRAPPER_CMD: &str = "frostw.cmd";

const WRAPPER_SH_TEXT: &str = include_str!("../assets/frostw");
const WRAPPER_CMD_TEXT: &str = include_str!("../assets/frostw.cmd");

/// The declared version, by the same rules the wrapper scripts use: strip a
/// `#` comment, strip all whitespace, take the first line with anything left.
///
/// Three implementations of this rule now exist — here, in `frostw` and in
/// `frostw.cmd` — so the rule is kept small enough that all three can be read
/// side by side and seen to agree.
pub fn parse_declared_version(text: &str) -> Option<String> {
    text.lines()
        .map(|line| {
            line.split('#')
                .next()
                .unwrap_or_default()
                .split_whitespace()
                .collect::<String>()
        })
        .find(|line| !line.is_empty())
}

/// The version this workspace requires, if it declares one.
pub fn declared_version(root: &Path) -> Option<String> {
    parse_declared_version(&std::fs::read_to_string(root.join(VERSION_FILE)).ok()?)
}

/// Warn when this binary is not the version the workspace pinned.
///
/// The wrapper exists so this never happens; this catches the invocation that
/// bypassed it. Warning rather than failing is deliberate: a version
/// difference is usually fine, and refusing to run would make frost unusable
/// in exactly the situation where someone is trying to work out what broke.
/// It goes to stderr, so `--json` consumers reading stdout are unaffected.
pub fn warn_on_version_mismatch(root: &Path) {
    let Some(declared) = declared_version(root) else {
        return;
    };
    let running = env!("CARGO_PKG_VERSION");
    if declared == running {
        return;
    }
    let wrapper = if cfg!(windows) {
        WRAPPER_CMD
    } else {
        "./frostw"
    };
    eprintln!(
        "frost: warning: {VERSION_FILE} requires frost {declared}, and this is frost {running}"
    );
    eprintln!("frost: warning: run {wrapper} to build with the version this workspace declares");
}

/// Write `.frost-version` and both wrapper scripts into `root`.
///
/// Returns the paths written, in the order a reader should see them.
pub fn write_wrapper(root: &Path, version: &str) -> Result<Vec<PathBuf>> {
    let version_file = root.join(VERSION_FILE);
    write_file(&version_file, format!("{version}\n").as_bytes())?;

    let script = root.join(WRAPPER_SH);
    write_file(&script, WRAPPER_SH_TEXT.as_bytes())?;
    make_executable(&script)?;

    // CRLF regardless of host: cmd.exe mis-parses `goto` labels and
    // parenthesised blocks in a batch file with bare LF endings, and this file
    // is written on the host that runs `frost init`, not on the host that runs
    // the wrapper.
    let cmd = root.join(WRAPPER_CMD);
    write_file(&cmd, WRAPPER_CMD_TEXT.replace('\n', "\r\n").as_bytes())?;

    Ok(vec![version_file, script, cmd])
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    std::fs::write(path, bytes).with_context(|| format!("writing {}", path.display()))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .with_context(|| format!("making {} executable", path.display()))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_is_read_the_way_the_wrapper_scripts_read_it() {
        assert_eq!(parse_declared_version("0.9.0\n").as_deref(), Some("0.9.0"));
        assert_eq!(
            parse_declared_version("  0.9.0  \n").as_deref(),
            Some("0.9.0")
        );
        assert_eq!(
            parse_declared_version("0.9.0\r\n").as_deref(),
            Some("0.9.0"),
            "a file checked out with CRLF names the same version"
        );
        assert_eq!(
            parse_declared_version("# pinned by the 0.9 manifest changes\n\n0.9.0\n").as_deref(),
            Some("0.9.0"),
            "the pin may say why it is set"
        );
        assert_eq!(
            parse_declared_version("0.9.0 # do not bump without the team\n").as_deref(),
            Some("0.9.0")
        );
        assert_eq!(parse_declared_version("").as_deref(), None);
        assert_eq!(
            parse_declared_version("\n# only a comment\n").as_deref(),
            None
        );
        assert_eq!(
            parse_declared_version("1.2\n0.9.0\n").as_deref(),
            Some("1.2"),
            "the first line wins; validating the shape is the wrapper's job, \
             and disagreeing about which line to read would be worse than \
             disagreeing about whether it parses"
        );
    }

    /// The scripts are shipped, not described, so the properties the CLI
    /// depends on are asserted against the bytes that get written.
    #[test]
    fn the_shipped_wrappers_agree_with_what_the_cli_promises() {
        assert!(
            WRAPPER_SH_TEXT.starts_with("#!/bin/sh\n"),
            "the POSIX wrapper must be runnable through its shebang"
        );
        assert!(!WRAPPER_SH_TEXT.contains('\r'), "the asset is stored as LF");
        for text in [WRAPPER_SH_TEXT, WRAPPER_CMD_TEXT] {
            assert!(
                text.contains(VERSION_FILE),
                "a wrapper that does not read {VERSION_FILE} pins nothing"
            );
            assert!(
                text.contains("SHA256SUMS"),
                "an unverified download must never be executed"
            );
            assert!(
                text.contains("FROSTW_RELEASE_BASE_URL"),
                "the release location has to be redirectable, or the download \
                 path cannot be tested without the network"
            );
            assert!(
                text.contains("FROST_HOME"),
                "the cache location has to be redirectable for the same reason"
            );
        }
    }
}
