//! Directory initialization logic for thrustwork.
//!
//! Handles creating the `.pushwork` directory structure and config files.

use crate::PushworkPaths;
use crate::config::Config;

pub(crate) async fn execute(cli: &crate::Cli) {
    // Get the current working directory
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Failed to get current directory: {}", e);
        std::process::exit(1);
    });

    // Check if already initialized
    let paths = PushworkPaths::new(&cwd);
    if paths.is_initialized() {
        eprintln!("Directory is already initialized (use --force to reinitialize)");
        std::process::exit(1);
    }

    // Create the directory structure
    println!("Initializing thrustwork in {:?}...", cwd);
    let mut config = Config::init(&cwd, false).await.unwrap_or_else(|e| {
        eprintln!("Failed to initialize directory: {}", e);
        std::process::exit(1);
    });

    // Apply CLI sync server override if provided
    if let Some(ref server) = cli.sync_server {
        config.set_sync_server_url(server.to_string());
        config.save().unwrap_or_else(|e| {
            eprintln!("Failed to save config: {}", e);
            std::process::exit(1);
        });
    }

    let repo = config.repo().await;
    let root_doc = repo
        .find(config.root_doc_url().document_id().clone())
        .await
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            // We just created the document, this _shouldn't_ happen
            eprintln!("Failed to find root directory");
            std::process::exit(1);
        });

    println!("Connecting to sync server...");
    let sync_server_connection = config
        .sync_server_connection(&repo)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to connect to sync server: {}", e);
            std::process::exit(1);
        });

    println!("Syncing root document...");
    root_doc
        .they_have_our_changes(sync_server_connection.id())
        .await;

    println!("\nInitialized thrustwork directory");
    println!("Root URL: {}", root_doc.url());
    println!("\nShare this URL to allow others to sync with this directory.");
}
