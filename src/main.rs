mod bundler;
mod embedded_template;
mod executable;
mod node_downloader;
mod node_version_manager;
mod platform;
mod rust_toolchain;

use clap::{Parser, Subcommand};
use indicatif::MultiProgress;
use indicatif_log_bridge::LogWrapper;
use log::LevelFilter;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "banderole")]
#[command(about = "A cross-platform Node.js single-executable bundler")]
#[command(version)]
#[command(
    long_about = "Banderole packages Node.js applications with portable Node binaries into a single binary for easy distribution and execution"
)]
struct Cli {
    /// Enable verbose output
    #[arg(short, long, global = true)]
    verbose: bool,
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
    // Initialize env_logger wrapped by indicatif's log bridge so logs play nice with progress bars
    let multi_progress = MultiProgress::new();
    let cli = Cli::parse();

    let default_level = if cli.verbose { "debug" } else { "warn" };
    let built_logger =
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(default_level))
            .build();
    let level: LevelFilter = built_logger.filter();
    LogWrapper::new(multi_progress.clone(), built_logger).try_init()?;
    log::set_max_level(level);

    match cli.command {
        Commands::Bundle {
            path,
            output,
            name,
            no_compression,
            ignore_cached_versions,
        } => {
            bundler::bundle_project(
                path,
                output,
                name,
                no_compression,
                ignore_cached_versions,
                &multi_progress,
            )
            .await?;
        }
    }

    Ok(())
}
