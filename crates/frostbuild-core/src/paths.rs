use anyhow::{bail, Result};

/// Validate and normalize a workspace-relative path from a manifest.
///
/// Rules: non-empty, relative, forward slashes only, no `.`/`..` components.
/// Returns the normalized form (leading `./` stripped).
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
    let mut parts = Vec::new();
    for part in raw.split('/') {
        match part {
            "" => bail!("path {raw:?} has an empty component"),
            "." => continue,
            ".." => bail!("path {raw:?} must not escape the workspace with `..`"),
            other => parts.push(other),
        }
    }
    if parts.is_empty() {
        bail!("path {raw:?} does not name a file");
    }
    Ok(parts.join("/"))
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
}
