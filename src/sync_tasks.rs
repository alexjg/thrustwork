//! Parallel sync task types and execution.
//!
//! This module defines the task-based architecture for syncing directories.
//! Instead of gathering all changes upfront, we use a streaming approach with
//! a work queue (FuturesUnordered) that allows:
//! - Parallel fetching and syncing of documents
//! - Incremental discovery of nested directories
//! - Efficient handling of deep hierarchies

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use automerge::ChangeHash;
use autosurgeon::hydrate;
use futures::stream::{FuturesUnordered, StreamExt};
use samod::{AutomergeUrl, ConnectionId, DocHandle, Repo};
use tokio::sync::Mutex;

use crate::documents::{DirectoryDocument, FileContent, FileDocument};
use crate::files::{self, FileInfo};
use crate::scanner;
use crate::snapshot::{Snapshot, SnapshotDirectoryEntry, SnapshotFileEntry};
use crate::sync_ops;

// =============================================================================
// Sync Tasks
// =============================================================================

/// A task in the sync work queue.
///
/// Each task represents a discrete unit of work that can be executed in parallel.
/// Tasks can spawn additional tasks when completed (e.g., a directory sync spawns
/// file sync tasks for its contents).
#[derive(Debug, Clone)]
pub enum SyncTask {
    /// Sync a directory: load doc, compare entries with local/snapshot, spawn child tasks
    SyncDirectory {
        /// Path relative to sync root ("" for root)
        relative_path: PathBuf,
        /// URL of the directory document
        url: AutomergeUrl,
    },

    /// Sync a tracked file: compare local/remote, push/pull/merge as needed
    SyncFile {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// URL of the file document
        url: AutomergeUrl,
        /// Document heads from snapshot (for detecting remote changes)
        snapshot_heads: Vec<ChangeHash>,
        /// Snapshot entry for this file (for metadata)
        snapshot_entry: SnapshotFileEntry,
    },

    /// Fetch a new remote file (exists on server, not in local snapshot)
    FetchNewFile {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// URL of the file document
        url: AutomergeUrl,
    },

    /// Push a new local file (exists locally, not on server)
    PushNewFile {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// Absolute path on disk
        absolute_path: PathBuf,
    },

    /// Fetch a new remote directory (exists on server, not locally)
    FetchNewDirectory {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// URL of the directory document
        url: AutomergeUrl,
    },

    /// Push a new local directory (exists locally, not on server)
    PushNewDirectory {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// Absolute path on disk
        absolute_path: PathBuf,
    },

    /// Delete a file from the remote (file was deleted locally)
    DeleteRemoteFile {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// URL of the file document (to check for remote modifications)
        url: AutomergeUrl,
        /// Snapshot heads for this file (to detect remote changes)
        snapshot_heads: Vec<ChangeHash>,
    },

    /// Delete a local file (file was deleted remotely)
    DeleteLocalFile {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// Absolute path on disk
        absolute_path: PathBuf,
    },
}

impl SyncTask {
    /// Get the relative path for this task (for logging/display)
    pub fn path(&self) -> &PathBuf {
        match self {
            SyncTask::SyncDirectory { relative_path, .. } => relative_path,
            SyncTask::SyncFile { relative_path, .. } => relative_path,
            SyncTask::FetchNewFile { relative_path, .. } => relative_path,
            SyncTask::PushNewFile { relative_path, .. } => relative_path,
            SyncTask::FetchNewDirectory { relative_path, .. } => relative_path,
            SyncTask::PushNewDirectory { relative_path, .. } => relative_path,
            SyncTask::DeleteRemoteFile { relative_path, .. } => relative_path,
            SyncTask::DeleteLocalFile { relative_path, .. } => relative_path,
        }
    }

    /// Get a human-readable description of this task
    pub fn description(&self) -> String {
        let path = self.path().display();
        match self {
            SyncTask::SyncDirectory { .. } => format!("sync directory: {}", path),
            SyncTask::SyncFile { .. } => format!("sync file: {}", path),
            SyncTask::FetchNewFile { .. } => format!("fetch new file: {}", path),
            SyncTask::PushNewFile { .. } => format!("push new file: {}", path),
            SyncTask::FetchNewDirectory { .. } => format!("fetch new directory: {}", path),
            SyncTask::PushNewDirectory { .. } => format!("push new directory: {}", path),
            SyncTask::DeleteRemoteFile { .. } => format!("delete remote file: {}", path),
            SyncTask::DeleteLocalFile { .. } => format!("delete local file: {}", path),
        }
    }
}

// =============================================================================
// Sync Results
// =============================================================================

/// The result of processing a sync task.
///
/// Used to track what happened for the final summary.
#[derive(Debug, Clone)]
pub enum SyncResult {
    /// Pushed a new file to the server
    PushedNew { path: String },

    /// Pushed modifications to an existing file
    PushedModified { path: String },

    /// Pulled a file from the server
    Pulled { path: String },

    /// Merged local and remote changes (CRDT merge)
    Merged { path: String },

    /// Created a new directory document
    CreatedDirectory { path: String },

    /// Deleted a file remotely (was deleted locally)
    DeletedRemote { path: String },

    /// Deleted a file locally (was deleted remotely)
    DeletedLocal { path: String },

    /// File was deleted locally but modified remotely - restored it
    Restored { path: String },

    /// No changes needed for this file
    NoChange { path: String },

    /// An error occurred processing this task
    Error { path: String, message: String },
}

impl SyncResult {
    /// Check if this result represents a successful change (not NoChange or Error)
    pub fn is_change(&self) -> bool {
        !matches!(self, SyncResult::NoChange { .. } | SyncResult::Error { .. })
    }

