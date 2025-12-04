//! Automerge document types for pushwork compatibility.
//!
//! This module defines Rust types that map to the pushwork document schemas
//! (see DESIGN.md Appendix A). Uses autosurgeon for serialization.
//!
//! **Important compatibility notes**:
//! - Pushwork uses collaborative text (Automerge Text objects) for metadata
//!   fields like `name`, `extension`, `mimeType`, and `@patchwork.type`
//! - Pushwork uses `ImmutableString` (scalar string) for text file `content`
//! - Pushwork uses `Bytes` for binary file `content`
//! - We use `autosurgeon::Text` for metadata fields and `FileContent` enum for content

use autosurgeon::reconcile::NoKey;
use autosurgeon::{Hydrate, HydrateError, Reconcile, Reconciler, Text};

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

/// File content that can be either text (String) or binary (bytes).
///
/// This enum has custom Reconcile/Hydrate implementations that:
/// - Reconcile: writes String scalar for Text, Bytes scalar for Binary
/// - Hydrate: inspects the Automerge value type to determine which variant
///
/// This matches pushwork's behavior where text files use ImmutableString
/// and binary files use Bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileContent {
    /// Text content stored as Automerge scalar string
    Text(String),
    /// Binary content stored as Automerge Bytes
    Binary(Vec<u8>),
}

impl FileContent {
    /// Create text content from a string.
    pub fn text(s: impl Into<String>) -> Self {
        Self::Text(s.into())
    }

    /// Create binary content from bytes.
    pub fn binary(b: impl Into<Vec<u8>>) -> Self {
        Self::Binary(b.into())
    }

    /// Returns true if this is text content.
    pub fn is_text(&self) -> bool {
        matches!(self, Self::Text(_))
    }

    /// Returns true if this is binary content.
    pub fn is_binary(&self) -> bool {
        matches!(self, Self::Binary(_))
    }

    /// Get as text if this is text content.
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(s) => Some(s),
            Self::Binary(_) => None,
        }
    }

    /// Get as bytes if this is binary content.
    pub fn as_binary(&self) -> Option<&[u8]> {
        match self {
            Self::Text(_) => None,
            Self::Binary(b) => Some(b),
        }
    }

    /// Get the content as bytes (works for both text and binary).
    pub fn as_bytes(&self) -> &[u8] {
        match self {
            Self::Text(s) => s.as_bytes(),
            Self::Binary(b) => b,
        }
    }
}

impl Reconcile for FileContent {
    type Key<'a> = NoKey;

    fn reconcile<R: Reconciler>(&self, mut reconciler: R) -> Result<(), R::Error> {
        match self {
            Self::Text(s) => reconciler.str(s),
            Self::Binary(b) => reconciler.bytes(b),
        }
    }
}

impl Hydrate for FileContent {
    fn hydrate_string(s: &str) -> Result<Self, HydrateError> {
        Ok(Self::Text(s.to_string()))
    }

    fn hydrate_bytes(bytes: &[u8]) -> Result<Self, HydrateError> {
        Ok(Self::Binary(bytes.to_vec()))
    }
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
///   "content": "file contents as string or bytes",
///   "metadata": { "permissions": 644 }
/// }
/// ```
///
/// Field types in pushwork:
/// - `@patchwork.type`, `name`, `extension`, `mimeType`: collaborative Text
/// - `content`: ImmutableString for text files, Bytes for binary files
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

