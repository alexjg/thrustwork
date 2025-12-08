//! Clone command implementation for pulling remote directories.
//!
//! Uses the new sync engine architecture. Clone is essentially syncing
//! with an empty filesystem and empty snapshot - everything in the repo
//! becomes RemoteNew and gets pulled.

use samod::{AutomergeUrl, Connection, Repo};
use thiserror::Error;

use crate::config::Config;
use crate::snapshot::Snapshot;
use crate::sync::{
    ExecuteContext, FsState, RepoState, SnapState, SyncSummary, build_sync_plan, execute_sync_plan,
};

/// Result of cloning a directory
pub struct CloneResult {
    pub files_cloned: usize,
    pub errors: usize,
}

/// Execute the full clone operation.
///
/// This works by treating clone as a sync with an empty local state.
/// Everything in the repo is classified as RemoteNew and pulled down.
pub(crate) async fn execute_clone(
    config: Config,
    repo: Repo,
    conn: Connection,
    _root_url: AutomergeUrl,
) -> Result<CloneResult, CloneError> {
    let conn_id = conn.id();
    let root = config.root_dir();
    let root_url = config.root_doc_url();

    // Create empty states - nothing on disk yet, no previous sync
    let fs_state = FsState::new();
    let snap_state = SnapState::new();

    // Load the full repo state from the root document
    let repo_state = RepoState::load(&repo, root_url, conn_id)
        .await
        .map_err(|e| CloneError::RepoState(e.to_string()))?;

    // Build sync plan - everything will be RemoteNew
    let plan = build_sync_plan(fs_state, snap_state, repo_state, root);

    // Create snapshot to track what we clone
    let mut snapshot = Snapshot::new(root, Some(root_url.clone()));

    // Execute the plan
    let mut ctx = ExecuteContext::new(&repo, conn_id, root, &mut snapshot);
    let results = execute_sync_plan(plan, &mut ctx).await;

    // Save the snapshot
    snapshot.update_timestamp();
    snapshot
        .save(&config.snapshot_path())
        .map_err(|e| CloneError::SaveSnapshot(e.to_string()))?;

    // Count results
    let summary = SyncSummary::from_results(&results);
    let files_cloned = summary.pulled + summary.dir_created_remote;
    let errors = summary.errors;

    Ok(CloneResult {
        files_cloned,
        errors,
    })
}

/// Errors that can occur during clone operations
#[derive(Debug, Error)]
pub enum CloneError {
    #[error("Failed to save snapshot: {0}")]
    SaveSnapshot(String),

    #[error("Failed to load repo state: {0}")]
    RepoState(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}
