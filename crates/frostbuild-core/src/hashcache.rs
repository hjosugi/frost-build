use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::RwLock;

use anyhow::{Context, Result};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Digest recorded for an input path that does not exist on disk. Missing
/// files still participate in action keys so that deleting an input forces a
/// re-run (which then surfaces the real error from the tool).
pub const MISSING: &str = "MISSING";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Entry {
    mtime_ns: i128,
    size: u64,
    ino: u64,
    hash: String,
}

/// Content-hash cache keyed by workspace-relative (or absolute, for system
/// headers) path, validated by a (mtime, size, inode) stat triple. Avoids
/// re-hashing unchanged files across builds.
///
/// One instance covers one build, and treats that build as a single point in
/// time: a path digested once is not stat'd again unless [`Self::invalidate`]
/// says frost rewrote it. Declared outputs always invalidate, so the guarantee
/// holds for every path frost is responsible for; a write frost was never told
/// about is an undeclared side effect, which the build model does not admit
/// and `--sandbox` exists to catch. The next build starts from a fresh
/// instance and re-stats everything.
///
/// Split into an immutable snapshot loaded from disk and the changes made by
/// this build. Every worker reads the snapshot without synchronizing; a lock
/// is taken only once something has actually changed. A no-op build changes
/// nothing, so its stat path — the one that decides whether frost has any
/// work at all — never contends.
#[derive(Debug, Default)]
pub struct HashCache {
    /// Loaded from disk; never mutated.
    snapshot: HashMap<String, Entry>,
    /// Entries this build recomputed. Reads consult it only after
    /// [`Self::changed`] flips, which a no-op build never does.
    updates: RwLock<HashMap<String, Entry>>,
    changed: AtomicBool,
    /// Digests already established during this build, so a path that is both
    /// one action's output and the next action's input is stat'd once rather
    /// than twice. A build is a single point in time: a path is re-stat'd
    /// only after frost itself writes it, which clears the entry.
    settled: RwLock<HashMap<String, String>>,
    /// The file `snapshot` was read from, so a save can append this build's
    /// changes to it instead of rewriting every entry.
    loaded: Option<LoadedFile>,
}

/// What `load` saw on disk. A save appends only to the very file it loaded
/// (same inode, same length): anything else — another build rewrote or
/// extended it, or it did not decode to the end — is replaced wholesale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LoadedFile {
    len: u64,
    ino: u64,
    /// Decoded entries that a later frame superseded: the file's garbage.
    superseded: usize,
    /// Every frame decoded; a torn or corrupt tail must not be appended to.
    complete: bool,
}

/// Entries per frame when the cache is written whole. Frames decode
/// independently, so a 20k-entry cache loads on several threads.
const FRAME_ENTRIES: usize = 4096;

/// One frame: a length prefix and a postcard list of `(path, entry)`. An entry
/// with an empty digest removes its path.
fn push_frame<'a>(
    bytes: &mut Vec<u8>,
    entries: impl Iterator<Item = (&'a String, &'a Entry)>,
) -> Result<()> {
    let entries: Vec<(&String, &Entry)> = entries.collect();
    let payload = postcard::to_allocvec(&entries)?;
    bytes.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    bytes.extend_from_slice(&payload);
    Ok(())
}

/// Decode a cache file: every complete frame in order, later entries
/// replacing earlier ones. Stops at the first frame that is torn or does not
/// decode, like the journal, and says so.
fn decode(bytes: &[u8]) -> Option<(HashMap<String, Entry>, usize, bool)> {
    if bytes.len() < CACHE_MAGIC.len() || &bytes[..CACHE_MAGIC.len()] != CACHE_MAGIC {
        return None;
    }
    let mut frames = Vec::new();
    let mut cursor = CACHE_MAGIC.len();
    let mut complete = true;
    while cursor < bytes.len() {
        let Some(prefix) = bytes.get(cursor..cursor + 4) else {
            complete = false;
            break;
        };
        let len = u32::from_le_bytes(prefix.try_into().unwrap()) as usize;
        let start = cursor + 4;
        let Some(frame) = start.checked_add(len).and_then(|end| bytes.get(start..end)) else {
            complete = false;
            break;
        };
        frames.push(frame);
        cursor = start + len;
    }
    let decoded: Vec<Option<Vec<(String, Entry)>>> = frames
        .par_iter()
        .map(|frame| postcard::from_bytes(frame).ok())
        .collect();
    let mut snapshot = HashMap::new();
    let mut total = 0usize;
    for frame in decoded {
        let Some(entries) = frame else {
            complete = false;
            break;
        };
        total += entries.len();
        for (path, entry) in entries {
            if entry.hash.is_empty() {
                snapshot.remove(&path);
            } else {
                snapshot.insert(path, entry);
            }
        }
    }
    let superseded = total.saturating_sub(snapshot.len());
    Some((snapshot, superseded, complete))
}

