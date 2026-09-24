use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use memmap2::Mmap;
use serde::{Deserialize, Serialize};

use crate::graph::BuildGraph;
use crate::manifest::{Manifest, HOST_PLATFORM};

const MAGIC: &[u8; 8] = b"FRSTGR01";
// Version 7 adds the host executable suffix to native binary graph paths.
// Version 9 adds `[stamp]` and per-action stamp references, which a warm
// invocation reads without parsing a manifest. Version 10 adds coverage
// configuration and per-action coverage metadata. Version 11 adds scheduler-
// only action resource requirements. Version 12 adds materialized fetch-tree
// files to target action inputs. Version 13 adds a BLAKE3 digest of the graph
// payload: the sources stamp proves the *definition* is unchanged, not that
// the stored bytes are, and a flipped bit in a command string or output path
// otherwise decodes into a different, plausible graph that the warm path
// trusts until a manifest changes. Version 14 stamps directories by their
// listing alone (with symlink targets), no longer by modification time.
const VERSION: u32 = 14;

/// Evidence that the definition inputs which produced a cached graph are
/// unchanged, checkable without parsing any manifest: exact bytes of every
/// contributing manifest/fetch-state file plus the listing of every workspace
/// directory. Package discovery and glob expansion read names, kinds and
/// where symlinks point — nothing else — so an equal stamp implies identical
/// discovery and expansion; file content edits cannot alter either. This makes
/// the warm path sound while skipping TOML parsing entirely.
///
/// The listing is the evidence rather than the directory's modification
/// time, which also moves when nothing a glob can see did: an editor saving by
/// rename, a tool creating and deleting a temporary file, Ninja compacting
/// `.ninja_log`. Each of those used to recompile a 10k-target manifest on the
/// next build (#152).
#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SourcesStamp {
    /// (workspace-relative path, BLAKE3 of bytes) per definition file.
    manifests: Vec<(String, String)>,
    /// Identity of every non-ignored directory and its immediate entries.
    dirs: Vec<DirStamp>,
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
struct DirStamp {
    path: String,
    /// BLAKE3 of sorted native entry names plus their filesystem kind, and
    /// each symlink's target. Content, not timestamps: Windows can expose the
    /// same directory timestamp immediately before and after an entry
    /// mutation, and every platform moves it for mutations that leave the
    /// listing as it was.
    entries_hash: [u8; 32],
}

pub struct GraphStore;

impl GraphStore {
    pub fn load_or_compile(root: &Path, manifest: &Manifest, profile: &str) -> Result<BuildGraph> {
        Self::load_or_compile_configured(root, manifest, profile, HOST_PLATFORM)
    }

    pub fn load_or_compile_configured(
        root: &Path,
        manifest: &Manifest,
        profile: &str,
        platform: &str,
    ) -> Result<BuildGraph> {
        Self::load_or_compile_instrumented(root, manifest, profile, platform, false)
    }

    /// As [`Self::load_or_compile_configured`], for a graph that may be
    /// instrumented for coverage.
    ///
    /// Coverage is part of the cache identity, not a detail inside the payload:
    /// an instrumented graph has different argv, extra outputs and a different
    /// output tree, so serving one where the other was asked for would build
    /// the wrong thing from a warm cache.
    pub fn load_or_compile_instrumented(
        root: &Path,
        manifest: &Manifest,
        profile: &str,
        platform: &str,
        coverage: bool,
    ) -> Result<BuildGraph> {
        let fingerprint = manifest_fingerprint_instrumented(manifest, profile, platform, coverage)?;
        let path = store_path_instrumented(root, profile, platform, coverage);
        if let Ok(graph) = load_graph(&path, Some(&fingerprint), None) {
            // Keep the warm path viable for workspaces whose builds write
            // outputs into the source tree: a stale sources stamp would
            // otherwise force every future invocation through a full parse.
            if load_graph(&path, None, Some(root)).is_err() {
                save_graph(root, &path, &fingerprint, &manifest.manifest_paths, &graph)?;
            }
            return Ok(graph);
        }
        let graph = BuildGraph::from_manifest_instrumented(manifest, profile, platform, coverage)?;
        save_graph(root, &path, &fingerprint, &manifest.manifest_paths, &graph)?;
        Ok(graph)
    }

