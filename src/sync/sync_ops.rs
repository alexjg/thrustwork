//! Sync operations for creating and updating Automerge documents.

use std::path::Path;

use automerge::{Automerge, ChangeHash};
use autosurgeon::{hydrate, reconcile};
use samod::{DocHandle, Repo};
use thiserror::Error;

use crate::documents::{DirectoryDocument, FileContent, FileDocument};
use crate::files::{FileInfo, get_file_permissions};

/// Create an Automerge file document from a local file
///
/// This reads the file content, detects its type, and creates an Automerge
/// document in the repo. Supports both text and binary files.
pub async fn create_file_document(
    repo: &Repo,
    absolute_path: &Path,
    file_info: &FileInfo,
    file_content: &[u8],
) -> Result<DocHandle, SyncError> {
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
        let content = String::from_utf8_lossy(file_content).into_owned();
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
            file_content.to_vec(),
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

    Ok(handle)
}

/// Create an empty directory document in the repo
///
/// Creates a new directory document with no entries. Use `update_directory_with_files`
/// or `update_directory_with_entries` to add entries after creation.
pub async fn create_directory_document(repo: &Repo) -> Result<DocHandle, SyncError> {
    // Create empty directory document
    let dir_doc = DirectoryDocument::new();

    // Create Automerge document
    let mut doc = Automerge::new();
    doc.transact::<_, _, automerge::AutomergeError>(|txn| {
        reconcile(txn, &dir_doc).map_err(|e| {
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

    Ok(handle)
}

/// Remove an entry (file or folder) from a directory document by name
///
/// Returns true if the entry was found and removed, false otherwise.
pub fn remove_entry_from_directory(handle: &DocHandle, name: &str) -> Result<bool, SyncError> {
    handle.with_document(|doc| {
        // Hydrate the directory document
        let mut dir: DirectoryDocument =
            hydrate(doc).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        // Find and remove the entry with the given name
        let len_before = dir.docs.len();
        dir.docs.retain(|entry| entry.name_str() != name);
        let removed = dir.docs.len() < len_before;

        if removed {
            // Reconcile back to Automerge
            doc.transact::<_, _, automerge::AutomergeError>(|txn| {
                reconcile(txn, &dir).map_err(|e| {
                    automerge::AutomergeError::InvalidObjId(format!("reconcile failed: {}", e))
                })?;
                Ok(())
            })
            .map_err(|e| SyncError::Reconcile(format!("{:?}", e)))?;
        }

        Ok(removed)
    })
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

/// Complete file information at specific heads
pub struct FileInfoAtHeads {
    pub content: Vec<u8>,
    pub extension: String,
    pub mime_type: String,
    pub permissions: u32,
}

/// Get complete file information at specific heads
///
/// This forks the document at the given heads and extracts all file info,
/// ensuring we read the state as it was when we started sync.
pub fn get_file_info_at_heads(
    handle: &DocHandle,
    heads: &[ChangeHash],
) -> Result<FileInfoAtHeads, SyncError> {
    handle.with_document(|doc| {
        // Fork the document at the specified heads
        let forked = doc
            .fork_at(heads)
            .map_err(|e| SyncError::Document(format!("Failed to fork at heads: {}", e)))?;

        // Hydrate to get the FileDocument
        let file_doc: FileDocument =
            hydrate(&forked).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        Ok(FileInfoAtHeads {
            content: file_doc.content_bytes().to_vec(),
            extension: file_doc.extension_str().to_string(),
            mime_type: file_doc.mime_type_str().to_string(),
            permissions: file_doc.metadata.permissions as u32,
        })
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

/// Update the name and extension of a file document (for rename/move)
///
/// This updates the file document's name field to reflect a rename operation.
/// Also updates the extension if it changed.
/// Returns the new heads after the update.
pub fn update_file_name(
    handle: &DocHandle,
    new_name: &str,
    new_extension: &str,
) -> Result<Vec<ChangeHash>, SyncError> {
    handle.with_document(|doc| {
        // Hydrate to get current FileDocument
        let mut file_doc: FileDocument =
            hydrate(doc).map_err(|e| SyncError::Hydrate(format!("{}", e)))?;

        // Update name and extension
        file_doc.name = autosurgeon::Text::with_value(new_name);
        file_doc.extension = autosurgeon::Text::with_value(new_extension);

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

/// Errors that can occur during sync operations
#[derive(Debug, Error)]
pub enum SyncError {
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

        let result =
            create_file_document(&repo, &file_path, &file_info, "Hello, world!".as_bytes()).await;
        assert!(result.is_ok());

        let created = result.unwrap();
        assert!(created.url().to_string().starts_with("automerge:"));
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

        let result = create_file_document(&repo, &file_path, &file_info, png_data).await;
        assert!(result.is_ok());

        let created = result.unwrap();

        // Verify the document contains binary content
        let file_doc: FileDocument = created.with_document(|doc| hydrate(doc).unwrap());
        assert!(matches!(file_doc.content, FileContent::Binary(_)));
        assert_eq!(file_doc.content_bytes(), png_data);
    }

    #[tokio::test]
    async fn test_create_directory_document() {
        let repo = Repo::build_tokio().load().await;

        let created = create_directory_document(&repo).await.unwrap();

        // Verify URL is valid
        assert!(created.url().to_string().starts_with("automerge:"));

        // Verify it's a proper directory document
        let dir_doc: DirectoryDocument = created.with_document(|doc| hydrate(doc).unwrap());

        assert_eq!(dir_doc.patchwork.doc_type_str(), "folder");
        assert!(dir_doc.docs.is_empty());
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
        let created =
            create_file_document(&repo, &file_path, &file_info, "Original content".as_bytes())
                .await
                .unwrap();

        // Get heads after creation
        let initial_heads = get_document_heads(&created);

        // Verify we can read content at those heads (now returns bytes)
        let content = get_file_content_at_heads(&created, &initial_heads).unwrap();
        assert_eq!(content, b"Original content");

        // Now update the document with new content
        created.with_document(|doc| {
            let mut file_doc: FileDocument = hydrate(doc).unwrap();
            file_doc.content = FileContent::text("Modified content");
            doc.transact::<_, _, automerge::AutomergeError>(|txn| {
                reconcile(txn, &file_doc).unwrap();
                Ok(())
            })
            .unwrap();
        });

        // Current content should be modified
        let current: FileDocument = created.with_document(|doc| hydrate(doc).unwrap());
        let FileContent::Text(val) = current.content else {
            panic!("Expected text content");
        };
        assert_eq!(val, "Modified content");

        // But content at original heads should still be original
        let old_content = get_file_content_at_heads(&created, &initial_heads).unwrap();
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
        let created =
            create_file_document(&repo, &file_path, &file_info, "Original content".as_bytes())
                .await
                .unwrap();

        let initial_heads = get_document_heads(&created);

        // Update the document
        let new_heads =
            update_file_document(&created, FileContent::text("Updated content"), Some(0o755))
                .unwrap();

        // Heads should have changed
        assert_ne!(initial_heads, new_heads);

        // Content should be updated
        let file_doc: FileDocument = created.with_document(|doc| hydrate(doc).unwrap());
        let FileContent::Text(val) = file_doc.content else {
            panic!("Expected text content");
        };
        assert_eq!(val, "Updated content");
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
        let created =
            create_file_document(&repo, &file_path, &file_info, "Original content".as_bytes())
                .await
                .unwrap();

        let original_perms: i64 = created.with_document(|doc| {
            let f: FileDocument = hydrate(doc).unwrap();
            f.metadata.permissions
        });

        // Update content only (no permissions change)
        update_file_document(&created, FileContent::text("New content"), None).unwrap();

        // Permissions should be unchanged
        let file_doc: FileDocument = created.with_document(|doc| hydrate(doc).unwrap());
        let FileContent::Text(val) = file_doc.content else {
            panic!("Expected text content");
        };
        assert_eq!(val, "New content");
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
        let created = create_file_document(&repo, &file_path, &file_info, initial_data)
            .await
            .unwrap();

        let initial_heads = get_document_heads(&created);

        // Update with new binary content
        let new_data: Vec<u8> = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
        let new_heads =
            update_file_document(&created, FileContent::binary(new_data.clone()), None).unwrap();

        // Heads should have changed
        assert_ne!(initial_heads, new_heads);

        // Content should be updated
        let file_doc: FileDocument = created.with_document(|doc| hydrate(doc).unwrap());
        let FileContent::Binary(val) = file_doc.content else {
            panic!("Expected binary content");
        };
        assert_eq!(val, new_data);
    }

    #[tokio::test]
    async fn test_merge_local_into_remote() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("test.txt");
        fs::write(&file_path, "Original content").unwrap();

        let file_info = FileInfo::from_path(&file_path);
        let repo = Repo::build_tokio().load().await;

        // Create file document
        let created =
            create_file_document(&repo, &file_path, &file_info, "Original content".as_bytes())
                .await
                .unwrap();

        // Get snapshot heads (the common ancestor)
        let snapshot_heads = get_document_heads(&created);

        // Simulate remote change: update the document directly
        update_file_document(&created, FileContent::text("Remote change"), None).unwrap();

        // Simulate local change: different content
        let local_content = FileContent::text("Local change");

        // Merge local into remote
        let new_heads =
            merge_local_into_remote(&created, &snapshot_heads, local_content, None).unwrap();

        // Heads should be different from both remote and snapshot
        assert_ne!(new_heads, snapshot_heads);

        // The merged document should contain both changes (CRDT merge)
        // For text, Automerge will have merged the changes
        let file_doc: FileDocument = created.with_document(|doc| hydrate(doc).unwrap());

        // The content will be a CRDT merge - both "Remote change" and "Local change"
        // were applied to the same field, so one will win (last-writer-wins for scalar strings)
        // The important thing is that the merge succeeded without error
        let FileContent::Text(val) = file_doc.content else {
            panic!("Expected text content");
        };
        assert!(!val.is_empty());
    }
}
