//! `frost coverage-lcov`: one run's gcov data, merged into an lcov tracefile.
//!
//! The format itself is [`frostbuild_core::coverage`]; what is here is getting
//! gcov to hand its data over. That is most of the work, because gcov wants the
//! `.gcda` and its `.gcno` side by side and a coverage build does not put them
//! there.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result};
use frostbuild_core::coverage::{to_lcov, GcovReport};

/// Merge one run's gcov data into an lcov tracefile.
///
/// The notes file is written by the compile and lives in the object tree,
/// while the counters are relocated by `GCOV_PREFIX` into a directory that can
/// be reset before every run — which is what keeps the numbers from
/// accumulating across executions and the tracefile from differing for a build
/// that did not change. gcov reads them as a pair, so the notes are staged
/// beside the counters here rather than the counters being left where gcc
/// wants them, in a tree that holds declared outputs and cannot be cleared.
///
/// `--json-format --stdout` rather than gcov's default: the default writes
/// `.gcov.json.gz`, and reading that would mean a gzip decoder for data gcov is
/// willing to hand over uncompressed.
pub(crate) fn run_coverage_lcov(
    root: &Path,
    gcda: &Path,
    objects: &Path,
    output: &Path,
    gcov: &str,
) -> Result<i32> {
    let gcda_dir = root.join(gcda);
    let objects_dir = root.join(objects);

    let mut counters = files_under(&gcda_dir)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "gcda"))
        .collect::<Vec<_>>();
    // Sorted so two runs read the same data in the same order. `to_lcov` sorts
    // its records too, but a stable read order keeps a failure reproducible
    // rather than depending on directory iteration.
    counters.sort();

    let notes: BTreeMap<OsString, PathBuf> = files_under(&objects_dir)
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "gcno"))
        .filter_map(|path| Some((path.file_stem()?.to_os_string(), path)))
        .collect();

    let mut reports = Vec::new();
    for counter in &counters {
        let Some(stem) = counter.file_stem() else {
            continue;
        };
        // A counter with no notes file is a compile that did not happen in
        // this configuration. Skipping is right and silence is not, because
        // the tracefile would otherwise be quietly short.
        let Some(note) = notes.get(stem) else {
            eprintln!(
                "frost: coverage: no .gcno for {}; was it compiled with coverage?",
                counter.display()
            );
            continue;
        };
        let staged = counter.with_extension("gcno");
        if staged != *note {
            std::fs::copy(note, &staged)
                .with_context(|| format!("staging {} beside its counters", note.display()))?;
        }
        let out = Command::new(gcov)
            .args(["--json-format", "--stdout"])
            .arg(counter)
            .current_dir(&gcda_dir)
            .output()
            .with_context(|| format!("running {gcov}"))?;
        if !out.status.success() {
            anyhow::bail!(
                "{gcov} failed on {}: {}",
                counter.display(),
                String::from_utf8_lossy(&out.stderr).trim()
            );
        }
        // A stream rather than one document: gcov prints one JSON object per
        // input it was given, concatenated, and `from_str` rejects the second
        // one as trailing characters.
        let stdout = String::from_utf8_lossy(&out.stdout);
        for report in serde_json::Deserializer::from_str(&stdout).into_iter::<GcovReport>() {
            reports.push(
                report
                    .with_context(|| format!("reading {gcov} output for {}", counter.display()))?,
            );
        }
    }

    let lcov = to_lcov(&reports, root);
    if lcov.is_empty() {
        // An empty tracefile reports 0% and looks like a result. Refusing says
        // the measurement did not happen, which is a different thing. This
        // also catches the case where gcov reported only files outside the
        // workspace, which `to_lcov` drops.
        anyhow::bail!(
            "no coverage data under {}: the tests either did not run or were \
             not built with coverage",
            gcda_dir.display()
        );
    }
    let destination = root.join(output);
    if let Some(parent) = destination.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&destination, &lcov)
        .with_context(|| format!("writing {}", destination.display()))?;
    println!("frost: wrote {}", destination.display());
    Ok(0)
}

/// Every file under `dir`, or nothing when it does not exist.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(current) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&current) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            match path.is_dir() {
                true => stack.push(path),
                false => found.push(path),
            }
        }
    }
    found
}
