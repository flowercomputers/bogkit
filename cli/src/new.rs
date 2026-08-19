use std::fs;

use anyhow::{Context, bail};

use crate::{Kind, workspace};

// Templates are embedded in the binary (not read from the repo) so `bogkit
// new` keeps working once the CLI is installed standalone. `{name}` is
// substituted with plain str::replace — these aren't format! strings.

const MANIFEST_EMBEDDED: &str = r#"[package]
name = "{name}"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
anny = { path = "../../anny" }
ese = { path = "../../ese", features = ["dim-512", "quant-8"] }
fold = { path = "../../fold" }
serde = { version = "1", features = ["derive"] }
"#;

const MAIN_EMBEDDED: &str = r#"fn main() {
    println!("welcome to bog kit. start hacking in examples/{name}/src/main.rs");
}
"#;

const MANIFEST_SERVER: &str = r#"[package]
name = "{name}"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
bog-serve = { path = "../../serve" }
fold = { path = "../../fold" }
schemars = "1"
serde = { version = "1", features = ["derive"] }
"#;

const MAIN_SERVER: &str = r#"//! A fold database served over HTTP.
//!
//! The pipeline below fans every inserted `Entry` out to two views; the
//! server generates the whole API from it. Once running, explore with:
//!
//!   curl localhost:7877/openapi.json
//!   curl -X POST localhost:7877/insert \
//!        -H 'content-type: application/json' -d '{"text": "hello"}'
//!   curl localhost:7877/views/total
//!
//! Add fields to `Entry` or sinks to the pipeline, and the API (and its
//! OpenAPI doc) follow automatically.

use bog_serve::App;
use fold::pipeline::terminal;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What `POST /insert` accepts. `JsonSchema` puts its shape in the doc.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
struct Entry {
    text: String,
}

fn main() {
    App::stream(
        // $BOG_DATA_DIR when run via `bogkit dev`, ./bog.db otherwise
        bog_serve::data_dir(),
        (
            terminal::Count::new("total"),
            terminal::Bag::<Entry>::new("entries"),
        ),
    )
    .run()
}
"#;

pub fn run(name: &str, kind: Kind) -> anyhow::Result<()> {
    validate_name(name)?;

    let project_dir = workspace::root()?.join("examples").join(name);
    if project_dir.exists() {
        bail!("{} already exists", project_dir.display());
    }

    let (manifest, main_rs) = match kind {
        Kind::Embedded => (MANIFEST_EMBEDDED, MAIN_EMBEDDED),
        Kind::Server => (MANIFEST_SERVER, MAIN_SERVER),
    };

    fs::create_dir_all(project_dir.join("src")).context("creating project directories")?;
    fs::write(project_dir.join("Cargo.toml"), manifest.replace("{name}", name))?;
    fs::write(project_dir.join("src/main.rs"), main_rs.replace("{name}", name))?;

    println!("created {}", project_dir.display());
    println!("run it with: bogkit dev -p {name}");
    if let Kind::Server = kind {
        println!("then explore: curl localhost:7877/openapi.json");
    }
    Ok(())
}

/// Same rules as scripts/new-project.sh: start with a letter or number,
/// then letters, numbers, '-' or '_'.
fn validate_name(name: &str) -> anyhow::Result<()> {
    let mut chars = name.chars();
    let first_ok = chars.next().is_some_and(|c| c.is_ascii_alphanumeric());
    let rest_ok = chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
    if !(first_ok && rest_ok) {
        bail!(
            "project name must start with a letter or number and contain only \
             letters, numbers, '-' or '_'"
        );
    }
    Ok(())
}
