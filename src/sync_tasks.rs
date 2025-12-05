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

use crate::documents::{DirectoryDocument, DirectoryEntry, FileContent, FileDocument};
use crate::files::{self, FileInfo};
use crate::move_detector::{self, DeletedFileCandidate, NewFileCandidate};
use crate::scanner;
use crate::snapshot::{Snapshot, SnapshotDirectoryEntry, SnapshotFileEntry};
use crate::sync_ops;

// =============================================================================
// Helper Functions
// =============================================================================

/// Parse directory entries from a DirectoryDocument into a Vec of (name, type, url) tuples.
fn parse_directory_entries(dir_doc: &DirectoryDocument) -> Vec<(String, String, AutomergeUrl)> {
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
}

// =============================================================================
// Collection Phase Types (for two-phase sync with cross-directory moves)
// =============================================================================

/// Information about a file that was deleted locally (exists in snapshot+remote, not on disk)
#[derive(Debug, Clone)]
pub struct DeletedFileInfo {
    /// Path relative to sync root (e.g., "src/foo.txt")
    pub relative_path: PathBuf,
    /// URL of the parent directory document
    pub dir_url: AutomergeUrl,
    /// URL of the file document
    pub file_url: AutomergeUrl,
    /// Snapshot entry for this file
    pub snapshot_entry: SnapshotFileEntry,
    /// Name of the file in the directory
    pub name: String,
}

/// Information about a new local file (exists on disk, not in snapshot or remote)
#[derive(Debug, Clone)]
pub struct NewLocalFileInfo {
    /// Path relative to sync root (e.g., "lib/foo.txt")
    pub relative_path: PathBuf,
    /// Absolute path on disk
    pub absolute_path: PathBuf,
    /// URL of the parent directory document
    pub dir_url: AutomergeUrl,
    /// Name of the file
    pub name: String,
}

/// Information about a file that needs syncing (exists in all three: local, remote, snapshot)
#[derive(Debug, Clone)]
pub struct SyncFileInfo {
    /// Path relative to sync root
    pub relative_path: PathBuf,
    /// URL of the file document
    pub file_url: AutomergeUrl,
    /// Snapshot entry for this file
    pub snapshot_entry: SnapshotFileEntry,
}

/// Information about a new remote file (exists in remote, not locally or in snapshot)
#[derive(Debug, Clone)]
pub struct NewRemoteFileInfo {
    /// Path relative to sync root
    pub relative_path: PathBuf,
    /// URL of the file document
    pub file_url: AutomergeUrl,
}

/// Information about a file deleted remotely (in snapshot, not in remote)
#[derive(Debug, Clone)]
pub struct RemotelyDeletedFileInfo {
    /// Path relative to sync root
    pub relative_path: PathBuf,
    /// Absolute path on disk
    pub absolute_path: PathBuf,
}

/// Pending updates to a directory document
#[derive(Debug, Clone)]
pub struct DirectoryUpdate {
    /// URL of the directory document
    pub dir_url: AutomergeUrl,
    /// Entries to add (name, type, url)
    pub entries_to_add: Vec<(String, String, AutomergeUrl)>,
    /// Entry names to remove
    pub entries_to_remove: Vec<String>,
    /// Snapshot update info
    pub relative_path: PathBuf,
    pub absolute_path: PathBuf,
    pub all_entry_names: Vec<String>,
}

/// A detected cross-directory move
#[derive(Debug, Clone)]
pub struct CrossDirectoryMove {
    /// Old path (e.g., "src/foo.txt")
    pub old_path: PathBuf,
    /// New path (e.g., "lib/foo.txt")
    pub new_path: PathBuf,
    /// The file document URL (preserved)
    pub file_url: AutomergeUrl,
    /// URL of the source directory (need to remove entry)
    pub old_dir_url: AutomergeUrl,
    /// URL of the destination directory (need to add entry)
    pub new_dir_url: AutomergeUrl,
    /// Old file name
    pub old_name: String,
    /// New file name
    pub new_name: String,
    /// Snapshot entry from the old location
    pub snapshot_entry: SnapshotFileEntry,
    /// Similarity score
    pub similarity: f64,
}

/// All collected changes from scanning the directory tree
#[derive(Debug, Default)]
pub struct CollectedChanges {
    /// Files deleted locally (candidates for move detection)
    pub deleted_files: Vec<DeletedFileInfo>,
    /// New local files (candidates for move detection)
    pub new_local_files: Vec<NewLocalFileInfo>,
    /// Files to sync (exist everywhere)
    pub files_to_sync: Vec<SyncFileInfo>,
    /// New remote files to fetch
    pub new_remote_files: Vec<NewRemoteFileInfo>,
    /// Files deleted remotely
    pub remotely_deleted_files: Vec<RemotelyDeletedFileInfo>,
    /// Directory updates to apply (keyed by directory URL string for merging)
    pub directory_updates: std::collections::HashMap<String, DirectoryUpdate>,
    /// New local directories to push
    pub new_local_directories: Vec<(PathBuf, PathBuf, AutomergeUrl)>, // (relative, absolute, parent_dir_url)
    /// New remote directories to fetch
    pub new_remote_directories: Vec<(PathBuf, AutomergeUrl)>, // (relative, dir_url)
    /// Directories deleted locally
    pub deleted_local_directories: Vec<(PathBuf, AutomergeUrl, Vec<ChangeHash>)>, // (relative, url, snapshot_heads)
    /// Directories deleted remotely
    pub deleted_remote_directories: Vec<PathBuf>,
}

impl CollectedChanges {
    /// Get or create a directory update entry
    pub fn get_or_create_dir_update(&mut self, dir_url: &AutomergeUrl, relative_path: PathBuf, absolute_path: PathBuf) -> &mut DirectoryUpdate {
        let key = dir_url.to_string();
        self.directory_updates.entry(key).or_insert_with(|| DirectoryUpdate {
            dir_url: dir_url.clone(),
            relative_path,
            absolute_path,
            entries_to_add: Vec::new(),
            entries_to_remove: Vec::new(),
            all_entry_names: Vec::new(),
        })
    }
}

