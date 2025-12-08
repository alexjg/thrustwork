//! Execution engine for applying the sync plan.
//!
//! This module takes a classified SyncPlan and executes the appropriate
//! operations for each file and directory change.

use std::path::{Path, PathBuf};

use autosurgeon::{hydrate, reconcile};
use samod::{AutomergeUrl, ConnectionId, DocHandle, Repo};

use super::sync_ops::{
    self, create_directory_document, create_file_document, get_document_heads,
    get_file_info_at_heads, update_file_document,
};
use crate::documents::{DirectoryDocument, DirectoryEntry, FileContent, FileDocument};
use crate::files::FileInfo;
use crate::snapshot::{Snapshot, SnapshotDirectoryEntry, SnapshotFileEntry};

use super::classify::{DirChange, FileChange, SyncPlan};
use super::state::{FsFile, RepoDir, RepoFile};

/// Result of executing a single sync operation.
#[derive(Debug, Clone)]
pub enum SyncResult {
    /// File was pushed (new local file)
    Pushed {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// File was pulled (new remote file)
    Pulled {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// File was merged (both local and remote changes)
    Merged {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// File was updated locally (remote edit)
    Updated {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// File was uploaded (local edit)
    Uploaded {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// File had a conflict (both edited)
    #[expect(dead_code)]
    Conflict { path: PathBuf },
    /// File was deleted locally
    #[expect(dead_code)]
    DeletedLocal { path: PathBuf },
    /// File was deleted remotely
    DeletedRemote {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// File was moved/renamed
    Moved {
        #[expect(dead_code)]
        old_path: PathBuf,
        #[expect(dead_code)]
        new_path: PathBuf,
    },
    /// Directory was created locally
    DirCreatedLocal {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// Directory was created remotely
    DirCreatedRemote {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// Directory was deleted locally
    DirDeletedLocal {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// Directory was deleted remotely
    DirDeletedRemote {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// No change needed
    NoChange {
        #[expect(dead_code)]
        path: PathBuf,
    },
    /// Operation failed
    Error {
        #[expect(dead_code)]
        path: PathBuf,
        #[expect(dead_code)]
        message: String,
    },
}

/// Context for sync execution.
pub struct ExecuteContext<'a> {
    /// The Automerge repo
    pub repo: &'a Repo,
    /// Connection ID for sync server
    pub conn_id: ConnectionId,
    /// Root directory on filesystem
    pub root: &'a Path,
    /// Snapshot to update during execution
    pub snapshot: &'a mut Snapshot,
    /// Handles that were modified and need to be synced
    modified_handles: Vec<DocHandle>,
}

impl<'a> ExecuteContext<'a> {
    /// Create a new execution context
    pub fn new(
        repo: &'a Repo,
        conn_id: ConnectionId,
        root: &'a Path,
        snapshot: &'a mut Snapshot,
    ) -> Self {
        Self {
            repo,
            conn_id,
            root,
            snapshot,
            modified_handles: Vec::new(),
        }
    }

    /// Track a handle as modified (needs to be synced)
    fn track_modified(&mut self, handle: DocHandle) {
        self.modified_handles.push(handle);
    }

    /// Wait for all modified documents to sync to server
    async fn sync_all_modified(&self) {
        for handle in &self.modified_handles {
            handle.they_have_our_changes(self.conn_id).await;
        }
    }
}

/// Execute a sync plan and return the results.
///
/// This processes directories first (in depth order), then files.
/// Updates the snapshot as operations complete.
pub async fn execute_sync_plan(plan: SyncPlan, ctx: &mut ExecuteContext<'_>) -> Vec<SyncResult> {
    let mut results = Vec::new();

    // Process directories first (depth order - shallowest first for creates, deepest first for deletes)
    let mut dir_paths: Vec<PathBuf> = plan.dirs.keys().cloned().collect();
    dir_paths.sort_by_key(|a| a.components().count());

    // First pass: create new directories (shallowest first)
    for path in &dir_paths {
        if let Some(change) = plan.dirs.get(path)
            && matches!(
                change,
                DirChange::LocalNew { .. } | DirChange::RemoteNew { .. }
            )
        {
            let result = execute_dir_change(path, change, ctx).await;
            results.push(result);
        }
    }

    // Process files
    for (path, change) in &plan.files {
        let result = execute_file_change(path, change, ctx).await;
        results.push(result);
    }

    // Second pass: delete directories (deepest first)
    dir_paths.reverse();
    for path in &dir_paths {
        if let Some(change) = plan.dirs.get(path)
            && matches!(
                change,
                DirChange::LocalDelete { .. } | DirChange::RemoteDelete { .. }
            )
        {
            let result = execute_dir_change(path, change, ctx).await;
            results.push(result);
        }
    }

    // Wait for all modified documents to sync to server
    ctx.sync_all_modified().await;

    results
}

/// Execute a single file change operation.
async fn execute_file_change(
    path: &PathBuf,
    change: &FileChange,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    match change {
        FileChange::Unchanged(remote) => {
            // Update snapshot with state at sync start
            update_file_snapshot(ctx, path, remote);
            SyncResult::NoChange { path: path.clone() }
        }

        FileChange::LocalNew { local, parent_url } => {
            // If parent_url is None, look it up from the snapshot
            // (the parent directory should have been created first)
            let resolved_parent_url = match parent_url {
                Some(url) => url.clone(),
                None => {
                    // Look up parent directory URL from snapshot
                    let parent = path.parent();
                    let is_root_level = parent.map(|p| p.as_os_str().is_empty()).unwrap_or(true);

                    if is_root_level {
                        // Root-level file - use root_directory_url from snapshot
                        match &ctx.snapshot.root_directory_url {
                            Some(url) => url.clone(),
                            None => {
                                return SyncResult::Error {
                                    path: path.clone(),
                                    message: "Root directory URL not found".to_string(),
                                };
                            }
                        }
                    } else {
                        // Nested file - look up parent directory from snapshot
                        let parent_path = ctx.root.join(parent.unwrap());
                        match ctx.snapshot.get_directory(&parent_path) {
                            Some(entry) => entry.url.clone(),
                            None => {
                                return SyncResult::Error {
                                    path: path.clone(),
                                    message: format!(
                                        "Parent directory not found in snapshot: {:?}",
                                        parent_path
                                    ),
                                };
                            }
                        }
                    }
                }
            };
            execute_local_new_file(path, local, &resolved_parent_url, ctx).await
        }

        FileChange::RemoteNew { remote } => execute_remote_new_file(path, remote, ctx).await,

        FileChange::LocalEdit {
            local,
            snap: _,
            remote,
        } => execute_local_edit(path, local, remote, ctx).await,

        FileChange::RemoteEdit {
            local: _,
            snap: _,
            remote,
        } => execute_remote_edit(path, remote, ctx).await,

        FileChange::Conflict {
            local,
            snap,
            remote,
        } => execute_conflict(path, local, snap, remote, ctx).await,

        FileChange::LocalDelete { snap: _, remote } => {
            execute_local_delete_file(path, remote, ctx).await
        }

        FileChange::RemoteDelete { local, snap } => {
            execute_remote_delete_file(path, local, snap, ctx).await
        }

        FileChange::Gone => {
            // Both sides deleted - just remove from snapshot
            ctx.snapshot.remove_file(&ctx.root.join(path));
            SyncResult::NoChange { path: path.clone() }
        }

        FileChange::BothNew { local: _, remote } => {
            // Concurrent creation with no common ancestor.
            // Since there's no snapshot (common ancestor), we can't do a proper merge.
            // Pull remote content instead (it was created first on the server).
            execute_remote_new_file(path, remote, ctx).await
        }

        FileChange::Moved {
            old_path,
            snap: _,
            remote,
            new_content,
        } => execute_move(path, old_path, remote, new_content, ctx).await,
    }
}

/// Execute a single directory change operation.
async fn execute_dir_change(
    path: &PathBuf,
    change: &DirChange,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    match change {
        DirChange::Unchanged(remote) => {
            update_dir_snapshot(ctx, path, remote);
            SyncResult::NoChange { path: path.clone() }
        }

        DirChange::LocalNew {
            abs_path: _,
            parent_url,
        } => {
            // If parent_url is None, look it up from the snapshot
            let resolved_parent_url = match parent_url {
                Some(url) => url.clone(),
                None => {
                    let parent = path.parent();
                    let is_root_level = parent.map(|p| p.as_os_str().is_empty()).unwrap_or(true);

                    if is_root_level {
                        // Root-level directory - use root_directory_url
                        match &ctx.snapshot.root_directory_url {
                            Some(url) => url.clone(),
                            None => {
                                return SyncResult::Error {
                                    path: path.clone(),
                                    message: "Root directory URL not found".to_string(),
                                };
                            }
                        }
                    } else {
                        // Nested directory - look up parent from snapshot
                        let parent_path = ctx.root.join(parent.unwrap());
                        match ctx.snapshot.get_directory(&parent_path) {
                            Some(entry) => entry.url.clone(),
                            None => {
                                return SyncResult::Error {
                                    path: path.clone(),
                                    message: format!(
                                        "Parent directory not found: {:?}",
                                        parent_path
                                    ),
                                };
                            }
                        }
                    }
                }
            };
            execute_local_new_dir(path, &resolved_parent_url, ctx).await
        }

        DirChange::RemoteNew { remote } => execute_remote_new_dir(path, remote, ctx).await,

        DirChange::LocalDelete { snap: _, remote } => {
            execute_local_delete_dir(path, remote, ctx).await
        }

        DirChange::RemoteDelete { snap } => execute_remote_delete_dir(path, snap, ctx).await,

        DirChange::Gone => {
            ctx.snapshot.remove_directory(&ctx.root.join(path));
            SyncResult::NoChange { path: path.clone() }
        }
    }
}

// File operation implementations

async fn execute_local_new_file(
    path: &PathBuf,
    local: &FsFile,
    parent_url: &AutomergeUrl,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let abs_path = ctx.root.join(path);
    let file_info = FileInfo::from_path(&abs_path);

    // Create file document
    let created = match create_file_document(ctx.repo, &abs_path, &file_info, &local.content).await
    {
        Ok(c) => c,
        Err(e) => {
            return SyncResult::Error {
                path: path.clone(),
                message: format!("Failed to create file document: {}", e),
            };
        }
    };

    // Add to parent directory
    let parent_handle = match ctx.repo.find(parent_url.document_id().clone()).await {
        Ok(Some(h)) => h,
        _ => {
            return SyncResult::Error {
                path: path.clone(),
                message: "Parent directory not found".to_string(),
            };
        }
    };

    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    if let Err(e) = add_entry_to_directory(&parent_handle, &file_name, &created.url(), "file") {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to add to directory: {}", e),
        };
    }

    // Track modified documents for sync
    ctx.track_modified(created.clone());
    ctx.track_modified(parent_handle);

    // Update snapshot
    let heads = get_document_heads(&created);
    ctx.snapshot.add_file(
        path.to_string_lossy().to_string(),
        SnapshotFileEntry {
            path: abs_path,
            url: created.url(),
            head: heads,
            extension: local.extension.clone(),
            mime_type: local.mime_type.clone(),
        },
    );

    SyncResult::Pushed { path: path.clone() }
}

async fn execute_remote_new_file(
    path: &PathBuf,
    remote: &RepoFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let abs_path = ctx.root.join(path);

    // Get file content from document at the recorded heads
    let file_info = match get_file_info_at_heads(&remote.handle, &remote.heads) {
        Ok(info) => info,
        Err(e) => {
            return SyncResult::Error {
                path: path.clone(),
                message: format!("Failed to read file document: {}", e),
            };
        }
    };
    let (content, extension, mime_type, permissions) = (
        file_info.content,
        file_info.extension,
        file_info.mime_type,
        file_info.permissions,
    );

    // Create parent directories if needed
    if let Some(parent) = abs_path.parent()
        && let Err(e) = std::fs::create_dir_all(parent)
    {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to create parent directories: {}", e),
        };
    }

    // Write file
    if let Err(e) = std::fs::write(&abs_path, &content) {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to write file: {}", e),
        };
    }

    // Set permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&abs_path, std::fs::Permissions::from_mode(permissions));
    }

    // Update snapshot
    ctx.snapshot.add_file(
        path.to_string_lossy().to_string(),
        SnapshotFileEntry {
            path: abs_path,
            url: remote.url.clone(),
            head: remote.heads.clone(),
            extension,
            mime_type,
        },
    );

    SyncResult::Pulled { path: path.clone() }
}

async fn execute_local_edit(
    path: &PathBuf,
    local: &FsFile,
    remote: &RepoFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    // Update the remote document with local content
    let new_content = if local.is_text {
        FileContent::text(String::from_utf8_lossy(&local.content).into_owned())
    } else {
        FileContent::binary(local.content.clone())
    };

    match update_file_document(&remote.handle, new_content, Some(local.permissions as i64)) {
        Ok(heads) => {
            // Track modified document for sync
            ctx.track_modified(remote.handle.clone());

            // Update snapshot
            ctx.snapshot.add_file(
                path.to_string_lossy().to_string(),
                SnapshotFileEntry {
                    path: ctx.root.join(path),
                    url: remote.url.clone(),
                    head: heads,
                    extension: local.extension.clone(),
                    mime_type: local.mime_type.clone(),
                },
            );
            SyncResult::Uploaded { path: path.clone() }
        }
        Err(e) => SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to update file: {}", e),
        },
    }
}

async fn execute_remote_edit(
    path: &PathBuf,
    remote: &RepoFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let abs_path = ctx.root.join(path);

    // Get content from remote at the recorded heads
    let file_info = match get_file_info_at_heads(&remote.handle, &remote.heads) {
        Ok(info) => info,
        Err(e) => {
            return SyncResult::Error {
                path: path.clone(),
                message: format!("Failed to read file document: {}", e),
            };
        }
    };
    let (content, extension, mime_type, permissions) = (
        file_info.content,
        file_info.extension,
        file_info.mime_type,
        file_info.permissions,
    );

    // Write to filesystem
    if let Err(e) = std::fs::write(&abs_path, &content) {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to write file: {}", e),
        };
    }

    // Set permissions
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&abs_path, std::fs::Permissions::from_mode(permissions));
    }

    // Update snapshot
    ctx.snapshot.add_file(
        path.to_string_lossy().to_string(),
        SnapshotFileEntry {
            path: abs_path,
            url: remote.url.clone(),
            head: remote.heads.clone(),
            extension,
            mime_type,
        },
    );

    SyncResult::Updated { path: path.clone() }
}

async fn execute_conflict(
    path: &PathBuf,
    local: &FsFile,
    snap: &super::state::SnapFile,
    remote: &RepoFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    // Merge local changes into the document using CRDT merge.
    // This:
    // 1. Forks the document at snapshot heads (common ancestor)
    // 2. Applies local changes to the fork
    // 3. Merges the fork back into the main document (which has remote changes)
    // 4. The CRDT automatically resolves conflicts
    let abs_path = ctx.root.join(path);

    let local_content = if local.is_text {
        FileContent::text(String::from_utf8_lossy(&local.content).into_owned())
    } else {
        FileContent::binary(local.content.clone())
    };

    let new_heads = match sync_ops::merge_local_into_remote(
        &remote.handle,
        &snap.heads,
        local_content,
        Some(local.permissions as i64),
    ) {
        Ok(heads) => heads,
        Err(e) => {
            return SyncResult::Error {
                path: path.clone(),
                message: format!("Failed to merge: {}", e),
            };
        }
    };

    // Track the modified document
    ctx.track_modified(remote.handle.clone());

    // Read the merged content from the document and write to filesystem
    let (merged_content, extension, mime_type) = match remote.handle.with_document(|doc| {
        let file_doc: FileDocument = hydrate(doc).ok()?;
        Some((
            file_doc.content_bytes().to_vec(),
            file_doc.extension_str().to_string(),
            file_doc.mime_type_str().to_string(),
        ))
    }) {
        Some(data) => data,
        None => {
            return SyncResult::Error {
                path: path.clone(),
                message: "Failed to read merged content".to_string(),
            };
        }
    };

    // Write merged content to filesystem
    if let Err(e) = std::fs::write(&abs_path, &merged_content) {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to write file: {}", e),
        };
    }

