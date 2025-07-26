mod bundler;
mod executable;
mod node_downloader;
mod node_version_manager;
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
        /// Custom name for the executable (optional)
        #[arg(short, long)]
        name: Option<String>,
        /// Disable compression for faster bundling (useful for testing)
        #[arg(long)]
        no_compression: bool,
        /// Ignore cached version resolution results
        #[arg(long)]
        ignore_cached_versions: bool,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Commands::Bundle {
            path,
            output,
            name,
            no_compression,
            ignore_cached_versions,
        } => {
            bundler::bundle_project(path, output, name, no_compression, ignore_cached_versions).await?;
        }
    }

    Ok(())
}
