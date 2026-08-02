//! `frost init`: a starter manifest for a directory that has sources but no
//! `frost.toml`.

use anyhow::bail;
use anyhow::Result;

use crate::cli::InitLanguage;
use crate::wrapper;

/// Write a starter manifest for a directory that has sources but no
/// `frost.toml`, so the first thing a newcomer runs is not a dead end.
pub(crate) fn run_init(
    root: &std::path::Path,
    dry_run: bool,
    language: Option<InitLanguage>,
    wrapper_only: bool,
) -> Result<i32> {
    let version = env!("CARGO_PKG_VERSION");
    // `--wrapper` is the path into a workspace that already has a manifest,
    // which is where a version pin is most wanted and where plain `init`
    // refuses to touch anything.
    if wrapper_only {
        if dry_run {
            println!("frost init --wrapper would write, pinned to frost {version}:");
            for name in [
                wrapper::VERSION_FILE,
                wrapper::WRAPPER_SH,
                wrapper::WRAPPER_CMD,
            ] {
                println!("  {}", root.join(name).display());
            }
            return Ok(0);
        }
        report_wrapper(&wrapper::write_wrapper(root, version)?, version);
        return Ok(0);
    }

    let manifest_path = root.join(frostbuild_core::manifest::MANIFEST_FILE);
    if manifest_path.exists() && !dry_run {
        bail!(
            "{} already exists. delete it first, use --wrapper to add only the \
             version wrapper, or use --dry-run to see what init would write",
            manifest_path.display()
        );
    }
    let scaffold = match language {
        Some(language) => frostbuild_core::manifest::scaffold_for(root, language.into())?,
        None => frostbuild_core::manifest::scaffold(root)?,
    };
    if dry_run {
        print!("{}", scaffold.manifest);
        return Ok(0);
    }
    std::fs::write(&manifest_path, &scaffold.manifest)?;
    println!("frost: wrote {}", manifest_path.display());
    for line in &scaffold.summary {
        println!("  {line}");
    }
    // A workspace is scaffolded once and built by everyone, so the version it
    // was written against is worth recording while it is still known.
    report_wrapper(&wrapper::write_wrapper(root, version)?, version);
    println!();
    println!("  read it before trusting it, then: frost build");
    Ok(0)
}

fn report_wrapper(written: &[std::path::PathBuf], version: &str) {
    println!("frost: pinned this workspace to frost {version}");
    for path in written {
        println!("  {}", path.display());
    }
    println!("  commit these, then build with ./frostw build on any machine");
}