    // Update snapshot with new merged heads
    ctx.snapshot.add_file(
        path.to_string_lossy().to_string(),
        SnapshotFileEntry {
            path: abs_path,
            url: remote.url.clone(),
            head: new_heads,
            extension,
            mime_type,
        },
    );

    SyncResult::Merged { path: path.clone() }
}

async fn execute_local_delete_file(
    path: &PathBuf,
    _remote: &RepoFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let file_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Find parent directory URL from snapshot
    let parent_path = path.parent();
    let is_root_level = parent_path
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true);

    let parent_url = if is_root_level {
        // Root level file - use root_directory_url
        ctx.snapshot.root_directory_url.clone()
    } else {
        // Look up parent directory in snapshot
        let parent_abs = ctx.root.join(parent_path.unwrap());
        ctx.snapshot
            .get_directory(&parent_abs)
            .map(|d| d.url.clone())
    };

    // Remove from parent directory document if we have the URL
    if let Some(url) = parent_url
        && let Ok(Some(handle)) = ctx.repo.find(url.document_id().clone()).await
        && sync_ops::remove_entry_from_directory(&handle, file_name).is_ok()
    {
        ctx.track_modified(handle);
    }

    ctx.snapshot.remove_file(&ctx.root.join(path));
    SyncResult::DeletedRemote { path: path.clone() }
}