// =============================================================================
// Two-Phase Sync Implementation
// =============================================================================

/// Scan a directory and collect changes without processing them.
/// This is Phase 1 of the two-phase sync.
async fn scan_directory_for_changes(
    relative_path: PathBuf,
    url: AutomergeUrl,
    ctx: &SyncContext,
    changes: &mut CollectedChanges,
    subdirs_to_scan: &mut Vec<(PathBuf, AutomergeUrl)>,
) -> Result<(), String> {
    let path_str = relative_path.display().to_string();
    let abs_path = ctx.absolute_path(&relative_path);

    // Load directory document
    let handle = match ctx.repo.find(url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => return Err(format!("Directory document not found: {}", path_str)),
        Err(_) => return Err("Repo stopped".into()),
    };

    // Wait for remote changes
    handle.we_have_their_changes(ctx.conn_id).await;

    // Get directory entries from document (filtering out any invalid entries)
    let remote_entries: Vec<(String, String, AutomergeUrl)> = handle.with_document(|doc| {
        let dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        parse_directory_entries(&dir_doc)
    });

    // Get snapshot for this directory
    let snapshot = ctx.snapshot.lock().await;
    let snapshot_dir = snapshot.get_directory(&abs_path);
    let snapshot_entries: HashSet<String> = snapshot_dir
        .map(|d| d.entries.iter().cloned().collect())
        .unwrap_or_default();
    drop(snapshot);

    // Get local filesystem entries
    let local_entries: Vec<(String, PathBuf)> = match std::fs::read_dir(&abs_path) {
        Ok(entries) => entries
            .filter_map(|e| e.ok())
            .map(|e| (e.file_name().to_string_lossy().to_string(), e.path()))
            .filter(|(name, _)| !scanner::is_excluded(name, &ctx.exclude_patterns))
            .collect(),
        Err(e) => {
            if e.kind() == std::io::ErrorKind::NotFound {
                Vec::new()
            } else {
                return Err(format!("Failed to read directory: {}", e));
            }
        }
    };

    let local_names: HashSet<String> = local_entries.iter().map(|(n, _)| n.clone()).collect();
    let remote_names: HashSet<String> = remote_entries.iter().map(|(n, _, _)| n.clone()).collect();

    // Initialize directory update for tracking entry names
    // We need to use the key approach to avoid holding a mutable borrow while modifying other parts of changes
    changes.get_or_create_dir_update(&url, relative_path.clone(), abs_path.clone());
    let url_key = url.to_string();
    if let Some(dir_update) = changes.directory_updates.get_mut(&url_key) {
        dir_update.all_entry_names = remote_entries.iter().map(|(n, _, _)| n.clone()).collect();
    }

    // We'll collect entries to remove and add them to the directory update at the end
    let mut entries_to_remove_from_dir: Vec<String> = Vec::new();

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
            if in_snapshot && !exists_locally {
                // Directory deleted locally - collect for later processing
                let snapshot = ctx.snapshot.lock().await;
                if let Some(dir_entry) = snapshot.get_directory(&ctx.absolute_path(&child_relative)) {
                    changes.deleted_local_directories.push((
                        child_relative,
                        entry_url.clone(),
                        dir_entry.head.clone(),
                    ));
                    // Mark for removal from parent directory document
                    entries_to_remove_from_dir.push(name.clone());
                }
                drop(snapshot);
            } else if exists_locally || in_snapshot {
                // Known directory - queue for scanning
                subdirs_to_scan.push((child_relative, entry_url.clone()));
            } else {
                // New remote directory
                changes.new_remote_directories.push((child_relative, entry_url.clone()));
            }
        } else {
            // It's a file
            if in_snapshot && exists_locally {
                // File exists everywhere - needs sync check
                let child_abs = ctx.absolute_path(&child_relative);
                let snapshot = ctx.snapshot.lock().await;
                if let Some(file_entry) = snapshot.get_file(&child_abs) {
                    changes.files_to_sync.push(SyncFileInfo {
                        relative_path: child_relative,
                        file_url: entry_url.clone(),
                        snapshot_entry: file_entry.clone(),
                    });
                }
                drop(snapshot);
            } else if in_snapshot && !exists_locally {
                // File deleted locally (candidate for move detection)
                let child_abs = ctx.absolute_path(&child_relative);
                let snapshot = ctx.snapshot.lock().await;
                if let Some(file_entry) = snapshot.get_file(&child_abs) {
                    changes.deleted_files.push(DeletedFileInfo {
                        relative_path: child_relative,
                        dir_url: url.clone(),
                        file_url: entry_url.clone(),
                        snapshot_entry: file_entry.clone(),
                        name: name.clone(),
                    });
                    // Mark for removal from directory document
                    entries_to_remove_from_dir.push(name.clone());
                }
                drop(snapshot);
            } else if !in_snapshot && !exists_locally {
                // New remote file
                changes.new_remote_files.push(NewRemoteFileInfo {
                    relative_path: child_relative,
                    file_url: entry_url.clone(),
                });
            }
            // Note: in_snapshot && exists_locally is handled above (files_to_sync)
        }
    }

    // Process local-only entries
    for (name, local_path) in &local_entries {
        if !remote_names.contains(name) && !snapshot_entries.contains(name) {
            // New local entry (not in remote, not in snapshot)
            let child_relative = if relative_path.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                relative_path.join(name)
            };

            if local_path.is_dir() {
                changes.new_local_directories.push((
                    child_relative,
                    local_path.clone(),
                    url.clone(),
                ));
            } else if local_path.is_file() {
                changes.new_local_files.push(NewLocalFileInfo {
                    relative_path: child_relative,
                    absolute_path: local_path.clone(),
                    dir_url: url.clone(),
                    name: name.clone(),
                });
            }
        } else if !remote_names.contains(name) && snapshot_entries.contains(name) {
            // File/dir in snapshot but not in remote = remotely deleted
            let child_relative = if relative_path.as_os_str().is_empty() {
                PathBuf::from(name)
            } else {
                relative_path.join(name)
            };

            if local_path.is_file() {
                changes.remotely_deleted_files.push(RemotelyDeletedFileInfo {
                    relative_path: child_relative,
                    absolute_path: local_path.clone(),
                });
            } else if local_path.is_dir() {
                changes.deleted_remote_directories.push(child_relative);
            }
        }
    }

    // Add collected entries_to_remove to the directory update
    if !entries_to_remove_from_dir.is_empty() {
        if let Some(dir_update) = changes.directory_updates.get_mut(&url_key) {
            dir_update.entries_to_remove.extend(entries_to_remove_from_dir);
        }
    }

    // Update snapshot with this directory's current state
    // This ensures the directory entry exists for subsequent syncs
    let dir_heads = sync_ops::get_document_heads(&handle);
    let all_entry_names: Vec<String> = remote_entries.iter().map(|(n, _, _)| n.clone()).collect();
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

    Ok(())
}