    /// Get the path associated with this result
    pub fn path(&self) -> &str {
        match self {
            SyncResult::PushedNew { path } => path,
            SyncResult::PushedModified { path } => path,
            SyncResult::Pulled { path } => path,
            SyncResult::Merged { path } => path,
            SyncResult::CreatedDirectory { path } => path,
            SyncResult::DeletedRemote { path } => path,
            SyncResult::DeletedLocal { path } => path,
            SyncResult::Restored { path } => path,
            SyncResult::NoChange { path } => path,
            SyncResult::Error { path, .. } => path,
        }
    }
}

// =============================================================================
// Sync Context
// =============================================================================

/// Shared context for sync operations.
///
/// Contains all the state needed by sync tasks. Uses Arc for sharing between
/// parallel tasks and Mutex for mutable state (snapshot).
#[derive(Clone)]
pub struct SyncContext {
    /// The automerge repo
    pub repo: Repo,

    /// Connection ID for the sync server
    pub conn_id: ConnectionId,

    /// Root path on the filesystem
    pub root_path: PathBuf,

    /// URL of the root directory document
    pub root_url: AutomergeUrl,

    /// Patterns to exclude from sync
    pub exclude_patterns: Vec<String>,

    /// The snapshot (wrapped in mutex for concurrent updates)
    pub snapshot: Arc<Mutex<Snapshot>>,
}

impl SyncContext {
    /// Create a new sync context
    pub fn new(
        repo: Repo,
        conn_id: ConnectionId,
        root_path: PathBuf,
        root_url: AutomergeUrl,
        exclude_patterns: Vec<String>,
        snapshot: Snapshot,
    ) -> Self {
        Self {
            repo,
            conn_id,
            root_path,
            root_url,
            exclude_patterns,
            snapshot: Arc::new(Mutex::new(snapshot)),
        }
    }

    /// Get the absolute path for a relative path
    pub fn absolute_path(&self, relative_path: &PathBuf) -> PathBuf {
        if relative_path.as_os_str().is_empty() {
            self.root_path.clone()
        } else {
            self.root_path.join(relative_path)
        }
    }
}

// =============================================================================
// Sync Summary
// =============================================================================

/// Summary of all sync results for display
#[derive(Debug, Default)]
pub struct SyncSummary {
    pub pushed_new: usize,
    pub pushed_modified: usize,
    pub pulled: usize,
    pub merged: usize,
    pub created_directories: usize,
    pub deleted_remote: usize,
    pub deleted_local: usize,
    pub restored: usize,
    pub errors: Vec<(String, String)>, // (path, message)
}

impl SyncSummary {
    /// Create a summary from a list of results
    pub fn from_results(results: &[SyncResult]) -> Self {
        let mut summary = Self::default();

        for result in results {
            match result {
                SyncResult::PushedNew { .. } => summary.pushed_new += 1,
                SyncResult::PushedModified { .. } => summary.pushed_modified += 1,
                SyncResult::Pulled { .. } => summary.pulled += 1,
                SyncResult::Merged { .. } => summary.merged += 1,
                SyncResult::CreatedDirectory { .. } => summary.created_directories += 1,
                SyncResult::DeletedRemote { .. } => summary.deleted_remote += 1,
                SyncResult::DeletedLocal { .. } => summary.deleted_local += 1,
                SyncResult::Restored { .. } => summary.restored += 1,
                SyncResult::NoChange { .. } => {}
                SyncResult::Error { path, message } => {
                    summary.errors.push((path.clone(), message.clone()));
                }
            }
        }

        summary
    }

    /// Check if any changes were made
    pub fn has_changes(&self) -> bool {
        self.pushed_new > 0
            || self.pushed_modified > 0
            || self.pulled > 0
            || self.merged > 0
            || self.created_directories > 0
            || self.deleted_remote > 0
            || self.deleted_local > 0
            || self.restored > 0
    }

    /// Print the summary to stdout
    pub fn print(&self) {
        if !self.has_changes() && self.errors.is_empty() {
            println!("No changes.");
            return;
        }

        let total_pushed = self.pushed_new + self.pushed_modified;
        if total_pushed > 0 {
            let mut parts = Vec::new();
            if self.pushed_new > 0 {
                parts.push(format!("{} new", self.pushed_new));
            }
            if self.pushed_modified > 0 {
                parts.push(format!("{} modified", self.pushed_modified));
            }
            println!("\nPushed {} file(s) ({}).", total_pushed, parts.join(", "));
        }

        if self.pulled > 0 {
            println!("Pulled {} file(s).", self.pulled);
        }

        if self.merged > 0 {
            println!(
                "Merged {} file(s) (changed locally and remotely).",
                self.merged
            );
        }

        if self.created_directories > 0 {
            println!("Created {} directory(s).", self.created_directories);
        }

        if self.deleted_remote > 0 {
            println!("Deleted {} file(s) remotely.", self.deleted_remote);
        }

        if self.deleted_local > 0 {
            println!("Deleted {} file(s) locally.", self.deleted_local);
        }

        if self.restored > 0 {
            println!(
                "Restored {} file(s) (deleted locally but modified remotely).",
                self.restored
            );
        }

        for (path, message) in &self.errors {
            eprintln!("Error syncing {}: {}", path, message);
        }
    }
}

// =============================================================================
// Task Processing
// =============================================================================

/// Output of processing a single task
pub struct TaskOutput {
    /// Results from this task (for summary)
    pub results: Vec<SyncResult>,
    /// New tasks spawned by this task
    pub new_tasks: Vec<SyncTask>,
    /// Handles that need sync confirmation before completing
    pub handles_to_sync: Vec<DocHandle>,
}

impl TaskOutput {
    fn empty() -> Self {
        Self {
            results: Vec::new(),
            new_tasks: Vec::new(),
            handles_to_sync: Vec::new(),
        }
    }