async fn execute_remote_delete_file(
    path: &PathBuf,
    _local: &FsFile,
    _snap: &super::state::SnapFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    // Delete file from filesystem
    let abs_path = ctx.root.join(path);
    if let Err(e) = std::fs::remove_file(&abs_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to delete file: {}", e),
        };
    }

    ctx.snapshot.remove_file(&abs_path);
    SyncResult::DeletedRemote { path: path.clone() }
}

async fn execute_move(
    new_path: &PathBuf,
    old_path: &PathBuf,
    remote: &RepoFile,
    new_content: &FsFile,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let old_name = old_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let new_name = new_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");
    let new_extension = new_path.extension().and_then(|e| e.to_str()).unwrap_or("");

    // Update file document with new name
    if let Err(e) = sync_ops::update_file_name(&remote.handle, new_name, new_extension) {
        return SyncResult::Error {
            path: new_path.clone(),
            message: format!("Failed to update file name: {}", e),
        };
    }

    // Update parent directory: remove old entry, add new entry
    let old_parent = old_path.parent();
    let new_parent = new_path.parent();
    let old_is_root = old_parent.map(|p| p.as_os_str().is_empty()).unwrap_or(true);
    let new_is_root = new_parent.map(|p| p.as_os_str().is_empty()).unwrap_or(true);

    // Get old parent directory handle
    let old_parent_url = if old_is_root {
        ctx.snapshot.root_directory_url.clone()
    } else {
        let old_parent_abs = ctx.root.join(old_parent.unwrap());
        ctx.snapshot
            .get_directory(&old_parent_abs)
            .map(|d| d.url.clone())
    };

    // Remove from old parent
    if let Some(url) = old_parent_url
        && let Ok(Some(handle)) = ctx.repo.find(url.document_id().clone()).await
        && sync_ops::remove_entry_from_directory(&handle, old_name).is_ok()
    {
        ctx.track_modified(handle.clone());

        // If same directory, also add the new entry here
        if (old_parent == new_parent || (old_is_root && new_is_root))
            && add_entry_to_directory(&handle, new_name, &remote.url, "file").is_ok()
        {
            // Already tracked above
        }
    }

    // If different directories, add to new parent
    if old_parent != new_parent && !(old_is_root && new_is_root) {
        let new_parent_url = if new_is_root {
            ctx.snapshot.root_directory_url.clone()
        } else {
            let new_parent_abs = ctx.root.join(new_parent.unwrap());
            ctx.snapshot
                .get_directory(&new_parent_abs)
                .map(|d| d.url.clone())
        };

        if let Some(url) = new_parent_url
            && let Ok(Some(handle)) = ctx.repo.find(url.document_id().clone()).await
            && add_entry_to_directory(&handle, new_name, &remote.url, "file").is_ok()
        {
            ctx.track_modified(handle);
        }
    }

    // Update content if changed
    let new_content_fc = if new_content.is_text {
        FileContent::text(String::from_utf8_lossy(&new_content.content).into_owned())
    } else {
        FileContent::binary(new_content.content.clone())
    };

    let heads = match update_file_document(
        &remote.handle,
        new_content_fc,
        Some(new_content.permissions as i64),
    ) {
        Ok(h) => h,
        Err(e) => {
            return SyncResult::Error {
                path: new_path.clone(),
                message: format!("Failed to update file content: {}", e),
            };
        }
    };

    // Track modified document for sync
    ctx.track_modified(remote.handle.clone());

    // Remove old snapshot entry, add new one
    ctx.snapshot.remove_file(&ctx.root.join(old_path));
    ctx.snapshot.add_file(
        new_path.to_string_lossy().to_string(),
        SnapshotFileEntry {
            path: ctx.root.join(new_path),
            url: remote.url.clone(),
            head: heads,
            extension: new_content.extension.clone(),
            mime_type: new_content.mime_type.clone(),
        },
    );

    SyncResult::Moved {
        old_path: old_path.clone(),
        new_path: new_path.clone(),
    }
}

