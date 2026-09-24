//! Placing a file somewhere else without the platform primitives Linux has.
//!
//! `--hermetic` runs an action in a tree holding only what it may read, and
//! that tree has to be filled for every execution. Copying is always correct
//! and always slowest; a copy-on-write clone costs a metadata operation and
//! shares nothing observable; a hard link costs the same but shares the inode,
//! so a tool that writes to its input in place writes to the workspace. Which
//! of these a host can do depends on the filesystem, not only the OS, so the
//! choice is probed where the tree will live rather than assumed from `cfg`.
//!
//! The order is per OS and is a decision, not a discovery:
//!
//! | host | order | why |
//! |---|---|---|
//! | Linux | reflink, hardlink, copy | `FICLONE` works on btrfs/XFS/bcachefs; ext4 and tmpfs refuse it |
//! | macOS | reflink, hardlink, copy | `clonefile` works on APFS, the default volume format |
//! | Windows | hardlink, copy | ReFS block cloning is not implemented; NTFS hard links need no privilege |
//!
//! A hard link is only ever used for a file the action reads. [`Tree`] records
//! each one and [`Tree::verify_links`] fails the action if the linked file's
//! size or modification time moved, because that means the action wrote
//! through the link into the workspace.

use std::fmt;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use anyhow::Context;
use anyhow::Result;

/// One way of placing a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Strategy {
    /// A copy-on-write clone: `FICLONE` on Linux, `clonefile` on macOS.
    Reflink,
    /// A second name for the same inode.
    Hardlink,
    /// A byte copy.
    Copy,
}

impl Strategy {
    pub const ALL: [Strategy; 3] = [Strategy::Reflink, Strategy::Hardlink, Strategy::Copy];

    pub fn as_str(self) -> &'static str {
        match self {
            Strategy::Reflink => "reflink",
            Strategy::Hardlink => "hardlink",
            Strategy::Copy => "copy",
        }
    }

    pub fn parse(text: &str) -> Option<Self> {
        Strategy::ALL
            .into_iter()
            .find(|strategy| strategy.as_str() == text)
    }
}

impl fmt::Display for Strategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// What the caller asked for: the host's first working strategy, or one named.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Materialization {
    #[default]
    Auto,
    Only(Strategy),
}

impl Materialization {
    pub fn as_str(self) -> &'static str {
        match self {
            Materialization::Auto => "auto",
            Materialization::Only(strategy) => strategy.as_str(),
        }
    }
}

/// The strategies `auto` tries on this host, in order.
pub fn host_order() -> &'static [Strategy] {
    if cfg!(windows) {
        &[Strategy::Hardlink, Strategy::Copy]
    } else {
        &[Strategy::Reflink, Strategy::Hardlink, Strategy::Copy]
    }
}

/// Place `source` at `destination`, which must not exist, with `strategy`.
///
/// The executable bit travels with the file on every strategy: a clone on
/// Linux copies data only, so the mode is set afterwards.
pub fn place(strategy: Strategy, source: &Path, destination: &Path) -> io::Result<()> {
    match strategy {
        Strategy::Reflink => reflink(source, destination),
        Strategy::Hardlink => std::fs::hard_link(source, destination),
        Strategy::Copy => std::fs::copy(source, destination).map(|_| ()),
    }
}

#[cfg(target_os = "linux")]
fn reflink(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::unix::io::AsRawFd as _;

    let input = std::fs::File::open(source)?;
    let metadata = input.metadata()?;
    let output = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(destination)?;
    // SAFETY: both descriptors are open for the duration of the call, and
    // FICLONE takes the source descriptor as its integer argument.
    let status = unsafe { libc::ioctl(output.as_raw_fd(), libc::FICLONE, input.as_raw_fd()) };
    if status != 0 {
        let error = io::Error::last_os_error();
        drop(output);
        let _ = std::fs::remove_file(destination);
        return Err(error);
    }
    output.set_permissions(metadata.permissions())
}

#[cfg(target_os = "macos")]
fn reflink(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt as _;

    let source = CString::new(source.as_os_str().as_bytes())?;
    let destination = CString::new(destination.as_os_str().as_bytes())?;
    // SAFETY: both arguments are valid NUL-terminated paths; clonefile copies
    // the mode itself and fails without side effects when it cannot clone.
    let status = unsafe { libc::clonefile(source.as_ptr(), destination.as_ptr(), 0) };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn reflink(_source: &Path, _destination: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "copy-on-write cloning is not implemented on this host",
    ))
}

/// The result of trying one strategy in a directory.
#[derive(Debug, Clone)]
pub struct Probe {
    pub strategy: Strategy,
    /// `None` when it worked, otherwise why not.
    pub error: Option<String>,
}