/// Detect cross-directory moves from collected changes.
/// Returns detected moves and updates the changes struct to remove matched files.
pub fn detect_cross_directory_moves(
    changes: &mut CollectedChanges,
    threshold: f64,
    ctx: &SyncContext,
) -> Vec<CrossDirectoryMove> {
    if changes.deleted_files.is_empty() || changes.new_local_files.is_empty() || threshold >= 1.0 {
        return Vec::new();
    }

    // Build candidates for move detection
    // We need to read file contents for comparison
    let mut deleted_with_content: Vec<(usize, DeletedFileCandidate)> = Vec::new();
    let mut new_with_content: Vec<(usize, NewFileCandidate)> = Vec::new();

    // For deleted files, we'll need to load content from the document later
    // For now, collect indices and read content synchronously for new files
    for (idx, new_file) in changes.new_local_files.iter().enumerate() {
        if let Ok(content) = std::fs::read(&new_file.absolute_path) {
            new_with_content.push((idx, NewFileCandidate {
                name: new_file.name.clone(),
                content,
            }));
        }
    }

    // Return empty if we couldn't read any new files
    if new_with_content.is_empty() {
        return Vec::new();
    }

    // For deleted files, we need the snapshot content
    // Since we can't easily get remote content here synchronously,
    // we'll use the snapshot path to try to get size hints
    // The actual content comparison will happen in the async phase

    // For now, return empty - we'll need to make this async
    // or pre-load deleted file content during scanning
    Vec::new()
}

/// Run two-phase sync: scan all directories, detect moves, then process changes
pub async fn run_sync_two_phase(ctx: &SyncContext) -> Vec<SyncResult> {
    // Phase 1: Scan all directories and collect changes
    let mut changes = CollectedChanges::default();
    let mut dirs_to_scan = vec![(PathBuf::new(), ctx.root_url.clone())];
    let mut scan_errors: Vec<SyncResult> = Vec::new();

    while let Some((relative_path, url)) = dirs_to_scan.pop() {
        let mut subdirs = Vec::new();
        match scan_directory_for_changes(relative_path, url, ctx, &mut changes, &mut subdirs).await {
            Ok(()) => {
                dirs_to_scan.extend(subdirs);
            }
            Err(e) => {
                scan_errors.push(SyncResult::Error {
                    path: "scan".into(),
                    message: e,
                });
            }
        }
    }

    // Phase 2: Detect cross-directory moves
    let moves = detect_cross_directory_moves_async(&mut changes, ctx).await;

    // Phase 3: Process all changes
    let mut results = scan_errors;

    // Process moves first
    for mv in moves {
        match process_cross_directory_move(&mv, ctx).await {
            Ok(result) => results.push(result),
            Err(e) => results.push(SyncResult::Error {
                path: mv.old_path.display().to_string(),
                message: e,
            }),
        }
    }

    // Process remaining changes using the task queue
    let remaining_tasks = build_tasks_from_changes(&changes, ctx).await;

    // Run remaining tasks in parallel
    let mut tasks: FuturesUnordered<_> = FuturesUnordered::new();
    for task in remaining_tasks {
        tasks.push(process_task(task, ctx));
    }

    while let Some(output) = tasks.next().await {
        results.extend(output.results);
        for task in output.new_tasks {
            tasks.push(process_task(task, ctx));
        }
    }

    // Apply directory updates
    for (_, dir_update) in changes.directory_updates {
        if let Err(e) = apply_directory_update(&dir_update, ctx).await {
            results.push(SyncResult::Error {
                path: dir_update.relative_path.display().to_string(),
                message: e,
            });
        }
    }

    results
}

