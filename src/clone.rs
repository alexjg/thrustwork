//! Clone command implementation for pulling remote directories.
//!
//! Uses the parallel task-based architecture for efficient fetching
//! of nested directories and files.

use samod::{AutomergeUrl, Connection, Repo};
use thiserror::Error;

use crate::config::Config;
use crate::snapshot::Snapshot;
use crate::sync_tasks::{SyncContext, SyncSummary, run_clone};

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
    config: Config,
    repo: Repo,
    conn: Connection,
    root_url: AutomergeUrl,
) -> Result<CloneResult, CloneError> {
    let conn_id = conn.id();

    // Create empty snapshot
    let snapshot = Snapshot::new(config.root_dir(), Some(root_url.clone()));

    // Create sync context (move_threshold not relevant for clone, but required)
    let ctx = SyncContext::new(config.clone(), repo.clone(), conn_id, snapshot);

    println!("Cloning from: {}", root_url);

    let results = run_clone(&ctx).await;

    // Get the snapshot and save it
    let snapshot = ctx.snapshot.lock().await;

    // Save snapshot
    let mut snapshot_to_save = snapshot.clone();
    snapshot_to_save.update_timestamp();
    snapshot_to_save
        .save(&config.snapshot_path())
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
