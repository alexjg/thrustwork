//! Automerge document types for pushwork compatibility.
//!
//! This module defines Rust types that map to the pushwork document schemas
//! (see DESIGN.md Appendix A). Uses autosurgeon for serialization.
//!
//! **Important compatibility notes**:
//! - Pushwork uses collaborative text (Automerge Text objects) for metadata
//!   fields like `name`, `extension`, `mimeType`, and `@patchwork.type`
//! - Pushwork uses `ImmutableString` (scalar string) for file `content`
//! - We use `autosurgeon::Text` for metadata fields and `String` for content

use autosurgeon::{Hydrate, Reconcile, Text};

/// The `@patchwork` type marker present in all pushwork documents.
///
/// This nested structure contains the document type discriminator.
/// The `type` field is a Text object (collaborative string) in pushwork.
#[derive(Debug, Clone, Reconcile, Hydrate)]
pub struct PatchworkMarker {
    /// The document type: "file" or "folder" (as collaborative Text)
    #[autosurgeon(rename = "type")]
    pub doc_type: Text,
}

impl PatchworkMarker {
    pub fn file() -> Self {
        Self {
            doc_type: Text::with_value("file"),
        }
    }

    pub fn folder() -> Self {
        Self {
            doc_type: Text::with_value("folder"),
        }
    }

    pub fn doc_type_str(&self) -> &str {
        self.doc_type.as_str()
    }
}

/// Metadata for a file document.
#[derive(Debug, Clone, PartialEq, Eq, Reconcile, Hydrate)]
pub struct FileMetadata {
    /// Unix permissions as decimal (e.g., 644)
    /// Stored as i64 in Automerge (JavaScript number -> Int)
    pub permissions: i64,
}

/// A file document matching the pushwork schema.
///
/// Schema:
/// ```json
/// {
///   "@patchwork": { "type": "file" },
///   "name": "README.md",
///   "extension": "md",
///   "mimeType": "text/markdown",
///   "content": "file contents as string",
///   "metadata": { "permissions": 644 }
/// }
/// ```
///
/// Field types in pushwork:
/// - `@patchwork.type`, `name`, `extension`, `mimeType`: collaborative Text
/// - `content`: ImmutableString (scalar string, last-write-wins)
#[derive(Debug, Clone, Reconcile, Hydrate)]
pub struct FileDocument {
    /// Type marker - always `{ type: "file" }`
    #[autosurgeon(rename = "@patchwork")]
    pub patchwork: PatchworkMarker,

    /// The filename (e.g., "README.md") - collaborative Text
    pub name: Text,

    /// File extension without dot (e.g., "md") - collaborative Text
    pub extension: Text,

    /// MIME type (e.g., "text/markdown") - collaborative Text
    #[autosurgeon(rename = "mimeType")]
    pub mime_type: Text,

    /// File contents as a scalar string (ImmutableString in pushwork)
    /// Note: This is NOT collaborative text - concurrent edits use last-write-wins
    pub content: String,

    /// File metadata including permissions
    pub metadata: FileMetadata,
}

impl FileDocument {
    /// Create a new file document with text content.
    pub fn new(
        name: String,
        extension: String,
        mime_type: String,
        content: &str,
        permissions: i64,
    ) -> Self {
        Self {
            patchwork: PatchworkMarker::file(),
            name: Text::with_value(name),
            extension: Text::with_value(extension),
            mime_type: Text::with_value(mime_type),
            content: content.to_string(),
            metadata: FileMetadata { permissions },
        }
    }

    /// Get the name as a string slice.
    pub fn name_str(&self) -> &str {
        self.name.as_str()
    }

    /// Get the extension as a string slice.
    pub fn extension_str(&self) -> &str {
        self.extension.as_str()
    }

    /// Get the MIME type as a string slice.
    pub fn mime_type_str(&self) -> &str {
        self.mime_type.as_str()
    }

    /// Get the content as a String.
    pub fn content_string(&self) -> String {
        self.content.clone()
    }
}

/// An entry in a directory's `docs` array.
/// All fields are collaborative Text in pushwork.
#[derive(Debug, Clone, Reconcile, Hydrate)]
pub struct DirectoryEntry {
    /// Entry name (filename or subdirectory name) - collaborative Text
    pub name: Text,

