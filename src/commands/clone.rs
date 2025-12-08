use samod::AutomergeUrl;

use crate::{Cli, PushworkPaths, config::Config};

pub(crate) async fn execute(cli: &Cli, url: AutomergeUrl) {
    println!("Cloning from: {}", url);

    // Get the current working directory
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Failed to get current directory: {}", e);
        std::process::exit(1);
    });

    // Check if already initialized
    let paths = PushworkPaths::new(&cwd);
    if paths.is_initialized() {
        eprintln!(
            "Directory is already initialized. Cannot clone into an existing thrustwork directory."
        );
        std::process::exit(1);
    }

    // Create the directory structure
    println!("Cloning thrustwork project in {:?}...", cwd);
    let mut config = Config::init_for_clone(&cwd, url.clone(), false)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Failed to initialize directory: {}", e);
            std::process::exit(1);
        });

    // Apply CLI sync server override if provided
    if let Some(ref server) = cli.sync_server {
        config.set_sync_server_url(server.clone());
        config.save().unwrap_or_else(|e| {
            eprintln!("Failed to save config: {}", e);
            std::process::exit(1);
        });
    }

    let repo = config.repo().await;

    println!("Connecting to sync server: {}", config.sync_server_url());
    let conn = config
        .sync_server_connection(&repo)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Unable to connect to sync server: {}", e);
            std::process::exit(1);
        });

    println!("Connected to sync server");

    // Execute the clone operation
    match crate::clone::execute_clone(config, repo, conn, url).await {
        Ok(result) => {
            println!("\nDone! {} file(s) cloned.", result.files_cloned);
            if result.errors > 0 {
                println!("{} error(s) occurred.", result.errors);
            }
        }
        Err(e) => {
            eprintln!("Clone failed: {}", e);
            std::process::exit(1);
        }
    }
}
