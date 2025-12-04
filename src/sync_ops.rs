//! Sync operations for creating and updating Automerge documents.

use std::path::Path;

use automerge::Automerge;
use autosurgeon::reconcile;
use samod::{AutomergeUrl, DocHandle, Repo};
use thiserror::Error;

use crate::documents::{DirectoryDocument, DirectoryEntry, FileDocument};
use crate::files::{get_file_permissions, read_text_file, FileInfo};

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

    /// File is not a text file
    #[error("File '{0}' is not a text file (binary files not yet supported)")]
    NotTextFile(String),

    /// Failed to create document in repo
    #[error("Failed to create document: {0}")]
    CreateDocument(String),

    /// Automerge/autosurgeon error
    #[error("Document error: {0}")]
    Document(String),
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
/// document in the repo.
pub async fn create_file_document(
    repo: &Repo,
    absolute_path: &Path,
    file_info: &FileInfo,
) -> Result<CreatedFileDocument, SyncError> {
    // For Phase 4, only support text files
    if !file_info.is_text {
        return Err(SyncError::NotTextFile(
            absolute_path.to_string_lossy().to_string(),
        ));
    }

    // Read file content
    let content = read_text_file(absolute_path).map_err(|e| SyncError::ReadFile {
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

    // Create FileDocument
    let file_doc = FileDocument::new(
        name.clone(),
        file_info.extension.clone(),
        file_info.mime_type.clone(),
        &content,
        permissions,
    );

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
    async fn test_create_file_document_not_text() {
        let temp_dir = TempDir::new().unwrap();
        let file_path = temp_dir.path().join("image.png");
        fs::write(&file_path, &[0x89, 0x50, 0x4E, 0x47]).unwrap(); // PNG header

        let file_info = FileInfo::from_path(&file_path);
        assert!(!file_info.is_text);

        let repo = Repo::build_tokio().load().await;

        let result = create_file_document(&repo, &file_path, &file_info).await;
        assert!(matches!(result, Err(SyncError::NotTextFile(_))));
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
}
