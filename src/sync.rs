//! Sync command implementation.
//!
//! Handles synchronizing local file changes with the Automerge sync server.
//! Uses a parallel task-based architecture for efficient handling of
//! nested directories and concurrent document operations.

use samod::{ConnDirection, Connection, Repo};
use tokio_tungstenite::connect_async;

use crate::config::Config;
use crate::snapshot::Snapshot;
use crate::sync_tasks::{SyncContext, SyncSummary, run_sync_two_phase};

// =============================================================================
// Public API
// =============================================================================

/// Execute the sync command
pub async fn execute(config: Config, repo: Repo, sync_server_conn: Connection) {
    // Load or create snapshot
    let snapshot = load_or_create_snapshot(&config);

    // Create sync context
    let ctx = SyncContext::new(config.clone(), repo, sync_server_conn.id(), snapshot);

    println!("Syncing directory: {:?}", config.root_dir().display());

    // Run the two-phase sync (enables cross-directory move detection)
    let results = run_sync_two_phase(&ctx).await;

    // Get the snapshot back and save it
    let snapshot = ctx.snapshot.lock().await;

    // Only save if there were changes
    let summary = SyncSummary::from_results(&results);
    if summary.has_changes() {
        let mut snapshot_to_save = snapshot.clone();
        snapshot_to_save.update_timestamp();
        snapshot_to_save
            .save(&config.snapshot_path())
            .unwrap_or_else(|e| {
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
fn load_or_create_snapshot(config: &Config) -> Snapshot {
    let snapshot_path = config.snapshot_path();

    if snapshot_path.exists() {
        if let Ok(s) = Snapshot::load(&snapshot_path) {
            return s;
        } else {
            eprintln!("Warning: Failed to load snapshot, starting fresh");
        }
    }
    Snapshot::new(
        config.root_dir().to_path_buf(),
        Some(config.root_doc_url().clone()),
    )
}
