//! Snapshot types for tracking sync state.
//!
//! The snapshot file (`.pushwork/snapshot.json`) tracks the state at the last
//! successful sync. This is used to detect what has changed since the last sync.

use std::path::{Path, PathBuf};

use automerge::ChangeHash;
use samod::AutomergeUrl;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// The snapshot file name
const SNAPSHOT_FILENAME: &str = "snapshot.json";

/// Serialize/deserialize Vec<ChangeHash> as Vec<String> (hex-encoded)
mod change_hash_vec {
    use automerge::ChangeHash;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(hashes: &[ChangeHash], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let hex_strings: Vec<String> = hashes.iter().map(|h| h.to_string()).collect();
        hex_strings.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<ChangeHash>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let hex_strings: Vec<String> = Vec::deserialize(deserializer)?;
        hex_strings
            .into_iter()
            .map(|s| {
                s.parse::<ChangeHash>()
                    .map_err(|e| serde::de::Error::custom(format!("invalid change hash: {}", e)))
            })
            .collect()
    }
}

/// Serialize/deserialize AutomergeUrl as String
mod automerge_url_serde {
    use samod::AutomergeUrl;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(url: &AutomergeUrl, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&url.to_string())
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<AutomergeUrl, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse::<AutomergeUrl>()
            .map_err(|e| serde::de::Error::custom(format!("invalid automerge url: {}", e)))
    }
}

/// Serialize/deserialize Option<AutomergeUrl> as Option<String>
mod automerge_url_option_serde {
    use samod::AutomergeUrl;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S>(url: &Option<AutomergeUrl>, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match url {
            Some(u) => serializer.serialize_some(&u.to_string()),
            None => serializer.serialize_none(),
        }
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Option<AutomergeUrl>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let opt: Option<String> = Option::deserialize(deserializer)?;
        match opt {
            Some(s) => s
                .parse::<AutomergeUrl>()
                .map(Some)
                .map_err(|e| serde::de::Error::custom(format!("invalid automerge url: {}", e))),
            None => Ok(None),
        }
    }
}

/// Errors that can occur when working with snapshots
#[derive(Debug, Error)]
pub enum SnapshotError {
    /// IO error reading/writing snapshot file
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Error parsing snapshot JSON
    #[error("Failed to parse snapshot: {0}")]
    Parse(#[source] serde_json::Error),

    /// Error serializing snapshot to JSON
    #[error("Failed to serialize snapshot: {0}")]
    Serialize(#[source] serde_json::Error),
}

/// A file entry in the snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotFileEntry {
    /// Full filesystem path
    pub path: PathBuf,

    /// Automerge URL of the file document
    #[serde(with = "automerge_url_serde")]
    pub url: AutomergeUrl,

    /// Document heads at last sync
    #[serde(with = "change_hash_vec")]
    pub head: Vec<ChangeHash>,

    /// File extension (without dot)
    pub extension: String,

    /// MIME type
    pub mime_type: String,
}

/// A directory entry in the snapshot
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SnapshotDirectoryEntry {
    /// Full filesystem path
    pub path: PathBuf,

    /// Automerge URL of the directory document
    #[serde(with = "automerge_url_serde")]
    pub url: AutomergeUrl,

    /// Document heads at last sync
    #[serde(with = "change_hash_vec")]
    pub head: Vec<ChangeHash>,

    /// Names of child entries at last sync
    pub entries: Vec<String>,
}

/// The snapshot file structure
///
/// This matches pushwork's snapshot format for compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    /// Unix timestamp (ms) when snapshot was saved
    pub timestamp: u64,

    /// Absolute path to the synced directory
    pub root_path: PathBuf,

    /// Automerge URL of the root directory document
    #[serde(
        with = "automerge_url_option_serde",
        skip_serializing_if = "Option::is_none"
    )]
    pub root_directory_url: Option<AutomergeUrl>,

    /// Array of [relative-path, file-entry] tuples
    pub files: Vec<(String, SnapshotFileEntry)>,

    /// Array of [relative-path, directory-entry] tuples
    pub directories: Vec<(String, SnapshotDirectoryEntry)>,
}

