//! Explicit, pinned archive acquisition.
//!
//! No build path calls this module. That boundary is the offline guarantee:
//! graph construction and action execution only read the materialized tree.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use frostbuild_core::cas::LocalCas;
use frostbuild_core::fetch::{
    reject_symlink_components, snapshot_tree, FetchState, STATE_FILE, STATE_SCHEMA,
};
use frostbuild_core::hashcache::hash_file;
use frostbuild_core::manifest::{closest, FetchSpec, Manifest};
use sha2::{Digest, Sha256};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

pub(crate) fn run_fetch(
    workspace_root: &Path,
    requested: Vec<String>,
    force: bool,
    offline: bool,
) -> Result<i32> {
    let manifest = Manifest::load_for_fetch(workspace_root)?;
    if manifest.fetches.is_empty() {
        bail!("manifest declares no [fetch.*] entries");
    }
    let names = select_names(&manifest, requested)?;
    for name in names {
        fetch_one(
            workspace_root,
            &name,
            &manifest.fetches[&name],
            force,
            offline,
        )?;
    }
    Ok(0)
}

fn select_names(manifest: &Manifest, requested: Vec<String>) -> Result<Vec<String>> {
    if requested.is_empty() {
        return Ok(manifest.fetches.keys().cloned().collect());
    }
    let mut names = Vec::new();
    for name in requested {
        if !manifest.fetches.contains_key(&name) {
            let known: Vec<&str> = manifest.fetches.keys().map(String::as_str).collect();
            if let Some(hint) = closest(&name, known.iter().copied()) {
                bail!("unknown fetch {name:?}. did you mean {hint:?}?");
            }
            bail!(
                "unknown fetch {name:?}. declared fetches: {}",
                known.join(", ")
            );
        }
        if !names.contains(&name) {
            names.push(name);
        }
    }
    Ok(names)
}

fn fetch_one(
    workspace_root: &Path,
    name: &str,
    spec: &FetchSpec,
    force: bool,
    offline: bool,
) -> Result<()> {
    reject_symlink_components(workspace_root, &spec.vendor_dir)?;
    let vendor_root = workspace_root.join(&spec.vendor_dir);
    let state = FetchState::read(&vendor_root).ok();
    let declaration_matches = state
        .as_ref()
        .is_some_and(|state| state_matches(state, name, spec));
    let materialization_matches = declaration_matches
        && state.as_ref().is_some_and(|state| {
            snapshot_tree(&vendor_root)
                .map(|snapshot| snapshot.digest == state.tree_digest)
                .unwrap_or(false)
        });
    if materialization_matches && !force {
        println!("frost: fetch {name} is up to date at {}", spec.vendor_dir);
        return Ok(());
    }
    if offline {
        bail!(
            "fetch {name:?} is missing or stale at {:?}, and --offline forbids downloading it",
            spec.vendor_dir
        );
    }
    if vendor_root.exists() {
        let Some(state) = state.as_ref() else {
            bail!(
                "refusing to replace {:?}: it has no {} proving Frost owns it",
                spec.vendor_dir,
                STATE_FILE
            );
        };
        if state.name != name || state.vendor_dir != spec.vendor_dir {
            bail!(
                "refusing to replace {:?}: its fetch state belongs to {:?}",
                spec.vendor_dir,
                state.name
            );
        }
    }

    let parent = vendor_root.parent().unwrap_or(workspace_root);
    std::fs::create_dir_all(parent)?;
    let scratch = Scratch::create(parent, name)?;
    let archive_path = scratch.path.join("download");
    download(&spec.url, &archive_path).with_context(|| format!("failed to fetch {name:?}"))?;
    let actual_sha256 = sha256_file(&archive_path)?;
    if actual_sha256 != spec.sha256 {
        bail!(
            "fetch {name:?} SHA-256 mismatch: expected {}, got {} (vendor directory was not changed)",
            spec.sha256,
            actual_sha256
        );
    }

    let cas_digest = hash_file(&archive_path)?;
    LocalCas::new(workspace_root, frostbuild_exec::DEFAULT_CAS_MAX_BYTES)
        .put(&archive_path, &cas_digest)
        .with_context(|| format!("failed to store fetch {name:?} in the local CAS"))?;

    let unpacked = scratch.path.join("unpacked");
    std::fs::create_dir(&unpacked)?;
    extract(&archive_path, &unpacked)
        .with_context(|| format!("failed to extract fetch {name:?}"))?;
    let source = match &spec.strip_prefix {
        Some(prefix) => unpacked.join(prefix),
        None => unpacked,
    };
    if !source.is_dir() {
        bail!(
            "fetch {name:?} strip_prefix {:?} is not a directory in the archive",
            spec.strip_prefix
        );
    }
    if source.join(STATE_FILE).exists() {
        bail!("fetch {name:?} archive contains reserved file {STATE_FILE:?}");
    }
    let snapshot = snapshot_tree(&source)?;
    FetchState {
        schema: STATE_SCHEMA.to_string(),
        name: name.to_string(),
        url: spec.url.clone(),
        sha256: spec.sha256.clone(),
        strip_prefix: spec.strip_prefix.clone(),
        vendor_dir: spec.vendor_dir.clone(),
        tree_digest: snapshot.digest,
        cas_digest,
    }
    .write(&source)?;

    publish_tree(&source, &vendor_root, &scratch.path)?;
    println!("frost: fetched {name} -> {}", spec.vendor_dir);
    Ok(())
}

