use std::collections::BTreeMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Result of one action execution, recorded for incremental rebuilds.
/// This is the constructive-trace store: an action whose key digest matches
/// its journal entry, and whose recorded outputs are intact on disk, can be
/// skipped without running (which also yields early cutoff for downstream
/// actions, because their keys are computed from output *content* hashes).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JournalEntry {
    /// Action key digest at the time of the recorded run, including
    /// discovered (depfile) inputs.
    pub key: String,
    /// path -> content digest of every input that fed the key.
    pub inputs: BTreeMap<String, String>,
    /// Inputs discovered from the depfile (subset of `inputs` keys).
    pub discovered: Vec<String>,
    /// path -> content digest of every declared output after the run.
    pub outputs: BTreeMap<String, String>,
    pub duration_ms: u64,
    #[serde(default)]
    pub reason: String,
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Journal {
    /// Keyed by stable action id (e.g. `compile:app:src/main.c`).
    pub actions: BTreeMap<String, JournalEntry>,
    /// Reused by the execution engine so a 10k-action build does not
    /// open/close the same append-only file 10k times. Each record is still
    /// flushed before the action is reported complete, preserving crash-tail
    /// recovery.
    #[serde(skip)]
    writer: Option<(PathBuf, std::fs::File)>,
    /// The file as [`Self::load`] found it, so the first append can discard
    /// an unreadable tail without decoding the journal a second time.
    #[serde(skip)]
    loaded: Option<Extent>,
}

/// How much of a journal file was readable when it was loaded.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Extent {
    file_len: u64,
    /// End of the last record that decoded. Everything after it — a frame
    /// torn by a crash mid-append, or bytes damaged on disk — is unreadable,
    /// and so is anything appended after it.
    valid_len: u64,
}

pub const JOURNAL_REL_PATH: &str = ".frost/journal.bin";
const LEGACY_JOURNAL_REL_PATH: &str = ".frost/journal.json";
/// Version 2 adds a checksum to every record. A journal is append-only and
/// never rewritten in place, so damage is found per record or not at all; and
/// a record that still decodes after a flipped bit can name a different file
/// in an owned output tree, which restoration would then write.
const MAGIC: &[u8; 8] = b"FRSTJR02";
/// Bytes of BLAKE3 over each record's payload kept in its frame.
const CHECK_LEN: usize = 8;

#[derive(Debug, Serialize, Deserialize)]
struct Record {
    id: String,
    entry: JournalEntry,
}

impl Journal {
    pub fn load(workspace_root: &Path) -> Self {
        let path = workspace_root.join(JOURNAL_REL_PATH);
        let mut actions = BTreeMap::new();
        let mut loaded = None;
        if let Ok(mut file) = std::fs::File::open(&path) {
            let mut bytes = Vec::new();
            if file.read_to_end(&mut bytes).is_ok() {
                let valid_len;
                (actions, valid_len) = decode_prefix(&bytes);
                loaded = Some(Extent {
                    file_len: bytes.len() as u64,
                    valid_len: valid_len as u64,
                });
            }
        }
        if actions.is_empty() {
            let legacy = workspace_root.join(LEGACY_JOURNAL_REL_PATH);
            actions = std::fs::read_to_string(&legacy)
                .ok()
                .and_then(|text| serde_json::from_str(&text).ok())
                .unwrap_or_default();
        }
        Self {
            actions,
            writer: None,
            loaded,
        }
    }

    /// An empty journal for recording one build's results into the file
    /// this one was loaded from.
    ///
    /// Recording starts empty — the engine keeps the loaded entries apart —
    /// but it inherits what the load learned about the file, so an
    /// unreadable tail is cut before the first append rather than found by a
    /// second decode.
    pub fn recorder(&self) -> Self {
        Self {
            actions: BTreeMap::new(),
            writer: None,
            loaded: self.loaded,
        }
    }

