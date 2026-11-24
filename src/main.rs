mod artifacts;
mod build;
mod config;
mod git;
mod inspect;
mod plan;
mod process;
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
    #[arg(long, default_value = "release.toml", global = true)]
    config: PathBuf,
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
    let project = config::Project::load(&args.config)?;
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
