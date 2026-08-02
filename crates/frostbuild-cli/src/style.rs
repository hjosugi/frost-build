//! `frost fmt` and `frost lint`: the manifest as a document.
//!
//! Both read manifests and neither builds anything, which is why they are here
//! rather than in [`crate::build`]. The rules themselves live in
//! `frostbuild-core`; this is the command around them.

use anyhow::Context;
use anyhow::Result;
use frostbuild_core::manifest::Manifest;

/// Write what fed every action's key, joined from the four places that hold it.
///
/// The journal has the keys and input digests, the graph has argv and
/// environment, the toolchain fingerprint is computed per run, and the profile
/// and platform come from this invocation. Only together do they explain a
/// cache miss.
/// Rewrite every manifest in the workspace in canonical form.
///
/// Nested package manifests are included: a workspace where only the root is
/// formatted is one where `--check` passes and the packages still drift.
pub(crate) fn run_fmt(root: &std::path::Path, check: bool) -> Result<i32> {
    let mut manifests = vec![root.join(frostbuild_core::manifest::MANIFEST_FILE)];
    manifests.extend(frostbuild_core::manifest::package_manifests(root)?);

    let mut changed = Vec::new();
    for path in &manifests {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        let formatted = frostbuild_core::fmt::format_manifest(&text)
            .with_context(|| format!("failed to format {}", path.display()))?;
        if formatted == text {
            continue;
        }
        changed.push(path.clone());
        if !check {
            std::fs::write(path, &formatted)
                .with_context(|| format!("failed to write {}", path.display()))?;
        }
    }

    // `/` on every host, for the reason a manifest error is normalized the same
    // way (`a_manifest_error_reads_the_same_on_every_host`): the path is
    // workspace-relative so that it reads identically everywhere, and
    // `core\frost.toml` on Windows beside `core/frost.toml` elsewhere gives
    // that up. A manifest spells its own paths with `/` too.
    let relative = |path: &std::path::Path| {
        path.strip_prefix(root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    };
    if changed.is_empty() {
        println!("fmt: {} manifest(s) already canonical", manifests.len());
        return Ok(0);
    }
    for path in &changed {
        println!("{}", relative(path));
    }
    if check {
        println!("fmt: {} would change; run `frost fmt`", changed.len());
        // The "your code" side of the exit-code split, like a failing lint.
        return Ok(1);
    }
    println!("fmt: {} rewritten", changed.len());
    Ok(0)
}

/// Report manifest patterns that parse, build, and cost something later.
pub(crate) fn run_lint(root: &std::path::Path, json: bool) -> Result<i32> {
    let manifest = Manifest::load(root)?;
    let findings = frostbuild_core::lint::lint(&manifest, root);
    if json {
        let report = frostbuild_core::lint::LintReport::new(&findings);
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if findings.is_empty() {
        println!("lint: clean");
    } else {
        for finding in &findings {
            println!("{}: {}", finding.target, finding.message);
            println!("  {} ({})", finding.why, finding.rule);
        }
        println!(
            "lint: {} finding{}",
            findings.len(),
            if findings.len() == 1 { "" } else { "s" }
        );
    }
    // Findings are an answer about the manifest, which is the "your code" side
    // of the exit-code split -- the same 1 a failing test returns, so `frost
    // lint` can gate CI without a wrapper that interprets output.
    Ok(i32::from(!findings.is_empty()))
}