    /// Append one completed action. A torn final frame is ignored on load.
    pub fn record(&mut self, workspace_root: &Path, id: String, entry: JournalEntry) -> Result<()> {
        let path = workspace_root.join(JOURNAL_REL_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let reuse = self
            .writer
            .as_ref()
            .is_some_and(|(writer_path, _)| writer_path == &path);
        if !reuse {
            // Append only behind a header this version can read. A journal
            // left by another version holds no records this build can decode,
            // so appending after it would make every future load see the same
            // unreadable header and rebuild from scratch forever. Replacing it
            // costs exactly one cold build. A torn final frame keeps its valid
            // magic and is still appended to; the decoder drops the tear.
            let appendable = std::fs::File::open(&path)
                .ok()
                .and_then(|mut file| {
                    let mut header = [0u8; MAGIC.len()];
                    file.read_exact(&mut header).ok()?;
                    Some(&header == MAGIC)
                })
                .unwrap_or(false);
            let file = if appendable {
                let file = std::fs::OpenOptions::new().append(true).open(&path)?;
                // Records appended after an unreadable tail are unreadable
                // too: the decoder stops at the first frame it cannot parse,
                // so every build would redo the work it just recorded,
                // forever. Cut the tail back to the last whole record first.
                let len = file.metadata()?.len();
                let valid_len = match self.loaded {
                    Some(extent) if extent.file_len == len => extent.valid_len,
                    _ => decode_prefix(&std::fs::read(&path)?).1 as u64,
                };
                if valid_len < len {
                    file.set_len(valid_len)?;
                }
                file
            } else {
                let mut file = std::fs::File::create(&path)?;
                file.write_all(MAGIC)?;
                file
            };
            self.writer = Some((path.clone(), file));
        }
        let file = &mut self.writer.as_mut().unwrap().1;
        let payload = postcard::to_allocvec(&Record {
            id: id.clone(),
            entry: entry.clone(),
        })?;
        // One write per record, so a crash tears at most the final frame.
        file.write_all(&frame(&payload))?;
        file.flush()?;
        self.actions.insert(id, entry);
        Ok(())
    }

    pub fn save(&self, workspace_root: &Path) -> Result<()> {
        let path = workspace_root.join(JOURNAL_REL_PATH);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let tmp = path.with_extension("bin.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(MAGIC)?;
        for (id, entry) in &self.actions {
            let payload = postcard::to_allocvec(&Record {
                id: id.clone(),
                entry: entry.clone(),
            })?;
            file.write_all(&frame(&payload))?;
        }
        file.flush()?;
        std::fs::rename(&tmp, &path)
            .with_context(|| format!("failed to persist {}", path.display()))?;
        Ok(())
    }
}

/// Total decoder used by startup and fuzzing. Invalid/torn data yields the
/// prefix of fully validated records, never a panic or false record.
pub fn decode_bytes(bytes: &[u8]) -> BTreeMap<String, JournalEntry> {
    decode_prefix(bytes).0
}

/// `len: u32 LE`, the payload's checksum, then the payload.
fn frame(payload: &[u8]) -> Vec<u8> {
    let mut frame = Vec::with_capacity(4 + CHECK_LEN + payload.len());
    frame.extend_from_slice(&(payload.len() as u32).to_le_bytes());
    frame.extend_from_slice(&blake3::hash(payload).as_bytes()[..CHECK_LEN]);
    frame.extend_from_slice(payload);
    frame
}

/// [`decode_bytes`], plus where the readable prefix ends: after the magic and
/// the last record that decoded, or 0 when the magic itself is not this
/// version's.
fn decode_prefix(bytes: &[u8]) -> (BTreeMap<String, JournalEntry>, usize) {
    let mut actions = BTreeMap::new();
    if !bytes.starts_with(MAGIC) {
        return (actions, 0);
    }
    let mut cursor = MAGIC.len();
    while cursor + 4 + CHECK_LEN <= bytes.len() {
        let len = u32::from_le_bytes(bytes[cursor..cursor + 4].try_into().unwrap()) as usize;
        let check = &bytes[cursor + 4..cursor + 4 + CHECK_LEN];
        let start = cursor + 4 + CHECK_LEN;
        let Some(end) = start.checked_add(len) else {
            break;
        };
        if end > bytes.len() {
            break;
        }
        let payload = &bytes[start..end];
        if blake3::hash(payload).as_bytes()[..CHECK_LEN] != *check {
            break;
        }
        match postcard::from_bytes::<Record>(payload) {
            Ok(record) => {
                actions.insert(record.id, record.entry);
            }
            Err(_) => break,
        }
        cursor = end;
    }
    (actions, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let dir = std::env::temp_dir().join(format!("frost-journal-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();

        let mut journal = Journal::default();
        journal.actions.insert(
            "compile:app:src/main.c".into(),
            JournalEntry {
                key: "abc".into(),
                inputs: BTreeMap::from([("src/main.c".into(), "h1".into())]),
                discovered: vec!["include/util.h".into()],
                outputs: BTreeMap::from([(".frost/obj/app/src/main.c.o".into(), "h2".into())]),
                duration_ms: 12,
                reason: "input changed: src/main.c".into(),
            },
        );
        journal.save(&dir).unwrap();

        let loaded = Journal::load(&dir);
        assert_eq!(loaded.actions.len(), 1);
        assert_eq!(loaded.actions["compile:app:src/main.c"].key, "abc");

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn missing_or_corrupt_journal_loads_empty() {
        let dir =
            std::env::temp_dir().join(format!("frost-journal-corrupt-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".frost")).unwrap();
        assert!(Journal::load(&dir).actions.is_empty());
        std::fs::write(
            dir.join(JOURNAL_REL_PATH),
            [&MAGIC[..], b"\xff\xff"].concat(),
        )
        .unwrap();
        assert!(Journal::load(&dir).actions.is_empty());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn incomplete_tail_preserves_completed_records() {
        let dir = std::env::temp_dir().join(format!("frost-journal-tail-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let entry = JournalEntry {
            key: "k".into(),
            inputs: BTreeMap::new(),
            discovered: Vec::new(),
            outputs: BTreeMap::new(),
            duration_ms: 1,
            reason: "first".into(),
        };
        let mut journal = Journal::default();
        journal.record(&dir, "a".into(), entry).unwrap();
        let mut file = std::fs::OpenOptions::new()
            .append(true)
            .open(dir.join(JOURNAL_REL_PATH))
            .unwrap();
        file.write_all(&100u32.to_le_bytes()).unwrap();
        file.write_all(b"partial").unwrap();
        assert!(Journal::load(&dir).actions.contains_key("a"));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn records_appended_after_an_unreadable_tail_stay_readable() {
        let dir =
            std::env::temp_dir().join(format!("frost-journal-reappend-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = |reason: &str| JournalEntry {
            key: "k".into(),
            inputs: BTreeMap::new(),
            discovered: Vec::new(),
            outputs: BTreeMap::new(),
            duration_ms: 1,
            reason: reason.into(),
        };
        let mut first = Journal::default();
        first.record(&dir, "a".into(), entry("first")).unwrap();
        drop(first);
        let path = dir.join(JOURNAL_REL_PATH);

        // A torn frame (a crash mid-append), then a whole frame whose payload
        // does not decode (damage on disk). Both end the readable prefix.
        for tail in [
            [&100u32.to_le_bytes()[..], &b"partial"[..]].concat(),
            [
                &3u32.to_le_bytes()[..],
                &[0u8; CHECK_LEN][..],
                &b"\xff\xff\xff"[..],
            ]
            .concat(),
        ] {
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.extend_from_slice(&tail);
            std::fs::write(&path, &bytes).unwrap();

            // The engine's path: load, then record through the loaded extent.
            let loaded = Journal::load(&dir);
            assert!(loaded.actions.contains_key("a"));
            let mut recorder = loaded.recorder();
            recorder
                .record(&dir, "b".into(), entry("after load"))
                .unwrap();
            drop(recorder);
            let reloaded = Journal::load(&dir);
            assert!(reloaded.actions.contains_key("a"));
            assert!(
                reloaded.actions.contains_key("b"),
                "a record appended after an unreadable tail was lost"
            );
            assert_eq!(
                reloaded.loaded.unwrap().valid_len,
                reloaded.loaded.unwrap().file_len
            );

            // And a journal recorded into without loading it first.
            let mut bytes = std::fs::read(&path).unwrap();
            bytes.extend_from_slice(&tail);
            std::fs::write(&path, &bytes).unwrap();
            let mut fresh = Journal::default();
            fresh.record(&dir, "c".into(), entry("unloaded")).unwrap();
            drop(fresh);
            let reloaded = Journal::load(&dir);
            assert!(["a", "b", "c"]
                .iter()
                .all(|id| reloaded.actions.contains_key(*id)));
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_record_damaged_in_place_ends_the_readable_prefix() {
        let dir = std::env::temp_dir().join(format!("frost-journal-flip-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let entry = |output: &str| JournalEntry {
            key: "k".into(),
            inputs: BTreeMap::new(),
            discovered: Vec::new(),
            outputs: BTreeMap::from([(output.to_string(), "d".into())]),
            duration_ms: 1,
            reason: String::new(),
        };
        let mut journal = Journal::default();
        journal
            .record(&dir, "a".into(), entry("tree/f1.txt"))
            .unwrap();
        journal
            .record(&dir, "b".into(), entry("tree/f2.txt"))
            .unwrap();
        drop(journal);
        let path = dir.join(JOURNAL_REL_PATH);
        let pristine = std::fs::read(&path).unwrap();

        // `f1` -> `f7`: still a valid record, naming a different file. Every
        // single-bit flip anywhere in a record must be refused instead.
        let at = pristine.windows(2).position(|w| w == b"f1").unwrap() + 1;
        let mut flipped = pristine.clone();
        flipped[at] = b'7';
        let loaded = decode_bytes(&flipped);
        assert!(!loaded.contains_key("a"), "a damaged record was decoded");
        assert!(loaded
            .values()
            .all(|e| !e.outputs.contains_key("tree/f7.txt")));
        for position in MAGIC.len()..pristine.len() {
            for bit in 0..8 {
                let mut damaged = pristine.clone();
                damaged[position] ^= 1 << bit;
                let decoded = decode_bytes(&damaged);
                for (id, entry) in &decoded {
                    let original = &decode_bytes(&pristine)[id];
                    assert_eq!(entry.outputs, original.outputs, "byte {position} bit {bit}");
                    assert_eq!(entry.key, original.key, "byte {position} bit {bit}");
                }
            }
        }
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn a_foreign_journal_is_replaced_so_the_next_build_is_warm_again() {
        let dir =
            std::env::temp_dir().join(format!("frost-journal-foreign-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join(".frost")).unwrap();
        let mut foreign = b"FRSTJR99".to_vec();
        foreign.extend_from_slice(&[0x11; 32]);
        std::fs::write(dir.join(JOURNAL_REL_PATH), &foreign).unwrap();

        let entry = JournalEntry {
            key: "k".into(),
            inputs: BTreeMap::new(),
            discovered: Vec::new(),
            outputs: BTreeMap::new(),
            duration_ms: 1,
            reason: "after a foreign journal".into(),
        };
        let mut journal = Journal::default();
        journal.record(&dir, "a".into(), entry).unwrap();

        // Appending behind a header this version cannot read would make every
        // later load see the same unreadable header, so an unrecognized
        // journal would cost a cold build forever instead of once.
        assert!(
            Journal::load(&dir).actions.contains_key("a"),
            "a record written after a foreign journal was not readable back"
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