// Directory operation implementations

async fn execute_local_new_dir(
    path: &PathBuf,
    parent_url: &AutomergeUrl,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    // Create directory document
    let created = match create_directory_document(ctx.repo).await {
        Ok(c) => c,
        Err(e) => {
            return SyncResult::Error {
                path: path.clone(),
                message: format!("Failed to create directory document: {}", e),
            };
        }
    };

    // Add to parent directory
    let parent_handle = match ctx.repo.find(parent_url.document_id().clone()).await {
        Ok(Some(h)) => h,
        _ => {
            return SyncResult::Error {
                path: path.clone(),
                message: "Parent directory not found".to_string(),
            };
        }
    };

    let dir_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    if let Err(e) = add_entry_to_directory(&parent_handle, &dir_name, &created.url(), "folder") {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to add to parent directory: {}", e),
        };
    }

    // Track modified documents for sync
    ctx.track_modified(created.clone());
    ctx.track_modified(parent_handle);

    // Update snapshot
    let heads = get_document_heads(&created);
    ctx.snapshot.add_directory(
        path.to_string_lossy().to_string(),
        SnapshotDirectoryEntry {
            path: ctx.root.join(path),
            url: created.url(),
            head: heads,
            entries: vec![],
        },
    );

    SyncResult::DirCreatedLocal { path: path.clone() }
}