    fn result(result: SyncResult) -> Self {
        Self {
            results: vec![result],
            new_tasks: Vec::new(),
            handles_to_sync: Vec::new(),
        }
    }

    fn with_tasks(results: Vec<SyncResult>, new_tasks: Vec<SyncTask>) -> Self {
        Self {
            results,
            new_tasks,
            handles_to_sync: Vec::new(),
        }
    }
}

// =============================================================================
// Parallel Task Runner
// =============================================================================

/// Run the sync process starting from the root directory
///
/// Uses `FuturesUnordered` to process tasks in parallel. As tasks complete,
/// their spawned tasks are added to the queue. This allows:
/// - Parallel processing of independent tasks
/// - Incremental discovery of nested directories
/// - Efficient handling of deep hierarchies
pub async fn run_sync(ctx: &SyncContext) -> Vec<SyncResult> {
    let mut all_results = Vec::new();
    let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();

    // Start with the root directory
    let initial_task = SyncTask::SyncDirectory {
        relative_path: PathBuf::new(),
        url: ctx.root_url.clone(),
    };

    tasks.push(process_task(initial_task, ctx));

    // Process tasks as they complete
    while let Some(output) = tasks.next().await {
        // Collect results
        all_results.extend(output.results);

        // Add new tasks to the queue
        for task in output.new_tasks {
            tasks.push(process_task(task, ctx));
        }
    }

    all_results
}

/// Run clone process - fetch-only mode starting from root directory
///
/// Similar to run_sync but only fetches remote files/directories without
/// comparing to local state (since this is a fresh clone).
pub async fn run_clone(ctx: &SyncContext) -> Vec<SyncResult> {
    let mut all_results = Vec::new();
    let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();

    // Start by fetching the root directory contents
    let initial_task = CloneTask::FetchDirectory {
        relative_path: PathBuf::new(),
        url: ctx.root_url.clone(),
    };

    tasks.push(process_clone_task(initial_task, ctx));

    // Process tasks as they complete
    while let Some(output) = tasks.next().await {
        // Collect results
        all_results.extend(output.results);

        // Add new tasks to the queue
        for task in output.new_tasks {
            tasks.push(process_clone_task(task, ctx));
        }
    }

    all_results
}

/// Task type for clone operations (fetch-only)
#[derive(Debug, Clone)]
pub enum CloneTask {
    /// Fetch a directory and spawn tasks for its contents
    FetchDirectory {
        relative_path: PathBuf,
        url: AutomergeUrl,
    },
    /// Fetch a file and write to disk
    FetchFile {
        relative_path: PathBuf,
        url: AutomergeUrl,
    },
}

/// Output of processing a clone task
pub struct CloneTaskOutput {
    pub results: Vec<SyncResult>,
    pub new_tasks: Vec<CloneTask>,
}

/// Process a clone task
async fn process_clone_task(task: CloneTask, ctx: &SyncContext) -> CloneTaskOutput {
    match task {
        CloneTask::FetchDirectory { relative_path, url } => {
            process_clone_directory(relative_path, url, ctx).await
        }
        CloneTask::FetchFile { relative_path, url } => {
            process_clone_file(relative_path, url, ctx).await
        }
    }
}

/// Process FetchDirectory for clone: fetch directory doc, spawn tasks for contents
async fn process_clone_directory(
    relative_path: PathBuf,
    url: AutomergeUrl,
    ctx: &SyncContext,
) -> CloneTaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    // Create local directory if it doesn't exist
    if !abs_path.exists() {
        if let Err(e) = std::fs::create_dir_all(&abs_path) {
            return CloneTaskOutput {
                results: vec![SyncResult::Error {
                    path: path_str,
                    message: format!("Failed to create directory: {}", e),
                }],
                new_tasks: Vec::new(),
            };
        }
    }

    // Load directory document
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return CloneTaskOutput {
                results: vec![SyncResult::Error {
                    path: path_str,
                    message: "Directory document not found".into(),
                }],
                new_tasks: Vec::new(),
            };
        }
        Err(_) => {
            return CloneTaskOutput {
                results: vec![SyncResult::Error {
                    path: path_str,
                    message: "Repo stopped".into(),
                }],
                new_tasks: Vec::new(),
            };
        }
    };

    // Wait for remote changes
    handle.we_have_their_changes(ctx.conn_id).await;

    // Get directory entries from document
    let entries: Vec<(String, String, AutomergeUrl)> = handle.with_document(|doc| {
        let dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        dir_doc
            .docs
            .iter()
            .map(|e| {
                (
                    e.name_str().to_string(),
                    e.entry_type_str().to_string(),
                    e.url_str().parse().expect("Invalid URL in directory entry"),
                )
            })
            .collect()
    });

    // Spawn tasks for each entry
    let mut new_tasks = Vec::new();
    for (name, entry_type, entry_url) in entries {
        // Skip excluded files
        if scanner::is_excluded(&name, &ctx.exclude_patterns) {
            continue;
        }

        let child_relative = if relative_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            relative_path.join(&name)
        };

        if entry_type == "folder" {
            new_tasks.push(CloneTask::FetchDirectory {
                relative_path: child_relative,
                url: entry_url,
            });
        } else {
            new_tasks.push(CloneTask::FetchFile {
                relative_path: child_relative,
                url: entry_url,
            });
        }
    }

    // Update snapshot with directory info
    let dir_heads = sync_ops::get_document_heads(&handle);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_directory(
        path_str.clone(),
        SnapshotDirectoryEntry {
            path: abs_path,
            url,
            head: dir_heads,
            entries: new_tasks
                .iter()
                .map(|t| match t {
                    CloneTask::FetchDirectory { relative_path, .. }
                    | CloneTask::FetchFile { relative_path, .. } => relative_path
                        .file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default(),
                })
                .collect(),
        },
    );
    drop(snapshot);

    CloneTaskOutput {
        results: Vec::new(),
        new_tasks,
    }
}