/// Try every strategy between two files in `directory`.
///
/// Every strategy is tried, not only the host's `auto` order, so `frost doctor`
/// can say *why* one was passed over — "reflink: Operation not supported" on
/// ext4 is the answer a reader of the report is looking for.
pub fn probe(directory: &Path) -> Result<Vec<Probe>> {
    std::fs::create_dir_all(directory)
        .with_context(|| format!("failed to create {}", directory.display()))?;
    let scratch = directory.join(format!(".probe-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&scratch);
    std::fs::create_dir_all(&scratch)
        .with_context(|| format!("failed to create {}", scratch.display()))?;
    let source = scratch.join("source");
    std::fs::write(&source, b"frost materialization probe\n")
        .with_context(|| format!("failed to write {}", source.display()))?;
    let probes = Strategy::ALL
        .into_iter()
        .map(|strategy| {
            let destination = scratch.join(strategy.as_str());
            let error = match place(strategy, &source, &destination) {
                Ok(()) => match std::fs::read(&destination) {
                    Ok(bytes) if bytes == b"frost materialization probe\n" => None,
                    Ok(_) => Some("placed file has different contents".to_string()),
                    Err(error) => Some(error.to_string()),
                },
                Err(error) => Some(error.to_string()),
            };
            Probe { strategy, error }
        })
        .collect();
    let _ = std::fs::remove_dir_all(&scratch);
    Ok(probes)
}

/// Resolve a request against what `directory`'s filesystem can do.
///
/// A named strategy that does not work is an error rather than a silent
/// fallback: someone who asked for `copy` to rule out links, or for `reflink`
/// to measure it, must not get something else.
pub fn select(directory: &Path, requested: Materialization) -> Result<Strategy> {
    let probes = probe(directory)?;
    let works = |strategy: Strategy| {
        probes
            .iter()
            .find(|probe| probe.strategy == strategy)
            .and_then(|probe| probe.error.clone())
    };
    match requested {
        Materialization::Only(strategy) => match works(strategy) {
            None => Ok(strategy),
            Some(error) => anyhow::bail!(
                "--materialize {strategy} is not supported in {}: {error}. \
                 `--materialize auto` picks the first of {} that works; \
                 `frost doctor` shows what each one does here",
                directory.display(),
                describe_order()
            ),
        },
        Materialization::Auto => host_order()
            .iter()
            .copied()
            .find(|&strategy| works(strategy).is_none())
            .with_context(|| {
                format!(
                    "no materialization strategy works in {}",
                    directory.display()
                )
            }),
    }
}

/// `reflink > hardlink > copy`, for messages.
pub fn describe_order() -> String {
    host_order()
        .iter()
        .map(|strategy| strategy.as_str())
        .collect::<Vec<_>>()
        .join(" > ")
}

/// A file placed by hard link, and what it looked like beforehand.
#[derive(Debug)]
struct Linked {
    workspace: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
}

/// A directory being filled for one action.
#[derive(Debug)]
pub struct Tree {
    pub root: PathBuf,
    strategy: Strategy,
    linked: Vec<Linked>,
}

impl Tree {
    /// Start an empty tree at `root`, replacing anything left there by a run
    /// that was killed before it could clean up.
    pub fn create(root: PathBuf, strategy: Strategy) -> Result<Self> {
        if root.exists() {
            std::fs::remove_dir_all(&root)
                .with_context(|| format!("failed to remove stale {}", root.display()))?;
        }
        std::fs::create_dir_all(&root)
            .with_context(|| format!("failed to create {}", root.display()))?;
        Ok(Self {
            root,
            strategy,
            linked: Vec::new(),
        })
    }

    /// Place one workspace file the action reads at `relative` in the tree.
    ///
    /// Symbolic links are followed: the tree holds the file the action would
    /// have read, and a relative link recreated elsewhere would point at
    /// nothing. A strategy the filesystem refuses for this particular file
    /// (a hard link across a mount, a clone of a file on another volume)
    /// falls back to a copy, which is always correct.
    pub fn place_file(&mut self, source: &Path, relative: &Path) -> Result<()> {
        self.place_with(self.strategy, source, relative)
    }

    /// Place a file the action will write, such as the previous output of a
    /// `preserve_outputs` tool. Never a hard link: writing it would write the
    /// workspace's copy, which is still the recorded output until this run
    /// succeeds.
    pub fn place_writable(&mut self, source: &Path, relative: &Path) -> Result<()> {
        let strategy = match self.strategy {
            Strategy::Reflink => Strategy::Reflink,
            Strategy::Hardlink | Strategy::Copy => Strategy::Copy,
        };
        self.place_with(strategy, source, relative)
    }

    /// [`Tree::place_writable`] for every file under a directory.
    pub fn place_writable_dir(&mut self, source: &Path, relative: &Path) -> Result<()> {
        std::fs::create_dir_all(self.root.join(relative))?;
        for entry in std::fs::read_dir(source)
            .with_context(|| format!("failed to read {}", source.display()))?
        {
            let entry = entry?;
            let child = relative.join(entry.file_name());
            if entry.file_type()?.is_dir() {
                self.place_writable_dir(&entry.path(), &child)?;
            } else if std::fs::metadata(entry.path()).is_ok_and(|m| m.is_file()) {
                self.place_writable(&entry.path(), &child)?;
            }
        }
        Ok(())
    }

    fn place_with(&mut self, strategy: Strategy, source: &Path, relative: &Path) -> Result<()> {
        let destination = self.root.join(relative);
        if destination.exists() {
            return Ok(());
        }
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let real = std::fs::canonicalize(source).unwrap_or_else(|_| source.to_path_buf());
        match place(strategy, &real, &destination) {
            Ok(()) => {
                if strategy == Strategy::Hardlink {
                    let metadata = std::fs::metadata(&real)?;
                    self.linked.push(Linked {
                        workspace: source.to_path_buf(),
                        len: metadata.len(),
                        modified: metadata.modified().ok(),
                    });
                }
                Ok(())
            }
            Err(_) if strategy != Strategy::Copy => {
                let _ = std::fs::remove_file(&destination);
                std::fs::copy(&real, &destination)
                    .map(|_| ())
                    .with_context(|| {
                        format!(
                            "failed to materialize {} at {}",
                            source.display(),
                            destination.display()
                        )
                    })
            }
            Err(error) => Err(error).with_context(|| {
                format!(
                    "failed to materialize {} at {}",
                    source.display(),
                    destination.display()
                )
            }),
        }
    }

    /// Place every file under a workspace directory, skipping the directory
    /// names in `skip` (absolute paths) wherever they occur.
    pub fn place_dir(&mut self, source: &Path, relative: &Path, skip: &[PathBuf]) -> Result<()> {
        if skip.iter().any(|skipped| skipped == source) {
            return Ok(());
        }
        std::fs::create_dir_all(self.root.join(relative))?;
        let entries = match std::fs::read_dir(source) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => {
                return Err(error).with_context(|| format!("failed to read {}", source.display()))
            }
        };
        let mut entries = entries.collect::<io::Result<Vec<_>>>()?;
        entries.sort_by_key(|entry| entry.file_name());
        for entry in entries {
            let path = entry.path();
            let child = relative.join(entry.file_name());
            // `metadata` follows links, so a link to a directory is walked as
            // the directory and a link to a file is placed as the file. A
            // dangling link is skipped: there is nothing it would let the
            // action read.
            let Ok(metadata) = std::fs::metadata(&path) else {
                continue;
            };
            if metadata.is_dir() {
                if entry.file_type().is_ok_and(|kind| kind.is_symlink()) {
                    // Following a directory link can loop; the target is
                    // reachable through its own name if it is in scope.
                    continue;
                }
                self.place_dir(&path, &child, skip)?;
            } else if metadata.is_file() {
                self.place_file(&path, &child)?;
            }
        }
        Ok(())
    }

    /// Fail if a hard-linked input changed, which means the action wrote
    /// through the link into the workspace.
    pub fn verify_links(&self) -> std::result::Result<(), String> {
        for linked in &self.linked {
            let Ok(metadata) = std::fs::metadata(&linked.workspace) else {
                return Err(format!(
                    "{} disappeared while the action ran",
                    linked.workspace.display()
                ));
            };
            if metadata.len() != linked.len || metadata.modified().ok() != linked.modified {
                return Err(format!(
                    "the action wrote to its input {} in place, and the hermetic tree \
                     shared that file with the workspace through a hard link, so the \
                     workspace copy changed too. Declare what it writes as an output, \
                     or run with `--materialize copy`",
                    linked.workspace.display()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("frost-materialize-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn strategies_have_one_spelling_each() {
        for strategy in Strategy::ALL {
            assert_eq!(Strategy::parse(strategy.as_str()), Some(strategy));
        }
        assert_eq!(Strategy::parse("auto"), None);
        assert_eq!(Strategy::parse("symlink"), None);
    }

    #[test]
    fn the_host_order_ends_in_a_copy_and_windows_never_clones() {
        let order = host_order();
        assert_eq!(order.last(), Some(&Strategy::Copy));
        assert_eq!(
            order.contains(&Strategy::Reflink),
            !cfg!(windows),
            "{order:?}"
        );
    }

    #[test]
    fn a_copy_and_a_hard_link_work_everywhere_and_auto_picks_in_order() {
        let dir = scratch("probe");
        let probes = probe(&dir).unwrap();
        for strategy in [Strategy::Hardlink, Strategy::Copy] {
            let probe = probes.iter().find(|p| p.strategy == strategy).unwrap();
            assert!(probe.error.is_none(), "{strategy}: {:?}", probe.error);
        }
        let selected = select(&dir, Materialization::Auto).unwrap();
        let first_working = host_order()
            .iter()
            .copied()
            .find(|&strategy| {
                probes
                    .iter()
                    .any(|probe| probe.strategy == strategy && probe.error.is_none())
            })
            .unwrap();
        assert_eq!(selected, first_working);
        assert_eq!(
            select(&dir, Materialization::Only(Strategy::Copy)).unwrap(),
            Strategy::Copy
        );
        // The probe cleans up after itself.
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 0);
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_requested_strategy_that_cannot_work_is_refused_not_replaced() {
        let dir = scratch("refuse");
        let probes = probe(&dir).unwrap();
        let reflink = probes
            .iter()
            .find(|probe| probe.strategy == Strategy::Reflink)
            .unwrap();
        let selected = select(&dir, Materialization::Only(Strategy::Reflink));
        match &reflink.error {
            None => assert_eq!(selected.unwrap(), Strategy::Reflink),
            Some(_) => {
                let error = format!("{:#}", selected.unwrap_err());
                assert!(error.contains("--materialize reflink"), "{error}");
                assert!(error.contains("frost doctor"), "{error}");
            }
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[cfg(unix)]
    #[test]
    fn every_strategy_keeps_the_executable_bit() {
        use std::os::unix::fs::PermissionsExt as _;

        let dir = scratch("mode");
        let source = dir.join("tool");
        std::fs::write(&source, b"#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o755)).unwrap();
        for strategy in Strategy::ALL {
            let destination = dir.join(strategy.as_str());
            if place(strategy, &source, &destination).is_err() {
                assert_eq!(strategy, Strategy::Reflink, "only a clone may be refused");
                continue;
            }
            let mode = std::fs::metadata(&destination)
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o111, 0o111, "{strategy} lost the executable bit");
        }
        std::fs::remove_dir_all(dir).ok();
    }

    #[test]
    fn a_tree_skips_what_it_is_told_to_and_detects_writes_through_links() {
        let dir = scratch("tree");
        let workspace = dir.join("ws");
        std::fs::create_dir_all(workspace.join("src/nested")).unwrap();
        std::fs::create_dir_all(workspace.join(".frost/cas")).unwrap();
        std::fs::write(workspace.join("src/a.c"), b"a").unwrap();
        std::fs::write(workspace.join("src/nested/b.h"), b"b").unwrap();
        std::fs::write(workspace.join(".frost/cas/object"), b"x").unwrap();

        let mut tree = Tree::create(dir.join("tree"), Strategy::Hardlink).unwrap();
        tree.place_dir(&workspace, Path::new(""), &[workspace.join(".frost")])
            .unwrap();
        assert_eq!(std::fs::read(tree.root.join("src/a.c")).unwrap(), b"a");
        assert_eq!(
            std::fs::read(tree.root.join("src/nested/b.h")).unwrap(),
            b"b"
        );
        assert!(!tree.root.join(".frost").exists());
        assert!(tree.verify_links().is_ok());

        // A write through the link reaches the workspace; the tree notices.
        std::thread::sleep(std::time::Duration::from_millis(20));
        std::fs::write(tree.root.join("src/a.c"), b"changed").unwrap();
        let error = tree.verify_links().unwrap_err();
        assert!(error.contains("src") && error.contains("a.c"), "{error}");
        assert!(error.contains("--materialize copy"), "{error}");

        // A copy tree shares nothing.
        let mut copied = Tree::create(dir.join("tree"), Strategy::Copy).unwrap();
        copied
            .place_file(
                &workspace.join("src/nested/b.h"),
                Path::new("src/nested/b.h"),
            )
            .unwrap();
        std::fs::write(copied.root.join("src/nested/b.h"), b"changed").unwrap();
        assert_eq!(
            std::fs::read(workspace.join("src/nested/b.h")).unwrap(),
            b"b"
        );
        assert!(copied.verify_links().is_ok());
        std::fs::remove_dir_all(dir).ok();
    }
}
