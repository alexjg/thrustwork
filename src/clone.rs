//! Clone command implementation for pulling remote directories.
//!
//! Uses the parallel task-based architecture for efficient fetching
//! of nested directories and files.

use std::path::PathBuf;

use samod::{AutomergeUrl, ConnDirection, Repo};
use thiserror::Error;
use tokio_tungstenite::connect_async;

use crate::config::DirectoryConfig;
use crate::init::PushworkPaths;
use crate::snapshot::Snapshot;
use crate::sync_tasks::{run_clone, SyncContext, SyncSummary};

/// Errors that can occur during clone operations
#[derive(Debug, Error)]
pub enum CloneError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Failed to connect to sync server: {0}")]
    ConnectionFailed(String),

    #[error("Failed to save config: {0}")]
    SaveConfig(String),

    #[error("Failed to save snapshot: {0}")]
    SaveSnapshot(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of cloning a directory
pub struct CloneResult {
    pub files_cloned: usize,
    pub errors: usize,
}

/// Execute the full clone operation
pub(crate) async fn execute_clone(
    repo: &Repo,
    url: &str,
    cwd: PathBuf,
    paths: PushworkPaths,
) -> Result<CloneResult, CloneError> {
    // Parse URL
    let root_url = parse_automerge_url(url)?;

    // Connect to sync server (use default URL from config)
    let config = DirectoryConfig::load(&paths.config_file).unwrap_or_default();
    let sync_url = config.sync_server_url();

    println!("Connecting to sync server: {}", sync_url);
    let conn_id = connect_to_server(repo, &sync_url).await?;

    // Create empty snapshot
    let snapshot = Snapshot::new(cwd.clone(), Some(root_url.clone()));

    // Create sync context
    let ctx = SyncContext::new(
        repo.clone(),
        conn_id,
        cwd.clone(),
        root_url.clone(),
        config.exclude_patterns.clone(),
        snapshot,
    );

    println!("Cloning from: {}", url);

    // Run the parallel clone
    let results = run_clone(&ctx).await;

    // Get the snapshot and save it
    let snapshot = ctx.snapshot.lock().await;

    // Save config with root directory URL
    let mut config = DirectoryConfig::load(&paths.config_file).unwrap_or_default();
    config.root_directory_url = Some(root_url.to_string());
    config
        .save(&paths.config_file)
        .map_err(|e| CloneError::SaveConfig(e.to_string()))?;

    // Save snapshot
    let mut snapshot_to_save = snapshot.clone();
    snapshot_to_save.update_timestamp();
    let snapshot_path = Snapshot::path_in(&paths.pushwork_dir);
    snapshot_to_save
        .save(&snapshot_path)
        .map_err(|e| CloneError::SaveSnapshot(e.to_string()))?;

    drop(snapshot);

    // Count results
    let summary = SyncSummary::from_results(&results);
    let files_cloned = summary.pulled;
    let errors = summary.errors.len();

    println!("\nCloned {} file(s).", files_cloned);
    if errors > 0 {
        println!("{} error(s) occurred.", errors);
    }

    Ok(CloneResult {
        files_cloned,
        errors,
    })
}

/// Connect to the sync server
async fn connect_to_server(
    repo: &Repo,
    sync_url: &str,
) -> Result<samod::ConnectionId, CloneError> {
    let (ws_stream, _response) = connect_async(sync_url)
        .await
        .map_err(|e| CloneError::ConnectionFailed(e.to_string()))?;

    let conn = repo
        .connect_tungstenite(ws_stream, ConnDirection::Outgoing)
        .map_err(|_| CloneError::ConnectionFailed("Repo stopped".into()))?;

    conn.handshake_complete()
        .await
        .map_err(|_| CloneError::ConnectionFailed("Handshake failed".into()))?;

    Ok(conn.id())
}

/// Parse an Automerge URL
fn parse_automerge_url(url: &str) -> Result<AutomergeUrl, CloneError> {
    url.parse()
        .map_err(|e| CloneError::InvalidUrl(format!("{}", e)))
}
