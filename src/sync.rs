//! Sync command implementation.
//!
//! Handles synchronizing local file changes with the Automerge sync server.

use std::path::Path;

use automerge::ChangeHash;
use autosurgeon::hydrate;
use samod::{AutomergeUrl, ConnDirection, ConnectionId, DocHandle, Repo};
use tokio_tungstenite::connect_async;

use crate::changes::{detect_modified_files, detect_remote_changes, ModifiedFile, RemotelyChangedFile};
use crate::config::DirectoryConfig;
use crate::documents::{FileContent, FileDocument};
use crate::files::{self, FileInfo};
use crate::init::PushworkPaths;
use crate::scanner::{self, FileToSync};
use crate::snapshot::Snapshot;
use crate::sync_ops;

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

    // Load root directory document
    let dir_handle = load_root_directory(repo, &root_url).await;

    // Load or create snapshot
    let mut snapshot = load_or_create_snapshot(paths, &root_url);

    // Pre-load all tracked documents and wait for sync from server
    // This ensures we have the latest state before checking for changes
    preload_tracked_documents(repo, &snapshot, conn_id).await;

    // Scan for new files
    let scan_result =
        scanner::scan_for_changes(&paths.root, &config.exclude_patterns, &snapshot)
            .unwrap_or_else(|e| {
                eprintln!("Failed to scan directory: {}", e);
                std::process::exit(1);
            });

    // Detect local modifications
    let modified_files = detect_modified_files(repo, &paths.root, &snapshot).await;

    // Detect remote changes BEFORE pushing (to find conflicts)
    let remote_changes = detect_remote_changes(repo, &snapshot).await;

    // Find conflicts: files changed both locally and remotely
    // Build a map so we can get the remote handle for merging
    let remote_map: std::collections::HashMap<_, _> = remote_changes
        .iter()
        .map(|r| (r.relative_path.as_str(), r))
        .collect();

    let (conflicts, safe_modified): (Vec<_>, Vec<_>) = modified_files
        .into_iter()
        .partition(|m| remote_map.contains_key(m.relative_path.as_str()));

    // Track counts for summary
    let mut pushed_new = 0;
    let mut pushed_modified = 0;
    let mut merged_count = 0;

    // Process conflicts using CRDT merge
    for conflict in &conflicts {
        println!("  Merging: {} (changed locally and remotely)", conflict.relative_path);

        let remote = remote_map.get(conflict.relative_path.as_str()).unwrap();
        let abs_path = paths.root.join(&conflict.relative_path);

        // Determine content type
        let is_text = files::is_text_mime_type(&conflict.snapshot_entry.mime_type);
        let local_content = if is_text {
            FileContent::text(String::from_utf8_lossy(&conflict.new_content))
        } else {
            FileContent::binary(conflict.new_content.clone())
        };

        // Get local permissions
        let local_perms = files::get_file_permissions(&abs_path)
            .ok()
            .map(|p| p as i64);

        // Merge local changes into the remote document
        match sync_ops::merge_local_into_remote(
            &remote.handle,
            &conflict.snapshot_entry.head,
            local_content,
            local_perms,
        ) {
            Ok(new_heads) => {
                // Write merged content to disk
                if let Err(e) = sync_ops::write_remote_file_to_disk(&remote.handle, &abs_path) {
                    eprintln!("  Error writing merged content for {}: {}", conflict.relative_path, e);
                    continue;
                }
                // Update snapshot with new heads
                snapshot.update_file_heads(&conflict.relative_path, new_heads);
                merged_count += 1;
            }
            Err(e) => {
                eprintln!("  Error merging {}: {}", conflict.relative_path, e);
            }
        }
    }

    // Process local changes if any (excluding conflicts)
    let has_local_changes = scan_result.has_changes() || !safe_modified.is_empty();
    if has_local_changes {
        if !scan_result.new_files.is_empty() {
            println!("Found {} new file(s) to push", scan_result.new_files.len());
        }
        if !safe_modified.is_empty() {
            println!("Found {} modified file(s) to push", safe_modified.len());
        }

        // Process new files
        let new_result = process_new_files(repo, &scan_result.new_files).await;
        pushed_new = new_result.created.len();

        // Update root directory with new files
        update_root_directory(&dir_handle, &new_result.created);

        // Process modified files (excluding conflicts)
        let modified_result = process_modified_files(repo, &paths.root, &safe_modified).await;
        pushed_modified = modified_result.handles.len();

        // Wait for sync if we pushed anything
        if pushed_new > 0 || pushed_modified > 0 {
            let dir_handle_for_sync = if pushed_new > 0 {
                Some(&dir_handle)
            } else {
                None
            };
            wait_for_sync(
                &new_result.created,
                &modified_result.handles,
                dir_handle_for_sync,
                conn_id,
            )
            .await;

            // Update snapshot with pushed files
            update_snapshot(
                &mut snapshot,
                &paths.root,
                &new_result.created,
                modified_result.entries,
            );
        }
    }

    // Pull remote changes (excluding files that were already merged as conflicts)
    let conflict_paths: std::collections::HashSet<_> = conflicts
        .iter()
        .map(|c| c.relative_path.as_str())
        .collect();

    let remote_only: Vec<_> = remote_changes
        .iter()
        .filter(|r| !conflict_paths.contains(r.relative_path.as_str()))
        .collect();

    let pulled_count = if !remote_only.is_empty() {
        println!("Found {} file(s) with remote changes", remote_only.len());
        process_remote_changes_refs(&mut snapshot, &paths.root, &remote_only)
    } else {
        0
    };

    // Check if anything happened
    if pushed_new == 0 && pushed_modified == 0 && pulled_count == 0 && merged_count == 0 {
        println!("No changes.");
        return;
    }

    // Save snapshot
    let snapshot_path = Snapshot::path_in(&paths.pushwork_dir);
    snapshot.save(&snapshot_path).unwrap_or_else(|e| {
        eprintln!("Warning: Failed to save snapshot: {}", e);
    });

    // Print summary
    print_summary(pushed_new, pushed_modified, pulled_count, merged_count);
}