async fn execute_remote_new_dir(
    path: &PathBuf,
    remote: &RepoDir,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let abs_path = ctx.root.join(path);

    // Create directory on filesystem
    if let Err(e) = std::fs::create_dir_all(&abs_path) {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to create directory: {}", e),
        };
    }

    // Update snapshot
    let entry_names: Vec<String> = remote.entries.iter().map(|e| e.name.clone()).collect();
    ctx.snapshot.add_directory(
        path.to_string_lossy().to_string(),
        SnapshotDirectoryEntry {
            path: abs_path,
            url: remote.url.clone(),
            head: remote.heads.clone(),
            entries: entry_names,
        },
    );

    SyncResult::DirCreatedRemote { path: path.clone() }
}

async fn execute_local_delete_dir(
    path: &PathBuf,
    _remote: &RepoDir,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let dir_name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown");

    // Find parent directory URL from snapshot
    let parent_path = path.parent();
    let is_root_level = parent_path
        .map(|p| p.as_os_str().is_empty())
        .unwrap_or(true);

    let parent_url = if is_root_level {
        // Root level directory - use root_directory_url
        ctx.snapshot.root_directory_url.clone()
    } else {
        // Look up parent directory in snapshot
        let parent_abs = ctx.root.join(parent_path.unwrap());
        ctx.snapshot
            .get_directory(&parent_abs)
            .map(|d| d.url.clone())
    };

    // Remove from parent directory document if we have the URL
    if let Some(url) = parent_url
        && let Ok(Some(handle)) = ctx.repo.find(url.document_id().clone()).await
        && sync_ops::remove_entry_from_directory(&handle, dir_name).is_ok()
    {
        ctx.track_modified(handle);
    }

    ctx.snapshot.remove_directory(&ctx.root.join(path));
    SyncResult::DirDeletedRemote { path: path.clone() }
}