    /// Entry type: "file" or "folder" - collaborative Text
    #[autosurgeon(rename = "type")]
    pub entry_type: Text,

    /// Automerge URL of the child document - collaborative Text
    pub url: Text,
}

impl DirectoryEntry {
    pub fn file(name: String, url: String) -> Self {
        Self {
            name: Text::with_value(name),
            entry_type: Text::with_value("file"),
            url: Text::with_value(url),
        }
    }

    pub fn folder(name: String, url: String) -> Self {
        Self {
            name: Text::with_value(name),
            entry_type: Text::with_value("folder"),
            url: Text::with_value(url),
        }
    }

    pub fn name_str(&self) -> &str {
        self.name.as_str()
    }

    pub fn entry_type_str(&self) -> &str {
        self.entry_type.as_str()
    }

    pub fn url_str(&self) -> &str {
        self.url.as_str()
    }
}

/// A directory document matching the pushwork schema.
///
/// Schema:
/// ```json
/// {
///   "@patchwork": { "type": "folder" },
///   "docs": [
///     { "name": "file.txt", "type": "file", "url": "automerge:..." },
///     ...
///   ],
///   "lastSyncAt": 1234567890
/// }
/// ```
#[derive(Debug, Clone, Reconcile, Hydrate)]
pub struct DirectoryDocument {
    /// Type marker - always `{ type: "folder" }`
    #[autosurgeon(rename = "@patchwork")]
    pub patchwork: PatchworkMarker,

    /// Array of directory entries
    pub docs: Vec<DirectoryEntry>,

    /// Unix timestamp (ms) of last sync, if any.
    /// This field may be absent in pushwork documents, so we use missing = "Default::default"
    #[autosurgeon(rename = "lastSyncAt", missing = "Default::default")]
    pub last_sync_at: Option<u64>,
}

impl DirectoryDocument {
    /// Create a new empty directory document.
    pub fn new() -> Self {
        Self {
            patchwork: PatchworkMarker::folder(),
            docs: Vec::new(),
            last_sync_at: None,
        }
    }

    /// Create a directory with entries.
    pub fn with_entries(entries: Vec<DirectoryEntry>) -> Self {
        Self {
            patchwork: PatchworkMarker::folder(),
            docs: entries,
            last_sync_at: None,
        }
    }

    /// Add a file entry to the directory.
    pub fn add_file(&mut self, name: String, url: String) {
        self.docs.push(DirectoryEntry::file(name, url));
    }

    /// Add a folder entry to the directory.
    pub fn add_folder(&mut self, name: String, url: String) {
        self.docs.push(DirectoryEntry::folder(name, url));
    }
}

impl Default for DirectoryDocument {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::{AutoCommit, ReadDoc};
    use autosurgeon::{hydrate, reconcile};

    #[test]
    fn test_file_document_schema() {
        let file = FileDocument::new(
            "test.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "Hello, world!",
            644,
        );

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &file).expect("reconcile failed");

        // Verify the keys match pushwork schema
        let keys: Vec<_> = doc.keys(automerge::ROOT).collect();
        assert!(keys.contains(&"@patchwork".to_string()));
        assert!(keys.contains(&"name".to_string()));
        assert!(keys.contains(&"extension".to_string()));
        assert!(keys.contains(&"mimeType".to_string()));
        assert!(keys.contains(&"content".to_string()));
        assert!(keys.contains(&"metadata".to_string()));

