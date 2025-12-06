//! Change detection for comparing local files against synced state.

use std::path::Path;

use automerge::ChangeHash;
use samod::{DocHandle, Repo};

use crate::snapshot::{Snapshot, SnapshotFileEntry};
use crate::sync_ops::{get_document_heads, get_file_content_at_heads};

/// A file that has been modified locally since the last sync
#[derive(Debug)]
pub struct ModifiedFile {
    /// Path relative to the root directory
    pub relative_path: String,
    /// The snapshot entry for this file
    pub snapshot_entry: SnapshotFileEntry,
    /// The new content from disk (raw bytes - works for text and binary)
    pub new_content: Vec<u8>,
}

/// A file that has been modified remotely since the last sync
pub struct RemotelyChangedFile {
    /// Path relative to the root directory
    pub relative_path: String,
    /// The snapshot entry for this file
    pub snapshot_entry: SnapshotFileEntry,
    /// The document handle (for reading content)
    pub handle: DocHandle,
    /// The new heads from the remote document
    pub new_heads: Vec<ChangeHash>,
}

/// Detect files that have been modified locally since the last sync
///
/// This compares the current disk content against the document content
/// at the heads stored in the snapshot. Files where the content differs
/// are returned as modified.
pub async fn detect_modified_files(
    repo: &Repo,
    root: &Path,
    snapshot: &Snapshot,
) -> Vec<ModifiedFile> {
    let mut modified = Vec::new();

    for (relative_path, entry) in &snapshot.files {
        // Check if file still exists on disk
        let absolute_path = root.join(relative_path);
        if !absolute_path.exists() {
            // File was deleted - handled in a later phase
            continue;
        }

        // Read current content from disk (as bytes - works for text and binary)
        let disk_content = match std::fs::read(&absolute_path) {
            Ok(content) => content,
            Err(_) => {
                // Can't read file - skip for now
                continue;
            }
        };

        // Load the document and get content at snapshot heads
        let handle = match repo.find(entry.url.document_id().clone()).await.expect("Repo stopped") {
            Some(h) => h,
            None => continue,
        };

        let snapshot_content = match get_file_content_at_heads(&handle, &entry.head) {
            Ok(content) => content,
            Err(_) => continue,
        };

        // Compare content (byte-level comparison works for both text and binary)
        if disk_content != snapshot_content {
            modified.push(ModifiedFile {
                relative_path: relative_path.clone(),
                snapshot_entry: entry.clone(),
                new_content: disk_content,
            });
        }
    }

    modified
}

/// Detect files that have been modified remotely since the last sync
///
/// This compares the current document heads against the heads stored in
/// the snapshot. Files where the document has different heads are returned
/// as remotely changed.
pub async fn detect_remote_changes(repo: &Repo, snapshot: &Snapshot) -> Vec<RemotelyChangedFile> {
    // Collect file entries for parallel processing
    let entries: Vec<_> = snapshot.files.iter().collect();

    // Load all documents in parallel
    let futures: Vec<_> = entries
        .iter()
        .map(|(relative_path, entry)| check_single_file_remote(repo, relative_path, entry))
        .collect();

    let results = futures::future::join_all(futures).await;

    // Filter to only changed files
    results.into_iter().flatten().collect()
}

/// Check a single file for remote changes
async fn check_single_file_remote(
    repo: &Repo,
    relative_path: &str,
    entry: &SnapshotFileEntry,
) -> Option<RemotelyChangedFile> {
    // Load the document
    let handle = repo
        .find(entry.url.document_id().clone())
        .await
        .expect("Repo stopped")?;

    // Get current document heads
    let current_heads = get_document_heads(&handle);

    // Compare with snapshot heads
    if current_heads != entry.head {
        Some(RemotelyChangedFile {
            relative_path: relative_path.to_string(),
            snapshot_entry: entry.clone(),
            handle,
            new_heads: current_heads,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::files::FileInfo;
    use crate::sync_ops::{create_file_document, create_snapshot_file_entry};
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_detect_modified_files_no_changes() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let repo = Repo::build_tokio().load().await;
        let file_info = FileInfo::from_path(&file_path);

        // Create file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Create snapshot with current heads
        let entry = create_snapshot_file_entry(&created.handle, file_path.clone(), &file_info);
        let mut snapshot = Snapshot::new(root.to_path_buf(), None);
        snapshot.add_file("test.txt".to_string(), entry);

        // File hasn't changed - should detect no modifications
        let modified = detect_modified_files(&repo, root, &snapshot).await;
        assert!(modified.is_empty());
    }

    #[tokio::test]
    async fn test_detect_modified_files_with_changes() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let repo = Repo::build_tokio().load().await;
        let file_info = FileInfo::from_path(&file_path);

        // Create file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Create snapshot with current heads
        let entry = create_snapshot_file_entry(&created.handle, file_path.clone(), &file_info);
        let mut snapshot = Snapshot::new(root.to_path_buf(), None);
        snapshot.add_file("test.txt".to_string(), entry);

        // Now modify the file on disk
        fs::write(&file_path, "Modified content").unwrap();

        // Should detect the modification
        let modified = detect_modified_files(&repo, root, &snapshot).await;
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].relative_path, "test.txt");
        assert_eq!(modified[0].new_content, b"Modified content");
    }

    #[tokio::test]
    async fn test_detect_modified_files_deleted_file() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let repo = Repo::build_tokio().load().await;
        let file_info = FileInfo::from_path(&file_path);

        // Create file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Create snapshot with current heads
        let entry = create_snapshot_file_entry(&created.handle, file_path.clone(), &file_info);
        let mut snapshot = Snapshot::new(root.to_path_buf(), None);
        snapshot.add_file("test.txt".to_string(), entry);

        // Delete the file
        fs::remove_file(&file_path).unwrap();

        // Should not detect as modified (deletions handled separately)
        let modified = detect_modified_files(&repo, root, &snapshot).await;
        assert!(modified.is_empty());
    }

    #[tokio::test]
    async fn test_detect_remote_changes_no_changes() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let repo = Repo::build_tokio().load().await;
        let file_info = FileInfo::from_path(&file_path);

        // Create file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Create snapshot with current heads
        let entry = create_snapshot_file_entry(&created.handle, file_path.clone(), &file_info);
        let mut snapshot = Snapshot::new(root.to_path_buf(), None);
        snapshot.add_file("test.txt".to_string(), entry);

        // Document hasn't changed - should detect no remote changes
        let changed = detect_remote_changes(&repo, &snapshot).await;
        assert!(changed.is_empty());
    }

    #[tokio::test]
    async fn test_detect_remote_changes_with_changes() {
        use crate::documents::FileContent;
        use crate::sync_ops::update_file_document;

        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let file_path = root.join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let repo = Repo::build_tokio().load().await;
        let file_info = FileInfo::from_path(&file_path);

        // Create file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Create snapshot with current heads
        let entry = create_snapshot_file_entry(&created.handle, file_path.clone(), &file_info);
        let mut snapshot = Snapshot::new(root.to_path_buf(), None);
        snapshot.add_file("test.txt".to_string(), entry);

        // Now modify the document (simulating a remote change)
        update_file_document(
            &created.handle,
            FileContent::text("Modified by remote"),
            None,
        )
        .unwrap();

        // Should detect the remote change
        let changed = detect_remote_changes(&repo, &snapshot).await;
        assert_eq!(changed.len(), 1);
        assert_eq!(changed[0].relative_path, "test.txt");
        // New heads should be different from snapshot heads
        assert_ne!(changed[0].new_heads, changed[0].snapshot_entry.head);
    }
}