    /// File contents - text (String) for text files, binary (Bytes) for binary files.
    /// Note: This is NOT collaborative - concurrent edits use last-write-wins
    pub content: FileContent,

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
            content: FileContent::text(content),
            metadata: FileMetadata { permissions },
        }
    }

    /// Create a new file document with binary content.
    pub fn new_binary(
        name: String,
        extension: String,
        mime_type: String,
        content: Vec<u8>,
        permissions: i64,
    ) -> Self {
        Self {
            patchwork: PatchworkMarker::file(),
            name: Text::with_value(name),
            extension: Text::with_value(extension),
            mime_type: Text::with_value(mime_type),
            content: FileContent::binary(content),
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

    /// Get the content as a String (for text files).
    /// Returns the text content, or an empty string if binary.
    pub fn content_string(&self) -> String {
        match &self.content {
            FileContent::Text(s) => s.clone(),
            FileContent::Binary(_) => String::new(),
        }
    }

    /// Get the content as bytes (works for both text and binary).
    pub fn content_bytes(&self) -> &[u8] {
        self.content.as_bytes()
    }

    /// Returns true if this file has text content.
    pub fn is_text(&self) -> bool {
        self.content.is_text()
    }

    /// Returns true if this file has binary content.
    pub fn is_binary(&self) -> bool {
        self.content.is_binary()
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

    /// Test binary file document roundtrip
    #[test]
    fn test_binary_file_document_roundtrip() {
        // PNG file header bytes
        let binary_content: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];

        let original = FileDocument::new_binary(
            "image.png".to_string(),
            "png".to_string(),
            "image/png".to_string(),
            binary_content.clone(),
            644,
        );

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &original).expect("reconcile failed");

        let hydrated: FileDocument = hydrate(&doc).expect("hydrate failed");

        assert_eq!(hydrated.patchwork.doc_type_str(), "file");
        assert_eq!(hydrated.name_str(), "image.png");
        assert_eq!(hydrated.extension_str(), "png");
        assert_eq!(hydrated.mime_type_str(), "image/png");
        assert!(hydrated.is_binary());
        assert!(!hydrated.is_text());
        assert_eq!(hydrated.content_bytes(), binary_content.as_slice());
        assert_eq!(hydrated.metadata.permissions, 644);
    }

    /// Test that binary content field is stored as Bytes scalar
    #[test]
    fn test_binary_content_field_is_bytes() {
        let binary_content: Vec<u8> = vec![0x00, 0x01, 0x02, 0x03];

        let file = FileDocument::new_binary(
            "data.bin".to_string(),
            "bin".to_string(),
            "application/octet-stream".to_string(),
            binary_content.clone(),
            644,
        );

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &file).expect("reconcile failed");

        // Check that content is a Bytes scalar
        if let Some((value, _)) = doc.get(automerge::ROOT, "content").unwrap() {
            println!("Content value type: {:?}", value);
            match value {
                automerge::Value::Object(obj_type) => {
                    panic!(
                        "Expected Bytes scalar for binary content, got Object({:?})",
                        obj_type
                    );
                }
                automerge::Value::Scalar(scalar) => {
                    println!("Scalar: {:?}", scalar);
                    // Verify it's a bytes scalar
                    match scalar.as_ref() {
                        automerge::ScalarValue::Bytes(b) => {
                            assert_eq!(b, &binary_content);
                        }
                        other => {
                            panic!("Expected Bytes scalar, got {:?}", other);
                        }
                    }
                }
            }
        }
    }

    /// Test FileContent enum directly
    #[test]
    fn test_file_content_text_roundtrip() {
        let content = FileContent::text("Hello, world!");

        let mut doc = AutoCommit::new();
        autosurgeon::reconcile_prop(&mut doc, automerge::ROOT, "content", &content)
            .expect("reconcile failed");

        let hydrated: FileContent =
            autosurgeon::hydrate_prop(&doc, automerge::ROOT, "content").expect("hydrate failed");

        assert!(hydrated.is_text());
        assert_eq!(hydrated.as_text(), Some("Hello, world!"));
    }

    /// Test FileContent enum with binary data
    #[test]
    fn test_file_content_binary_roundtrip() {
        let bytes = vec![0xDE, 0xAD, 0xBE, 0xEF];
        let content = FileContent::binary(bytes.clone());

        let mut doc = AutoCommit::new();
        autosurgeon::reconcile_prop(&mut doc, automerge::ROOT, "content", &content)
            .expect("reconcile failed");

        let hydrated: FileContent =
            autosurgeon::hydrate_prop(&doc, automerge::ROOT, "content").expect("hydrate failed");

        assert!(hydrated.is_binary());
        assert_eq!(hydrated.as_binary(), Some(bytes.as_slice()));
    }
}