#[cfg(unix)]
fn inode(metadata: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    metadata.ino()
}

#[cfg(not(unix))]
fn inode(_metadata: &std::fs::Metadata) -> u64 {
    0
}

pub const CACHE_REL_PATH: &str = ".frost/hashcache.bin";
/// Pre-0.2 JSON cache location, removed opportunistically on save.
pub const LEGACY_CACHE_REL_PATH: &str = ".frost/hashcache.json";
/// `03` frames the entries so that a build which changed a few files appends
/// those few instead of rewriting the whole cache. An `02` cache is a foreign
/// version and starts empty (one re-hash).
const CACHE_MAGIC: &[u8; 8] = b"FRSTHC03";

impl HashCache {
    pub fn load(workspace_root: &Path) -> Self {
        let path = workspace_root.join(CACHE_REL_PATH);
        crate::phases::time("hashcache.load", || {
            let Ok(mut file) = std::fs::File::open(&path) else {
                return Self::default();
            };
            let Ok(metadata) = file.metadata() else {
                return Self::default();
            };
            let mut bytes = Vec::with_capacity(metadata.len() as usize);
            if file.read_to_end(&mut bytes).is_err() {
                return Self::default();
            }
            let Some((snapshot, superseded, complete)) = decode(&bytes) else {
                return Self::default();
            };
            Self {
                snapshot,
                loaded: Some(LoadedFile {
                    len: bytes.len() as u64,
                    ino: inode(&metadata),
                    superseded,
                    complete,
                }),
                ..Self::default()
            }
        })
    }

    #[cfg(test)]
    fn snapshot_entry(&self, rel: &str) -> Entry {
        self.updates
            .read()
            .unwrap()
            .get(rel)
            .cloned()
            .unwrap_or_else(|| self.snapshot[rel].clone())
    }

    /// Cached entry for `rel`, newest first. Lock-free until this build has
    /// changed something.
    fn lookup(&self, rel: &str) -> Option<Entry> {
        if self.changed.load(Ordering::Relaxed) {
            if let Some(hit) = self.updates.read().unwrap().get(rel) {
                return Some(hit.clone());
            }
        }
        self.snapshot.get(rel).cloned()
    }

    fn store(&self, rel: &str, entry: Entry) {
        self.updates.write().unwrap().insert(rel.to_string(), entry);
        self.changed.store(true, Ordering::Relaxed);
    }

    /// Digest already established during this build, if any.
    fn settled(&self, rel: &str) -> Option<String> {
        self.settled.read().unwrap().get(rel).cloned()
    }

    fn settle(&self, rel: &str, hash: &str) {
        self.settled
            .write()
            .unwrap()
            .insert(rel.to_string(), hash.to_string());
    }

    fn forget(&self, rel: &str) {
        // A removed path must not fall back to the snapshot, so remember the
        // removal explicitly rather than deleting an entry that only exists
        // in the read-only half.
        self.updates.write().unwrap().insert(
            rel.to_string(),
            Entry {
                mtime_ns: i128::MIN,
                size: 0,
                ino: 0,
                hash: String::new(),
            },
        );
        self.settled.write().unwrap().remove(rel);
        self.changed.store(true, Ordering::Relaxed);
    }

