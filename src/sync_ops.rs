//! Sync operations for creating and updating Automerge documents.

use std::path::{Path, PathBuf};

use automerge::{Automerge, ChangeHash};
use autosurgeon::{hydrate, reconcile};
use samod::{AutomergeUrl, ConnectionId, DocHandle, Repo};
use thiserror::Error;

use crate::documents::{DirectoryDocument, DirectoryEntry, FileContent, FileDocument};
use crate::files::{get_file_permissions, FileInfo};
use crate::snapshot::SnapshotFileEntry;

/// Errors that can occur during sync operations
#[derive(Debug, Error)]
pub enum SyncError {
    /// IO error reading file
    #[error("Failed to read file '{path}': {source}")]
    ReadFile {
        path: String,
        #[source]
        source: std::io::Error,
    },

    /// Failed to create document in repo
    #[error("Failed to create document: {0}")]
    CreateDocument(String),

    /// Automerge/autosurgeon error
    #[error("Document error: {0}")]
    Document(String),

    /// Failed to hydrate document
    #[error("Failed to hydrate document: {0}")]
    Hydrate(String),

    /// Failed to reconcile document
    #[error("Failed to reconcile document: {0}")]
    Reconcile(String),
}

/// Result of creating a file document
pub struct CreatedFileDocument {
    /// The document handle
    pub handle: DocHandle,
    /// The automerge URL
    pub url: AutomergeUrl,
    /// The filename (for directory entry)
    pub name: String,
}

/// Create an Automerge file document from a local file
///
/// This reads the file content, detects its type, and creates an Automerge
/// document in the repo. Supports both text and binary files.
pub async fn create_file_document(
    repo: &Repo,
    absolute_path: &Path,
    file_info: &FileInfo,
) -> Result<CreatedFileDocument, SyncError> {
    // Read file content - binary read works for both text and binary
    let bytes = std::fs::read(absolute_path).map_err(|e| SyncError::ReadFile {
        path: absolute_path.to_string_lossy().to_string(),
        source: e,
    })?;

    // Get file permissions
    let permissions = get_file_permissions(absolute_path).unwrap_or(0o644) as i64;

    // Get filename
    let name = absolute_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    // Create FileDocument with appropriate content type
    let file_doc = if file_info.is_text {
        // For text files, convert to String
        let content = String::from_utf8_lossy(&bytes).into_owned();
        FileDocument::new(
            name.clone(),
            file_info.extension.clone(),
            file_info.mime_type.clone(),
            &content,
            permissions,
        )
    } else {
        // For binary files, use raw bytes
        FileDocument::new_binary(
            name.clone(),
            file_info.extension.clone(),
            file_info.mime_type.clone(),
            bytes,
            permissions,
        )
    };

    // Create Automerge document
    let mut doc = Automerge::new();
    doc.transact::<_, _, automerge::AutomergeError>(|txn| {
        reconcile(txn, &file_doc).map_err(|e| {
            automerge::AutomergeError::InvalidObjId(format!("reconcile failed: {}", e))
        })?;
        Ok(())
    })
    .map_err(|e| SyncError::Document(format!("{:?}", e)))?;

    // Create document in repo
    let handle = repo
        .create(doc)
        .await
        .map_err(|e| SyncError::CreateDocument(format!("{:?}", e)))?;

    let url = handle.url();

    Ok(CreatedFileDocument {
        handle,
        url,
        name,
    })
}

/// Add a file entry to a directory document
///
/// This modifies the directory document to include a new file entry.
pub fn add_file_to_directory(dir: &mut DirectoryDocument, name: String, url: &AutomergeUrl) {
    dir.docs
        .push(DirectoryEntry::file(name, url.to_string()));
}