// =============================================================================
// Internal types
// =============================================================================

/// Result of processing new files
struct NewFilesResult {
    /// Successfully created files: (relative_path, url, handle, file_info)
    created: Vec<(String, AutomergeUrl, DocHandle, FileInfo)>,
    /// Number of files skipped (e.g., binary files)
    skipped: usize,
}

/// Result of processing modified files
struct ModifiedFilesResult {
    /// Handles of updated documents
    handles: Vec<DocHandle>,
    /// Updated entries: (relative_path, new_heads)
    entries: Vec<(String, Vec<ChangeHash>)>,
}

// =============================================================================
// High-level operations (called directly from execute)
// =============================================================================

/// Process new files: create documents for each
async fn process_new_files(repo: &Repo, new_files: &[FileToSync]) -> NewFilesResult {
    let mut created = Vec::new();

    for file in new_files {
        let file_type = if file.info.is_text { "text" } else { "binary" };
        println!("  Pushing ({}): {}", file_type, file.relative_path);

        match sync_ops::create_file_document(repo, &file.absolute_path, &file.info).await {
            Ok(result) => {
                created.push((
                    file.relative_path.clone(),
                    result.url,
                    result.handle,
                    file.info.clone(),
                ));
            }
            Err(e) => {
                eprintln!(
                    "  Error creating document for {}: {}",
                    file.relative_path, e
                );
            }
        }
    }

    NewFilesResult { created, skipped: 0 }
}

/// Process modified files: update their documents
async fn process_modified_files(
    repo: &Repo,
    root: &Path,
    modified_files: &[ModifiedFile],
) -> ModifiedFilesResult {
    let mut handles = Vec::new();
    let mut entries = Vec::new();

    for modified in modified_files {
        println!("  Updating: {}", modified.relative_path);

        // Load the document by URL
        let doc_id = modified.snapshot_entry.url.doc_id().clone();

        let handle = match repo.find(doc_id).await.expect("Repo stopped") {
            Some(h) => h,
            None => {
                eprintln!("  Error: document not found for {}", modified.relative_path);
                continue;
            }
        };

        // Get current permissions from disk
        let abs_path = root.join(&modified.relative_path);
        let new_perms = files::get_file_permissions(&abs_path)
            .ok()
            .map(|p| p as i64);

        // Determine if file is text or binary based on MIME type
        let is_text = files::is_text_mime_type(&modified.snapshot_entry.mime_type);
        let content = if is_text {
            FileContent::text(String::from_utf8_lossy(&modified.new_content))
        } else {
            FileContent::binary(modified.new_content.clone())
        };
        match sync_ops::update_file_document(&handle, content, new_perms) {
            Ok(new_heads) => {
                entries.push((modified.relative_path.clone(), new_heads));
                handles.push(handle);
            }
            Err(e) => {
                eprintln!("  Error updating {}: {}", modified.relative_path, e);
            }
        }
    }

    ModifiedFilesResult { handles, entries }
}

/// Process remote changes: write updated content to disk
fn process_remote_changes_refs(
    snapshot: &mut Snapshot,
    root: &Path,
    remote_changes: &[&RemotelyChangedFile],
) -> usize {
    let mut pulled = 0;

    for changed in remote_changes {
        let abs_path = root.join(&changed.relative_path);
        println!("  Pulling: {}", changed.relative_path);

        match sync_ops::write_remote_file_to_disk(&changed.handle, &abs_path) {
            Ok(()) => {
                // Update snapshot with new heads
                snapshot.update_file_heads(&changed.relative_path, changed.new_heads.clone());
                pulled += 1;
            }
            Err(e) => {
                eprintln!("  Error pulling {}: {}", changed.relative_path, e);
            }
        }
    }

    pulled
}

