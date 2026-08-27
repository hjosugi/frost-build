use std::path::PathBuf;

fn main() -> anyhow::Result<()> {
    let output = std::env::args_os()
        .nth(1)
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("usage: cargo run --example release-assets -- OUTPUT"))?;
    let date = std::env::args().nth(2).ok_or_else(|| {
        anyhow::anyhow!("usage: cargo run --example release-assets -- OUTPUT YYYY-MM-DD")
    })?;
    if std::env::args_os().nth(3).is_some() {
        anyhow::bail!("usage: cargo run --example release-assets -- OUTPUT YYYY-MM-DD");
    }
    frostbuild_cli::generate_release_assets(&output, &date)
}
