mod bundler_simple;
mod node_downloader;
mod platform;

use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "banderole")]
#[command(about = "A cross-platform Node.js single-executable bundler")]
#[command(version)]
#[command(
    long_about = "Banderole packages Node.js applications with portable Node binaries into a single binary for easy distribution and execution"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Bundle a Node.js project into a self-contained executable
    Bundle {
        /// Path to the directory containing package.json
        path: PathBuf,
        /// Output path for the bundle (optional)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Bundle { path, output } => {
            bundler_simple::bundle_project(path, output).await?;
        }
    }

    Ok(())
}
