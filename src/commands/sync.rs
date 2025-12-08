use crate::{Cli, config};

pub(crate) async fn execute(cli: &Cli) {
    // Find the .pushwork directory
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Failed to get current directory: {}", e);
        std::process::exit(1);
    });

    // Load the config
    let mut config = config::Config::load(&cwd).unwrap_or_else(|e| {
        eprintln!("Failed to load config: {}", e);
        std::process::exit(1);
    });

    println!("Syncing directory: {:?}", config.root_dir());

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

    // Execute sync
    crate::sync::execute(config, repo, conn).await;
}