    /// Warm fast path: return the cached graph when the sources stamp proves
    /// the manifest inputs are unchanged, without loading the manifest at
    /// all. `None` means the caller must fall back to `Manifest::load` +
    /// [`GraphStore::load_or_compile_configured`].
    pub fn load_cached(root: &Path, profile: &str, platform: &str) -> Option<BuildGraph> {
        Self::load_cached_instrumented(root, profile, platform, false)
    }

    /// As [`Self::load_cached`], for the coverage configuration.
    pub fn load_cached_instrumented(
        root: &Path,
        profile: &str,
        platform: &str,
        coverage: bool,
    ) -> Option<BuildGraph> {
        let path = store_path_instrumented(root, profile, platform, coverage);
        load_graph(&path, None, Some(root)).ok()
    }

    /// Validate the manifest/package-discovery evidence in a cached graph
    /// without deserializing the graph payload itself.
    ///
    /// A whole-workspace no-op certificate already describes the files that
    /// must remain unchanged. It still needs to prove that the graph
    /// definition is current, but decoding thousands of actions merely to
    /// learn that no action will run defeats that fast path.
    pub fn cached_sources_current(root: &Path, profile: &str, platform: &str) -> bool {
        Self::cached_fingerprint(root, profile, platform).is_some()
    }

    /// Fingerprint of the manifest/profile/platform tuple embedded in a
    /// source-current graph store, without deserializing its graph payload.
    pub fn cached_fingerprint(root: &Path, profile: &str, platform: &str) -> Option<[u8; 32]> {
        let path = store_path(root, profile, platform);
        validate_cached_sources(&path, root).ok()
    }

    pub fn validate_bytes(bytes: &[u8]) -> Result<()> {
        let parsed = parse_header(bytes)?;
        ensure_payload_intact(&parsed)?;
        let _: BuildGraph = postcard::from_bytes(parsed.payload).context("corrupt graph store")?;
        Ok(())
    }
}

/// Where the compiled graph for one configuration lives. Public because
/// `frost info` answers "which file is this configuration's graph cache?" for
/// wrappers and editors that would otherwise hardcode the naming rule.
pub fn store_path(root: &Path, profile: &str, platform: &str) -> PathBuf {
    store_path_instrumented(root, profile, platform, false)
}

/// As [`store_path`], distinguishing the coverage configuration.
///
/// The suffix is the same `+coverage` the output tree uses, and for the same
/// reason: a profile name cannot contain `+`, so this cannot collide with a
/// declared profile. See [`crate::paths::configured`].
pub fn store_path_instrumented(
    root: &Path,
    profile: &str,
    platform: &str,
    coverage: bool,
) -> PathBuf {
    let profile = crate::paths::instrumented_profile(profile, coverage);
    if platform == HOST_PLATFORM {
        root.join(format!(".frost/graph-{profile}.bin"))
    } else {
        root.join(format!(".frost/graph-{platform}-{profile}.bin"))
    }
}

fn manifest_fingerprint_instrumented(
    manifest: &Manifest,
    profile: &str,
    platform: &str,
    coverage: bool,
) -> Result<[u8; 32]> {
    let bytes = postcard::to_allocvec(&(manifest, profile, platform, coverage))?;
    Ok(*blake3::hash(&bytes).as_bytes())
}

fn sources_stamp(root: &Path, manifest_paths: &[String]) -> Result<SourcesStamp> {
    let mut manifests = Vec::with_capacity(manifest_paths.len() + 2);
    for rel in manifest_paths {
        let bytes = std::fs::read(root.join(rel))
            .with_context(|| format!("missing graph definition input {rel}"))?;
        manifests.push((rel.clone(), blake3::hash(&bytes).to_hex().to_string()));
    }
    // Root ignore files gate glob expansion, so their content (or absence)
    // is part of the stamp even though it never touches a dir mtime.
    for ignore in [".gitignore", ".frostignore"] {
        if manifest_paths.iter().any(|p| p == ignore) {
            continue;
        }
        let digest = match std::fs::read(root.join(ignore)) {
            Ok(bytes) => blake3::hash(&bytes).to_hex().to_string(),
            Err(_) => "ABSENT".to_string(),
        };
        manifests.push((ignore.to_string(), digest));
    }
    let mut dirs = Vec::new();
    walk_dirs(root, root, &mut dirs)?;
    dirs.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(SourcesStamp { manifests, dirs })
}