    pub fn save(&self, workspace_root: &Path) -> Result<()> {
        if !self.changed.load(Ordering::Relaxed) {
            return Ok(());
        }
        let updates = self.updates.read().unwrap();
        let path = workspace_root.join(CACHE_REL_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Append when the file is exactly the one this build loaded and the
        // garbage appending leaves behind stays under half the live entries.
        // A build that changed one file then writes two entries, not twenty
        // thousand.
        let unchanged_on_disk = self.loaded.is_some_and(|loaded| {
            loaded.complete
                && std::fs::metadata(&path)
                    .is_ok_and(|now| now.len() == loaded.len && inode(&now) == loaded.ino)
        });
        let superseded = self.loaded.map_or(0, |loaded| loaded.superseded);
        if unchanged_on_disk && superseded + updates.len() <= self.snapshot.len() / 2 {
            let mut frame = Vec::new();
            crate::phases::time("hashcache.encode", || {
                push_frame(&mut frame, updates.iter())
            })?;
            drop(updates);
            // One write of one buffer on an append-mode file. A crash can
            // tear only this frame, which the next load drops (and then
            // rewrites the file whole).
            return crate::phases::time("hashcache.write", || -> Result<()> {
                use std::io::Write;
                std::fs::OpenOptions::new()
                    .append(true)
                    .open(&path)?
                    .write_all(&frame)
                    .with_context(|| format!("failed to append to {}", path.display()))
            });
        }

        let tmp = path.with_extension("bin.tmp");
        let mut bytes = CACHE_MAGIC.to_vec();
        // Encoded straight from the two halves, never cloning the snapshot to
        // merge into: that was twenty thousand string copies on a 10k-target
        // graph.
        crate::phases::time("hashcache.encode", || -> Result<()> {
            let live = self
                .snapshot
                .iter()
                .filter(|(path, _)| !updates.contains_key(*path))
                .chain(updates.iter().filter(|(_, entry)| !entry.hash.is_empty()));
            let mut live = live.peekable();
            while live.peek().is_some() {
                push_frame(&mut bytes, live.by_ref().take(FRAME_ENTRIES))?;
            }
            Ok(())
        })?;
        drop(updates);
        crate::phases::time("hashcache.write", || -> Result<()> {
            std::fs::write(&tmp, bytes)?;
            std::fs::rename(&tmp, &path)
                .with_context(|| format!("failed to persist {}", path.display()))?;
            let _ = std::fs::remove_file(workspace_root.join(LEGACY_CACHE_REL_PATH));
            Ok(())
        })
    }

    /// Digest for `rel` (workspace-relative, or absolute e.g. a system
    /// header). Returns [`MISSING`] when the file does not exist.
    pub fn digest(&self, workspace_root: &Path, rel: &str) -> Result<String> {
        if let Some(hit) = self.settled(rel) {
            return Ok(hit);
        }
        let full = resolve(workspace_root, rel);
        let Ok(meta) = std::fs::metadata(&full) else {
            if self.lookup(rel).is_some_and(|e| !e.hash.is_empty()) {
                self.forget(rel);
            }
            return Ok(MISSING.to_string());
        };
        let stat = stat_triple(&meta);
        if let Some(entry) = self.lookup(rel) {
            if !entry.hash.is_empty() && (entry.mtime_ns, entry.size, entry.ino) == stat {
                self.settle(rel, &entry.hash);
                return Ok(entry.hash);
            }
        }
        let hash =
            hash_file(&full).with_context(|| format!("failed to hash {}", full.display()))?;
        self.settle(rel, &hash);
        self.store(
            rel,
            Entry {
                mtime_ns: stat.0,
                size: stat.1,
                ino: stat.2,
                hash: hash.clone(),
            },
        );
        Ok(hash)
    }

    /// Resolve a fileset with cached stat checks and hash misses in parallel.
    pub fn digest_many(
        &self,
        workspace_root: &Path,
        paths: &[String],
    ) -> Result<std::collections::BTreeMap<String, String>> {
        let mut ready = std::collections::BTreeMap::new();
        let mut misses = Vec::new();
        for rel in paths {
            if let Some(hit) = self.settled(rel) {
                ready.insert(rel.clone(), hit);
                continue;
            }
            let full = resolve(workspace_root, rel);
            let Ok(meta) = std::fs::metadata(&full) else {
                if self.lookup(rel).is_some_and(|e| !e.hash.is_empty()) {
                    self.forget(rel);
                }
                ready.insert(rel.clone(), MISSING.to_string());
                continue;
            };
            let stat = stat_triple(&meta);
            if let Some(entry) = self.lookup(rel) {
                if !entry.hash.is_empty() && (entry.mtime_ns, entry.size, entry.ino) == stat {
                    self.settle(rel, &entry.hash);
                    ready.insert(rel.clone(), entry.hash);
                    continue;
                }
            }
            misses.push((rel.clone(), full, stat));
        }
        let hashed: Result<Vec<_>> = misses
            .into_par_iter()
            .map(|(rel, full, stat)| {
                hash_file(&full)
                    .with_context(|| format!("failed to hash {}", full.display()))
                    .map(|hash| (rel, stat, hash))
            })
            .collect();
        for (rel, stat, hash) in hashed? {
            self.settle(&rel, &hash);
            self.store(
                &rel,
                Entry {
                    mtime_ns: stat.0,
                    size: stat.1,
                    ino: stat.2,
                    hash: hash.clone(),
                },
            );
            ready.insert(rel, hash);
        }
        Ok(ready)
    }

    /// Check a large set of expected digests without populating the per-build
    /// `settled` map. This is the fully-cached build path: every unique file
    /// is checked exactly once, stat calls run in parallel, and only entries
    /// whose stat identity changed are re-hashed and written back.
    pub fn matches_many(&self, workspace_root: &Path, expected: &[(&str, &str)]) -> Result<bool> {
        Ok(self
            .matches_each(workspace_root, expected)?
            .into_iter()
            .all(|matched| matched))
    }

    /// As [`Self::matches_many`], with one verdict per expectation, so a
    /// build in which one file changed can still certify every action that
    /// does not read it.
    pub fn matches_each(
        &self,
        workspace_root: &Path,
        expected: &[(&str, &str)],
    ) -> Result<Vec<bool>> {
        let checked: Result<Vec<_>> = expected
            .par_iter()
            .map(|&(rel, digest)| {
                let full = resolve(workspace_root, rel);
                let Ok(meta) = std::fs::metadata(&full) else {
                    return Ok((digest == MISSING, None));
                };
                let stat = stat_triple(&meta);
                if let Some(entry) = self.lookup(rel) {
                    if !entry.hash.is_empty() && (entry.mtime_ns, entry.size, entry.ino) == stat {
                        return Ok((entry.hash == digest, None));
                    }
                }
                let hash = hash_file(&full)
                    .with_context(|| format!("failed to hash {}", full.display()))?;
                let entry = Entry {
                    mtime_ns: stat.0,
                    size: stat.1,
                    ino: stat.2,
                    hash: hash.clone(),
                };
                Ok((hash == digest, Some((rel.to_string(), entry))))
            })
            .collect();

        let checked = checked?;
        let mut verdicts = Vec::with_capacity(checked.len());
        for (matched, update) in checked {
            verdicts.push(matched);
            if let Some((rel, entry)) = update {
                self.store(&rel, entry);
            }
        }
        Ok(verdicts)
    }

    /// Drop the cached stat for a path we just (re)wrote, forcing a re-hash.
    /// Needed for action outputs: a fast rewrite can land in the same mtime
    /// granule with the same size.
    pub fn invalidate(&self, rel: &str) {
        self.forget(rel);
    }
}

#[cfg(unix)]
fn is_executable(meta: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    meta.permissions().mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_meta: &std::fs::Metadata) -> bool {
    false
}

fn resolve(workspace_root: &Path, rel: &str) -> std::path::PathBuf {
    let p = Path::new(rel);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workspace_root.join(p)
    }
}

