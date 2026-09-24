use std::borrow::Cow;

use anyhow::{bail, Result};

/// Validate and normalize a workspace-relative path from a manifest.
///
/// Rules: non-empty, relative, forward slashes only, no `.`/`..` components.
/// Returns the normalized form (leading `./` stripped).
/// The `${config}` segment for a (platform, profile) pair.
///
/// One function because this is a *rule*, and docs/28 promises callers need not
/// encode it. It was written out in three places — the graph building the output
/// tree, `frost info` answering where things land, and `frost explain` naming
/// the configuration it is describing — and `info`'s own comment said not to
/// reimplement it directly above a reimplementation of it. Three copies of a
/// rule are three chances for one of them to be the odd one out.
///
/// The host keeps a single segment so existing workspaces, journals and
/// documentation stay valid verbatim.
pub fn config(platform: &str, profile: &str) -> String {
    configured(platform, profile, false)
}

/// The profile-shaped part of a configuration, with instrumentation axes.
///
/// Kept separate from [`configured`] because the graph store and execution
/// journal need the same collision-free identity without the platform path
/// separator. `+` cannot occur in a declared profile name, so an instrumented
/// configuration cannot alias a user-authored profile.
pub fn instrumented_profile(profile: &str, coverage: bool) -> Cow<'_, str> {
    match coverage {
        true => Cow::Owned(format!("{profile}+coverage")),
        false => Cow::Borrowed(profile),
    }
}

/// The `${config}` segment, including whether coverage is instrumented.
///
/// Coverage is an axis here rather than a profile because a profile name has to
/// be one the manifest declares — `graph.rs` refuses an undeclared one, which
/// is what stops `--profile relase` from silently building into its own tree.
/// A synthesized `debug-coverage` would therefore fail on every workspace that
/// declares any profile at all. Being an axis instead means an instrumented
/// build reaches its own output tree, journal identity and cache through the
/// machinery already described in docs/28, and an ordinary build cannot serve a
/// cache hit to one that measures coverage.
///
/// `+` separates it, and the character matters: profile names are
/// `[A-Za-z0-9_-]`, so `debug+coverage` is a segment no profile can spell. With
/// `-` a workspace declaring `[profile.debug-coverage]` would quietly share one
/// output tree with `--profile debug --coverage`.
pub fn configured(platform: &str, profile: &str, coverage: bool) -> String {
    let profile = instrumented_profile(profile, coverage);
    match platform == crate::manifest::HOST_PLATFORM {
        true => profile.into_owned(),
        false => format!("{platform}/{profile}"),
    }
}

pub fn validate_rel_path(raw: &str) -> Result<String> {
    if raw.is_empty() {
        bail!("empty path");
    }
    if raw.contains('\\') {
        bail!("path {raw:?} must use forward slashes");
    }
    if raw.starts_with('/') {
        bail!("path {raw:?} must be workspace-relative, not absolute");
    }
    // `C:/x` passes every check above, and on Windows joining it to the
    // workspace root yields `C:/x` itself — a path outside the workspace. It
    // is refused on every host so a manifest means the same thing everywhere.
    let bytes = raw.as_bytes();
    if bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' {
        bail!(
            "path {raw:?} starts with a drive designator, which on Windows names a path \
             outside the workspace; use a workspace-relative path"
        );
    }
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" => bail!("path {raw:?} has an empty component"),
            "." => continue,
            ".." => bail!("path {raw:?} must not escape the workspace with `..`"),
            other => {
                if cfg!(windows) {
                    windows_component(raw, other)?;
                }
                parts.push(other)
            }
        }
    }
    if parts.is_empty() {
        bail!("path {raw:?} does not name a file");
    }
    Ok(parts.join("/"))
}