/// Mirrors `discover_package_manifests` skip rules so the stamp covers
/// exactly the tree that package discovery and glob expansion can see.
fn walk_dirs(root: &Path, dir: &Path, out: &mut Vec<DirStamp>) -> Result<()> {
    let mut entries = Vec::new();
    let mut child_dirs = Vec::new();
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if matches!(
            entry.file_name().to_str(),
            Some(".git" | ".frost" | "target")
        ) {
            continue;
        }
        let ty = entry.file_type()?;
        let kind = if ty.is_symlink() {
            b'l'
        } else if ty.is_dir() {
            b'd'
        } else if ty.is_file() {
            b'f'
        } else {
            b'o'
        };
        // A retargeted symlink keeps its name and kind; its target is what
        // a glob that follows it would see change.
        let target = if ty.is_symlink() {
            std::fs::read_link(entry.path()).ok()
        } else {
            None
        };
        entries.push((entry.file_name(), kind, target));
        if ty.is_dir() && !ty.is_symlink() {
            child_dirs.push(entry.path());
        }
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0).then(left.1.cmp(&right.1)));
    let mut hasher = blake3::Hasher::new();
    for (name, kind, target) in entries {
        hash_os_str(&mut hasher, &name);
        hasher.update(&[kind]);
        if let Some(target) = target {
            hash_os_str(&mut hasher, target.as_os_str());
        }
    }
    let path = dir
        .strip_prefix(root)
        .unwrap()
        .to_string_lossy()
        .replace('\\', "/");
    out.push(DirStamp {
        path,
        entries_hash: *hasher.finalize().as_bytes(),
    });
    child_dirs.sort();
    for child in child_dirs {
        walk_dirs(root, &child, out)?;
    }
    Ok(())
}

#[cfg(unix)]
fn hash_os_str(hasher: &mut blake3::Hasher, value: &OsStr) {
    use std::os::unix::ffi::OsStrExt;
    let bytes = value.as_bytes();
    hasher.update(&(bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
}

#[cfg(windows)]
fn hash_os_str(hasher: &mut blake3::Hasher, value: &OsStr) {
    use std::os::windows::ffi::OsStrExt;
    let units: Vec<u16> = value.encode_wide().collect();
    hasher.update(&(units.len() as u64).to_le_bytes());
    for unit in units {
        hasher.update(&unit.to_le_bytes());
    }
}

#[cfg(not(any(unix, windows)))]
fn hash_os_str(hasher: &mut blake3::Hasher, value: &OsStr) {
    let value = value.to_string_lossy();
    hasher.update(&(value.len() as u64).to_le_bytes());
    hasher.update(value.as_bytes());
}

struct ParsedStore<'a> {
    fingerprint: &'a [u8],
    stamp: SourcesStamp,
    payload_digest: &'a [u8],
    payload: &'a [u8],
}

/// BLAKE3 of the serialized graph. Large payloads hash in parallel so the
/// check stays small beside decoding them.
fn payload_digest(payload: &[u8]) -> [u8; 32] {
    let mut hasher = blake3::Hasher::new();
    if payload.len() >= 1 << 20 {
        hasher.update_rayon(payload);
    } else {
        hasher.update(payload);
    }
    *hasher.finalize().as_bytes()
}

fn ensure_payload_intact(parsed: &ParsedStore<'_>) -> Result<()> {
    anyhow::ensure!(
        payload_digest(parsed.payload) == parsed.payload_digest,
        "graph store payload does not match its digest"
    );
    Ok(())
}

fn parse_header(bytes: &[u8]) -> Result<ParsedStore<'_>> {
    anyhow::ensure!(
        bytes.len() >= 48 && &bytes[..8] == MAGIC,
        "invalid graph header"
    );
    let version = u32::from_le_bytes(bytes[8..12].try_into().unwrap());
    anyhow::ensure!(version == VERSION, "unsupported graph version");
    let stamp_len = u32::from_le_bytes(bytes[44..48].try_into().unwrap()) as usize;
    anyhow::ensure!(bytes.len() >= 48 + stamp_len, "truncated graph header");
    let stamp: SourcesStamp =
        postcard::from_bytes(&bytes[48..48 + stamp_len]).context("corrupt sources stamp")?;
    let payload_start = 48 + stamp_len + 32;
    anyhow::ensure!(bytes.len() >= payload_start, "truncated graph header");
    Ok(ParsedStore {
        fingerprint: &bytes[12..44],
        stamp,
        payload_digest: &bytes[48 + stamp_len..payload_start],
        payload: &bytes[payload_start..],
    })
}