/// Update the root directory with new file entries
fn update_root_directory(
    dir_handle: &DocHandle,
    created_files: &[(String, AutomergeUrl, DocHandle, FileInfo)],
) {
    if created_files.is_empty() {
        return;
    }

    let new_entries: Vec<(String, AutomergeUrl)> = created_files
        .iter()
        .map(|(_, url, handle, _)| {
            let name: String = handle.with_document(|doc| {
                let file_doc: FileDocument = hydrate(doc).expect("Failed to hydrate file");
                file_doc.name_str().to_string()
            });
            (name, url.clone())
        })
        .collect();

    println!("Updating root directory...");
    sync_ops::update_directory_with_files(dir_handle, &new_entries).unwrap_or_else(|e| {
        eprintln!("Failed to update root directory: {}", e);
        std::process::exit(1);
    });
}

/// Wait for all documents to sync with the server
async fn wait_for_sync(
    created_files: &[(String, AutomergeUrl, DocHandle, FileInfo)],
    modified_handles: &[DocHandle],
    dir_handle: Option<&DocHandle>,
    conn_id: ConnectionId,
) {
    println!("Waiting for sync to complete...");

    let mut all_handles: Vec<&DocHandle> = created_files
        .iter()
        .map(|(_, _, handle, _)| handle)
        .collect();

    for h in modified_handles {
        all_handles.push(h);
    }

    if let Some(dir) = dir_handle {
        all_handles.push(dir);
    }

    sync_ops::wait_for_all_synced(&all_handles, conn_id).await;
}

/// Update snapshot with synced file information
fn update_snapshot(
    snapshot: &mut Snapshot,
    root: &Path,
    created_files: &[(String, AutomergeUrl, DocHandle, FileInfo)],
    modified_entries: Vec<(String, Vec<ChangeHash>)>,
) {
    // Add new files to snapshot
    for (relative_path, _, handle, info) in created_files {
        let entry = sync_ops::create_snapshot_file_entry(handle, root.join(relative_path), info);
        snapshot.add_file(relative_path.clone(), entry);
    }

    // Update heads for modified files
    for (relative_path, new_heads) in modified_entries {
        snapshot.update_file_heads(&relative_path, new_heads);
    }

    snapshot.update_timestamp();
}

// =============================================================================
// Low-level helpers (setup, connection, output)
// =============================================================================

/// Connect to the sync server and return the connection ID
async fn connect_to_server(repo: &Repo, sync_url: &str) -> ConnectionId {
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

/// Load the root directory document handle
async fn load_root_directory(repo: &Repo, root_url: &AutomergeUrl) -> DocHandle {
    repo.find(root_url.doc_id().clone())
        .await
        .expect("Repo stopped")
        .unwrap_or_else(|| {
            eprintln!("Root directory document not found");
            std::process::exit(1);
        })
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

/// Pre-load all tracked documents and wait for sync from the server
///
/// When we call repo.find() on a document that exists locally, it returns
/// immediately but syncs in the background. This function loads all tracked
/// documents and waits for each to receive all changes from the server.
async fn preload_tracked_documents(repo: &Repo, snapshot: &Snapshot, conn_id: ConnectionId) {
    if snapshot.files.is_empty() {
        return;
    }

    // Load all tracked documents in parallel
    let futures: Vec<_> = snapshot
        .files
        .iter()
        .map(|(_, entry)| repo.find(entry.url.doc_id().clone()))
        .collect();

    let handles: Vec<_> = futures::future::join_all(futures)
        .await
        .into_iter()
        .flatten()
        .flatten()
        .collect();

    // Wait for all documents to receive changes from the server
    let sync_futures: Vec<_> = handles
        .iter()
        .map(|h| h.we_have_their_changes(conn_id))
        .collect();

    futures::future::join_all(sync_futures).await;
}

/// Print sync summary
fn print_summary(pushed_new: usize, pushed_modified: usize, pulled: usize, merged: usize) {
    let total_pushed = pushed_new + pushed_modified;

    let mut parts = Vec::new();
    if pushed_new > 0 {
        parts.push(format!("{} new", pushed_new));
    }
    if pushed_modified > 0 {
        parts.push(format!("{} modified", pushed_modified));
    }
    if total_pushed > 0 {
        println!(
            "\nPushed {} file(s) ({}).",
            total_pushed,
            parts.join(", ")
        );
    }
    if pulled > 0 {
        println!("Pulled {} file(s).", pulled);
    }
    if merged > 0 {
        println!("Merged {} file(s) (changed locally and remotely).", merged);
    }
}