impl Snapshot {
    /// Create a new empty snapshot
    pub fn new(root_path: PathBuf, root_directory_url: Option<AutomergeUrl>) -> Self {
        Self {
            timestamp: 0,
            root_path,
            root_directory_url,
            files: Vec::new(),
            directories: Vec::new(),
        }
    }

    /// Get the path to the snapshot file in a .pushwork directory
    pub fn path_in(pushwork_dir: &Path) -> PathBuf {
        pushwork_dir.join(SNAPSHOT_FILENAME)
    }

    /// Load snapshot from a file
    pub fn load(path: &Path) -> Result<Self, SnapshotError> {
        let content = std::fs::read_to_string(path)?;
        let snapshot = serde_json::from_str(&content).map_err(SnapshotError::Parse)?;
        Ok(snapshot)
    }

    /// Save snapshot to a file
    pub fn save(&self, path: &Path) -> Result<(), SnapshotError> {
        let content = serde_json::to_string_pretty(self).map_err(SnapshotError::Serialize)?;
        std::fs::write(path, content)?;
        Ok(())
    }

    /// Update the timestamp to now (in milliseconds)
    pub fn update_timestamp(&mut self) {
        use std::time::{SystemTime, UNIX_EPOCH};
        self.timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_millis() as u64;
    }

    /// Add a file entry to the snapshot
    pub fn add_file(&mut self, relative_path: String, entry: SnapshotFileEntry) {
        // Remove existing entry with same path if present
        self.files.retain(|(p, _)| p != &relative_path);
        self.files.push((relative_path, entry));
    }

    /// Add a directory entry to the snapshot
    pub fn add_directory(&mut self, relative_path: String, entry: SnapshotDirectoryEntry) {
        // Remove existing entry with same path if present
        self.directories.retain(|(p, _)| p != &relative_path);
        self.directories.push((relative_path, entry));
    }

    /// Get a file entry by relative path
    pub fn get_file(&self, relative_path: &str) -> Option<&SnapshotFileEntry> {
        self.files
            .iter()
            .find(|(p, _)| p == relative_path)
            .map(|(_, e)| e)
    }

