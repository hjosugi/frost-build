//! `frost cache`: the local content-addressed store.

use std::path::Path;

use anyhow::Result;

use crate::cli::CacheCmd;
use crate::human_bytes;

/// The local content-addressed store: what is in it, and emptying it.
pub(crate) fn run_cache(root: &Path, command: CacheCmd) -> Result<i32> {
    match command {
        CacheCmd::Stats { json } => {
            let stats =
                frostbuild_core::cas::LocalCas::new(root, frostbuild_exec::DEFAULT_CAS_MAX_BYTES)
                    .stats()?;
            if json {
                println!("{}", serde_json::to_string_pretty(&stats)?);
            } else {
                println!("frost: local CAS");
                println!(
                    "|-- blobs     {:>8} · {}",
                    stats.object_count,
                    human_bytes(stats.object_bytes)
                );
                println!(
                    "|-- chunks    {:>8} · {}",
                    stats.chunk_count,
                    human_bytes(stats.chunk_bytes)
                );
                println!(
                    "|-- deltas    {:>8} · {}",
                    stats.delta_count,
                    human_bytes(stats.delta_bytes)
                );
                println!("|-- manifests {:>8}", stats.manifest_count);
                println!(
                    "`-- reuse      {:>7.2}% · {} / {} logical bytes",
                    stats.chunk_reuse_ratio * 100.0,
                    human_bytes(stats.reused_chunk_bytes),
                    human_bytes(stats.logical_chunk_bytes)
                );
            }
            Ok(0)
        }
    }
}