/// Process FetchFile for clone: fetch file doc and write to disk
async fn process_clone_file(
    relative_path: PathBuf,
    url: AutomergeUrl,
    ctx: &SyncContext,
) -> CloneTaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    // Load file document
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return CloneTaskOutput {
                results: vec![SyncResult::Error {
                    path: path_str,
                    message: "File document not found".into(),
                }],
                new_tasks: Vec::new(),
            };
        }
        Err(_) => {
            return CloneTaskOutput {
                results: vec![SyncResult::Error {
                    path: path_str,
                    message: "Repo stopped".into(),
                }],
                new_tasks: Vec::new(),
            };
        }
    };

    // Wait for content
    handle.we_have_their_changes(ctx.conn_id).await;

    // Get file info from document
    let (extension, mime_type, content, permissions) = handle.with_document(|doc| {
        let file_doc: FileDocument = hydrate(doc).expect("Failed to hydrate file");
        (
            file_doc.extension_str().to_string(),
            file_doc.mime_type_str().to_string(),
            file_doc.content_bytes().to_vec(),
            file_doc.metadata.permissions,
        )
    });

    // Ensure parent directory exists
    if let Some(parent) = abs_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return CloneTaskOutput {
                results: vec![SyncResult::Error {
                    path: path_str,
                    message: format!("Failed to create parent directory: {}", e),
                }],
                new_tasks: Vec::new(),
            };
        }
    }

    // Write content to disk
    if let Err(e) = std::fs::write(&abs_path, &content) {
        return CloneTaskOutput {
            results: vec![SyncResult::Error {
                path: path_str,
                message: format!("Failed to write file: {}", e),
            }],
            new_tasks: Vec::new(),
        };
    }

    // Set file permissions (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(permissions as u32);
        if let Err(e) = std::fs::set_permissions(&abs_path, perms) {
            eprintln!(
                "  Warning: Failed to set permissions for '{}': {}",
                path_str, e
            );
        }
    }

    println!("  Cloned: {}", path_str);

    // Add to snapshot
    let heads = sync_ops::get_document_heads(&handle);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_file(
        path_str.clone(),
        SnapshotFileEntry {
            path: abs_path,
            url,
            head: heads,
            extension,
            mime_type,
        },
    );
    drop(snapshot);

    CloneTaskOutput {
        results: vec![SyncResult::Pulled { path: path_str }],
        new_tasks: Vec::new(),
    }
}

/// Process a single sync task
///
/// Returns the results of processing and any new tasks to spawn.
pub async fn process_task(task: SyncTask, ctx: &SyncContext) -> TaskOutput {
    match task {
        SyncTask::SyncDirectory { relative_path, url } => {
            process_sync_directory(relative_path, url, ctx).await
        }
        SyncTask::SyncFile {
            relative_path,
            url,
            snapshot_heads,
            snapshot_entry,
        } => process_sync_file(relative_path, url, snapshot_heads, snapshot_entry, ctx).await,
        SyncTask::FetchNewFile { relative_path, url } => {
            process_fetch_new_file(relative_path, url, ctx).await
        }
        SyncTask::PushNewFile {
            relative_path,
            absolute_path,
        } => process_push_new_file(relative_path, absolute_path, ctx).await,
        SyncTask::FetchNewDirectory { relative_path, url } => {
            process_fetch_new_directory(relative_path, url, ctx).await
        }
        SyncTask::PushNewDirectory {
            relative_path,
            absolute_path,
        } => process_push_new_directory(relative_path, absolute_path, ctx).await,
        SyncTask::DeleteRemoteFile {
            relative_path,
            url,
            snapshot_heads,
        } => process_delete_remote_file(relative_path, url, snapshot_heads, ctx).await,
        SyncTask::DeleteLocalFile {
            relative_path,
            absolute_path,
        } => process_delete_local_file(relative_path, absolute_path, ctx).await,
    }
}