/// Loads a stored graph, validated either against a manifest fingerprint
/// (fallback path, manifest already parsed) or against a freshly computed
/// sources stamp (warm path, no manifest parse).
fn load_graph(
    path: &Path,
    fingerprint: Option<&[u8; 32]>,
    stamp_root: Option<&Path>,
) -> Result<BuildGraph> {
    let file = File::open(path)?;
    // SAFETY: the mapping is read-only and `file` remains alive until mapping creation.
    let mmap = unsafe { Mmap::map(&file)? };
    let parsed = parse_header(&mmap)?;
    if let Some(fingerprint) = fingerprint {
        anyhow::ensure!(parsed.fingerprint == fingerprint, "stale graph store");
    }
    if let Some(root) = stamp_root {
        crate::phases::time("graph.sources_stamp", || {
            ensure_sources_current(root, &parsed.stamp)
        })?;
    }
    ensure_payload_intact(&parsed)?;
    crate::phases::time("graph.decode", || postcard::from_bytes(parsed.payload))
        .context("corrupt graph store")
}

fn validate_cached_sources(path: &Path, root: &Path) -> Result<[u8; 32]> {
    let file = File::open(path)?;
    // SAFETY: the mapping is read-only and `file` remains alive until mapping creation.
    let mmap = unsafe { Mmap::map(&file)? };
    let parsed = parse_header(&mmap)?;
    ensure_sources_current(root, &parsed.stamp)?;
    Ok(parsed
        .fingerprint
        .try_into()
        .expect("graph header fingerprint is always 32 bytes"))
}

fn ensure_sources_current(root: &Path, stamp: &SourcesStamp) -> Result<()> {
    // Ignore-file entries are re-added by sources_stamp (and may be ABSENT);
    // only real manifests are required to exist.
    let manifest_paths: Vec<String> = stamp
        .manifests
        .iter()
        .map(|(p, _)| p.clone())
        .filter(|p| p != ".gitignore" && p != ".frostignore")
        .collect();
    let current = sources_stamp(root, &manifest_paths)?;
    anyhow::ensure!(current == *stamp, "workspace sources changed");
    Ok(())
}