fn state_matches(state: &FetchState, name: &str, spec: &FetchSpec) -> bool {
    state.schema == STATE_SCHEMA
        && state.name == name
        && state.url == spec.url
        && state.sha256 == spec.sha256
        && state.strip_prefix == spec.strip_prefix
        && state.vendor_dir == spec.vendor_dir
}

fn download(url: &str, destination: &Path) -> Result<()> {
    let (program, args) = if let Some(curl) = crate::launch::find_on_path("curl") {
        (
            curl,
            vec![
                "--fail".to_string(),
                "--location".to_string(),
                "--silent".to_string(),
                "--show-error".to_string(),
                "--output".to_string(),
                destination.to_string_lossy().into_owned(),
                "--".to_string(),
                url.to_string(),
            ],
        )
    } else if let Some(wget) = crate::launch::find_on_path("wget") {
        (
            wget,
            vec![
                "--quiet".to_string(),
                "--output-document".to_string(),
                destination.to_string_lossy().into_owned(),
                "--".to_string(),
                url.to_string(),
            ],
        )
    } else {
        bail!("fetch requires curl or wget in PATH");
    };
    let status = Command::new(&program)
        .args(&args)
        .status()
        .with_context(|| format!("failed to start {}", program.display()))?;
    if !status.success() {
        bail!("{} exited with {status}", program.display());
    }
    Ok(())
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect())
}

fn extract(archive_path: &Path, destination: &Path) -> Result<()> {
    let mut magic = [0u8; 4];
    let mut file = File::open(archive_path)?;
    let count = file.read(&mut magic)?;
    drop(file);
    if count >= 2 && magic[..2] == [0x1f, 0x8b] {
        extract_tar_gz(archive_path, destination)
    } else if count >= 4 && &magic[..2] == b"PK" {
        extract_zip(archive_path, destination)
    } else {
        bail!("unsupported archive format (expected .tar.gz/.tgz or .zip)");
    }
}

fn extract_tar_gz(archive_path: &Path, destination: &Path) -> Result<()> {
    let decoder = GzDecoder::new(File::open(archive_path)?);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive.entries()? {
        let mut entry = entry?;
        let kind = entry.header().entry_type();
        if !kind.is_file() && !kind.is_dir() {
            bail!("archive contains a link or special entry");
        }
        let path = entry.path()?.into_owned();
        validate_archive_path(&path)?;
        if !entry.unpack_in(destination)? {
            bail!("archive entry {:?} would escape the extraction root", path);
        }
    }
    Ok(())
}

