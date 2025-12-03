// Quick test to verify autosurgeon works for our use case
use automerge::{AutoCommit, ReadDoc};
use autosurgeon::{hydrate, reconcile, Hydrate, Reconcile, Text};

/// The @patchwork type marker
#[derive(Debug, Clone, Reconcile, Hydrate, PartialEq)]
struct PatchworkType {
    #[autosurgeon(rename = "type")]
    doc_type: String,
}

/// File document structure matching pushwork schema
#[derive(Debug, Clone, Reconcile, Hydrate)]
struct FileDocument {
    #[autosurgeon(rename = "@patchwork")]
    patchwork: PatchworkType,
    name: String,
    extension: String,
    #[autosurgeon(rename = "mimeType")]
    mime_type: String,
    content: Text,
    metadata: FileMetadata,
}

#[derive(Debug, Clone, Reconcile, Hydrate)]
struct FileMetadata {
    permissions: u64,
}

/// Directory entry
#[derive(Debug, Clone, Reconcile, Hydrate)]
struct DirectoryEntry {
    name: String,
    #[autosurgeon(rename = "type")]
    entry_type: String,
    url: String,
}

/// Directory document structure
#[derive(Debug, Clone, Reconcile, Hydrate)]
struct DirectoryDocument {
    #[autosurgeon(rename = "@patchwork")]
    patchwork: PatchworkType,
    docs: Vec<DirectoryEntry>,
    #[autosurgeon(rename = "lastSyncAt")]
    last_sync_at: Option<u64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_file_document_roundtrip() {
        // Create a file document
        let file_doc = FileDocument {
            patchwork: PatchworkType {
                doc_type: "file".to_string(),
            },
            name: "test.txt".to_string(),
            extension: "txt".to_string(),
            mime_type: "text/plain".to_string(),
            content: Text::with_value("Hello, world!"),
            metadata: FileMetadata { permissions: 644 },
        };

        // Write to automerge document
        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &file_doc).expect("reconcile failed");

        // Verify the keys are correct
        let keys: Vec<_> = doc.keys(automerge::ROOT).collect();
        println!("File document keys: {:?}", keys);

        // Check @patchwork key exists
        assert!(keys.contains(&"@patchwork".to_string()), "Missing @patchwork key");
        assert!(keys.contains(&"mimeType".to_string()), "Missing mimeType key");

        // Hydrate back
        let hydrated: FileDocument = hydrate(&doc).expect("hydrate failed");
        assert_eq!(hydrated.patchwork.doc_type, "file");
        assert_eq!(hydrated.name, "test.txt");
        assert_eq!(hydrated.mime_type, "text/plain");
    }

    #[test]
    fn test_directory_document_roundtrip() {
        let dir_doc = DirectoryDocument {
            patchwork: PatchworkType {
                doc_type: "folder".to_string(),
            },
            docs: vec![DirectoryEntry {
                name: "test.txt".to_string(),
                entry_type: "file".to_string(),
                url: "automerge:abc123".to_string(),
            }],
            last_sync_at: Some(1234567890),
        };

        let mut doc = AutoCommit::new();
        reconcile(&mut doc, &dir_doc).expect("reconcile failed");

        let keys: Vec<_> = doc.keys(automerge::ROOT).collect();
        println!("Directory document keys: {:?}", keys);

        assert!(keys.contains(&"@patchwork".to_string()), "Missing @patchwork key");
        assert!(keys.contains(&"lastSyncAt".to_string()), "Missing lastSyncAt key");

        let hydrated: DirectoryDocument = hydrate(&doc).expect("hydrate failed");
        assert_eq!(hydrated.patchwork.doc_type, "folder");
        assert_eq!(hydrated.docs.len(), 1);
        assert_eq!(hydrated.docs[0].entry_type, "file");
    }
}

pub fn run_tests() {
    println!("Running autosurgeon tests...");

    // Test file document
    let file_doc = FileDocument {
        patchwork: PatchworkType {
            doc_type: "file".to_string(),
        },
        name: "test.txt".to_string(),
        extension: "txt".to_string(),
        mime_type: "text/plain".to_string(),
        content: Text::with_value("Hello, world!"),
        metadata: FileMetadata { permissions: 644 },
    };

    let mut doc = AutoCommit::new();
    reconcile(&mut doc, &file_doc).expect("reconcile failed");

    let keys: Vec<_> = doc.keys(automerge::ROOT).collect();
    println!("File document keys: {:?}", keys);

    // Check the @patchwork nested structure
    if let Some((value, obj_id)) = doc.get(automerge::ROOT, "@patchwork").unwrap() {
        println!("@patchwork value: {:?}", value);
        if let automerge::Value::Object(automerge::ObjType::Map) = value {
            let inner_keys: Vec<_> = doc.keys(&obj_id).collect();
            println!("@patchwork inner keys: {:?}", inner_keys);
            if let Some((type_val, _)) = doc.get(&obj_id, "type").unwrap() {
                println!("@patchwork.type = {:?}", type_val);
            }
        }
    }

    // Hydrate back
    let hydrated: FileDocument = hydrate(&doc).expect("hydrate failed");
    println!("Hydrated: name={}, type={}", hydrated.name, hydrated.patchwork.doc_type);

    println!("All tests passed!");
}