fn save_graph(
    root: &Path,
    path: &Path,
    fingerprint: &[u8; 32],
    manifest_paths: &[String],
    graph: &BuildGraph,
) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let stamp = sources_stamp(root, manifest_paths)?;
    let stamp_bytes = postcard::to_allocvec(&stamp)?;
    let payload = postcard::to_allocvec(graph)?;
    let tmp = path.with_extension("bin.tmp");
    let mut file = File::create(&tmp)?;
    file.write_all(MAGIC)?;
    file.write_all(&VERSION.to_le_bytes())?;
    file.write_all(fingerprint)?;
    file.write_all(&(stamp_bytes.len() as u32).to_le_bytes())?;
    file.write_all(&stamp_bytes)?;
    file.write_all(&payload_digest(&payload))?;
    file.write_all(&payload)?;
    file.flush()?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::MANIFEST_FILE;

    fn workspace(name: &str) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("frost-graph-store-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn version_mismatch_falls_back_to_recompile() {
        let root = workspace("ver");
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='cc_binary'\nsrcs=['a.c']\n",
        )
        .unwrap();
        let manifest = Manifest::load(&root).unwrap();
        let graph = GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();
        assert_eq!(graph.actions.len(), 2);
        std::fs::write(root.join(".frost/graph-debug.bin"), b"bad").unwrap();
        assert_eq!(
            GraphStore::load_or_compile(&root, &manifest, "debug")
                .unwrap()
                .actions
                .len(),
            2
        );
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_damaged_payload_is_recompiled_rather_than_decoded() {
        let root = workspace("payload");
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='genrule'\ncmd='printf ok > ${out}'\noutputs=['a.txt']\n",
        )
        .unwrap();
        let manifest = Manifest::load(&root).unwrap();
        let graph = GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();
        let path = store_path(&root, "debug", HOST_PLATFORM);
        let pristine = std::fs::read(&path).unwrap();
        let command = graph.actions[0].argv.join(" ");
        assert!(command.contains("printf ok"), "{command}");

        // Every single-bit flip in the payload: the ones that still decode are
        // exactly the ones that would otherwise build something else.
        let payload_start = pristine.len() - postcard::to_allocvec(&graph).unwrap().len();
        for position in payload_start..pristine.len() {
            let mut damaged = pristine.clone();
            damaged[position] ^= 0x04;
            std::fs::write(&path, &damaged).unwrap();
            assert!(
                GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none(),
                "byte {position}: a damaged payload was served from the warm path"
            );
            assert!(GraphStore::validate_bytes(&damaged).is_err());
            let recompiled = GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();
            assert_eq!(recompiled.actions[0].argv.join(" "), command);
            // The recompile rewrote a sound store.
            assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_some());
        }
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn warm_path_hits_without_manifest_and_misses_on_change() {
        let root = workspace("warm");
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='cc_binary'\nsrcs=['a.c']\n",
        )
        .unwrap();
        assert!(
            GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none(),
            "no store yet"
        );
        let manifest = Manifest::load(&root).unwrap();
        GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();

        assert!(GraphStore::cached_sources_current(
            &root,
            "debug",
            HOST_PLATFORM
        ));
        let cached = load_graph(
            &store_path(&root, "debug", HOST_PLATFORM),
            None,
            Some(&root),
        )
        .expect("warm hit after save");
        assert_eq!(cached.actions.len(), 2);
        // The manifest declares no driver, so this is the host default.
        assert_eq!(cached.toolchain.cc, crate::manifest::default_cc());

        // Manifest edit invalidates the warm path.
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='cc_binary'\nsrcs=['a.c']\ncflags=['-O2']\n",
        )
        .unwrap();
        assert!(!GraphStore::cached_sources_current(
            &root,
            "debug",
            HOST_PLATFORM
        ));
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn warm_path_misses_when_directories_change() {
        let root = workspace("dirs");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='cc_binary'\nsrcs=['a.c']\n",
        )
        .unwrap();
        let manifest = Manifest::load(&root).unwrap();
        GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_some());

        // Adding a file changes the listing → warm miss (globs and package
        // discovery may see a different tree).
        std::fs::write(root.join("src/new.c"), "int x;").unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn a_directory_touched_without_a_listing_change_stays_warm() {
        let root = workspace("touched");
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("src/a.c"), "int a;").unwrap();
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='cc_binary'\nsrcs=['src/*.c']\n",
        )
        .unwrap();
        let manifest = Manifest::load(&root).unwrap();
        GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();

        // Save-by-rename and a temporary file that comes and goes both move
        // the directory's mtime and leave the listing as it was.
        std::fs::write(root.join("src/a.c.tmp"), "int a = 1;").unwrap();
        std::fs::rename(root.join("src/a.c.tmp"), root.join("src/a.c")).unwrap();
        std::fs::write(root.join("scratch"), "").unwrap();
        std::fs::remove_file(root.join("scratch")).unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_some());

        // Removing a file a glob matched is a different tree.
        std::fs::write(root.join("src/b.c"), "int b;").unwrap();
        GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_some());
        std::fs::remove_file(root.join("src/b.c")).unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none());
        GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();

        // A kind change under the same name is a different tree.
        std::fs::remove_file(root.join("src/a.c")).unwrap();
        std::fs::create_dir(root.join("src/a.c")).unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none());
        std::fs::remove_dir_all(root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_retargeted_symlink_is_a_different_tree() {
        let root = workspace("symlink");
        std::fs::create_dir_all(root.join("one")).unwrap();
        std::fs::create_dir_all(root.join("two")).unwrap();
        std::os::unix::fs::symlink("one", root.join("link")).unwrap();
        std::fs::write(
            root.join(MANIFEST_FILE),
            "[target.a]\nkind='cc_binary'\nsrcs=['a.c']\n",
        )
        .unwrap();
        let manifest = Manifest::load(&root).unwrap();
        GraphStore::load_or_compile(&root, &manifest, "debug").unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_some());

        std::fs::remove_file(root.join("link")).unwrap();
        std::os::unix::fs::symlink("two", root.join("link")).unwrap();
        assert!(GraphStore::load_cached(&root, "debug", HOST_PLATFORM).is_none());
        std::fs::remove_dir_all(root).ok();
    }
}