    /// Get a directory entry by relative path
    pub fn get_directory(&self, relative_path: &str) -> Option<&SnapshotDirectoryEntry> {
        self.directories
            .iter()
            .find(|(p, _)| p == relative_path)
            .map(|(_, e)| e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    /// Create a test ChangeHash from a hex string (must be 64 hex chars = 32 bytes)
    fn test_hash(hex: &str) -> ChangeHash {
        hex.parse().expect("invalid test hash")
    }

    /// Create a test AutomergeUrl from a UUID string
    fn test_url(uuid: &str) -> AutomergeUrl {
        format!("automerge:{}", uuid)
            .parse()
            .expect("invalid test url")
    }

    // A valid 64-char hex string for testing (ChangeHash = 32 bytes)
    const HASH_A: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    const HASH_B: &str = "cafebabecafebabecafebabecafebabecafebabecafebabecafebabecafebabe";
    const HASH_C: &str = "aabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccddaabbccdd";

    // Valid UUIDs for test URLs (DocumentId accepts UUID format)
    const UUID_ROOT: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_FILE: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    const UUID_DIR: &str = "6ba7b811-9dad-11d1-80b4-00c04fd430c8";
    const UUID_A: &str = "f47ac10b-58cc-4372-a567-0e02b2c3d479";
    const UUID_B: &str = "7c9e6679-7425-40de-944b-e07fc1f90ae7";

    #[test]
    fn test_snapshot_new() {
        let snapshot = Snapshot::new(PathBuf::from("/tmp/test"), Some(test_url(UUID_ROOT)));

        assert_eq!(snapshot.root_path, PathBuf::from("/tmp/test"));
        assert!(snapshot.root_directory_url.is_some());
        assert!(snapshot.files.is_empty());
        assert!(snapshot.directories.is_empty());
    }

    #[test]
    fn test_snapshot_roundtrip() {
        let mut snapshot = Snapshot::new(PathBuf::from("/tmp/test"), Some(test_url(UUID_ROOT)));
        snapshot.timestamp = 1234567890;

        snapshot.add_file(
            "test.txt".to_string(),
            SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/test.txt"),
                url: test_url(UUID_FILE),
                head: vec![test_hash(HASH_A)],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );

        snapshot.add_directory(
            "subdir".to_string(),
            SnapshotDirectoryEntry {
                path: PathBuf::from("/tmp/test/subdir"),
                url: test_url(UUID_DIR),
                head: vec![test_hash(HASH_B)],
                entries: vec!["file1.txt".to_string(), "file2.txt".to_string()],
            },
        );

        // Serialize to JSON
        let json = serde_json::to_string_pretty(&snapshot).unwrap();
        println!("Snapshot JSON:\n{}", json);

        // Verify hex encoding in JSON
        assert!(json.contains(HASH_A));

        // Parse back
        let parsed: Snapshot = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.timestamp, snapshot.timestamp);
        assert_eq!(parsed.root_path, snapshot.root_path);
        assert_eq!(parsed.files.len(), 1);
        assert_eq!(parsed.directories.len(), 1);

        let (path, file_entry) = &parsed.files[0];
        assert_eq!(path, "test.txt");
        assert_eq!(file_entry.head, vec![test_hash(HASH_A)]);
    }

    #[test]
    fn test_snapshot_save_load() {
        let mut snapshot = Snapshot::new(PathBuf::from("/tmp/test"), Some(test_url(UUID_ROOT)));
        snapshot.timestamp = 9876543210;

        snapshot.add_file(
            "hello.txt".to_string(),
            SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/hello.txt"),
                url: test_url(UUID_FILE),
                head: vec![test_hash(HASH_C)],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );

        // Save to temp file
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        snapshot.save(&path).unwrap();

        // Load back
        let loaded = Snapshot::load(&path).unwrap();

        assert_eq!(loaded.timestamp, snapshot.timestamp);
        assert_eq!(loaded.root_path, snapshot.root_path);
        assert_eq!(loaded.files.len(), 1);
        assert_eq!(loaded.files[0].1.head, vec![test_hash(HASH_C)]);
    }

    #[test]
    fn test_snapshot_get_file() {
        let mut snapshot = Snapshot::new(PathBuf::from("/tmp/test"), None);

        snapshot.add_file(
            "a.txt".to_string(),
            SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/a.txt"),
                url: test_url(UUID_A),
                head: vec![],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );

        assert!(snapshot.get_file("a.txt").is_some());
        assert!(snapshot.get_file("b.txt").is_none());
    }

    #[test]
    fn test_snapshot_add_file_replaces_existing() {
        let mut snapshot = Snapshot::new(PathBuf::from("/tmp/test"), None);

        let url_a = test_url(UUID_A);
        let url_b = test_url(UUID_B);

        snapshot.add_file(
            "test.txt".to_string(),
            SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/test.txt"),
                url: url_a,
                head: vec![test_hash(HASH_A)],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );

        // Add again with different URL - should replace
        snapshot.add_file(
            "test.txt".to_string(),
            SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/test.txt"),
                url: url_b.clone(),
                head: vec![test_hash(HASH_B)],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );

        assert_eq!(snapshot.files.len(), 1);
        // URL changed to url_b - check head was updated (proves replacement occurred)
        assert_eq!(
            snapshot.get_file("test.txt").unwrap().head,
            vec![test_hash(HASH_B)]
        );
    }

    #[test]
    fn test_camel_case_serialization() {
        let snapshot = Snapshot::new(PathBuf::from("/tmp/test"), Some(test_url(UUID_ROOT)));

        let json = serde_json::to_string(&snapshot).unwrap();

        // Verify camelCase field names
        assert!(json.contains("\"rootPath\""));
        assert!(json.contains("\"rootDirectoryUrl\""));
        assert!(!json.contains("\"root_path\""));
        assert!(!json.contains("\"root_directory_url\""));
    }
}
