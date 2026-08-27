//! Verified acquisition primitives shared by pinned fetches and distribution.
//!
//! Downloading, hashing and unpacking are deliberately kept below the callers
//! that decide *what* to trust. Both callers stage beside their destination,
//! verify SHA-256 before extraction, reject archive path tricks and publish only
//! a complete tree or binary.

use std::fs::File;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{bail, Context, Result};
use flate2::read::GzDecoder;
use sha2::{Digest, Sha256};

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

pub(crate) fn download(url: &str, destination: &Path) -> Result<()> {
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
        bail!("downloading requires curl or wget in PATH");
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

pub(crate) fn sha256_file(path: &Path) -> Result<String> {
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

pub(crate) fn extract(archive_path: &Path, destination: &Path) -> Result<()> {
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

pub(crate) struct Scratch {
    path: PathBuf,
}

impl Scratch {
    pub(crate) fn create(parent: &Path, label: &str) -> Result<Self> {
        let label: String = label
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '-'
                }
            })
            .collect();
        for _ in 0..100 {
            let id = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let path = parent.join(format!(".{label}.{}.{id}.tmp", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }
        bail!(
            "could not allocate a staging directory in {}",
            parent.display()
        )
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
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
        std::env::temp_dir().join(format!("frost-cli-acquire-{name}-{}", std::process::id()))
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