/// Update a directory document with new file entries
///
/// This loads the directory document, adds the new file entries, and saves it back.
/// Returns the updated DirectoryDocument.
pub fn update_directory_with_files(
    dir_handle: &DocHandle,
    new_files: &[(String, AutomergeUrl)],
) -> Result<DirectoryDocument, SyncError> {
    dir_handle.with_document(|doc| {
        // Hydrate the existing directory document
        let mut dir: DirectoryDocument =
            hydrate(doc).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        // Add new file entries
        for (name, url) in new_files {
            add_file_to_directory(&mut dir, name.clone(), url);
        }

        // Reconcile back to Automerge
        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &dir).map_err(|e| {
                automerge::AutomergeError::InvalidObjId(format!("reconcile failed: {}", e))
            })?;
            Ok(())
        })
        .map_err(|e| SyncError::Reconcile(format!("{:?}", e)))?;

        Ok(dir)
    })
}

/// Wait for the sync server to have our changes for multiple documents
///
/// This ensures that changes have been replicated to the sync server for all
/// provided document handles. Waits concurrently for efficiency.
pub async fn wait_for_all_synced(handles: &[&DocHandle], conn_id: ConnectionId) {
    let futures: Vec<_> = handles
        .iter()
        .map(|h| h.they_have_our_changes(conn_id))
        .collect();

    futures::future::join_all(futures).await;
}

/// Get the current heads of a document
pub fn get_document_heads(handle: &DocHandle) -> Vec<ChangeHash> {
    handle.with_document(|doc| doc.get_heads())
}

/// Get the content of a file document at specific heads
///
/// This forks the document at the given heads and extracts the content,
/// allowing comparison against the current local file content.
/// Returns raw bytes which works for both text and binary files.
pub fn get_file_content_at_heads(
    handle: &DocHandle,
    heads: &[ChangeHash],
) -> Result<Vec<u8>, SyncError> {
    handle.with_document(|doc| {
        // Fork the document at the specified heads
        let forked = doc
            .fork_at(heads)
            .map_err(|e| SyncError::Document(format!("Failed to fork at heads: {}", e)))?;

        // Hydrate to get the FileDocument
        let file_doc: FileDocument =
            hydrate(&forked).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        Ok(file_doc.content_bytes().to_vec())
    })
}

/// Update an existing file document with new content
///
/// This loads the document, updates the content field, and reconciles
/// the changes back. Returns the new heads after the update.
pub fn update_file_document(
    handle: &DocHandle,
    new_content: FileContent,
    new_permissions: Option<i64>,
) -> Result<Vec<ChangeHash>, SyncError> {
    handle.with_document(|doc| {
        // Hydrate to get current FileDocument
        let mut file_doc: FileDocument =
            hydrate(doc).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        // Update content
        file_doc.content = new_content;

        // Update permissions if provided
        if let Some(perms) = new_permissions {
            file_doc.metadata.permissions = perms;
        }

        // Reconcile changes back
        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &file_doc).map_err(|e| {
                automerge::AutomergeError::InvalidObjId(format!("reconcile failed: {}", e))
            })?;
            Ok(())
        })
        .map_err(|e| SyncError::Reconcile(format!("{:?}", e)))?;

        // Return new heads
        Ok(doc.get_heads())
    })
}

/// Create a SnapshotFileEntry for a synced file
///
/// This captures the current state of the file document for the snapshot.
pub fn create_snapshot_file_entry(
    handle: &DocHandle,
    absolute_path: PathBuf,
    file_info: &FileInfo,
) -> SnapshotFileEntry {
    let heads = get_document_heads(handle);
    let url = handle.url();

    SnapshotFileEntry {
        path: absolute_path,
        url,
        head: heads,
        extension: file_info.extension.clone(),
        mime_type: file_info.mime_type.clone(),
    }
}

/// Write remote file content to the local filesystem
///
/// Reads the current content from the document and writes it to the specified path.
/// Also sets file permissions on Unix systems.
pub fn write_remote_file_to_disk(handle: &DocHandle, path: &Path) -> Result<(), SyncError> {
    // Hydrate once to get both content and permissions
    let (content, permissions) = handle.with_document(|doc| {
        let file_doc: FileDocument =
            hydrate(doc).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;
        Ok((file_doc.content_bytes().to_vec(), file_doc.metadata.permissions))
    })?;

    // Write to disk
    std::fs::write(path, &content).map_err(|e| SyncError::ReadFile {
        path: path.to_string_lossy().to_string(),
        source: e,
    })?;

    // Set permissions (Unix only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(permissions as u32);
        if let Err(e) = std::fs::set_permissions(path, perms) {
            eprintln!(
                "  Warning: Failed to set permissions for '{}': {}",
                path.display(),
                e
            );
        }
    }

    Ok(())
}

