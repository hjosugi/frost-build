//! Explicit, checksum-verified replacement of the running `frost` binary.
//!
//! Discovery uses GitHub's public latest-release endpoint. The archive and the
//! release's `SHA256SUMS` are both selected from that response, the archive is
//! hashed before it is unpacked, and the candidate must report the requested
//! version before `self-replace` atomically swaps it in. No command invokes
//! this path implicitly and no request carries telemetry or machine identity.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{bail, Context, Result};
use serde::Deserialize;

use crate::acquire::{download, extract, sha256_file, Scratch};

const LATEST_RELEASE_API: &str = "https://api.github.com/repos/hjosugi/frost-build/releases/latest";
const API_URL_ENV: &str = "FROST_SELF_UPDATE_API_URL";
const CURRENT_VERSION_ENV: &str = "FROST_SELF_UPDATE_CURRENT_VERSION";

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    prerelease: bool,
    assets: Vec<ReleaseAsset>,
}

#[derive(Deserialize)]
struct ReleaseAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
struct Version(u64, u64, u64);

impl Version {
    fn parse(value: &str) -> Result<Self> {
        let mut parts = value.split('.');
        let major = parse_component(parts.next(), value)?;
        let minor = parse_component(parts.next(), value)?;
        let patch = parse_component(parts.next(), value)?;
        if parts.next().is_some() {
            bail!("release version {value:?} is not X.Y.Z");
        }
        Ok(Self(major, minor, patch))
    }
}

fn parse_component(component: Option<&str>, whole: &str) -> Result<u64> {
    let component = component
        .filter(|part| !part.is_empty())
        .with_context(|| format!("release version {whole:?} is not X.Y.Z"))?;
    if !component.bytes().all(|byte| byte.is_ascii_digit()) {
        bail!("release version {whole:?} is not X.Y.Z");
    }
    component
        .parse()
        .with_context(|| format!("release version {whole:?} is out of range"))
}

struct Target {
    triple: &'static str,
    archive_suffix: &'static str,
    binary: &'static str,
}

fn release_target() -> Result<Target> {
    if cfg!(all(target_os = "linux", target_arch = "x86_64")) {
        Ok(Target {
            triple: "x86_64-unknown-linux-musl",
            archive_suffix: ".tar.gz",
            binary: "frost",
        })
    } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
        Ok(Target {
            triple: "aarch64-apple-darwin",
            archive_suffix: ".tar.gz",
            binary: "frost",
        })
    } else if cfg!(all(target_os = "macos", target_arch = "x86_64")) {
        Ok(Target {
            triple: "x86_64-apple-darwin",
            archive_suffix: ".tar.gz",
            binary: "frost",
        })
    } else if cfg!(all(target_os = "windows", target_arch = "x86_64")) {
        Ok(Target {
            triple: "x86_64-pc-windows-msvc",
            archive_suffix: ".zip",
            binary: "frost.exe",
        })
    } else {
        bail!(
            "self-update has no published release for {} {}. use `cargo install --locked frostbuild-cli`",
            std::env::consts::OS,
            std::env::consts::ARCH
        );
    }
}

fn running_version() -> String {
    // This override exists only to let an E2E exercise a real replacement
    // with a binary built from the same checkout as the candidate. The caller
    // controls its own environment and gains no trust by changing this value.
    std::env::var(CURRENT_VERSION_ENV).unwrap_or_else(|_| env!("CARGO_PKG_VERSION").to_string())
}

pub(crate) fn run_self_update(check: bool) -> Result<i32> {
    let current_text = running_version();
    let current = Version::parse(&current_text)?;
    let scratch = Scratch::create(&std::env::temp_dir(), "frost-self-update")?;
    let release_path = scratch.path().join("latest.json");
    let api_url = std::env::var(API_URL_ENV).unwrap_or_else(|_| LATEST_RELEASE_API.to_string());
    download(&api_url, &release_path).context("failed to query the latest FrostBuild release")?;
    let release: Release = serde_json::from_slice(
        &std::fs::read(&release_path).context("failed to read the latest release response")?,
    )
    .context("the latest release response was not valid JSON")?;
    if release.draft || release.prerelease {
        bail!("the latest release endpoint returned a draft or prerelease");
    }
    let latest_text = release
        .tag_name
        .strip_prefix('v')
        .context("the latest release tag does not start with 'v'")?;
    let latest = Version::parse(latest_text)?;

    match latest.cmp(&current) {
        Ordering::Less => {
            println!(
                "frost: this is {current_text}; latest stable release is {latest_text} (no downgrade)"
            );
            return Ok(0);
        }
        Ordering::Equal => {
            println!("frost: {current_text} is up to date");
            return Ok(0);
        }
        Ordering::Greater if check => {
            println!("frost: update available {current_text} -> {latest_text}");
            return Ok(0);
        }
        Ordering::Greater => {}
    }

    let executable = std::env::current_exe().context("cannot locate the running frost binary")?;
    if installed_by_cargo(&executable) {
        bail!(
            "this frost is managed by cargo; refusing to replace it. run `cargo install --locked frostbuild-cli` instead"
        );
    }

    let target = release_target()?;
    let archive_name = format!(
        "frostbuild-v{latest_text}-{}{}",
        target.triple, target.archive_suffix
    );
    let sums_url = asset_url(&release, "SHA256SUMS")?;
    let archive_url = asset_url(&release, &archive_name)?;
    let sums_path = scratch.path().join("SHA256SUMS");
    let archive_path = scratch.path().join(&archive_name);
    download(sums_url, &sums_path).context("failed to download release checksums")?;
    let expected = checksum_for(
        &std::fs::read_to_string(&sums_path).context("failed to read release checksums")?,
        &archive_name,
    )?;
    download(archive_url, &archive_path).context("failed to download the release archive")?;
    let actual = sha256_file(&archive_path)?;
    if actual != expected {
        bail!(
            "SHA-256 mismatch for {archive_name}: expected {expected}, got {actual}; the current binary was not changed"
        );
    }

    let unpacked = scratch.path().join("unpacked");
    std::fs::create_dir(&unpacked)?;
    extract(&archive_path, &unpacked).context("failed to unpack the verified release archive")?;
    let candidate = unpacked
        .join(format!("frostbuild-v{latest_text}-{}", target.triple))
        .join(target.binary);
    if !candidate.is_file() {
        bail!("{archive_name} does not contain {}", candidate.display());
    }
    verify_candidate(&candidate, latest_text)?;

    self_replace::self_replace(&candidate).with_context(|| {
        format!(
            "failed to atomically replace {} (the verified candidate remains temporary and the current binary is unchanged)",
            executable.display()
        )
    })?;
    println!("frost: updated {current_text} -> {latest_text}");
    Ok(0)
}