fn extract_zip(archive_path: &Path, destination: &Path) -> Result<()> {
    let mut archive = zip::ZipArchive::new(File::open(archive_path)?)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index)?;
        let path = entry.enclosed_name().with_context(|| {
            format!(
                "zip entry {:?} would escape the extraction root",
                entry.name()
            )
        })?;
        validate_archive_path(&path)?;
        if let Some(mode) = entry.unix_mode() {
            let kind = mode & 0o170000;
            if kind != 0 && kind != 0o100000 && kind != 0o040000 {
                bail!("zip entry {:?} is a link or special file", entry.name());
            }
        }
        let output = destination.join(&path);
        if entry.is_dir() {
            std::fs::create_dir_all(&output)?;
            continue;
        }
        if let Some(parent) = output.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut file = File::create(&output)?;
        std::io::copy(&mut entry, &mut file)?;
        file.flush()?;
        set_zip_permissions(&output, entry.unix_mode())?;
    }
    Ok(())
}

fn validate_archive_path(path: &Path) -> Result<()> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        bail!("archive contains unsafe path {path:?}");
    }
    path.to_str()
        .context("non-UTF-8 archive paths are not supported")?;
    Ok(())
}

#[cfg(unix)]
fn set_zip_permissions(path: &Path, mode: Option<u32>) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    if let Some(mode) = mode {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode & 0o777))?;
    }
    Ok(())
}

#[cfg(not(unix))]
fn set_zip_permissions(_path: &Path, _mode: Option<u32>) -> Result<()> {
    Ok(())
}

fn publish_tree(source: &Path, destination: &Path, scratch: &Path) -> Result<()> {
    let backup = scratch.join("previous");
    let had_previous = destination.exists();
    if had_previous {
        std::fs::rename(destination, &backup).with_context(|| {
            format!(
                "failed to stage existing vendor tree {}",
                destination.display()
            )
        })?;
    }
    if let Err(error) = std::fs::rename(source, destination) {
        if had_previous {
            if let Err(restore) = std::fs::rename(&backup, destination) {
                bail!(
                    "failed to publish vendor tree {}: {error}; restoring the previous tree also failed: {restore}",
                    destination.display()
                );
            }
        }
        return Err(error)
            .with_context(|| format!("failed to publish vendor tree {}", destination.display()));
    }
    if had_previous {
        // Publication is already complete. Scratch's Drop retries cleanup; a
        // locked old file must not turn a valid new tree into a fetch failure.
        let _ = std::fs::remove_dir_all(&backup);
    }
    Ok(())
}

struct Scratch {
    path: PathBuf,
}

impl Scratch {
    fn create(parent: &Path, name: &str) -> Result<Self> {
        for _ in 0..100 {
            let id = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(
                ".frost-fetch-{name}.{}.{id}.tmp",
                std::process::id()
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!(
            "could not allocate a fetch staging directory in {}",
            parent.display()
        )
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use zip::write::SimpleFileOptions;
    use zip::ZipWriter;

    fn scratch(name: &str) -> PathBuf {
        std::env::temp_dir().join(format!("frost-cli-fetch-{name}-{}", std::process::id()))
    }

    #[test]
    fn zip_archives_extract_without_external_tools() {
        let root = scratch("zip");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        let archive_path = root.join("archive.zip");
        let mut archive = ZipWriter::new(File::create(&archive_path).unwrap());
        archive
            .start_file(
                "package/value.txt",
                SimpleFileOptions::DEFAULT.unix_permissions(0o644),
            )
            .unwrap();
        archive.write_all(b"zip\n").unwrap();
        archive.finish().unwrap();

        let destination = root.join("out");
        std::fs::create_dir(&destination).unwrap();
        extract(&archive_path, &destination).unwrap();
        assert_eq!(
            std::fs::read(destination.join("package/value.txt")).unwrap(),
            b"zip\n"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn archive_paths_cannot_escape_the_staging_tree() {
        assert!(validate_archive_path(Path::new("../escape")).is_err());
        assert!(validate_archive_path(Path::new("/absolute")).is_err());
        assert!(validate_archive_path(Path::new("safe/path")).is_ok());
    }
}