#[cfg(unix)]
fn stat_triple(meta: &std::fs::Metadata) -> (i128, u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (
        i128::from(meta.mtime()) * 1_000_000_000 + i128::from(meta.mtime_nsec()),
        // chmod updates ctime, not mtime, so a mode change would otherwise
        // reuse the cached digest and never reach the hash above.
        meta.size() ^ ((meta.mode() as u64 & 0o111) << 40),
        meta.ino(),
    )
}

#[cfg(not(unix))]
fn stat_triple(meta: &std::fs::Metadata) -> (i128, u64, u64) {
    let mtime = meta
        .modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos() as i128)
        .unwrap_or(0);
    (mtime, meta.len(), 0)
}

/// Content digest of a file, including whether it is executable.
///
/// The mode is part of the digest rather than a separate field because it is
/// part of what the file *is*: `chmod -x` on a script a genrule runs leaves
/// the bytes untouched, so a content-only digest reports the build as current
/// while a clean build of the same tree fails. Mixing one bit into the hash
/// costs nothing and closes that gap; the CAS then stores the two modes as
/// distinct objects, which is also what restoring them correctly requires.
pub fn hash_file(path: &Path) -> Result<String> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    let mut hasher = blake3::Hasher::new();
    hasher.update(if is_executable(&metadata) { b"x" } else { b"-" });
    let mut file = file;
    // A fixed 4 MiB allocation per file is reasonable for large compiler
    // artifacts but pathological for thousands of tiny depfiles/classes:
    // hashing 100 ~300-byte class files allocated 400 MiB cumulatively.
    // Retain the large sequential-read buffer where it helps, while keeping
    // small-file hashing within an allocator-friendly floor.
    let buffer_len = hash_buffer_len(metadata.len());
    let mut buf = vec![0u8; buffer_len];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        if n >= 128 * 1024 {
            hasher.update_rayon(&buf[..n]);
        } else {
            hasher.update(&buf[..n]);
        }
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn hash_buffer_len(file_len: u64) -> usize {
    usize::try_from(file_len)
        .unwrap_or(4 * 1024 * 1024)
        .clamp(8 * 1024, 4 * 1024 * 1024)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_foreign_cache_version_starts_empty_instead_of_misreading_it() {
        let dir =
            std::env::temp_dir().join(format!("frost-hashcache-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".frost")).unwrap();

        // A cache written by another version is not data this version can
        // interpret. Re-hashing costs time; believing it costs correctness.
        let mut foreign = b"FRSTHC99".to_vec();
        foreign.extend_from_slice(&[0xAB; 64]);
        std::fs::write(dir.join(CACHE_REL_PATH), &foreign).unwrap();

        let cache = HashCache::load(&dir);
        assert!(cache.snapshot.is_empty());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn missing_file_digests_as_missing() {
        let dir = std::env::temp_dir().join("frost-hashcache-test-missing");
        std::fs::create_dir_all(&dir).unwrap();
        let cache = HashCache::default();
        assert_eq!(cache.digest(&dir, "no/such/file").unwrap(), MISSING);
    }

    #[test]
    fn hashing_buffer_adapts_to_small_and_large_files() {
        assert_eq!(hash_buffer_len(0), 8 * 1024);
        assert_eq!(hash_buffer_len(300), 8 * 1024);
        assert_eq!(hash_buffer_len(64 * 1024), 64 * 1024);
        assert_eq!(hash_buffer_len(64 * 1024 * 1024), 4 * 1024 * 1024);
    }

    #[test]
    fn caches_and_detects_content_change() {
        let dir = std::env::temp_dir().join(format!("frost-hashcache-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("a.txt");

        std::fs::write(&file, "one").unwrap();
        let cache = HashCache::default();
        let h1 = cache.digest(&dir, "a.txt").unwrap();
        assert_eq!(cache.digest(&dir, "a.txt").unwrap(), h1, "repeat is stable");

        // A cache instance covers one build, and a build is one point in
        // time: a path already digested is not re-stat'd. This is what makes
        // a file that is one action's output and the next action's input cost
        // a single stat instead of two.
        std::fs::write(&file, "two-longer").unwrap();
        assert_eq!(
            cache.digest(&dir, "a.txt").unwrap(),
            h1,
            "a write frost did not make is not observed mid-build"
        );

        // Whenever frost writes a path it says so, and the next digest is
        // fresh. Every engine path that produces outputs calls this.
        cache.invalidate("a.txt");
        let h2 = cache.digest(&dir, "a.txt").unwrap();
        assert_ne!(h1, h2, "invalidate restores freshness");

        // A new build sees the current content with no invalidation needed.
        assert_eq!(HashCache::default().digest(&dir, "a.txt").unwrap(), h2);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn save_and_load_roundtrip() {
        let dir =
            std::env::temp_dir().join(format!("frost-hashcache-roundtrip-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.txt"), "content").unwrap();

        let cache = HashCache::load(&dir);
        let h = cache.digest(&dir, "a.txt").unwrap();
        cache.save(&dir).unwrap();

        let reloaded = HashCache::load(&dir);
        assert_eq!(reloaded.digest(&dir, "a.txt").unwrap(), h);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_save_applies_updates_and_removals_to_the_loaded_snapshot() {
        let dir =
            std::env::temp_dir().join(format!("frost-hashcache-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        for name in ["keep", "change", "remove"] {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let first = HashCache::load(&dir);
        for name in ["keep", "change", "remove"] {
            first.digest(&dir, name).unwrap();
        }
        first.save(&dir).unwrap();

        // The next build rewrites one file, loses another and adds a third.
        std::fs::write(dir.join("change"), "changed contents").unwrap();
        std::fs::remove_file(dir.join("remove")).unwrap();
        std::fs::write(dir.join("new"), "new").unwrap();
        let second = HashCache::load(&dir);
        for name in ["keep", "change", "remove", "new"] {
            second.digest(&dir, name).unwrap();
        }
        second.save(&dir).unwrap();

        let reloaded = HashCache::load(&dir);
        let mut paths: Vec<&str> = reloaded.snapshot.keys().map(String::as_str).collect();
        paths.sort_unstable();
        assert_eq!(paths, ["change", "keep", "new"]);
        assert_eq!(
            reloaded.snapshot["change"].hash,
            hash_file(&dir.join("change")).unwrap()
        );
        assert_eq!(reloaded.snapshot["keep"], first.snapshot_entry("keep"));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_small_change_is_appended_and_a_torn_tail_forces_a_rewrite() {
        let dir =
            std::env::temp_dir().join(format!("frost-hashcache-append-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let names: Vec<String> = (0..(FRAME_ENTRIES + 10)).map(|i| format!("f{i}")).collect();
        for name in &names {
            std::fs::write(dir.join(name), name).unwrap();
        }
        let first = HashCache::load(&dir);
        first.digest_many(&dir, &names).unwrap();
        first.save(&dir).unwrap();
        let cache = dir.join(CACHE_REL_PATH);
        let whole = std::fs::metadata(&cache).unwrap().len();

        // One file changes: the save appends one small frame.
        std::fs::write(dir.join("f3"), "a different length").unwrap();
        let second = HashCache::load(&dir);
        assert_eq!(second.snapshot.len(), names.len(), "all frames decoded");
        second.digest(&dir, "f3").unwrap();
        second.save(&dir).unwrap();
        let appended = std::fs::metadata(&cache).unwrap().len();
        assert!(
            appended > whole && appended - whole < 200,
            "{whole} -> {appended}"
        );
        let third = HashCache::load(&dir);
        assert_eq!(third.snapshot.len(), names.len());
        assert_eq!(
            third.snapshot["f3"].hash,
            hash_file(&dir.join("f3")).unwrap(),
            "the appended entry replaces the chunked one"
        );
        assert_eq!(third.loaded.unwrap().superseded, 1);

        // A torn append: everything before it still loads, and the next save
        // rewrites the file instead of appending after the tear.
        let mut torn = std::fs::read(&cache).unwrap();
        torn.extend_from_slice(&500u32.to_le_bytes());
        torn.extend_from_slice(b"partial");
        std::fs::write(&cache, &torn).unwrap();
        let fourth = HashCache::load(&dir);
        assert_eq!(fourth.snapshot.len(), names.len());
        assert!(!fourth.loaded.unwrap().complete);
        std::fs::write(dir.join("f4"), "changed again").unwrap();
        fourth.digest(&dir, "f4").unwrap();
        fourth.save(&dir).unwrap();
        let rewritten = HashCache::load(&dir);
        assert!(rewritten.loaded.unwrap().complete);
        assert_eq!(rewritten.loaded.unwrap().superseded, 0);
        assert_eq!(
            rewritten.snapshot["f4"].hash,
            hash_file(&dir.join("f4")).unwrap()
        );
        assert_eq!(rewritten.snapshot["f3"], third.snapshot["f3"]);

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn matches_each_reports_one_verdict_per_expectation() {
        let dir = std::env::temp_dir().join(format!("frost-hashcache-each-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a"), "a").unwrap();
        std::fs::write(dir.join("b"), "b").unwrap();
        let a = hash_file(&dir.join("a")).unwrap();
        let cache = HashCache::default();
        let verdicts = cache
            .matches_each(
                &dir,
                &[("a", a.as_str()), ("b", a.as_str()), ("absent", MISSING)],
            )
            .unwrap();
        assert_eq!(verdicts, [true, false, true]);
        assert!(!cache
            .matches_many(&dir, &[("a", a.as_str()), ("b", a.as_str())])
            .unwrap());
        std::fs::remove_dir_all(&dir).ok();
    }
}