async fn execute_remote_delete_dir(
    path: &PathBuf,
    _snap: &super::state::SnapDir,
    ctx: &mut ExecuteContext<'_>,
) -> SyncResult {
    let abs_path = ctx.root.join(path);

    // Delete directory from filesystem
    if let Err(e) = std::fs::remove_dir_all(&abs_path)
        && e.kind() != std::io::ErrorKind::NotFound
    {
        return SyncResult::Error {
            path: path.clone(),
            message: format!("Failed to delete directory: {}", e),
        };
    }

    ctx.snapshot.remove_directory(&abs_path);
    SyncResult::DirDeletedLocal { path: path.clone() }
}

// Helper functions

fn add_entry_to_directory(
    handle: &DocHandle,
    name: &str,
    url: &AutomergeUrl,
    entry_type: &str,
) -> Result<(), String> {
    handle.with_document(|doc| {
        let mut dir: DirectoryDocument =
            hydrate(doc).map_err(|e| format!("Failed to hydrate: {}", e))?;

        let entry = if entry_type == "folder" {
            DirectoryEntry::folder(name.to_string(), url.to_string())
        } else {
            DirectoryEntry::file(name.to_string(), url.to_string())
        };
        dir.docs.push(entry);

        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &dir)
                .map_err(|e| automerge::AutomergeError::InvalidObjId(format!("{}", e)))?;
            Ok(())
        })
        .map_err(|e| format!("Failed to reconcile: {:?}", e))?;

        Ok(())
    })
}

