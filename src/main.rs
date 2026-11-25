mod artifacts;
mod build;
mod config;
mod discovery;
mod git;
mod inspect;
mod node;
mod plan;
mod process;
mod selection;
mod signing;
#[cfg(test)]
mod test_support;
mod transaction;

use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about)]
struct Cli {
    /// Optional overrides; defaults to release.toml in the Git root when present.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// Read Git and project files, without building, writing files or opening keys.
    Plan {
        #[arg(long)]
        json: bool,
    },
    /// Build, verify and atomically save the release, then finish its Git tag.
    Build,
    /// Verify an existing release using the application's pinned public key.
    Verify { directory: PathBuf },
}

fn run() -> Result<()> {
    let args = Cli::parse();
    let selection =
        selection::Selection::for_cli(matches!(args.command, Action::Plan { json: true }));
    let project = match args.config {
        Some(path) => config::Project::load(&path, selection)?,
        None => config::Project::discover(&std::env::current_dir()?, selection)?,
    };
    match args.command {
        Action::Plan { json } => {
            let plan = plan::calculate(&project)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&plan)?);
            } else {
                print!("{}", plan.display());
            }
        }
        Action::Build => transaction::build(&project)?,
        Action::Verify { directory } => {
            let plan = plan::calculate(&project)?;
            artifacts::verify(&project, &plan, &directory)?;
            println!("Проверен выпуск: {}", directory.display());
        }
    }
    Ok(())
}

fn main() {
    if let Err(error) = run() {
        eprintln!("veruna-pack: {error:#}");
        std::process::exit(1);
    }
}
