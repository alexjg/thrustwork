use crate::{
    Cli, config,
    snapshot::Snapshot,
    sync::{SyncSummary, run_sync},
};

pub(crate) async fn execute(cli: &Cli) {
    // Find the .pushwork directory
    let cwd = std::env::current_dir().unwrap_or_else(|e| {
        eprintln!("Failed to get current directory: {}", e);
        std::process::exit(1);
    });

    let mut config = match config::Config::find_from_cwd(&cwd) {
        Ok(config) => config,
        Err(e) => {
            eprintln!("Failed to load config: {}", e);
            std::process::exit(1);
        }
    };

    println!("Syncing directory: {:?}", config.root_dir());

    // Apply CLI sync server override if provided
    if let Some(ref server) = cli.sync_server {
        config.set_sync_server_url(server.clone());
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

    // Load or create snapshot
    let mut snapshot = Snapshot::load_or_create(&config);

    println!("Syncing directory: {:?}", config.root_dir().display());

    // Run the new sync engine
    let results = run_sync(&config, &repo, conn.id(), &mut snapshot).await;

    // Save snapshot if there were changes
    let summary = SyncSummary::from_results(&results);
    if summary.has_changes() {
        snapshot.update_timestamp();
        snapshot.save(&config.snapshot_path()).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to save snapshot: {}", e);
        });
    }

    // Print summary
    summary.print();
}
