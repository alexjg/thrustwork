use clap::{Parser, Subcommand};
use samod::AutomergeUrl;

mod clone;
mod commands;
mod config;
mod documents;
mod files;
mod pushwork_paths;
pub(crate) use pushwork_paths::PushworkPaths;

use crate::config::Config;
mod snapshot;
mod sync;

#[derive(Parser)]
#[command(name = "thrustwork")]
#[command(about = "A Rust implementation of pushwork - sync files via Automerge")]
struct Cli {
    /// Sync server URL (defaults to wss://sync3.automerge.org)
    #[arg(long, global = true)]
    sync_server: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new thrustwork directory
    Init,

    /// Sync local changes with the remote
    Sync,

    /// Clone a remote pushwork directory
    Clone {
        /// Automerge URL of the directory to clone
        url: AutomergeUrl,
    },

    /// Print the root directory URL
    Url,
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => commands::init::execute(&cli).await,
        Commands::Sync => commands::sync::execute(&cli).await,
        Commands::Clone { ref url } => commands::clone::execute(&cli, url.clone()).await,
        Commands::Url => {
            // Find the .pushwork directory
            let cwd = std::env::current_dir().unwrap_or_else(|e| {
                eprintln!("Failed to get current directory: {}", e);
                std::process::exit(1);
            });

            let config = Config::find_from_cwd(&cwd).unwrap_or_else(|_e| {
                eprintln!("Not in a thrustwork directory (no .pushwork found)");
                eprintln!("Run 'thrustwork init' to initialize this directory.");
                std::process::exit(1);
            });

            println!("{}", config.root_doc_url());
        }
    }
}