/// Merge local changes into a document that has remote changes
///
/// This implements CRDT merge for conflict resolution:
/// 1. Fork the document at the snapshot heads (common ancestor)
/// 2. Apply local changes to the fork
/// 3. Merge the fork back into the main document
/// 4. The CRDT automatically resolves conflicts
///
/// Returns the new heads after merge and the merged content.
pub fn merge_local_into_remote(
    handle: &DocHandle,
    snapshot_heads: &[ChangeHash],
    local_content: FileContent,
    local_permissions: Option<i64>,
) -> Result<Vec<ChangeHash>, SyncError> {
    handle.with_document(|doc| {
        // Fork at snapshot heads (the common ancestor state)
        let mut fork = doc
            .fork_at(snapshot_heads)
            .map_err(|e| SyncError::Document(format!("Failed to fork at snapshot heads: {}", e)))?;

        // Apply local changes to the fork
        let mut file_doc: FileDocument =
            hydrate(&fork).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        file_doc.content = local_content;
        if let Some(perms) = local_permissions {
            file_doc.metadata.permissions = perms;
        }

        fork.transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &file_doc).map_err(|e| {
                automerge::AutomergeError::InvalidObjId(format!("reconcile failed: {}", e))
            })?;
            Ok(())
        })
        .map_err(|e| SyncError::Reconcile(format!("{:?}", e)))?;

        // Merge fork (with local changes) back into main doc (with remote changes)
        doc.merge(&mut fork)
            .map_err(|e| SyncError::Document(format!("Failed to merge: {}", e)))?;

        Ok(doc.get_heads())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_create_file_document() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello, world!").unwrap();

        let file_info = FileInfo::from_path(&file_path);

        // Create an in-memory repo
        let repo = Repo::build_tokio().load().await;

        let result = create_file_document(&repo, &file_path, &file_info).await;
        assert!(result.is_ok());

        let created = result.unwrap();
        assert_eq!(created.name, "test.txt");
        assert!(created.url.to_string().starts_with("automerge:"));
    }

    #[tokio::test]
    async fn test_create_file_document_binary() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("image.png");
        let png_data = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]; // PNG header
        fs::write(&file_path, png_data).unwrap();

        let file_info = FileInfo::from_path(&file_path);
        assert!(!file_info.is_text);

        let repo = Repo::build_tokio().load().await;

        let result = create_file_document(&repo, &file_path, &file_info).await;
        assert!(result.is_ok());

        let created = result.unwrap();
        assert_eq!(created.name, "image.png");

        // Verify the document contains binary content
        let file_doc: FileDocument =
            created.handle.with_document(|doc| hydrate(doc).unwrap());
        assert!(file_doc.is_binary());
        assert_eq!(file_doc.content_bytes(), png_data);
    }

    #[test]
    fn test_add_file_to_directory() {
        let mut dir = DirectoryDocument::new();
        let url: AutomergeUrl = "automerge:550e8400-e29b-41d4-a716-446655440000"
            .parse()
            .unwrap();

        add_file_to_directory(&mut dir, "test.txt".to_string(), &url);

        assert_eq!(dir.docs.len(), 1);
        assert_eq!(dir.docs[0].name_str(), "test.txt");
        assert_eq!(dir.docs[0].entry_type_str(), "file");
    }

    #[tokio::test]
    async fn test_update_directory_with_files() {
        // Create a repo and an empty directory document
        let repo = Repo::build_tokio().load().await;

        let dir = DirectoryDocument::new();
        let mut doc = Automerge::new();
        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &dir).unwrap();
            Ok(())
        })
        .unwrap();

        let dir_handle = repo.create(doc).await.unwrap();

        // Create URLs for new files
        let url1: AutomergeUrl = "automerge:550e8400-e29b-41d4-a716-446655440000"
            .parse()
            .unwrap();
        let url2: AutomergeUrl = "automerge:6ba7b810-9dad-11d1-80b4-00c04fd430c8"
            .parse()
            .unwrap();

        let new_files = vec![
            ("file1.txt".to_string(), url1),
            ("file2.txt".to_string(), url2),
        ];

        // Update the directory
        let updated_dir = update_directory_with_files(&dir_handle, &new_files).unwrap();

        assert_eq!(updated_dir.docs.len(), 2);
        assert_eq!(updated_dir.docs[0].name_str(), "file1.txt");
        assert_eq!(updated_dir.docs[1].name_str(), "file2.txt");

        // Verify the changes persisted by re-reading
        let reloaded: DirectoryDocument = dir_handle.with_document(|doc| hydrate(doc).unwrap());
        assert_eq!(reloaded.docs.len(), 2);
    }

    #[tokio::test]
    async fn test_create_snapshot_file_entry() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Hello, world!").unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        let entry = create_snapshot_file_entry(&created.handle, file_path.clone(), &file_info);

        assert_eq!(entry.path, file_path);
        assert_eq!(entry.extension, "txt");
        assert_eq!(entry.mime_type, "text/plain");
        assert!(!entry.head.is_empty()); // Should have at least one head
        assert!(entry.url.to_string().starts_with("automerge:"));
    }

    #[tokio::test]
    async fn test_get_document_heads() {
        let repo = Repo::build_tokio().load().await;

        let dir = DirectoryDocument::new();
        let mut doc = Automerge::new();
        doc.transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &dir).unwrap();
            Ok(())
        })
        .unwrap();

        let handle = repo.create(doc).await.unwrap();
        let heads = get_document_heads(&handle);

        // A document with one transaction should have exactly one head
        assert_eq!(heads.len(), 1);
    }

    #[tokio::test]
    async fn test_get_file_content_at_heads() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        // Create initial file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Get heads after creation
        let initial_heads = get_document_heads(&created.handle);

        // Verify we can read content at those heads (now returns bytes)
        let content = get_file_content_at_heads(&created.handle, &initial_heads).unwrap();
        assert_eq!(content, b"Original content");

        // Now update the document with new content
        created.handle.with_document(|doc| {
            let mut file_doc: FileDocument = hydrate(doc).unwrap();
            file_doc.content = FileContent::text("Modified content");
            doc.transact::<_, _, automerge::AutomergeError>(|txn| {
                reconcile(txn, &file_doc).unwrap();
                Ok(())
            })
            .unwrap();
        });

        // Current content should be modified
        let current: FileDocument =
            created.handle.with_document(|doc| hydrate(doc).unwrap());
        assert_eq!(current.content_string(), "Modified content");

        // But content at original heads should still be original
        let old_content = get_file_content_at_heads(&created.handle, &initial_heads).unwrap();
        assert_eq!(old_content, b"Original content");
    }

    #[tokio::test]
    async fn test_update_file_document() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        // Create initial file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        let initial_heads = get_document_heads(&created.handle);

        // Update the document
        let new_heads =
            update_file_document(&created.handle, FileContent::text("Updated content"), Some(0o755)).unwrap();

        // Heads should have changed
        assert_ne!(initial_heads, new_heads);

        // Content should be updated
        let file_doc: FileDocument =
            created.handle.with_document(|doc| hydrate(doc).unwrap());
        assert_eq!(file_doc.content_string(), "Updated content");
        assert_eq!(file_doc.metadata.permissions, 0o755);
    }

    #[tokio::test]
    async fn test_update_file_document_content_only() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        // Create initial file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        let original_perms: i64 =
            created.handle.with_document(|doc| {
                let f: FileDocument = hydrate(doc).unwrap();
                f.metadata.permissions
            });

        // Update content only (no permissions change)
        update_file_document(&created.handle, FileContent::text("New content"), None).unwrap();

        // Permissions should be unchanged
        let file_doc: FileDocument =
            created.handle.with_document(|doc| hydrate(doc).unwrap());
        assert_eq!(file_doc.content_string(), "New content");
        assert_eq!(file_doc.metadata.permissions, original_perms);
    }

    #[tokio::test]
    async fn test_update_file_document_binary() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("image.png");
        let initial_data = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        fs::write(&file_path, initial_data).unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        // Create initial binary file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        let initial_heads = get_document_heads(&created.handle);

        // Update with new binary content
        let new_data: Vec<u8> = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        let new_heads =
            update_file_document(&created.handle, FileContent::binary(new_data.clone()), None).unwrap();

        // Heads should have changed
        assert_ne!(initial_heads, new_heads);

        // Content should be updated
        let file_doc: FileDocument =
            created.handle.with_document(|doc| hydrate(doc).unwrap());
        assert!(file_doc.is_binary());
        assert_eq!(file_doc.content_bytes(), &new_data);
    }

    #[tokio::test]
    async fn test_write_remote_file_to_disk() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("source.txt");
        fs::write(&source_path, "Remote content").unwrap();

        let file_info = FileInfo::from_path(&source_path);
        let repo = Repo::build_tokio().load().await;

        // Create file document
        let created = create_file_document(&repo, &source_path, &file_info)
            .await
            .unwrap();

        // Write to a new path
        let dest_path = temp_dir.path().join("dest.txt");
        write_remote_file_to_disk(&created.handle, &dest_path).unwrap();

        // Verify content was written
        let written_content = fs::read(&dest_path).unwrap();
        assert_eq!(written_content, b"Remote content");

        // Verify permissions were set (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let source_perms = fs::metadata(&source_path).unwrap().permissions().mode();
            let dest_perms = fs::metadata(&dest_path).unwrap().permissions().mode();
            assert_eq!(source_perms & 0o777, dest_perms & 0o777);
        }
    }

    #[tokio::test]
    async fn test_write_remote_file_to_disk_binary() {
        let temp_dir = TempDir::new().unwrap();
        let source_path = temp_dir.path().join("image.png");
        let binary_data = &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, 0x00, 0x11, 0x22];
        fs::write(&source_path, binary_data).unwrap();

        let file_info = FileInfo::from_path(&source_path);
        let repo = Repo::build_tokio().load().await;

        // Create binary file document
        let created = create_file_document(&repo, &source_path, &file_info)
            .await
            .unwrap();

        // Write to a new path
        let dest_path = temp_dir.path().join("copy.png");
        write_remote_file_to_disk(&created.handle, &dest_path).unwrap();

        // Verify binary content was written correctly
        let written_content = fs::read(&dest_path).unwrap();
        assert_eq!(written_content, binary_data);
    }

    #[tokio::test]
    async fn test_merge_local_into_remote() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        // Create file document
        let created = create_file_document(&repo, &file_path, &file_info)
            .await
            .unwrap();

        // Get snapshot heads (the common ancestor)
        let snapshot_heads = get_document_heads(&created.handle);

        // Simulate remote change: update the document directly
        update_file_document(&created.handle, FileContent::text("Remote change"), None).unwrap();

        // Simulate local change: different content
        let local_content = FileContent::text("Local change");

        // Merge local into remote
        let new_heads = merge_local_into_remote(
            &created.handle,
            &snapshot_heads,
            local_content,
            None,
        )
        .unwrap();

        // Heads should be different from both remote and snapshot
        assert_ne!(new_heads, snapshot_heads);

        // The merged document should contain both changes (CRDT merge)
        // For text, Automerge will have merged the changes
        let file_doc: FileDocument =
            created.handle.with_document(|doc| hydrate(doc).unwrap());

        // The content will be a CRDT merge - both "Remote change" and "Local change"
        // were applied to the same field, so one will win (last-writer-wins for scalar strings)
        // The important thing is that the merge succeeded without error
        let content = file_doc.content_string();
        assert!(!content.is_empty());
    }
}
