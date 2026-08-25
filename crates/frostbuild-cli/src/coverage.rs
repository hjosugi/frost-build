//! `frost coverage-lcov`: one run's gcov data, merged into an lcov tracefile.
//!
//! The collection and the format both live in [`frostbuild_core::coverage`],
//! because `frost test --coverage` runs the same merge as an action in the
//! graph. What is here is the hand-driven entry point: pointing at a counter
//! directory and an object tree produced by something other than frost.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

/// Merge one run's gcov data into an lcov tracefile.
///
/// The notes files are discovered by walking `objects`, which is what makes
/// this usable against a tree frost did not build. `frost test --coverage`
/// names them explicitly instead, since it knows which compile produced each.
pub(crate) fn run_coverage_lcov(
    root: &Path,
    gcda: &Path,
    objects: &Path,
    output: &Path,
    gcov: &str,
) -> Result<i32> {
    let gcda_dir = root.join(gcda);
    let notes: Vec<PathBuf> = walk(&root.join(objects))
        .into_iter()
        .filter(|path| path.extension().is_some_and(|ext| ext == "gcno"))
        .collect();

    let lcov = frostbuild_core::coverage::merge(
        root,
        std::slice::from_ref(&gcda_dir),
        &notes,
        gcov,
        |warning| eprintln!("frost: coverage: {warning}"),
    )?;
    if lcov.is_empty() {
        // An empty tracefile reports 0% and looks like a result. Refusing says
        // the measurement did not happen, which is a different thing. This
        // also catches the case where gcov reported only files outside the
        // workspace, which the emitter drops.
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
fn walk(dir: &Path) -> Vec<PathBuf> {
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