fn update_file_snapshot(ctx: &mut ExecuteContext<'_>, path: &PathBuf, remote: &RepoFile) {
    // Get file info from the document at the recorded heads
    // (not from filesystem, which may have changed since sync started)
    let abs_path = ctx.root.join(path);

    let (extension, mime_type) =
        remote
            .handle
            .with_document(|doc| match doc.fork_at(&remote.heads) {
                Ok(forked) => {
                    let file_doc: Result<FileDocument, _> = hydrate(&forked);
                    match file_doc {
                        Ok(fd) => (
                            fd.extension_str().to_string(),
                            fd.mime_type_str().to_string(),
                        ),
                        Err(_) => (String::new(), String::new()),
                    }
                }
                Err(_) => (String::new(), String::new()),
            });

    ctx.snapshot.add_file(
        path.to_string_lossy().to_string(),
        SnapshotFileEntry {
            path: abs_path,
            url: remote.url.clone(),
            head: remote.heads.clone(),
            extension,
            mime_type,
        },
    );
}

fn update_dir_snapshot(ctx: &mut ExecuteContext<'_>, path: &PathBuf, remote: &RepoDir) {
    // Use entries from repo state at sync start (not from filesystem)
    let entry_names: Vec<String> = remote.entries.iter().map(|e| e.name.clone()).collect();
    ctx.snapshot.add_directory(
        path.to_string_lossy().to_string(),
        SnapshotDirectoryEntry {
            path: ctx.root.join(path),
            url: remote.url.clone(),
            head: remote.heads.clone(),
            entries: entry_names,
        },
    );
}
