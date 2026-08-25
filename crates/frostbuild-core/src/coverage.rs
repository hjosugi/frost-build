//! gcov's JSON report, turned into lcov.
//!
//! Frost emits lcov itself rather than shelling out to `lcov` or `gcovr`.
//! Neither is present on the CI images this repository builds on, and both
//! bring a Perl or Python dependency into anyone's build image for a format
//! that is six line types wide. `gcov --json-format` already reports per-file,
//! per-line hit counts, which map onto `SF:` / `DA:` / `LF:` / `LH:` directly.
//!
//! # Determinism
//!
//! `frost test --coverage` promises byte-identical lcov for identical input,
//! and two things here would break that if they were left to chance:
//!
//! - **Ordering.** gcov reports files and lines in whatever order it walked
//!   them. Everything is sorted on the way out.
//! - **Absolute paths.** gcov reports paths relative to the directory the
//!   compiler ran in, and `current_working_directory` is an absolute path that
//!   differs per machine and per checkout. `SF:` is emitted workspace-relative
//!   so the same sources produce the same bytes anywhere.
//!
//! The third thing that would break it is not solved here: `.gcda` counters
//! *accumulate* across executions, so running a test twice reports 2 where one
//! run reports 1. That is a property of gcov's data model, not of the report,
//! and it is handled by putting the `.gcda` tree in the action's `clean_dirs`
//! so every execution — including a `--check-determinism` rerun — starts from
//! zero.

use std::collections::BTreeMap;

use serde::Deserialize;

/// One file in a `gcov --json-format` report.
#[derive(Debug, Deserialize)]
pub struct GcovFile {
    pub file: String,
    #[serde(default)]
    pub lines: Vec<GcovLine>,
}

#[derive(Debug, Deserialize)]
pub struct GcovLine {
    pub line_number: u32,
    pub count: u64,
}

/// A whole `.gcov.json` document. Only the fields lcov needs are read; gcov
/// also reports `gcc_version`, `format_version` and branch detail, none of
/// which lcov's line coverage uses.
#[derive(Debug, Deserialize)]
pub struct GcovReport {
    #[serde(default)]
    pub files: Vec<GcovFile>,
    /// The directory gcov resolved relative paths against.
    #[serde(default)]
    pub current_working_directory: String,
}

