//! Sync command implementation.
//!
//! Handles synchronizing local file changes with the Automerge sync server.
//! Uses a parallel task-based architecture for efficient handling of
//! nested directories and concurrent document operations.

use samod::{AutomergeUrl, ConnDirection, Repo};
use tokio_tungstenite::connect_async;

use crate::config::DirectoryConfig;
use crate::init::PushworkPaths;
use crate::snapshot::Snapshot;
use crate::sync_tasks::{run_sync, SyncContext, SyncSummary};

// =============================================================================
// Public API
// =============================================================================

/// Execute the sync command
pub async fn execute(paths: &PushworkPaths, config: &DirectoryConfig, repo: &Repo) {
    let root_url: AutomergeUrl = config
        .root_directory_url
        .as_ref()
        .unwrap_or_else(|| {
            eprintln!("No root directory URL in config - directory not properly initialized");
            std::process::exit(1);
        })
        .parse()
        .unwrap_or_else(|e| {
            eprintln!("Invalid root directory URL: {}", e);
            std::process::exit(1);
        });

    // Connect to sync server
    let conn_id = connect_to_server(repo, &config.sync_server_url()).await;

    // Load or create snapshot
    let snapshot = load_or_create_snapshot(paths, &root_url);

    // Create sync context
    let ctx = SyncContext::new(
        repo.clone(),
        conn_id,
        paths.root.clone(),
        root_url,
        config.exclude_patterns.clone(),
        snapshot,
        config.sync.move_detection_threshold,
    );

    println!("Syncing directory: {:?}", paths.root);

    // Run the parallel sync
    let results = run_sync(&ctx).await;

    // Get the snapshot back and save it
    let snapshot = ctx.snapshot.lock().await;
    let snapshot_path = Snapshot::path_in(&paths.pushwork_dir);

    // Only save if there were changes
    let summary = SyncSummary::from_results(&results);
    if summary.has_changes() {
        let mut snapshot_to_save = snapshot.clone();
        snapshot_to_save.update_timestamp();
        snapshot_to_save.save(&snapshot_path).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to save snapshot: {}", e);
        });
    }
    drop(snapshot);

    // Print summary
    summary.print();
}

// =============================================================================
// Low-level helpers
// =============================================================================

/// Connect to the sync server and return the connection ID
async fn connect_to_server(repo: &Repo, sync_url: &str) -> samod::ConnectionId {
    println!("Connecting to sync server: {}", sync_url);

    let (ws_stream, _response) = connect_async(sync_url).await.unwrap_or_else(|e| {
        eprintln!("Failed to connect to sync server: {}", e);
        std::process::exit(1);
    });

    let conn = repo
        .connect_tungstenite(ws_stream, ConnDirection::Outgoing)
        .unwrap_or_else(|_| {
            eprintln!("Failed to set up connection: repo stopped");
            std::process::exit(1);
        });

    conn.handshake_complete().await.unwrap_or_else(|_| {
        eprintln!("Connection handshake failed");
        std::process::exit(1);
    });

    conn.id()
}

/// Load or create the snapshot for tracking sync state
fn load_or_create_snapshot(paths: &PushworkPaths, root_url: &AutomergeUrl) -> Snapshot {
    let snapshot_path = Snapshot::path_in(&paths.pushwork_dir);

    if snapshot_path.exists() {
        Snapshot::load(&snapshot_path).unwrap_or_else(|e| {
            eprintln!("Warning: Failed to load snapshot, starting fresh: {}", e);
            Snapshot::new(paths.root.clone(), Some(root_url.clone()))
        })
    } else {
        Snapshot::new(paths.root.clone(), Some(root_url.clone()))
    }
}