        // Verify @patchwork.type is a Text object containing "file"
        if let Some((_, obj_id)) = doc.get(automerge::ROOT, "@patchwork").unwrap() {
            if let Some((val, type_obj_id)) = doc.get(&obj_id, "type").unwrap() {
                // @patchwork.type should be a Text object (collaborative string)
                assert!(
                    matches!(val, automerge::Value::Object(automerge::ObjType::Text)),
                    "Expected Text object for @patchwork.type, got {:?}",
                    val
                );
                // Read the text content
                let text = doc.text(&type_obj_id).expect("Failed to read text");
                assert_eq!(text, "file");
            } else {
                panic!("@patchwork.type not found");
            }
        } else {
            panic!("@patchwork not found");
        }
    }

    #[test]
    fn test_directory_document_schema() {
        let dir = DirectoryDocument::with_entries(vec![DirectoryEntry::file(
            "test.txt".to_string(),
            "automerge:abc123".to_string(),
        )]);

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &dir).expect("reconcile failed");

        // Verify the keys match pushwork schema
        let keys: Vec<_> = doc.keys(automerge::ROOT).collect();
        assert!(keys.contains(&"@patchwork".to_string()));
        assert!(keys.contains(&"docs".to_string()));
        assert!(keys.contains(&"lastSyncAt".to_string()));

        // Verify @patchwork.type is a Text object containing "folder"
        if let Some((_, obj_id)) = doc.get(automerge::ROOT, "@patchwork").unwrap() {
            if let Some((val, type_obj_id)) = doc.get(&obj_id, "type").unwrap() {
                // @patchwork.type should be a Text object (collaborative string)
                assert!(
                    matches!(val, automerge::Value::Object(automerge::ObjType::Text)),
                    "Expected Text object for @patchwork.type, got {:?}",
                    val
                );
                // Read the text content
                let text = doc.text(&type_obj_id).expect("Failed to read text");
                assert_eq!(text, "folder");
            } else {
                panic!("@patchwork.type not found");
            }
        } else {
            panic!("@patchwork not found");
        }
    }

    #[test]
    fn test_file_document_roundtrip() {
        let original = FileDocument::new(
            "readme.md".to_string(),
            "md".to_string(),
            "text/markdown".to_string(),
            "# Hello\n\nThis is a test.",
            755,
        );

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &original).expect("reconcile failed");

        let hydrated: FileDocument = hydrate(&doc).expect("hydrate failed");

        assert_eq!(hydrated.patchwork.doc_type_str(), "file");
        assert_eq!(hydrated.name_str(), "readme.md");
        assert_eq!(hydrated.extension_str(), "md");
        assert_eq!(hydrated.mime_type_str(), "text/markdown");
        assert_eq!(hydrated.content_string(), "# Hello\n\nThis is a test.");
        assert_eq!(hydrated.metadata.permissions, 755);
    }

    #[test]
    fn test_directory_document_roundtrip() {
        let mut original = DirectoryDocument::new();
        original.add_file("file1.txt".to_string(), "automerge:abc".to_string());
        original.add_folder("subdir".to_string(), "automerge:def".to_string());
        original.last_sync_at = Some(1234567890);

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &original).expect("reconcile failed");

        let hydrated: DirectoryDocument = hydrate(&doc).expect("hydrate failed");

        assert_eq!(hydrated.patchwork.doc_type_str(), "folder");
        assert_eq!(hydrated.docs.len(), 2);
        assert_eq!(hydrated.docs[0].name_str(), "file1.txt");
        assert_eq!(hydrated.docs[0].entry_type_str(), "file");
        assert_eq!(hydrated.docs[0].url_str(), "automerge:abc");
        assert_eq!(hydrated.docs[1].name_str(), "subdir");
        assert_eq!(hydrated.docs[1].entry_type_str(), "folder");
        assert_eq!(hydrated.docs[1].url_str(), "automerge:def");
        assert_eq!(hydrated.last_sync_at, Some(1234567890));
    }

    /// Test that content field is a scalar string (ImmutableString compatible)
    #[test]
    fn test_content_field_is_scalar_string() {
        let file = FileDocument::new(
            "test.txt".to_string(),
            "txt".to_string(),
            "text/plain".to_string(),
            "Hello!",
            644,
        );

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &file).expect("reconcile failed");

        // Check that content is a scalar string (not a Text object)
        // This is required for pushwork compatibility (uses ImmutableString)
        if let Some((value, _)) = doc.get(automerge::ROOT, "content").unwrap() {
            println!("Content value type: {:?}", value);
            match value {
                automerge::Value::Object(obj_type) => {
                    panic!(
                        "Expected scalar string for pushwork compatibility, got Object({:?})",
                        obj_type
                    );
                }
                automerge::Value::Scalar(scalar) => {
                    println!("Scalar: {:?}", scalar);
                    // Verify it's a string scalar
                    assert!(scalar.to_str().is_some(), "Expected string scalar");
                    assert_eq!(scalar.to_str().unwrap(), "Hello!");
                }
            }
        }
    }
}