/// Process SyncDirectory: compare local vs remote entries, spawn child tasks
async fn process_sync_directory(
    relative_path: PathBuf,
    url: AutomergeUrl,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    // Load directory document
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Directory document not found".into(),
            });
        }
        Err(_) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Repo stopped".into(),
            });
        }
    };

    // Wait for remote changes
    handle.we_have_their_changes(ctx.conn_id).await;

    // Get directory entries from document
    let remote_entries: Vec<(String, String, AutomergeUrl)> = handle.with_document(|doc| {
        let dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        dir_doc
            .docs
            .iter()
            .map(|e| {
                (
                    e.name_str().to_string(),
                    e.entry_type_str().to_string(),
                    e.url_str().parse().expect("Invalid URL in directory entry"),
                )
            })
            .collect()
    });

    // Get snapshot for this directory
    let snapshot = ctx.snapshot.lock().await;
    let snapshot_dir = snapshot.get_directory(&abs_path);
    let snapshot_entries: HashSet<String> = snapshot_dir
        .map(|d| d.entries.iter().cloned().collect())
        .unwrap_or_default();
    drop(snapshot);

    // Get local filesystem entries
    let local_entries = match std::fs::read_dir(&abs_path) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
            .filter(|(name, _)| !scanner::is_excluded(name, &ctx.exclude_patterns))
            .collect::<Vec<_>>(),
        Err(e) => {
            // Directory might not exist locally (new remote directory)
            if e.kind() == std::io::ErrorKind::NotFound {
                Vec::new()
            } else {
                return TaskOutput::result(SyncResult::Error {
                    path: path_str,
                    message: format!("Failed to read directory: {}", e),
                });
            }
        }
    };

    let local_names: HashSet<String> = local_entries.iter().map(|(n, _)| n.clone()).collect();
    let remote_names: HashSet<String> = remote_entries.iter().map(|(n, _, _)| n.clone()).collect();

    let mut new_tasks = Vec::new();

    // Process remote entries
    for (name, entry_type, entry_url) in &remote_entries {
        let child_relative = if relative_path.as_os_str().is_empty() {
            PathBuf::from(name)
        } else {
            relative_path.join(name)
        };

        let in_snapshot = snapshot_entries.contains(name);
        let exists_locally = local_names.contains(name);

        if entry_type == "folder" {
            // It's a directory
            if exists_locally || in_snapshot {
                // Known directory - sync it
                new_tasks.push(SyncTask::SyncDirectory {
                    relative_path: child_relative,
                    url: entry_url.clone(),
                });
            } else {
                // New remote directory - fetch it
                new_tasks.push(SyncTask::FetchNewDirectory {
                    relative_path: child_relative,
                    url: entry_url.clone(),
                });
            }
        } else {
            // It's a file
            if in_snapshot {
                // Tracked file - sync it
                let child_abs = ctx.absolute_path(&child_relative);
                let snapshot = ctx.snapshot.lock().await;
                if let Some(file_entry) = snapshot.get_file(&child_abs) {
                    new_tasks.push(SyncTask::SyncFile {
                        relative_path: child_relative,
                        url: entry_url.clone(),
                        snapshot_heads: file_entry.head.clone(),
                        snapshot_entry: file_entry.clone(),
                    });
                }
                drop(snapshot);
            } else {
                // New remote file - fetch it
                new_tasks.push(SyncTask::FetchNewFile {
                    relative_path: child_relative,
                    url: entry_url.clone(),
                });
            }
        }
    }

    // Process local-only entries (not in remote) - create documents and add to this directory
    let mut new_entries: Vec<(String, String, AutomergeUrl)> = Vec::new(); // (name, type, url)
    let mut results = Vec::new();

    for (name, local_path) in &local_entries {
        if !remote_names.contains(name) {
            let child_relative = if relative_path.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                relative_path.join(name)
            };

            if local_path.is_dir() {
                // Create directory document immediately and add to current directory
                println!("  Creating directory: {}", child_relative.display());
                match sync_ops::create_directory_document(&ctx.repo).await {
                    Ok(created) => {
                        created.handle.they_have_our_changes(ctx.conn_id).await;
                        new_entries.push((name.clone(), "folder".into(), created.url.clone()));

                        // Spawn task to process contents of this new directory
                        new_tasks.push(SyncTask::SyncDirectory {
                            relative_path: child_relative.clone(),
                            url: created.url.clone(),
                        });

                        // Add directory to snapshot
                        let dir_heads = sync_ops::get_document_heads(&created.handle);
                        let mut snapshot = ctx.snapshot.lock().await;
                        snapshot.add_directory(
                            child_relative.display().to_string(),
                            SnapshotDirectoryEntry {
                                path: local_path.clone(),
                                url: created.url,
                                head: dir_heads,
                                entries: Vec::new(),
                            },
                        );
                        drop(snapshot);

                        results.push(SyncResult::CreatedDirectory {
                            path: child_relative.display().to_string(),
                        });
                    }
                    Err(e) => {
                        results.push(SyncResult::Error {
                            path: child_relative.display().to_string(),
                            message: format!("Failed to create directory: {}", e),
                        });
                    }
                }
            } else if local_path.is_file() {
                // Create file document immediately and add to current directory
                let file_info = FileInfo::from_path(local_path);
                let file_type = if file_info.is_text { "text" } else { "binary" };
                println!("  Pushing ({}): {}", file_type, child_relative.display());

                match sync_ops::create_file_document(&ctx.repo, local_path, &file_info).await {
                    Ok(created) => {
                        created.handle.they_have_our_changes(ctx.conn_id).await;
                        new_entries.push((name.clone(), "file".into(), created.url.clone()));

                        // Add file to snapshot
                        let heads = sync_ops::get_document_heads(&created.handle);
                        let mut snapshot = ctx.snapshot.lock().await;
                        snapshot.add_file(
                            child_relative.display().to_string(),
                            SnapshotFileEntry {
                                path: local_path.clone(),
                                url: created.url,
                                head: heads,
                                extension: file_info.extension.clone(),
                                mime_type: file_info.mime_type.clone(),
                            },
                        );
                        drop(snapshot);

                        results.push(SyncResult::PushedNew {
                            path: child_relative.display().to_string(),
                        });
                    }
                    Err(e) => {
                        results.push(SyncResult::Error {
                            path: child_relative.display().to_string(),
                            message: format!("Failed to create file: {}", e),
                        });
                    }
                }
            }
        }
    }

    // Detect deletions: files in snapshot but not locally or not in remote
    // We need to get snapshot file entries for this directory
    let snapshot = ctx.snapshot.lock().await;
    let snapshot_files: Vec<(String, SnapshotFileEntry)> = snapshot
        .files
        .iter()
        .filter(|(path, _)| {
            // Get the parent directory of this file
            let file_path = PathBuf::from(path);
            let parent = file_path.parent().map(|p| p.to_string_lossy().to_string());
            parent.as_deref() == Some(&path_str)
                || (path_str.is_empty() && !path.contains('/') && !path.contains('\\'))
        })
        .map(|(path, entry)| {
            let name = PathBuf::from(path)
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            (name, entry.clone())
        })
        .collect();
    drop(snapshot);

    // Track entries to remove from directory document (local deletions)
    let mut entries_to_remove: Vec<String> = Vec::new();

    for (name, file_entry) in snapshot_files {
        let exists_locally = local_names.contains(&name);
        let exists_in_remote = remote_names.contains(&name);

        let child_relative = if relative_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            relative_path.join(&name)
        };

        if !exists_locally && exists_in_remote {
            // File was deleted locally but exists remotely
            // Spawn task to handle deletion (may need to check for remote modifications)
            new_tasks.push(SyncTask::DeleteRemoteFile {
                relative_path: child_relative,
                url: file_entry.url.clone(),
                snapshot_heads: file_entry.head.clone(),
            });
            entries_to_remove.push(name.clone());
        } else if !exists_locally && !exists_in_remote {
            // File was deleted both locally and remotely - just clean up snapshot
            println!("  Cleaning up: {} (deleted)", child_relative.display());
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_file(&file_entry.path);
            drop(snapshot);
        } else if exists_locally && !exists_in_remote {
            // File exists locally but not in remote - this is a remote deletion
            let abs_path = ctx.absolute_path(&child_relative);
            new_tasks.push(SyncTask::DeleteLocalFile {
                relative_path: child_relative,
                absolute_path: abs_path,
            });
        }
    }

    // Update this directory document: add new entries and remove deleted entries
    if !new_entries.is_empty() || !entries_to_remove.is_empty() {
        // Load current document and modify entries
        handle.with_document(|doc| {
            let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");

            // Add new entries
            for (name, entry_type, entry_url) in &new_entries {
                if entry_type == "folder" {
                    dir_doc
                        .docs
                        .push(crate::documents::DirectoryEntry::folder(
                            name.clone(),
                            entry_url.to_string(),
                        ));
                } else {
                    dir_doc.docs.push(crate::documents::DirectoryEntry::file(
                        name.clone(),
                        entry_url.to_string(),
                    ));
                }
            }

            // Remove deleted entries
            for name in &entries_to_remove {
                dir_doc.docs.retain(|entry| entry.name_str() != name);
            }

            doc.transact::<_, _, automerge::AutomergeError>(|txn| {
                autosurgeon::reconcile(txn, &dir_doc).expect("Failed to reconcile directory");
                Ok(())
            })
            .expect("Transaction failed");
        });

        // Wait for this directory update to sync
        handle.they_have_our_changes(ctx.conn_id).await;
    }

    // Update snapshot with current directory state
    let dir_heads = sync_ops::get_document_heads(&handle);
    let mut all_entry_names: Vec<String> = remote_entries
        .iter()
        .filter(|(n, _, _)| !entries_to_remove.contains(n))
        .map(|(n, _, _)| n.clone())
        .collect();
    all_entry_names.extend(new_entries.iter().map(|(n, _, _)| n.clone()));

    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_directory(
        path_str,
        SnapshotDirectoryEntry {
            path: abs_path,
            url: url.clone(),
            head: dir_heads,
            entries: all_entry_names,
        },
    );
    drop(snapshot);

    TaskOutput::with_tasks(results, new_tasks)
}