fn asset_url<'a>(release: &'a Release, name: &str) -> Result<&'a str> {
    let mut matches = release.assets.iter().filter(|asset| asset.name == name);
    let asset = matches
        .next()
        .with_context(|| format!("release {} has no {name} asset", release.tag_name))?;
    if matches.next().is_some() {
        bail!(
            "release {} has more than one {name} asset",
            release.tag_name
        );
    }
    Ok(&asset.browser_download_url)
}

fn checksum_for(contents: &str, archive: &str) -> Result<String> {
    let mut found = None;
    for line in contents.lines() {
        let mut fields = line.split_whitespace();
        let Some(digest) = fields.next() else {
            continue;
        };
        let Some(name) = fields.next() else {
            continue;
        };
        if name.trim_start_matches('*') != archive {
            continue;
        }
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            bail!("SHA256SUMS contains an invalid digest for {archive}");
        }
        if found.is_some() {
            bail!("SHA256SUMS names {archive} more than once");
        }
        found = Some(digest.to_ascii_lowercase());
    }
    found.with_context(|| format!("SHA256SUMS does not name {archive}"))
}

fn verify_candidate(candidate: &Path, expected: &str) -> Result<()> {
    let output = Command::new(candidate)
        .arg("--version")
        .output()
        .with_context(|| format!("failed to run verified candidate {}", candidate.display()))?;
    if !output.status.success() {
        bail!("verified candidate failed its --version smoke test");
    }
    let stdout = String::from_utf8(output.stdout).context("candidate --version was not UTF-8")?;
    if stdout.trim() != format!("frost {expected}") {
        bail!(
            "verified candidate reports {:?}, expected {:?}; the current binary was not changed",
            stdout.trim(),
            format!("frost {expected}")
        );
    }
    Ok(())
}

fn installed_by_cargo(executable: &Path) -> bool {
    let executable = executable
        .canonicalize()
        .unwrap_or_else(|_| executable.to_path_buf());
    let mut roots = Vec::new();
    if let Some(root) = std::env::var_os("CARGO_INSTALL_ROOT") {
        roots.push(PathBuf::from(root));
    }
    if let Some(home) = std::env::var_os("CARGO_HOME") {
        roots.push(PathBuf::from(home));
    }
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        roots.push(PathBuf::from(home).join(".cargo"));
    }
    installed_under_cargo_root(&executable, &roots)
}

fn installed_under_cargo_root(executable: &Path, roots: &[PathBuf]) -> bool {
    roots.iter().any(|root| {
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        executable.parent() == Some(root.join("bin").as_path())
    }) || executable
        .parent()
        .and_then(Path::parent)
        .is_some_and(|root| {
            root.join(".crates.toml").is_file()
                && executable.parent() == Some(root.join("bin").as_path())
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_versions_are_numeric_and_ordered() {
        assert!(Version::parse("1.2").is_err());
        assert!(Version::parse("v1.2.3").is_err());
        assert!(Version::parse("1.2.3.4").is_err());
        assert!(Version::parse("1.2.x").is_err());
        assert!(Version::parse("1.12.0").unwrap() > Version::parse("1.2.99").unwrap());
    }

    #[test]
    fn checksum_lines_match_the_whole_asset_name() {
        let digest = "a".repeat(64);
        let sums = format!("{digest}  other.tar.gz\n{digest} *wanted.tar.gz\n");
        assert_eq!(checksum_for(&sums, "wanted.tar.gz").unwrap(), digest);
        assert!(checksum_for(&sums, "want").is_err());
        assert!(checksum_for("short  wanted.tar.gz\n", "wanted.tar.gz").is_err());
        let duplicate = format!("{digest}  wanted.tar.gz\n{digest}  wanted.tar.gz\n");
        assert!(checksum_for(&duplicate, "wanted.tar.gz").is_err());
    }

    #[test]
    fn cargo_managed_paths_are_recognized_without_guessing_other_bins() {
        let root = PathBuf::from("/tmp/example-cargo-root");
        assert!(installed_under_cargo_root(
            &root.join("bin/frost"),
            std::slice::from_ref(&root)
        ));
        assert!(!installed_under_cargo_root(
            &PathBuf::from("/tmp/example-local/bin/frost"),
            &[root]
        ));
    }
}
