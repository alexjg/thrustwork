//! New sync engine implementation.
//!
//! This module replaces the old `sync_tasks.rs` with a cleaner architecture
//! that matches the design in REFACTOR_DESIGN.md.
//!
//! The sync algorithm:
//! 1. Load three states: filesystem, snapshot (last sync), and repo (Automerge)
//! 2. Classify every path into one of the change types
//! 3. Execute the appropriate action for each path
//! 4. Save the new snapshot

use samod::{ConnectionId, Repo};

use crate::config::Config;
use crate::snapshot::Snapshot;

mod classify;
mod execute;
mod moves;
mod state;
mod sync_ops;

pub use classify::build_sync_plan;
pub use execute::{ExecuteContext, SyncResult, execute_sync_plan};
pub use moves::detect_and_apply_moves;
pub use state::{FsState, RepoState, SnapState};

/// Run a full sync operation.
///
/// This is the main entry point for the new sync engine.
/// It loads the three states, classifies changes, detects moves,
/// and executes the sync plan.
pub async fn run_sync(
    config: &Config,
    repo: &Repo,
    conn_id: ConnectionId,
    snapshot: &mut Snapshot,
) -> Vec<SyncResult> {
    let root = config.root_dir();
    let root_url = config.root_doc_url();

    // Load filesystem state
    let fs_state = match FsState::load(root, config.exclude_patterns()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to scan filesystem: {}", e);
            return vec![];
        }
    };

    // Load snapshot state
    let snap_state = SnapState::load(&config.snapshot_path()).unwrap_or_default();

    // Load repo state
    let repo_state = match RepoState::load(repo, root_url, conn_id).await {
        Ok(s) => s,
        Err(e) => {
            eprintln!("Failed to load repo state: {}", e);
            return vec![];
        }
    };

    // Build the sync plan
    let mut plan = build_sync_plan(fs_state, snap_state, repo_state, root);

    // Detect and apply moves
    let _moves = detect_and_apply_moves(&mut plan, config.move_detection_threshold());

    // Execute the plan
    let mut ctx = ExecuteContext::new(repo, conn_id, root, snapshot);

    execute_sync_plan(plan, &mut ctx).await
}

/// Summary of sync results for display.
pub struct SyncSummary {
    pub pushed: usize,
    pub pulled: usize,
    pub updated: usize,
    pub uploaded: usize,
    pub merged: usize,
    pub deleted_local: usize,
    pub deleted_remote: usize,
    pub moved: usize,
    pub dir_created_local: usize,
    pub dir_created_remote: usize,
    pub dir_deleted_local: usize,
    pub dir_deleted_remote: usize,
    pub no_change: usize,
    pub errors: usize,
}

impl SyncSummary {
    /// Create a summary from sync results.
    pub fn from_results(results: &[SyncResult]) -> Self {
        let mut summary = Self {
            pushed: 0,
            pulled: 0,
            updated: 0,
            uploaded: 0,
            merged: 0,
            deleted_local: 0,
            deleted_remote: 0,
            moved: 0,
            dir_created_local: 0,
            dir_created_remote: 0,
            dir_deleted_local: 0,
            dir_deleted_remote: 0,
            no_change: 0,
            errors: 0,
        };

        for result in results {
            match result {
                SyncResult::Pushed { .. } => summary.pushed += 1,
                SyncResult::Pulled { .. } => summary.pulled += 1,
                SyncResult::Updated { .. } => summary.updated += 1,
                SyncResult::Uploaded { .. } => summary.uploaded += 1,
                SyncResult::Merged { .. } => summary.merged += 1,
                SyncResult::Conflict { .. } => summary.merged += 1, // Old variant, count as merged
                SyncResult::DeletedLocal { .. } => summary.deleted_local += 1,
                SyncResult::DeletedRemote { .. } => summary.deleted_remote += 1,
                SyncResult::Moved { .. } => summary.moved += 1,
                SyncResult::DirCreatedLocal { .. } => summary.dir_created_local += 1,
                SyncResult::DirCreatedRemote { .. } => summary.dir_created_remote += 1,
                SyncResult::DirDeletedLocal { .. } => summary.dir_deleted_local += 1,
                SyncResult::DirDeletedRemote { .. } => summary.dir_deleted_remote += 1,
                SyncResult::NoChange { .. } => summary.no_change += 1,
                SyncResult::Error { .. } => summary.errors += 1,
            }
        }

        summary
    }

    /// Check if there were any changes.
    pub fn has_changes(&self) -> bool {
        self.pushed > 0
            || self.pulled > 0
            || self.updated > 0
            || self.uploaded > 0
            || self.merged > 0
            || self.deleted_local > 0
            || self.deleted_remote > 0
            || self.moved > 0
            || self.dir_created_local > 0
            || self.dir_created_remote > 0
            || self.dir_deleted_local > 0
            || self.dir_deleted_remote > 0
    }

    /// Print the summary.
    pub fn print(&self) {
        if !self.has_changes() && self.errors == 0 {
            println!("No changes.");
            return;
        }

        println!("Sync complete:");
        if self.pushed > 0 {
            println!("  {} file(s) pushed", self.pushed);
        }
        if self.pulled > 0 {
            println!("  {} file(s) pulled", self.pulled);
        }
        if self.updated > 0 {
            println!("  {} file(s) updated locally", self.updated);
        }
        if self.uploaded > 0 {
            println!("  {} file(s) uploaded", self.uploaded);
        }
        if self.merged > 0 {
            println!("  {} file(s) merged", self.merged);
        }
        if self.moved > 0 {
            println!("  {} file(s) moved/renamed", self.moved);
        }
        if self.deleted_local > 0 {
            println!("  {} file(s) deleted locally", self.deleted_local);
        }
        if self.deleted_remote > 0 {
            println!("  {} file(s) deleted remotely", self.deleted_remote);
        }
        if self.dir_created_local > 0 {
            println!(
                "  {} directory(ies) created locally",
                self.dir_created_local
            );
        }
        if self.dir_created_remote > 0 {
            println!("  {} directory(ies) pulled", self.dir_created_remote);
        }
        if self.dir_deleted_local > 0 {
            println!(
                "  {} directory(ies) deleted locally",
                self.dir_deleted_local
            );
        }
        if self.dir_deleted_remote > 0 {
            println!(
                "  {} directory(ies) deleted remotely",
                self.dir_deleted_remote
            );
        }
        if self.errors > 0 {
            println!("  {} error(s)", self.errors);
        }
    }
}