/// Process SyncFile: compare local/remote, push/pull/merge as needed
async fn process_sync_file(
    relative_path: PathBuf,
    url: AutomergeUrl,
    snapshot_heads: Vec<ChangeHash>,
    snapshot_entry: SnapshotFileEntry,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    // Load document
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "File document not found".into(),
            });
        }
        Err(_) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Repo stopped".into(),
            });
        }
    };

    // Wait for remote changes
    handle.we_have_their_changes(ctx.conn_id).await;

    // Check for remote changes
    let current_heads = sync_ops::get_document_heads(&handle);
    let has_remote_changes = current_heads != snapshot_heads;

    // Check for local changes
    let local_content = match std::fs::read(&abs_path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File was deleted locally - could handle deletion, for now just skip
            return TaskOutput::result(SyncResult::NoChange { path: path_str });
        }
        Err(e) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to read file: {}", e),
            });
        }
    };

    let snapshot_content = match sync_ops::get_file_content_at_heads(&handle, &snapshot_heads) {
        Ok(c) => c,
        Err(e) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to get snapshot content: {}", e),
            });
        }
    };

    let has_local_changes = local_content != snapshot_content;

    // Determine action
    let result = if has_local_changes && has_remote_changes {
        // Conflict - CRDT merge
        println!("  Merging: {} (changed locally and remotely)", path_str);

        let is_text = files::is_text_mime_type(&snapshot_entry.mime_type);
        let content = if is_text {
            FileContent::text(String::from_utf8_lossy(&local_content))
        } else {
            FileContent::binary(local_content)
        };

        let local_perms = files::get_file_permissions(&abs_path).ok().map(|p| p as i64);

        match sync_ops::merge_local_into_remote(&handle, &snapshot_heads, content, local_perms) {
            Ok(new_heads) => {
                // Write merged content to disk
                if let Err(e) = sync_ops::write_remote_file_to_disk(&handle, &abs_path) {
                    return TaskOutput::result(SyncResult::Error {
                        path: path_str,
                        message: format!("Failed to write merged content: {}", e),
                    });
                }

                // Update snapshot
                let mut snapshot = ctx.snapshot.lock().await;
                snapshot.update_file_heads(&abs_path, new_heads);
                drop(snapshot);

                SyncResult::Merged { path: path_str }
            }
            Err(e) => SyncResult::Error {
                path: path_str,
                message: format!("Merge failed: {}", e),
            },
        }
    } else if has_local_changes {
        // Push local changes
        println!("  Pushing: {}", path_str);

        let is_text = files::is_text_mime_type(&snapshot_entry.mime_type);
        let content = if is_text {
            FileContent::text(String::from_utf8_lossy(&local_content))
        } else {
            FileContent::binary(local_content)
        };

        let local_perms = files::get_file_permissions(&abs_path).ok().map(|p| p as i64);

        match sync_ops::update_file_document(&handle, content, local_perms) {
            Ok(new_heads) => {
                // Wait for sync
                handle.they_have_our_changes(ctx.conn_id).await;

                // Update snapshot
                let mut snapshot = ctx.snapshot.lock().await;
                snapshot.update_file_heads(&abs_path, new_heads);
                drop(snapshot);

                SyncResult::PushedModified { path: path_str }
            }
            Err(e) => SyncResult::Error {
                path: path_str,
                message: format!("Push failed: {}", e),
            },
        }
    } else if has_remote_changes {
        // Pull remote changes
        println!("  Pulling: {}", path_str);

        match sync_ops::write_remote_file_to_disk(&handle, &abs_path) {
            Ok(()) => {
                // Update snapshot
                let mut snapshot = ctx.snapshot.lock().await;
                snapshot.update_file_heads(&abs_path, current_heads);
                drop(snapshot);

                SyncResult::Pulled { path: path_str }
            }
            Err(e) => SyncResult::Error {
                path: path_str,
                message: format!("Pull failed: {}", e),
            },
        }
    } else {
        SyncResult::NoChange { path: path_str }
    };

    TaskOutput::result(result)
}