/// Detect cross-directory moves asynchronously (can load file content from documents)
async fn detect_cross_directory_moves_async(
    changes: &mut CollectedChanges,
    ctx: &SyncContext,
) -> Vec<CrossDirectoryMove> {
    if changes.deleted_files.is_empty() || changes.new_local_files.is_empty() || ctx.move_threshold >= 1.0 {
        return Vec::new();
    }

    // Load content for deleted files from their documents
    let mut deleted_with_content: Vec<(usize, Vec<u8>)> = Vec::new();
    for (idx, deleted) in changes.deleted_files.iter().enumerate() {
        if let Ok(Some(handle)) = ctx.repo.find(deleted.file_url.doc_id().clone()).await {
            handle.we_have_their_changes(ctx.conn_id).await;
            let content = handle.with_document(|doc| {
                let file_doc: FileDocument = hydrate(doc).ok()?;
                Some(file_doc.content_bytes().to_vec())
            });
            if let Some(content) = content {
                deleted_with_content.push((idx, content));
            }
        }
    }

    // Load content for new files from disk
    let mut new_with_content: Vec<(usize, Vec<u8>)> = Vec::new();
    for (idx, new_file) in changes.new_local_files.iter().enumerate() {
        if let Ok(content) = std::fs::read(&new_file.absolute_path) {
            new_with_content.push((idx, content));
        }
    }

    if deleted_with_content.is_empty() || new_with_content.is_empty() {
        return Vec::new();
    }

    // Build candidates for move detection
    let deleted_candidates: Vec<DeletedFileCandidate> = deleted_with_content
        .iter()
        .map(|(idx, content)| DeletedFileCandidate {
            name: changes.deleted_files[*idx].name.clone(),
            content: content.clone(),
        })
        .collect();

    let new_candidates: Vec<NewFileCandidate> = new_with_content
        .iter()
        .map(|(idx, content)| NewFileCandidate {
            name: changes.new_local_files[*idx].name.clone(),
            content: content.clone(),
        })
        .collect();

    // Run move detection
    let detected = move_detector::detect_moves(&deleted_candidates, &new_candidates, ctx.move_threshold);

    // Build CrossDirectoryMove structs and track which files were matched
    let mut moves = Vec::new();
    let mut matched_deleted_indices: HashSet<usize> = HashSet::new();
    let mut matched_new_indices: HashSet<usize> = HashSet::new();

    for detected_move in detected {
        // Find the original indices
        let deleted_idx = deleted_with_content
            .iter()
            .position(|(idx, _)| changes.deleted_files[*idx].name == detected_move.old_name);
        let new_idx = new_with_content
            .iter()
            .position(|(idx, _)| changes.new_local_files[*idx].name == detected_move.new_name);

        if let (Some(del_pos), Some(new_pos)) = (deleted_idx, new_idx) {
            let del_orig_idx = deleted_with_content[del_pos].0;
            let new_orig_idx = new_with_content[new_pos].0;

            let deleted_info = &changes.deleted_files[del_orig_idx];
            let new_info = &changes.new_local_files[new_orig_idx];

            moves.push(CrossDirectoryMove {
                old_path: deleted_info.relative_path.clone(),
                new_path: new_info.relative_path.clone(),
                file_url: deleted_info.file_url.clone(),
                old_dir_url: deleted_info.dir_url.clone(),
                new_dir_url: new_info.dir_url.clone(),
                old_name: deleted_info.name.clone(),
                new_name: new_info.name.clone(),
                snapshot_entry: deleted_info.snapshot_entry.clone(),
                similarity: detected_move.similarity,
            });

            matched_deleted_indices.insert(del_orig_idx);
            matched_new_indices.insert(new_orig_idx);
        }
    }

    // Remove matched files from changes (they'll be handled as moves)
    // We need to remove in reverse order to preserve indices
    let mut deleted_to_remove: Vec<usize> = matched_deleted_indices.into_iter().collect();
    deleted_to_remove.sort_by(|a, b| b.cmp(a)); // Descending
    for idx in deleted_to_remove {
        changes.deleted_files.remove(idx);
    }

    let mut new_to_remove: Vec<usize> = matched_new_indices.into_iter().collect();
    new_to_remove.sort_by(|a, b| b.cmp(a)); // Descending
    for idx in new_to_remove {
        changes.new_local_files.remove(idx);
    }

    moves
}

/// Process a cross-directory move
async fn process_cross_directory_move(
    mv: &CrossDirectoryMove,
    ctx: &SyncContext,
) -> Result<SyncResult, String> {
    println!(
        "  Moving: {} -> {} (similarity: {:.0}%)",
        mv.old_path.display(),
        mv.new_path.display(),
        mv.similarity * 100.0
    );

    // Get file handle
    let handle = ctx
        .repo
        .find(mv.file_url.doc_id().clone())
        .await
        .map_err(|_| "Repo stopped")?
        .ok_or("File document not found")?;

    // Update file document name
    let new_extension = mv.new_path
        .extension()
        .map(|e| e.to_string_lossy().to_string())
        .unwrap_or_default();

    let new_heads = sync_ops::update_file_name(&handle, &mv.new_name, &new_extension)
        .map_err(|e| format!("Failed to update file name: {}", e))?;

    handle.they_have_our_changes(ctx.conn_id).await;

    // Update old directory (remove entry)
    if let Ok(Some(old_dir_handle)) = ctx.repo.find(mv.old_dir_url.doc_id().clone()).await {
        old_dir_handle.we_have_their_changes(ctx.conn_id).await;
        old_dir_handle.with_document(|doc| {
            let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
            dir_doc.docs.retain(|e| e.name_str() != mv.old_name);
            doc.transact::<_, _, automerge::AutomergeError>(|txn| {
                autosurgeon::reconcile(txn, &dir_doc).expect("Failed to reconcile directory");
                Ok(())
            })
            .expect("Transaction failed");
        });
        old_dir_handle.they_have_our_changes(ctx.conn_id).await;
    }

    // Update new directory (add entry)
    if let Ok(Some(new_dir_handle)) = ctx.repo.find(mv.new_dir_url.doc_id().clone()).await {
        new_dir_handle.we_have_their_changes(ctx.conn_id).await;
        new_dir_handle.with_document(|doc| {
            let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
            dir_doc.docs.push(DirectoryEntry::file(
                mv.new_name.clone(),
                mv.file_url.to_string(),
            ));
            doc.transact::<_, _, automerge::AutomergeError>(|txn| {
                autosurgeon::reconcile(txn, &dir_doc).expect("Failed to reconcile directory");
                Ok(())
            })
            .expect("Transaction failed");
        });
        new_dir_handle.they_have_our_changes(ctx.conn_id).await;
    }

    // Update snapshot
    let new_abs_path = ctx.absolute_path(&mv.new_path);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.remove_file(&mv.snapshot_entry.path);
    snapshot.add_file(
        mv.new_path.display().to_string(),
        SnapshotFileEntry {
            path: new_abs_path,
            url: mv.file_url.clone(),
            head: new_heads,
            extension: new_extension,
            mime_type: mv.snapshot_entry.mime_type.clone(),
        },
    );
    drop(snapshot);

    Ok(SyncResult::Moved {
        old_path: mv.old_path.display().to_string(),
        new_path: mv.new_path.display().to_string(),
    })
}

