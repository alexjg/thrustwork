//! Sync command implementation.
//!
//! Handles synchronizing local file changes with the Automerge sync server.

use std::path::Path;

use automerge::ChangeHash;
use autosurgeon::hydrate;
use samod::{AutomergeUrl, ConnDirection, ConnectionId, DocHandle, Repo};
use tokio_tungstenite::connect_async;

use crate::changes::{detect_modified_files, ModifiedFile};
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

    // Scan for new files
    let scan_result =
        scanner::scan_for_changes(&paths.root, &config.exclude_patterns, &snapshot)
            .unwrap_or_else(|e| {
                eprintln!("Failed to scan directory: {}", e);
                std::process::exit(1);
            });

    // Detect modified files
    let modified_files = detect_modified_files(repo, &paths.root, &snapshot).await;

    // Check if there are any changes
    if !scan_result.has_changes() && modified_files.is_empty() {
        println!("No changes to sync.");
        return;
    }

    // Report what we found
    if !scan_result.new_files.is_empty() {
        println!("Found {} new file(s) to sync", scan_result.new_files.len());
    }
    if !modified_files.is_empty() {
        println!(
            "Found {} modified file(s) to sync",
            modified_files.len()
        );
    }

    // Process new files
    let new_result = process_new_files(repo, &scan_result.new_files).await;

    // Check if anything to sync
    if new_result.created.is_empty() && modified_files.is_empty() {
        println!("No files were synced.");
        return;
    }

    // Update root directory with new files
    update_root_directory(&dir_handle, &new_result.created);

    // Process modified files
    let modified_result = process_modified_files(repo, &paths.root, &modified_files).await;

    // Wait for sync
    let dir_handle_for_sync = if !new_result.created.is_empty() {
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

    // Update and save snapshot
    update_snapshot(
        &mut snapshot,
        &paths.root,
        &new_result.created,
        modified_result.entries,
    );

    let snapshot_path = Snapshot::path_in(&paths.pushwork_dir);
    snapshot.save(&snapshot_path).unwrap_or_else(|e| {
        eprintln!("Warning: Failed to save snapshot: {}", e);
    });

    // Print summary
    print_summary(
        new_result.created.len(),
        modified_result.handles.len(),
        new_result.skipped,
    );
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
    let mut skipped = 0;

    for file in new_files {
        // Skip binary files
        if !file.info.is_text {
            println!("  Skipping (binary): {}", file.relative_path);
            skipped += 1;
            continue;
        }

        println!("  Pushing: {}", file.relative_path);

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

    NewFilesResult { created, skipped }
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

        // Update the document (currently text only - binary handled in later phase)
        let content = FileContent::text(&modified.new_content);
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

/// Print sync summary
fn print_summary(new_count: usize, modified_count: usize, skipped_count: usize) {
    let total_synced = new_count + modified_count;

    if total_synced == 0 {
        println!("\nNo files were synced.");
        return;
    }

    let mut parts = Vec::new();
    if new_count > 0 {
        parts.push(format!("{} new", new_count));
    }
    if modified_count > 0 {
        parts.push(format!("{} modified", modified_count));
    }
    if skipped_count > 0 {
        parts.push(format!("{} skipped (binary)", skipped_count));
    }

    println!(
        "\nDone! {} file(s) synced ({}).",
        total_synced,
        parts.join(", ")
    );
}
