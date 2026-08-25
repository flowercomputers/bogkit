use std::path::{Path, PathBuf};

use anyhow::{Context, bail};

/// Walk up from the current directory to the bog-kit workspace root: the
/// nearest ancestor whose Cargo.toml declares `[workspace]`.
pub fn root() -> anyhow::Result<PathBuf> {
    let cwd = std::env::current_dir().context("cannot read current directory")?;
    for dir in cwd.ancestors() {
        if is_workspace_root(dir) {
            return Ok(dir.to_path_buf());
        }
    }
    bail!(
        "not inside the bog-kit workspace — standalone projects arrive once \
         fold is published to crates.io"
    );
}

fn is_workspace_root(dir: &Path) -> bool {
    match std::fs::read_to_string(dir.join("Cargo.toml")) {
        Ok(manifest) => manifest.lines().any(|l| l.trim() == "[workspace]"),
        Err(_) => false,
    }
}