/// Build tasks from collected changes (for changes that weren't moves)
async fn build_tasks_from_changes(
    changes: &CollectedChanges,
    ctx: &SyncContext,
) -> Vec<SyncTask> {
    let mut tasks = Vec::new();

    // Files to sync
    for file_info in &changes.files_to_sync {
        tasks.push(SyncTask::SyncFile {
            relative_path: file_info.relative_path.clone(),
            url: file_info.file_url.clone(),
            snapshot_heads: file_info.snapshot_entry.head.clone(),
            snapshot_entry: file_info.snapshot_entry.clone(),
        });
    }

    // New remote files
    for file_info in &changes.new_remote_files {
        tasks.push(SyncTask::FetchNewFile {
            relative_path: file_info.relative_path.clone(),
            url: file_info.file_url.clone(),
        });
    }

    // New local files (that weren't detected as moves)
    for file_info in &changes.new_local_files {
        tasks.push(SyncTask::PushNewFile {
            relative_path: file_info.relative_path.clone(),
            absolute_path: file_info.absolute_path.clone(),
            parent_dir_url: file_info.dir_url.clone(),
        });
    }

    // Deleted files (that weren't detected as moves)
    for file_info in &changes.deleted_files {
        tasks.push(SyncTask::DeleteRemoteFile {
            relative_path: file_info.relative_path.clone(),
            url: file_info.file_url.clone(),
            snapshot_heads: file_info.snapshot_entry.head.clone(),
        });
    }

    // Remotely deleted files
    for file_info in &changes.remotely_deleted_files {
        tasks.push(SyncTask::DeleteLocalFile {
            relative_path: file_info.relative_path.clone(),
            absolute_path: file_info.absolute_path.clone(),
        });
    }

    // New remote directories
    for (relative_path, url) in &changes.new_remote_directories {
        tasks.push(SyncTask::FetchNewDirectory {
            relative_path: relative_path.clone(),
            url: url.clone(),
        });
    }

    // New local directories
    for (relative_path, absolute_path, parent_url) in &changes.new_local_directories {
        tasks.push(SyncTask::PushNewDirectory {
            relative_path: relative_path.clone(),
            absolute_path: absolute_path.clone(),
            parent_dir_url: parent_url.clone(),
        });
    }

    // Deleted local directories
    for (relative_path, url, _heads) in &changes.deleted_local_directories {
        tasks.push(SyncTask::DeleteRemoteDirectory {
            relative_path: relative_path.clone(),
            absolute_path: ctx.absolute_path(relative_path),
        });
    }

    // Remotely deleted directories
    for relative_path in &changes.deleted_remote_directories {
        tasks.push(SyncTask::DeleteLocalDirectory {
            relative_path: relative_path.clone(),
            absolute_path: ctx.absolute_path(relative_path),
        });
    }

    tasks
}

