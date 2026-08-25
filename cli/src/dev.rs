use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, bail};

use crate::workspace;

pub fn run(project: Option<&str>, fresh: bool) -> anyhow::Result<()> {
    let root = workspace::root()?;
    let name = match project {
        Some(name) => name.to_string(),
        None => infer_project(&root)?,
    };

    let data_dir = data_dir(&name)?;
    if fresh && data_dir.exists() {
        std::fs::remove_dir_all(&data_dir).context("wiping data dir for --fresh")?;
    }
    std::fs::create_dir_all(&data_dir).context("creating data dir")?;
    println!("data dir: {}", data_dir.display());

    // Inherit stdio so the project owns the terminal (the search example's
    // interactive loop, server logs, ...). We only collect the exit status.
    let status = Command::new("cargo")
        .args(["run", "-p", &name])
        .current_dir(&root)
        .env("BOG_DATA_DIR", &data_dir)
        .env("PORT", std::env::var("PORT").unwrap_or_else(|_| "7877".into()))
        .status()
        .context("running cargo")?;

    if !status.success() {
        // mirror the project's exit code so `bogkit dev` composes in scripts
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

/// When run from inside examples/<name>, that project is the target —
/// scaffolded crates are named after their directory.
fn infer_project(root: &Path) -> anyhow::Result<String> {
    let cwd = std::env::current_dir()?;
    let rel = cwd.strip_prefix(root.join("examples")).ok();
    match rel.and_then(|rel| rel.iter().next()) {
        Some(first) => Ok(first.to_string_lossy().into_owned()),
        None => bail!("run from inside examples/<project> or pass -p <project>"),
    }
}

/// Stable per-project data dir: ~/.bogkit/data/<name>. Persistent across
/// runs by default; `--fresh` wipes it.
fn data_dir(name: &str) -> anyhow::Result<PathBuf> {
    let home = std::env::home_dir().context("cannot determine home directory")?;
    Ok(home.join(".bogkit").join("data").join(name))
}