/// Names a Windows host cannot store as written.
///
/// Checked only on Windows, where they fail: `aux.c` is an ordinary file on
/// Linux and a device on Windows, and refusing it on a host that can build it
/// would break a working workspace to protect one that cannot exist. What the
/// check buys on Windows is a sentence at load time instead of a hang reading
/// a device or an output that silently loses its trailing dot.
pub fn windows_component(raw: &str, component: &str) -> Result<()> {
    // `*` and `?` are also unstorable, but manifest paths are validated
    // before glob expansion, where they are pattern syntax; a pattern can
    // only ever expand to names the filesystem already holds.
    if let Some(bad) = component
        .chars()
        .find(|c| matches!(c, '<' | '>' | ':' | '"' | '|') || c.is_control())
    {
        bail!("path {raw:?} contains {bad:?}, which Windows does not allow in a file name");
    }
    if component.ends_with('.') || component.ends_with(' ') {
        bail!(
            "path {raw:?} has a component ending in a dot or space, which Windows \
             silently removes, so the file frost wrote would not be the one it declared"
        );
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or(component)
        .trim_end()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if reserved {
        bail!("path {raw:?} uses the reserved device name {component:?}, which Windows cannot store as a file");
    }
    Ok(())
}

/// Find an executable named without a directory component on `PATH`.
///
/// Windows stores the extension in the file name and the acceptable extensions
/// in `PATHEXT`, so `PATH`-joining the bare name finds nothing: a workspace that
/// asked for `gcc` failed with "not found in PATH" while `gcc --version` worked
/// in the same shell. Unix keeps the single-candidate behaviour.
pub fn find_on_path(
    name: &str,
    accept: impl Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    let path = std::env::var_os("PATH")?;
    find_in_directories(std::env::split_paths(&path), name, accept)
}

/// The search itself, over an explicit directory list. Separated from `PATH` so
/// it can be exercised without mutating the environment of a running process.
pub fn find_in_directories(
    directories: impl Iterator<Item = std::path::PathBuf>,
    name: &str,
    accept: impl Fn(&std::path::Path) -> bool,
) -> Option<std::path::PathBuf> {
    let extensions = executable_extensions();
    for directory in directories {
        let candidate = directory.join(name);
        if accept(&candidate) {
            return Some(candidate);
        }
        for extension in &extensions {
            let candidate = directory.join(format!("{name}{extension}"));
            if accept(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

/// Extensions that make a bare name executable on this host, in the order the
/// host itself would try them.
pub fn executable_extensions() -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let configured = std::env::var_os("PATHEXT")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| ".COM;.EXE;.BAT;.CMD".to_string());
    configured
        .split(';')
        .map(str::trim)
        .filter(|extension| extension.starts_with('.') && extension.len() > 1)
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod executable_tests {
    #[test]
    fn extension_candidates_match_the_host_convention() {
        let extensions = super::executable_extensions();
        if cfg!(windows) {
            assert!(
                extensions
                    .iter()
                    .any(|extension| extension.eq_ignore_ascii_case(".exe")),
                "a Windows host must try .exe: {extensions:?}"
            );
            assert!(
                extensions
                    .iter()
                    .all(|extension| extension.starts_with('.')),
                "every candidate is an extension: {extensions:?}"
            );
        } else {
            assert!(
                extensions.is_empty(),
                "a Unix host has no name extensions to try: {extensions:?}"
            );
        }
    }

    #[test]
    fn a_bare_name_is_found_through_its_host_extension() {
        let dir = std::env::temp_dir().join(format!("frost-pathext-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // What the host actually stores: `probe.exe` on Windows, `probe` else.
        let stored = format!(
            "probe{}",
            super::executable_extensions()
                .first()
                .map_or("", |e| e.as_str())
        );
        std::fs::write(dir.join(&stored), b"").unwrap();
        assert_eq!(
            super::find_in_directories(std::iter::once(dir.clone()), "probe", |candidate| {
                candidate.is_file()
            }),
            Some(dir.join(&stored)),
            "a bare name must resolve to the stored file"
        );
        assert_eq!(
            super::find_in_directories(std::iter::once(dir.clone()), "absent", |candidate| {
                candidate.is_file()
            }),
            None
        );
        std::fs::remove_dir_all(dir).ok();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_host_keeps_a_single_segment_and_a_platform_adds_one() {
        // Now that three callers share this, the host special case is a
        // contract rather than a local choice: docs/28 promises the layout,
        // and existing journals were written under the one-segment form.
        assert_eq!(config(crate::manifest::HOST_PLATFORM, "debug"), "debug");
        assert_eq!(config("device", "debug"), "device/debug");
        assert_eq!(config("device", "release"), "device/release");
    }

    #[test]
    fn accepts_and_normalizes() {
        assert_eq!(validate_rel_path("src/main.c").unwrap(), "src/main.c");
        assert_eq!(validate_rel_path("./src/main.c").unwrap(), "src/main.c");
    }

    #[test]
    fn rejects_bad_paths() {
        assert!(validate_rel_path("").is_err());
        assert!(validate_rel_path("/etc/passwd").is_err());
        assert!(validate_rel_path("../escape.c").is_err());
        assert!(validate_rel_path("a//b").is_err());
        assert!(validate_rel_path("a\\b").is_err());
        assert!(validate_rel_path(".").is_err());
    }

    #[test]
    fn a_drive_designator_is_absolute_on_windows_so_it_is_refused_everywhere() {
        for raw in ["C:/Windows/win.ini", "c:foo.c", "Z:"] {
            let error = validate_rel_path(raw).unwrap_err().to_string();
            assert!(error.contains("drive designator"), "{raw}: {error}");
        }
        // A colon later in a name is only a Windows problem.
        assert_eq!(
            validate_rel_path("man/Foo::Bar.3").is_ok(),
            !cfg!(windows),
            "a colon inside a name is refused on Windows only"
        );
    }

    #[test]
    fn windows_only_names_are_refused_on_windows_and_nowhere_else() {
        for raw in [
            "src/aux.c",
            "CON",
            "out/nul.txt",
            "gen/COM1.h",
            "lpt9",
            "file.",
            "dir /x",
            "a|b",
            "stream.txt:hidden",
        ] {
            assert_eq!(
                validate_rel_path(raw).is_err(),
                cfg!(windows),
                "{raw} on this host"
            );
            // The rule itself, independent of the host.
            let last = raw.rsplit('/').next().unwrap();
            let first = raw.split('/').next().unwrap();
            assert!(
                windows_component(raw, last).is_err() || windows_component(raw, first).is_err(),
                "{raw}"
            );
        }
        for fine in [
            "src/auxiliary.c",
            "COM0",
            "com10.txt",
            "console.c",
            "a.b.c",
            "src/*.c",
            "src/?.h",
        ] {
            assert!(validate_rel_path(fine).is_ok(), "{fine}");
            assert!(windows_component(fine, fine).is_ok(), "{fine}");
        }
    }

    #[test]
    fn long_relative_paths_are_accepted_as_written() {
        // Frost never shortens a path. Long ones are the filesystem's to
        // accept, and Rust's std prefixes `\\?\` on Windows when a path
        // exceeds MAX_PATH; the E2E suite builds one on every host.
        let long = (0..30)
            .map(|i| format!("segment-{i:02}"))
            .collect::<Vec<_>>()
            .join("/")
            + "/input.txt";
        assert!(long.len() > 300);
        assert_eq!(validate_rel_path(&long).unwrap(), long);
    }
}