/// Apply updates to a directory document
async fn apply_directory_update(
    update: &DirectoryUpdate,
    ctx: &SyncContext,
) -> Result<(), String> {
    if update.entries_to_add.is_empty() && update.entries_to_remove.is_empty() {
        return Ok(());
    }

    let handle = ctx
        .repo
        .find(update.dir_url.doc_id().clone())
        .await
        .map_err(|_| "Repo stopped")?
        .ok_or("Directory document not found")?;

    handle.we_have_their_changes(ctx.conn_id).await;

    handle.with_document(|doc| {
        let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");

        // Remove entries
        let to_remove: HashSet<&str> = update.entries_to_remove.iter().map(|s| s.as_str()).collect();
        dir_doc.docs.retain(|e| !to_remove.contains(e.name_str()));

        // Add entries
        for (name, entry_type, url) in &update.entries_to_add {
            if entry_type == "folder" {
                dir_doc.docs.push(DirectoryEntry::folder(name.clone(), url.to_string()));
            } else {
                dir_doc.docs.push(DirectoryEntry::file(name.clone(), url.to_string()));
            }
        }

        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            autosurgeon::reconcile(txn, &dir_doc).expect("Failed to reconcile directory");
            Ok(())
        })
        .expect("Transaction failed");
    });

    handle.they_have_our_changes(ctx.conn_id).await;

    // Update snapshot
    // Exclude removed entries and add new entries
    let removed: HashSet<&str> = update.entries_to_remove.iter().map(|s| s.as_str()).collect();
    let mut final_entries: Vec<String> = update.all_entry_names.iter()
        .filter(|n| !removed.contains(n.as_str()))
        .cloned()
        .collect();
    final_entries.extend(update.entries_to_add.iter().map(|(n, _, _)| n.clone()));

    let dir_heads = sync_ops::get_document_heads(&handle);
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.add_directory(
        update.relative_path.display().to_string(),
        SnapshotDirectoryEntry {
            path: update.absolute_path.clone(),
            url: update.dir_url.clone(),
            head: dir_heads,
            entries: final_entries,
        },
    );

    Ok(())
}

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
        /// URL of the parent directory document
        parent_dir_url: AutomergeUrl,
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
        /// URL of the parent directory document
        parent_dir_url: AutomergeUrl,
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

    /// Delete a directory from the remote (directory was deleted locally)
    DeleteRemoteDirectory {
        /// Path relative to sync root
        relative_path: PathBuf,
        /// Absolute path on disk
        absolute_path: PathBuf,
    },

    /// Delete a local directory (directory was deleted remotely)
    DeleteLocalDirectory {
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
            SyncTask::DeleteRemoteDirectory { relative_path, .. } => relative_path,
            SyncTask::DeleteLocalDirectory { relative_path, .. } => relative_path,
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
            SyncTask::DeleteRemoteDirectory { .. } => format!("delete remote directory: {}", path),
            SyncTask::DeleteLocalDirectory { .. } => format!("delete local directory: {}", path),
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

    /// File was moved/renamed (preserves document identity)
    Moved { old_path: String, new_path: String },

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
            SyncResult::Moved { new_path, .. } => new_path,
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

    /// Move detection threshold (0.0-1.0, default 0.7)
    pub move_threshold: f64,
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
        move_threshold: f64,
    ) -> Self {
        Self {
            repo,
            conn_id,
            root_path,
            root_url,
            exclude_patterns,
            snapshot: Arc::new(Mutex::new(snapshot)),
            move_threshold,
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
    pub moved: usize,
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
                SyncResult::Moved { .. } => summary.moved += 1,
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
            || self.moved > 0
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

        if self.moved > 0 {
            println!("Moved/renamed {} file(s).", self.moved);
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

    // Get directory entries from document (filtering out any invalid entries)
    let entries: Vec<(String, String, AutomergeUrl)> = handle.with_document(|doc| {
        let dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        parse_directory_entries(&dir_doc)
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
            parent_dir_url,
        } => process_push_new_file(relative_path, absolute_path, parent_dir_url, ctx).await,
        SyncTask::FetchNewDirectory { relative_path, url } => {
            process_fetch_new_directory(relative_path, url, ctx).await
        }
        SyncTask::PushNewDirectory {
            relative_path,
            absolute_path,
            parent_dir_url,
        } => process_push_new_directory(relative_path, absolute_path, parent_dir_url, ctx).await,
        SyncTask::DeleteRemoteFile {
            relative_path,
            url,
            snapshot_heads,
        } => process_delete_remote_file(relative_path, url, snapshot_heads, ctx).await,
        SyncTask::DeleteLocalFile {
            relative_path,
            absolute_path,
        } => process_delete_local_file(relative_path, absolute_path, ctx).await,
        SyncTask::DeleteRemoteDirectory {
            relative_path,
            absolute_path,
        } => process_delete_remote_directory(relative_path, absolute_path, ctx).await,
        SyncTask::DeleteLocalDirectory {
            relative_path,
            absolute_path,
        } => process_delete_local_directory(relative_path, absolute_path, ctx).await,
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

    // Get directory entries from document (filtering out any invalid entries)
    let remote_entries: Vec<(String, String, AutomergeUrl)> = handle.with_document(|doc| {
        let dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        parse_directory_entries(&dir_doc)
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
            if in_snapshot && !exists_locally {
                // Directory was deleted locally - will be handled in deletion detection section
                // Don't spawn SyncDirectory task, or it will re-fetch the directory
                continue;
            } else if exists_locally || in_snapshot {
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

    // ==========================================================================
    // Move Detection
    // ==========================================================================
    // Detect file moves/renames: files that were deleted locally and new files
    // that appeared locally with similar content should be treated as moves.

    // Collect deleted file candidates (in snapshot + remote, but not locally)
    let snapshot = ctx.snapshot.lock().await;
    let mut deleted_candidates: Vec<(String, SnapshotFileEntry)> = Vec::new();
    for (snap_path, entry) in snapshot.files.iter() {
        // Check if this file is in the current directory
        let file_path = PathBuf::from(snap_path);
        let parent = file_path.parent().map(|p| p.to_string_lossy().to_string());
        let in_this_dir = parent.as_deref() == Some(&path_str)
            || (path_str.is_empty() && !snap_path.contains('/') && !snap_path.contains('\\'));

        if !in_this_dir {
            continue;
        }

        let name = file_path
            .file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_default();

        // File is a deletion candidate if: in snapshot, in remote, but NOT locally
        if !local_names.contains(&name) && remote_names.contains(&name) {
            deleted_candidates.push((name, entry.clone()));
        }
    }
    drop(snapshot);

    // Collect new file candidates (local files not in snapshot and not in remote)
    let mut new_file_candidates: Vec<(String, PathBuf)> = Vec::new();
    for (name, local_path) in &local_entries {
        if local_path.is_file() && !remote_names.contains(name) && !snapshot_entries.contains(name) {
            new_file_candidates.push((name.clone(), local_path.clone()));
        }
    }

    // Detect moves if we have both deleted and new candidates
    let mut moved_files: HashSet<String> = HashSet::new(); // names that were part of a move
    let mut renamed_entries: Vec<(String, String, AutomergeUrl)> = Vec::new(); // (old_name, new_name, url)
    let mut results = Vec::new();

    if !deleted_candidates.is_empty() && !new_file_candidates.is_empty() && ctx.move_threshold < 1.0 {
        // Build candidates for move detection
        let mut deleted_for_detection: Vec<DeletedFileCandidate> = Vec::new();
        for (name, entry) in &deleted_candidates {
            // Get the content of the deleted file from the remote document
            if let Ok(Some(file_handle)) = ctx.repo.find(entry.url.doc_id().clone()).await {
                file_handle.we_have_their_changes(ctx.conn_id).await;
                let content = file_handle.with_document(|doc| {
                    let file_doc: FileDocument = hydrate(doc).ok()?;
                    Some(file_doc.content_bytes().to_vec())
                });
                if let Some(content) = content {
                    deleted_for_detection.push(DeletedFileCandidate {
                        name: name.clone(),
                        content,
                    });
                }
            }
        }

        let mut new_for_detection: Vec<NewFileCandidate> = Vec::new();
        for (name, path) in &new_file_candidates {
            if let Ok(content) = std::fs::read(path) {
                new_for_detection.push(NewFileCandidate {
                    name: name.clone(),
                    content,
                });
            }
        }

        // Run move detection
        let detected_moves = move_detector::detect_moves(
            &deleted_for_detection,
            &new_for_detection,
            ctx.move_threshold,
        );

        // Process detected moves
        for detected_move in detected_moves {
            let old_name = &detected_move.old_name;
            let new_name = &detected_move.new_name;

            // Find the snapshot entry for the old file
            let old_entry = deleted_candidates
                .iter()
                .find(|(n, _)| n == old_name)
                .map(|(_, e)| e.clone());

            if let Some(entry) = old_entry {
                println!(
                    "  Moving: {} -> {} (similarity: {:.0}%)",
                    old_name,
                    new_name,
                    detected_move.similarity * 100.0
                );

                // Get file handle and update the name
                if let Ok(Some(file_handle)) = ctx.repo.find(entry.url.doc_id().clone()).await {
                    // Get extension from new filename
                    let new_extension = PathBuf::from(new_name)
                        .extension()
                        .map(|e| e.to_string_lossy().to_string())
                        .unwrap_or_default();

                    // Update the file document's name
                    match sync_ops::update_file_name(&file_handle, new_name, &new_extension) {
                        Ok(new_heads) => {
                            // Wait for sync
                            file_handle.they_have_our_changes(ctx.conn_id).await;

                            // Track this as a renamed entry (for directory update)
                            renamed_entries.push((old_name.clone(), new_name.clone(), entry.url.clone()));

                            // Update snapshot: remove old path, add new path with same URL
                            let new_relative = if relative_path.as_os_str().is_empty() {
                                PathBuf::from(new_name)
                            } else {
                                relative_path.join(new_name)
                            };
                            let new_abs_path = ctx.absolute_path(&new_relative);

                            let mut snapshot = ctx.snapshot.lock().await;
                            snapshot.remove_file(&entry.path);
                            snapshot.add_file(
                                new_relative.display().to_string(),
                                SnapshotFileEntry {
                                    path: new_abs_path,
                                    url: entry.url.clone(),
                                    head: new_heads,
                                    extension: new_extension,
                                    mime_type: entry.mime_type.clone(),
                                },
                            );
                            drop(snapshot);

                            // Mark both old and new names as handled
                            moved_files.insert(old_name.clone());
                            moved_files.insert(new_name.clone());

                            results.push(SyncResult::Moved {
                                old_path: old_name.clone(),
                                new_path: new_name.clone(),
                            });
                        }
                        Err(e) => {
                            results.push(SyncResult::Error {
                                path: old_name.clone(),
                                message: format!("Failed to rename file: {}", e),
                            });
                        }
                    }
                }
            }
        }
    }

    // ==========================================================================
    // Process local-only entries (not in remote)
    // ==========================================================================
    // Skip files that were part of a move (already handled above)
    let mut new_entries: Vec<(String, String, AutomergeUrl)> = Vec::new(); // (name, type, url)

    for (name, local_path) in &local_entries {
        if !remote_names.contains(name) {
            // Skip if this was part of a move
            if moved_files.contains(name) {
                continue;
            }

            // Check if this entry is in the snapshot (meaning it was previously synced)
            // If it's in snapshot but not in remote, it was deleted remotely - don't push it back
            let in_snapshot = snapshot_entries.contains(name);
            if in_snapshot {
                // This is a remote deletion case - will be handled in deletion detection section
                continue;
            }

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
    // IMPORTANT: Exclude files we just pushed in this sync (they're in snapshot but not yet in remote)
    let just_pushed_names: HashSet<String> = new_entries.iter().map(|(name, _, _)| name.clone()).collect();

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
        .filter(|(name, _)| !just_pushed_names.contains(name)) // Exclude just-pushed files
        .collect();
    drop(snapshot);

    // Track entries to remove from directory document (local deletions)
    let mut entries_to_remove: Vec<String> = Vec::new();

    for (name, file_entry) in snapshot_files {
        // Skip files that were part of a move (already handled)
        if moved_files.contains(&name) {
            continue;
        }

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

    // Detect directory deletions: directories in snapshot but not locally or not in remote
    // Exclude directories we just created (they're in snapshot but not yet in remote)
    let snapshot = ctx.snapshot.lock().await;
    let snapshot_dirs: Vec<(String, SnapshotDirectoryEntry)> = snapshot
        .directories
        .iter()
        .filter(|(path, _)| {
            // Skip the root directory entry (empty path) - it's never a child
            if path.is_empty() {
                return false;
            }
            // Get the parent directory of this directory
            let dir_path = PathBuf::from(path);
            let parent = dir_path.parent().map(|p| p.to_string_lossy().to_string());
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
        .filter(|(name, _)| !just_pushed_names.contains(name)) // Exclude just-created directories
        .collect();
    drop(snapshot);

    for (name, dir_entry) in snapshot_dirs {
        let exists_locally = local_names.contains(&name);
        let exists_in_remote = remote_names.contains(&name);

        let child_relative = if relative_path.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            relative_path.join(&name)
        };

        if !exists_locally && exists_in_remote {
            // Directory was deleted locally but exists remotely
            // Remove from remote directory document
            new_tasks.push(SyncTask::DeleteRemoteDirectory {
                relative_path: child_relative,
                absolute_path: dir_entry.path.clone(),
            });
            entries_to_remove.push(name.clone());
        } else if !exists_locally && !exists_in_remote {
            // Directory was deleted both locally and remotely - just clean up snapshot
            println!("  Cleaning up: {}/ (deleted)", child_relative.display());
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_directory(&dir_entry.path);
            drop(snapshot);
        } else if exists_locally && !exists_in_remote {
            // Directory exists locally but not in remote - this is a remote deletion
            new_tasks.push(SyncTask::DeleteLocalDirectory {
                relative_path: child_relative,
                absolute_path: dir_entry.path.clone(),
            });
        }
    }

    // Update this directory document: add new entries, rename entries, and remove deleted entries
    // Note: For renames, we treat them as remove + add to avoid CRDT Text merging issues
    let old_names_from_renames: HashSet<String> =
        renamed_entries.iter().map(|(old, _, _)| old.clone()).collect();
    let all_entries_to_remove: HashSet<String> = entries_to_remove
        .iter()
        .cloned()
        .chain(old_names_from_renames)
        .collect();

    // Collect all new entries to add (including renamed files)
    let mut all_new_entries: Vec<(String, String, AutomergeUrl)> = new_entries.clone();
    for (_, new_name, url) in &renamed_entries {
        all_new_entries.push((new_name.clone(), "file".to_string(), url.clone()));
    }

    if !all_new_entries.is_empty() || !all_entries_to_remove.is_empty() {
        // Load current document and modify entries
        handle.with_document(|doc| {
            let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");

            // Remove entries first (deletions + old names from renames)
            dir_doc
                .docs
                .retain(|entry| !all_entries_to_remove.contains(entry.name_str()));

            // Add new entries (new files + renamed files)
            for (name, entry_type, entry_url) in &all_new_entries {
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

    // Collect old names from renames to exclude them
    let renamed_old_names: HashSet<String> = renamed_entries.iter().map(|(old, _, _)| old.clone()).collect();

    let mut all_entry_names: Vec<String> = remote_entries
        .iter()
        .filter(|(n, _, _)| !entries_to_remove.contains(n) && !renamed_old_names.contains(n))
        .map(|(n, _, _)| n.clone())
        .collect();
    all_entry_names.extend(new_entries.iter().map(|(n, _, _)| n.clone()));
    // Add new names from renames
    all_entry_names.extend(renamed_entries.iter().map(|(_, new, _)| new.clone()));

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

/// Process PushNewFile: create document, update parent directory, and push to server
async fn process_push_new_file(
    relative_path: PathBuf,
    absolute_path: PathBuf,
    parent_dir_url: AutomergeUrl,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let file_name = relative_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

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

    // Wait for file document to sync
    created.handle.they_have_our_changes(ctx.conn_id).await;

    // Update parent directory document to include this file
    let dir_handle = match ctx.repo.find(parent_dir_url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Parent directory document not found".into(),
            });
        }
        Err(_) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Repo stopped".into(),
            });
        }
    };

    // Add entry to directory document
    dir_handle.with_document(|doc| {
        let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        dir_doc.docs.push(crate::documents::DirectoryEntry::file(
            file_name.clone(),
            created.url.to_string(),
        ));
        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            autosurgeon::reconcile(txn, &dir_doc).expect("Failed to reconcile directory");
            Ok(())
        })
        .expect("Transaction failed");
    });

    // Wait for directory update to sync
    dir_handle.they_have_our_changes(ctx.conn_id).await;

    // Add to snapshot
    let heads = sync_ops::get_document_heads(&created.handle);
    let parent_dir_path = relative_path.parent()
        .map(|p| ctx.absolute_path(&p.to_path_buf()))
        .unwrap_or_else(|| ctx.root_path.clone());
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
    // Also add this file name to the parent directory's entries
    snapshot.add_directory_entry(&parent_dir_path, file_name.clone());
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

/// Process PushNewDirectory: create directory document, update parent, and push contents
async fn process_push_new_directory(
    relative_path: PathBuf,
    absolute_path: PathBuf,
    parent_dir_url: AutomergeUrl,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = relative_path.display().to_string();
    let dir_name = relative_path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_default();

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

    // Wait for directory document to sync
    created.handle.they_have_our_changes(ctx.conn_id).await;

    // Update parent directory document to include this directory
    let parent_handle = match ctx.repo.find(parent_dir_url.doc_id().clone()).await {
        Ok(Some(h)) => h,
        Ok(None) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Parent directory document not found".into(),
            });
        }
        Err(_) => {
            return TaskOutput::result(SyncResult::Error {
                path: path_str,
                message: "Repo stopped".into(),
            });
        }
    };

    // Add entry to parent directory document
    parent_handle.with_document(|doc| {
        let mut dir_doc: DirectoryDocument = hydrate(doc).expect("Failed to hydrate directory");
        dir_doc.docs.push(crate::documents::DirectoryEntry::folder(
            dir_name.clone(),
            created.url.to_string(),
        ));
        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            autosurgeon::reconcile(txn, &dir_doc).expect("Failed to reconcile directory");
            Ok(())
        })
        .expect("Transaction failed");
    });

    // Wait for parent directory update to sync
    parent_handle.they_have_our_changes(ctx.conn_id).await;

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

    // Create tasks for contents - use the newly created directory as parent
    let mut new_tasks = Vec::new();
    for (name, local_path) in local_entries {
        let child_relative = relative_path.join(&name);

        if local_path.is_dir() {
            new_tasks.push(SyncTask::PushNewDirectory {
                relative_path: child_relative,
                absolute_path: local_path,
                parent_dir_url: created.url.clone(),
            });
        } else if local_path.is_file() {
            new_tasks.push(SyncTask::PushNewFile {
                relative_path: child_relative,
                absolute_path: local_path,
                parent_dir_url: created.url.clone(),
            });
        }
    }

    // Add to snapshot
    let heads = sync_ops::get_document_heads(&created.handle);
    let parent_dir_path = relative_path.parent()
        .map(|p| ctx.absolute_path(&p.to_path_buf()))
        .unwrap_or_else(|| ctx.root_path.clone());
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
    // Also add this directory name to the parent directory's entries
    snapshot.add_directory_entry(&parent_dir_path, dir_name.clone());
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

/// Process DeleteRemoteDirectory: remove directory entry from parent's directory document
///
/// Unlike files, we don't check for remote modifications for directories.
/// If the directory was deleted locally, we remove it from the remote.
async fn process_delete_remote_directory(
    _relative_path: PathBuf,
    absolute_path: PathBuf,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = absolute_path.display().to_string();

    println!("  Deleting directory remotely: {}", path_str);

    // Remove from snapshot (the directory entry removal from parent doc
    // is handled by process_sync_directory via entries_to_remove)
    let mut snapshot = ctx.snapshot.lock().await;
    snapshot.remove_directory(&absolute_path);
    drop(snapshot);

    TaskOutput::result(SyncResult::DeletedRemote { path: path_str })
}

/// Process DeleteLocalDirectory: delete local directory that was deleted remotely
///
/// Uses recursive deletion (remove_dir_all) to handle non-empty directories.
async fn process_delete_local_directory(
    _relative_path: PathBuf,
    absolute_path: PathBuf,
    ctx: &SyncContext,
) -> TaskOutput {
    let path_str = absolute_path.display().to_string();

    println!("  Deleting directory locally: {}", path_str);

    // Delete the local directory recursively
    match std::fs::remove_dir_all(&absolute_path) {
        Ok(()) => {
            // Remove from snapshot
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_directory(&absolute_path);
            // Also remove any files that were inside this directory
            let prefix = absolute_path.to_string_lossy();
            snapshot.files.retain(|(_, entry)| {
                !entry.path.to_string_lossy().starts_with(prefix.as_ref())
            });
            snapshot.directories.retain(|(_, entry)| {
                !entry.path.to_string_lossy().starts_with(prefix.as_ref())
            });
            drop(snapshot);

            TaskOutput::result(SyncResult::DeletedLocal { path: path_str })
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            // Directory already gone - just clean up snapshot
            let mut snapshot = ctx.snapshot.lock().await;
            snapshot.remove_directory(&absolute_path);
            drop(snapshot);

            TaskOutput::result(SyncResult::NoChange { path: path_str })
        }
        Err(e) => TaskOutput::result(SyncResult::Error {
            path: path_str,
            message: format!("Failed to delete directory: {}", e),
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