/// Process FetchNewFile: pull new remote file to disk
async fn process_fetch_new_file(
    relative_path: PathBuf,
    url: AutomergeUrl,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    println!("  Fetching: {}", path_str);

    // Load document
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "File document not found".into(),
            });
        }
        Err(_) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Repo stopped".into(),
            });
        }
    };

    // Wait for content
    handle.we_have_their_changes(ctx.conn_id).await;

    // Get file info from document
    let (extension, mime_type) = handle.with_document(|doc| {
        let file_doc: FileDocument = hydrate(doc).expect("Failed to hydrate file");
        (
            file_doc.extension_str().to_string(),
            file_doc.mime_type_str().to_string(),
        )
    });

    // Ensure parent directory exists
    if let Some(parent) = abs_path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to create parent directory: {}", e),
            });
        }
    }

    // Write to disk
    if let Err(e) = sync_ops::write_remote_file_to_disk(&handle, &abs_path) {
        return TaskOutput::result(SyncResult::Error {
            path: path_str,
            message: format!("Failed to write file: {}", e),
        });
    }

    // Add to snapshot
    let heads = sync_ops::get_document_heads(&handle);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_file(
        path_str.clone(),
        SnapshotFileEntry {
            path: abs_path,
            url,
            head: heads,
            extension,
            mime_type,
        },
    );
    drop(snapshot);

    TaskOutput::result(SyncResult::Pulled { path: path_str })
}

/// Process PushNewFile: create document and push to server
async fn process_push_new_file(
    relative_path: PathBuf,
    absolute_path: PathBuf,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();

    let file_info = FileInfo::from_path(&absolute_path);
    let file_type = if file_info.is_text { "text" } else { "binary" };
    println!("  Pushing ({}): {}", file_type, path_str);

    // Create document
    let created = match sync_ops::create_file_document(&ctx.repo, &absolute_path, &file_info).await
    {
        Ok(c) => c,
        Err(e) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to create document: {}", e),
            });
        }
    };

    // Wait for sync
    created.handle.they_have_our_changes(ctx.conn_id).await;

    // Add to snapshot
    let heads = sync_ops::get_document_heads(&created.handle);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_file(
        path_str.clone(),
        SnapshotFileEntry {
            path: absolute_path,
            url: created.url.clone(),
            head: heads,
            extension: file_info.extension,
            mime_type: file_info.mime_type,
        },
    );
    drop(snapshot);

    // Return result with the handle and URL for parent directory update
    let mut output = TaskOutput::result(SyncResult::PushedNew { path: path_str });
    output.handles_to_sync.push(created.handle);
    output
}

/// Process FetchNewDirectory: create local directory and spawn tasks for contents
async fn process_fetch_new_directory(
    relative_path: PathBuf,
    url: AutomergeUrl,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    println!("  Fetching directory: {}", path_str);

    // Create local directory
    if let Err(e) = std::fs::create_dir_all(&abs_path) {
        return TaskOutput::result(SyncResult::Error {
            path: path_str,
            message: format!("Failed to create directory: {}", e),
        });
    }

    // Now sync it like a regular directory
    process_sync_directory(relative_path, url, ctx).await
}

/// Process PushNewDirectory: create directory document and push contents
async fn process_push_new_directory(
    relative_path: PathBuf,
    absolute_path: PathBuf,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();

    println!("  Creating directory: {}", path_str);

    // Create directory document
    let created = match sync_ops::create_directory_document(&ctx.repo).await {
        Ok(c) => c,
        Err(e) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to create directory document: {}", e),
            });
        }
    };

    // Scan local directory for contents
    let local_entries = match std::fs::read_dir(&absolute_path) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
            .filter(|(name, _)| !scanner::is_excluded(name, &ctx.exclude_patterns))
            .collect::<Vec<_>>(),
        Err(e) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to read directory: {}", e),
            });
        }
    };

    // Create tasks for contents
    let mut new_tasks = Vec::new();
    for (name, local_path) in local_entries {
        let child_relative = relative_path.join(&name);

        if local_path.is_dir() {
            new_tasks.push(SyncTask::PushNewDirectory {
                relative_path: child_relative,
                absolute_path: local_path,
            });
        } else if local_path.is_file() {
            new_tasks.push(SyncTask::PushNewFile {
                relative_path: child_relative,
                absolute_path: local_path,
            });
        }
    }

    // Wait for sync
    created.handle.they_have_our_changes(ctx.conn_id).await;

    // Add to snapshot
    let heads = sync_ops::get_document_heads(&created.handle);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_directory(
        path_str.clone(),
        SnapshotDirectoryEntry {
            path: absolute_path,
            url: created.url.clone(),
            head: heads,
            entries: Vec::new(), // Will be populated as child tasks complete
        },
    );
    drop(snapshot);

    let mut output = TaskOutput::with_tasks(
        vec![SyncResult::CreatedDirectory { path: path_str }],
        new_tasks,
    );
    output.handles_to_sync.push(created.handle);
    output
}

