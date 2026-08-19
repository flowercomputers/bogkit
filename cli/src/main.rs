//! The bogkit CLI: scaffold and run BogKit projects.
//!
//! Phase 0 (see docs/bog-cli-plan.md): `bogkit new --kind embedded` matches
//! scripts/new-project.sh, and `bogkit dev` wraps `cargo run` with the
//! data-dir conventions the server flavor will rely on in phase 1.

mod dev;
mod new;
mod workspace;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Parser)]
#[command(name = "bogkit", version, about = "Scaffold and run BogKit projects")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Create a new project in examples/<name>
    New {
        /// Project name (also the crate and directory name)
        name: String,

        /// Which flavor of project to create
        #[arg(long, value_enum, default_value_t = Kind::Server)]
        kind: Kind,
    },

    /// Run a project with bogkit conventions ($BOG_DATA_DIR, $PORT)
    Dev {
        /// Project to run; inferred when run from inside examples/<project>
        #[arg(short, long)]
        project: Option<String>,

        /// Wipe the project's data dir before running
        #[arg(long)]
        fresh: bool,
    },
}

#[derive(Clone, Copy, ValueEnum)]
pub enum Kind {
    /// A plain Rust binary using fold directly
    Embedded,
    /// A fold program served over HTTP by bog-serve
    Server,
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::New { name, kind } => new::run(&name, kind),
        Command::Dev { project, fresh } => dev::run(project.as_deref(), fresh),
    }
}