/// Collect the counters under `gcda_dirs` and render them as one lcov
/// tracefile, using `notes` to find each counter's `.gcno`.
///
/// gcov reads the two as a pair and a coverage build does not put them
/// together: the notes file is written by the compile into the object tree,
/// while the counters are relocated by `GCOV_PREFIX` into a directory that can
/// be emptied before every run — which is what stops them accumulating and the
/// tracefile from differing for a build that did not change. So the notes are
/// staged beside the counters rather than the counters being left where gcc
/// wants them, in a tree that holds declared outputs and cannot be cleared.
///
/// `--json-format --stdout` rather than gcov's default: the default writes
/// `.gcov.json.gz`, and reading that would mean a gzip decoder for data gcov is
/// willing to hand over uncompressed.
///
/// A counter with no matching notes file is reported through `warn` and skipped
/// — it is an object that was not compiled in this configuration, and the wrong
/// thing to do is to be quiet about a report that is therefore short.
pub fn merge(
    root: &std::path::Path,
    gcda_dirs: &[std::path::PathBuf],
    notes: &[std::path::PathBuf],
    gcov: &str,
    mut warn: impl FnMut(String),
) -> anyhow::Result<String> {
    use anyhow::Context;

    let mut by_stem = BTreeMap::new();
    for note in notes {
        let Some(stem) = note.file_stem() else {
            continue;
        };
        if let Some(previous) = by_stem.insert(stem.to_os_string(), note) {
            anyhow::bail!(
                "coverage notes {} and {} have the same object name; \
                 instrumented object names must be unique across the linked test",
                previous.display(),
                note.display()
            );
        }
    }

    let mut reports = Vec::new();
    for directory in gcda_dirs {
        let mut counters: Vec<std::path::PathBuf> = files_under(directory)
            .into_iter()
            .filter(|path| path.extension().is_some_and(|ext| ext == "gcda"))
            .collect();
        // Sorted so two runs read the same data in the same order. `to_lcov`
        // sorts its records too, but a stable read order keeps a failure
        // reproducible rather than depending on directory iteration.
        counters.sort();
        for counter in &counters {
            let Some(stem) = counter.file_stem() else {
                continue;
            };
            let Some(note) = by_stem.get(stem) else {
                warn(format!(
                    "no .gcno for {}; was it compiled with coverage?",
                    counter.display()
                ));
                continue;
            };
            let staged = counter.with_extension("gcno");
            if staged != **note {
                std::fs::copy(note, &staged)
                    .with_context(|| format!("staging {} beside its counters", note.display()))?;
            }
            let out = std::process::Command::new(gcov)
                .args(["--json-format", "--stdout"])
                .arg(counter)
                .current_dir(directory)
                .output()
                .with_context(|| format!("running {gcov}"))?;
            if !out.status.success() {
                anyhow::bail!(
                    "{gcov} failed on {}: {}",
                    counter.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            // A stream rather than one document: gcov prints one JSON object
            // per input it was given, concatenated, and `from_str` rejects the
            // second as trailing characters.
            let stdout = String::from_utf8_lossy(&out.stdout);
            for report in serde_json::Deserializer::from_str(&stdout).into_iter::<GcovReport>() {
                reports.push(
                    report.with_context(|| {
                        format!("reading {gcov} output for {}", counter.display())
                    })?,
                );
            }
        }
    }
    Ok(to_lcov(&reports, root))
}

/// Every file under `dir`, or nothing when it does not exist.
fn files_under(dir: &std::path::Path) -> Vec<std::path::PathBuf> {
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

/// Render one or more gcov reports as a single lcov tracefile.
///
/// `root` is the workspace root: every `SF:` is made relative to it, and a
/// file outside it is skipped rather than recorded with an absolute path,
/// because a system header's coverage is not this workspace's to report and
/// its path would differ on every machine.
pub fn to_lcov(reports: &[GcovReport], root: &std::path::Path) -> String {
    // Merged across reports: one test binary's report and another's may both
    // cover a shared library's source, and lcov wants one record per file.
    let mut merged: BTreeMap<String, BTreeMap<u32, u64>> = BTreeMap::new();
    for report in reports {
        let base = std::path::Path::new(&report.current_working_directory);
        for file in &report.files {
            let Some(path) = workspace_relative(&file.file, base, root) else {
                continue;
            };
            let lines = merged.entry(path).or_default();
            for line in &file.lines {
                // gcov reports a line once per function that contributes to
                // it, so an inline or a template body arrives more than once.
                // Summing matches what the counters mean: how many times the
                // line ran, across everything that generated it.
                *lines.entry(line.line_number).or_insert(0) += line.count;
            }
        }
    }

    let mut out = String::new();
    for (path, lines) in &merged {
        out.push_str("TN:\n");
        out.push_str(&format!("SF:{path}\n"));
        for (number, count) in lines {
            out.push_str(&format!("DA:{number},{count}\n"));
        }
        out.push_str(&format!("LF:{}\n", lines.len()));
        out.push_str(&format!(
            "LH:{}\n",
            lines.values().filter(|count| **count > 0).count()
        ));
        out.push_str("end_of_record\n");
    }
    out
}

/// `path` as a workspace-relative, forward-slashed string, or `None` when it
/// is not inside the workspace.
fn workspace_relative(
    path: &str,
    base: &std::path::Path,
    root: &std::path::Path,
) -> Option<String> {
    // Only *directories* are canonicalized, never the file itself.
    //
    // A symlinked root — macOS's `/var/folders/...` is one, and so is any
    // checkout reached through a symlink — has to be resolved on both sides or
    // `strip_prefix` compares `/var/…` against `/private/var/…` and reports
    // every file as outside the workspace. Canonicalizing the joined *file*
    // instead looks equivalent and is not: it silently fails for a source that
    // no longer exists, leaving one side resolved and the other not, so the
    // file is dropped on exactly the platform where the root is a symlink.
    fn real(directory: &std::path::Path) -> std::path::PathBuf {
        directory
            .canonicalize()
            .unwrap_or_else(|_| directory.to_path_buf())
    }

    let candidate = std::path::Path::new(path);
    let absolute = match (
        candidate.is_absolute(),
        candidate.parent(),
        candidate.file_name(),
    ) {
        (true, Some(parent), Some(name)) => real(parent).join(name),
        (true, ..) => candidate.to_path_buf(),
        (false, ..) => real(base).join(candidate),
    };
    let relative = absolute.strip_prefix(real(root)).ok()?;
    Some(relative.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicate_flattened_object_names_are_refused() {
        let root = std::env::temp_dir().join("frost-coverage-duplicate-notes");
        let notes = vec![
            root.join("first/shared.gcno"),
            root.join("second/shared.gcno"),
        ];
        let error = merge(&root, &[], &notes, "gcov", |_| {})
            .unwrap_err()
            .to_string();
        assert!(error.contains("same object name"), "{error}");
        assert!(error.contains("first/shared.gcno"), "{error}");
        assert!(error.contains("second/shared.gcno"), "{error}");
    }

    fn report(cwd: &str, files: &[(&str, &[(u32, u64)])]) -> GcovReport {
        GcovReport {
            current_working_directory: cwd.to_string(),
            files: files
                .iter()
                .map(|(name, lines)| GcovFile {
                    file: (*name).to_string(),
                    lines: lines
                        .iter()
                        .map(|(line_number, count)| GcovLine {
                            line_number: *line_number,
                            count: *count,
                        })
                        .collect(),
                })
                .collect(),
        }
    }

    #[test]
    fn a_gcov_report_becomes_the_six_line_types_lcov_defines() {
        let root = std::env::temp_dir();
        let cwd = root.to_string_lossy().into_owned();
        let lcov = to_lcov(
            &[report(&cwd, &[("m.c", &[(1, 1), (2, 0), (3, 1)])])],
            &root,
        );
        assert_eq!(
            lcov,
            "TN:\nSF:m.c\nDA:1,1\nDA:2,0\nDA:3,1\nLF:3\nLH:2\nend_of_record\n"
        );
    }

    #[test]
    fn ordering_from_gcov_does_not_reach_the_output() {
        // The property the whole file exists for: gcov walks files and lines
        // in an order nobody controls, and `--coverage` promises the same
        // bytes for the same input.
        let root = std::env::temp_dir();
        let cwd = root.to_string_lossy().into_owned();
        let forwards = to_lcov(
            &[report(
                &cwd,
                &[("a.c", &[(1, 1), (9, 1)]), ("b.c", &[(2, 0)])],
            )],
            &root,
        );
        let backwards = to_lcov(
            &[report(
                &cwd,
                &[("b.c", &[(2, 0)]), ("a.c", &[(9, 1), (1, 1)])],
            )],
            &root,
        );
        assert_eq!(forwards, backwards);
    }

    #[test]
    fn a_line_reported_once_per_function_is_summed_not_duplicated() {
        // An inline body arrives once per function that inlined it. Two `DA:`
        // records for one line is not valid lcov, and taking the first would
        // under-report a line that genuinely ran more often.
        let root = std::env::temp_dir();
        let cwd = root.to_string_lossy().into_owned();
        let lcov = to_lcov(&[report(&cwd, &[("m.c", &[(4, 2), (4, 3)])])], &root);
        assert_eq!(lcov, "TN:\nSF:m.c\nDA:4,5\nLF:1\nLH:1\nend_of_record\n");
    }

    #[test]
    fn reports_from_two_test_binaries_merge_into_one_record_per_file() {
        // Two tests linking the same library each report it. lcov wants one
        // record per file, and a line either test covered is covered.
        let root = std::env::temp_dir();
        let cwd = root.to_string_lossy().into_owned();
        let lcov = to_lcov(
            &[
                report(&cwd, &[("lib.c", &[(1, 1), (2, 0)])]),
                report(&cwd, &[("lib.c", &[(1, 0), (2, 4)])]),
            ],
            &root,
        );
        assert_eq!(
            lcov,
            "TN:\nSF:lib.c\nDA:1,1\nDA:2,4\nLF:2\nLH:2\nend_of_record\n"
        );
    }

    #[test]
    fn nothing_outside_the_workspace_is_reported() {
        // A system header's coverage is not this workspace's to report, and
        // its absolute path would differ on every machine — which is the same
        // failure as an unsorted file list, arriving by another route.
        let root = std::env::temp_dir().join("frost-coverage-root");
        std::fs::create_dir_all(&root).unwrap();
        let cwd = root.to_string_lossy().into_owned();
        let lcov = to_lcov(
            &[report(
                &cwd,
                &[("m.c", &[(1, 1)]), ("/usr/include/stdio.h", &[(9, 1)])],
            )],
            &root,
        );
        assert!(lcov.contains("SF:m.c"), "{lcov}");
        assert!(!lcov.contains("stdio.h"), "{lcov}");
        std::fs::remove_dir_all(&root).ok();
    }

    #[cfg(unix)]
    #[test]
    fn a_workspace_reached_through_a_symlink_still_reports_its_own_files() {
        // Where the first version of this failed, and it failed only on the
        // hosts CI runs rather than here: macOS's temp dir is a symlink
        // (`/var/…` -> `/private/var/…`) and Windows canonicalizes to a
        // `\\?\` verbatim path. Canonicalizing the *file* resolved the root
        // but not a source that does not exist on disk, so the two sides no
        // longer shared a prefix and every file was dropped as "outside the
        // workspace" — silently, as an empty report.
        //
        // Reproduced here by building the same asymmetry on purpose, so the
        // platform that has it by nature is no longer the only one that checks.
        let base = std::env::temp_dir().join(format!("frost-cov-{}", std::process::id()));
        let real = base.join("real");
        let link = base.join("link");
        std::fs::create_dir_all(&real).unwrap();
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(&real, &link).unwrap();

        // `link` is the workspace root and `m.c` was never written, exactly as
        // a source deleted since the build would be.
        let lcov = to_lcov(
            &[report(&link.to_string_lossy(), &[("m.c", &[(1, 1)])])],
            &link,
        );
        assert_eq!(
            lcov, "TN:\nSF:m.c\nDA:1,1\nLF:1\nLH:1\nend_of_record\n",
            "a symlinked root dropped the file it contains"
        );
        std::fs::remove_dir_all(&base).ok();
    }

    #[test]
    fn the_shape_gcov_actually_emits_parses() {
        // Field names taken from a real `gcov --json-format` document rather
        // than from its documentation; the extra keys it carries must not stop
        // the report deserializing.
        let document = r#"{
            "format_version": "2",
            "gcc_version": "13.3.0",
            "current_working_directory": "/w",
            "data_file": "m",
            "files": [{
                "file": "m.c",
                "lines": [
                    {"line_number": 1, "count": 1, "function_name": "add",
                     "unexecuted_block": false, "branches": []}
                ],
                "functions": []
            }]
        }"#;
        let parsed: GcovReport = serde_json::from_str(document).expect("gcov document");
        assert_eq!(parsed.current_working_directory, "/w");
        assert_eq!(parsed.files[0].file, "m.c");
        assert_eq!(parsed.files[0].lines[0].line_number, 1);
        assert_eq!(parsed.files[0].lines[0].count, 1);
    }
}