/// Process DeleteRemoteFile: check for remote modifications, then remove from directory
///
/// If the file was modified remotely since our last sync, restore it locally instead
/// of deleting (remote modification wins over local deletion).
async fn process_delete_remote_file(
    relative_path: PathBuf,
    url: AutomergeUrl,
    snapshot_heads: Vec<ChangeHash>,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    // Load document to check for remote changes
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            // Document not found - already deleted remotely, just clean snapshot
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_file(&abs_path);
            drop(snapshot);
            return TaskOutput::result(SyncResult::NoChange { path: path_str });
        }
        Err(_) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Repo stopped".into(),
            });
        }
    };

    // Wait for remote changes
    handle.we_have_their_changes(ctx.conn_id).await;

    // Check if remote has changed since our snapshot
    let current_heads = sync_ops::get_document_heads(&handle);
    let has_remote_changes = current_heads != snapshot_heads;

    if has_remote_changes {
        // Remote was modified - restore the file locally (remote wins)
        println!(
            "  Restoring: {} (deleted locally but modified remotely)",
            path_str
        );

        // Ensure parent directory exists
        if let Some(parent) = abs_path.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return TaskOutput::result(SyncResult::Error {
                    path: path_str,
                    message: format!("Failed to create parent directory: {}", e),
                });
            }
        }

        // Write remote content to disk
        if let Err(e) = sync_ops::write_remote_file_to_disk(&handle, &abs_path) {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: format!("Failed to restore file: {}", e),
            });
        }

        // Update snapshot with current heads
        let mut snapshot = ctx.snapshot.lock().await;
        snapshot.update_file_heads(&abs_path, current_heads);
        drop(snapshot);

        TaskOutput::result(SyncResult::Restored { path: path_str })
    } else {
        // No remote changes - proceed with deletion
        println!("  Deleting remotely: {}", path_str);

        // Remove from snapshot
        let mut snapshot = ctx.snapshot.lock().await;
        snapshot.remove_file(&abs_path);
        drop(snapshot);

        // Note: We don't actually remove the entry from the directory document here.
        // The parent directory's process_sync_directory will handle removing entries
        // that are in `entries_to_remove` after processing all children.
        // For now, this task just handles the snapshot cleanup and reporting.

        TaskOutput::result(SyncResult::DeletedRemote { path: path_str })
    }
}

/// Process DeleteLocalFile: delete local file that was deleted remotely
async fn process_delete_local_file(
    _relative_path: PathBuf,
    absolute_path: PathBuf,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = absolute_path.display().to_string();

    println!("  Deleting locally: {}", path_str);

    // Delete the local file
    match std::fs::remove_file(&absolute_path) {
        Ok(()) => {
            // Remove from snapshot
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_file(&absolute_path);
            drop(snapshot);

            TaskOutput::result(SyncResult::DeletedLocal { path: path_str })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // File already gone - just clean up snapshot
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_file(&absolute_path);
            drop(snapshot);

            TaskOutput::result(SyncResult::NoChange { path: path_str })
        }
        Err(e) => TaskOutput::result(SyncResult::Error {
            path: path_str,
            message: format!("Failed to delete file: {}", e),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_result_is_change() {
        assert!(SyncResult::PushedNew {
            path: "a.txt".into()
        }
        .is_change());
        assert!(SyncResult::Pulled {
            path: "b.txt".into()
        }
        .is_change());
        assert!(!SyncResult::NoChange {
            path: "c.txt".into()
        }
        .is_change());
        assert!(!SyncResult::Error {
            path: "d.txt".into(),
            message: "fail".into()
        }
        .is_change());
    }

    #[test]
    fn test_sync_summary_from_results() {
        let results = vec![
            SyncResult::PushedNew {
                path: "a.txt".into(),
            },
            SyncResult::PushedNew {
                path: "b.txt".into(),
            },
            SyncResult::PushedModified {
                path: "c.txt".into(),
            },
            SyncResult::Pulled {
                path: "d.txt".into(),
            },
            SyncResult::Merged {
                path: "e.txt".into(),
            },
            SyncResult::NoChange {
                path: "f.txt".into(),
            },
            SyncResult::Error {
                path: "g.txt".into(),
                message: "oops".into(),
            },
        ];

        let summary = SyncSummary::from_results(&results);

        assert_eq!(summary.pushed_new, 2);
        assert_eq!(summary.pushed_modified, 1);
        assert_eq!(summary.pulled, 1);
        assert_eq!(summary.merged, 1);
        assert_eq!(summary.errors.len(), 1);
        assert!(summary.has_changes());
    }

    #[test]
    fn test_sync_summary_no_changes() {
        let results = vec![
            SyncResult::NoChange {
                path: "a.txt".into(),
            },
            SyncResult::NoChange {
                path: "b.txt".into(),
            },
        ];

        let summary = SyncSummary::from_results(&results);
        assert!(!summary.has_changes());
    }

    #[test]
    fn test_sync_task_path() {
        let task = SyncTask::SyncFile {
            relative_path: PathBuf::from("src/main.rs"),
            url: "automerge:550e8400-e29b-41d4-a716-446655440000"
                .parse()
                .unwrap(),
            snapshot_heads: vec![],
            snapshot_entry: SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/src/main.rs"),
                url: "automerge:550e8400-e29b-41d4-a716-446655440000"
                    .parse()
                    .unwrap(),
                head: vec![],
                extension: "rs".into(),
                mime_type: "text/x-rust".into(),
            },
        };

        assert_eq!(task.path(), &PathBuf::from("src/main.rs"));
    }
}
