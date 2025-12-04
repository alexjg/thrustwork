//! Change detection for comparing local files against synced state.

use std::path::Path;

use samod::Repo;

use crate::files::read_text_file;
use crate::snapshot::{Snapshot, SnapshotFileEntry};
use crate::sync_ops::get_file_content_at_heads;

/// A file that has been modified locally since the last sync
#[derive(Debug)]
pub struct ModifiedFile {
    /// Path relative to the root directory
    pub relative_path: String,
    /// The snapshot entry for this file
    pub snapshot_entry: SnapshotFileEntry,
    /// The new content from disk
    pub new_content: String,
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

        // Read current content from disk
        let disk_content = match read_text_file(&absolute_path) {
            Ok(content) => content,
            Err(_) => {
                // Can't read file - skip for now
                continue;
            }
        };

        // Load the document and get content at snapshot heads
        let handle = match repo.find(entry.url.doc_id().clone()).await.expect("Repo stopped") {
            Some(h) => h,
            None => continue,
        };

        let snapshot_content = match get_file_content_at_heads(&handle, &entry.head) {
            Ok(content) => content,
            Err(_) => continue,
        };

        // Compare content
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
        assert_eq!(modified[0].new_content, "Modified content");
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
}
