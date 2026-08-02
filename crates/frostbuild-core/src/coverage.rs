//! Turning gcov's intermediate JSON into lcov tracefile text.
//!
//! frost emits lcov itself rather than shelling out to `lcov` or `gcovr`.
//! Neither is present on a plain toolchain image — `gcov` ships with gcc, the
//! Perl `lcov` does not — so delegating would add a dependency to every CI
//! image that wanted coverage. What is actually needed is a mapping: gcov's
//! `--json-format` output is per-file, per-line hit counts, and an lcov
//! tracefile is per-file, per-line hit counts. The format is a few record
//! types wide, and writing it keeps the dependency list where it is.
//!
//! **Records are sorted, and that is load bearing.** gcov emits files in
//! whatever order it walked them and lines in whatever order it recorded them;
//! a tracefile that inherited either would differ between runs of an unchanged
//! build, and `frost test --coverage` twice must produce byte-identical
//! output. Sorting is what makes that true of the *text*. What makes it true
//! of the *numbers* is separate and lives in the graph: `.gcda` counters
//! accumulate across executions, so the directory they land in is a
//! `clean_dir`, reset before every run including a determinism rerun.

use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Deserialize;

/// One file's worth of gcov JSON, narrowed to the fields a tracefile needs.
///
/// Deliberately not the whole schema: gcov reports functions, branches and
/// demangled names too, and a reader that deserialized all of it would break
/// whenever gcov added a field. Line coverage is what the issue asked for.
#[derive(Debug, Deserialize)]
struct GcovFile {
    file: String,
    #[serde(default)]
    lines: Vec<GcovLine>,
}

#[derive(Debug, Deserialize)]
struct GcovLine {
    line_number: u32,
    count: u64,
}

#[derive(Debug, Deserialize)]
struct GcovDocument {
    #[serde(default)]
    files: Vec<GcovFile>,
}

/// Hit counts per source file, merged across every gcov document given.
///
/// A test's object files each produce their own document, and two of them can
/// report the same header. Summing rather than replacing is what makes the
/// tracefile describe the test rather than whichever document came last.
#[derive(Debug, Default)]
pub struct Coverage {
    files: BTreeMap<String, BTreeMap<u32, u64>>,
}

impl Coverage {
    /// Absorb one `gcov --json-format` document.
    pub fn absorb(&mut self, json: &str) -> Result<()> {
        let document: GcovDocument =
            serde_json::from_str(json).context("gcov JSON is not in the expected shape")?;
        for file in document.files {
            let lines = self.files.entry(file.file).or_default();
            for line in file.lines {
                *lines.entry(line.line_number).or_default() += line.count;
            }
        }
        Ok(())
    }

    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// The lcov tracefile.
    ///
    /// `SF` names the source, `DA` gives a line and its hit count, `LF`/`LH`
    /// are the totals every lcov consumer expects to find rather than compute.
    /// Both maps are ordered, so the text is a function of the counts alone.
    pub fn to_lcov(&self) -> String {
        let mut out = String::new();
        for (file, lines) in &self.files {
            out.push_str(&format!("SF:{file}\n"));
            for (line, count) in lines {
                out.push_str(&format!("DA:{line},{count}\n"));
            }
            let hit = lines.values().filter(|count| **count > 0).count();
            out.push_str(&format!("LF:{}\n", lines.len()));
            out.push_str(&format!("LH:{hit}\n"));
            out.push_str("end_of_record\n");
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ONE: &str = r#"{"files":[{"file":"src/a.c","lines":[
        {"line_number":2,"count":1},{"line_number":1,"count":3}]}]}"#;

    #[test]
    fn a_document_becomes_one_record_per_file() {
        let mut coverage = Coverage::default();
        coverage.absorb(ONE).unwrap();
        assert_eq!(
            coverage.to_lcov(),
            "SF:src/a.c\nDA:1,3\nDA:2,1\nLF:2\nLH:2\nend_of_record\n"
        );
    }

    #[test]
    fn lines_and_files_come_out_sorted_whatever_order_gcov_used() {
        // The determinism criterion is about the text as much as the numbers:
        // gcov walks files in directory order, which is not stable between
        // machines, and a tracefile that inherited it would differ for a build
        // that did not.
        let mut forward = Coverage::default();
        forward
            .absorb(
                r#"{"files":[{"file":"b.c","lines":[{"line_number":1,"count":1}]},
                                 {"file":"a.c","lines":[{"line_number":1,"count":1}]}]}"#,
            )
            .unwrap();
        let mut reverse = Coverage::default();
        reverse
            .absorb(
                r#"{"files":[{"file":"a.c","lines":[{"line_number":1,"count":1}]},
                                 {"file":"b.c","lines":[{"line_number":1,"count":1}]}]}"#,
            )
            .unwrap();
        assert_eq!(forward.to_lcov(), reverse.to_lcov());
        assert!(forward.to_lcov().starts_with("SF:a.c\n"));
    }

    #[test]
    fn the_same_line_seen_twice_is_summed_rather_than_replaced() {
        // Two object files can report the same header. Replacing would make
        // the tracefile describe whichever document happened to come last.
        let mut coverage = Coverage::default();
        coverage
            .absorb(r#"{"files":[{"file":"h.h","lines":[{"line_number":4,"count":2}]}]}"#)
            .unwrap();
        coverage
            .absorb(r#"{"files":[{"file":"h.h","lines":[{"line_number":4,"count":5}]}]}"#)
            .unwrap();
        assert!(
            coverage.to_lcov().contains("DA:4,7"),
            "{}",
            coverage.to_lcov()
        );
    }

    #[test]
    fn an_uncovered_line_counts_toward_found_but_not_hit() {
        let mut coverage = Coverage::default();
        coverage
            .absorb(
                r#"{"files":[{"file":"a.c","lines":[
                    {"line_number":1,"count":0},{"line_number":2,"count":9}]}]}"#,
            )
            .unwrap();
        let lcov = coverage.to_lcov();
        assert!(lcov.contains("LF:2\n"), "{lcov}");
        assert!(lcov.contains("LH:1\n"), "{lcov}");
    }

    #[test]
    fn fields_gcov_adds_later_do_not_break_the_reader() {
        // gcov reports functions, branches and demangled names too. Narrowing
        // to what a tracefile needs is what keeps a gcc upgrade from being a
        // parse error.
        let mut coverage = Coverage::default();
        coverage
            .absorb(
                r#"{"format_version":"2","gcc_version":"13",
                    "files":[{"file":"a.c","functions":[{"name":"f"}],
                    "lines":[{"line_number":1,"count":1,"branches":[],"unexecuted_block":false}]}]}"#,
            )
            .unwrap();
        assert!(coverage.to_lcov().contains("DA:1,1"));
    }

    #[test]
    fn a_run_that_produced_nothing_says_so_rather_than_writing_an_empty_file() {
        assert!(Coverage::default().is_empty());
    }
}
