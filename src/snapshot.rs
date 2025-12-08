//! Snapshot types for tracking sync state.
//!
//! The snapshot file (`.pushwork/snapshot.json`) tracks the state at the last
//! successful sync. This is used to detect what has changed since the last sync.

use std::path::{Path, PathBuf};

use automerge::ChangeHash;
use samod::AutomergeUrl;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Serialize/deserialize Vec<ChangeHash> as Vec<String> (base58check-encoded)
///
/// This matches the format used by automerge-repo in JavaScript (UrlHeads).
mod change_hash_vec {
    use automerge::ChangeHash;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S>(hashes: &[ChangeHash], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let encoded: Vec<String> = hashes
            .iter()
            .map(|h| bs58::encode(h.as_ref()).with_check().into_string())
            .collect();
        encoded.serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Vec<ChangeHash>, D::Error>
    where
        D: Deserializer<'de>,
    {
        let encoded: Vec<String> = Vec::deserialize(deserializer)?;
        encoded
            .into_iter()
            .map(|s| {
                let bytes = bs58::decode(&s)
                    .with_check(None)
                    .into_vec()
                    .map_err(|e| serde::de::Error::custom(format!("invalid base58: {}", e)))?;
                ChangeHash::try_from(bytes.as_slice())
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
    pub fn new<P: AsRef<Path>>(root_path: P, root_directory_url: Option<AutomergeUrl>) -> Self {
        Self {
            timestamp: 0,
            root_path: root_path.as_ref().to_path_buf(),
            root_directory_url,
            files: Vec::new(),
            directories: Vec::new(),
        }
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

    /// Get a file entry by absolute path
    pub fn get_file(&self, absolute_path: &Path) -> Option<&SnapshotFileEntry> {
        self.files
            .iter()
            .find(|(_, entry)| entry.path == absolute_path)
            .map(|(_, e)| e)
    }

    /// Update the heads for an existing file entry
    ///
    /// Returns true if the file was found and updated, false if not found.
    pub fn update_file_heads(&mut self, absolute_path: &Path, new_heads: Vec<ChangeHash>) -> bool {
        for (_, entry) in &mut self.files {
            if entry.path == absolute_path {
                entry.head = new_heads;
                return true;
            }
        }
        false
    }

    /// Get a directory entry by absolute path
    pub fn get_directory(&self, absolute_path: &Path) -> Option<&SnapshotDirectoryEntry> {
        self.directories
            .iter()
            .find(|(_, entry)| entry.path == absolute_path)
            .map(|(_, e)| e)
    }

    /// Remove a file entry from the snapshot by absolute path
    ///
    /// Returns true if the file was found and removed, false if not found.
    pub fn remove_file(&mut self, absolute_path: &Path) -> bool {
        let len_before = self.files.len();
        self.files.retain(|(_, entry)| entry.path != absolute_path);
        self.files.len() < len_before
    }

    /// Remove a directory entry from the snapshot by absolute path
    ///
    /// Returns true if the directory was found and removed, false if not found.
    pub fn remove_directory(&mut self, absolute_path: &Path) -> bool {
        let len_before = self.directories.len();
        self.directories
            .retain(|(_, entry)| entry.path != absolute_path);
        self.directories.len() < len_before
    }

    /// Add an entry name to a directory's entries list
    ///
    /// This is used when a new file/folder is added to track which names
    /// belong to which directory.
    pub fn add_directory_entry(&mut self, dir_absolute_path: &Path, entry_name: String) {
        for (_, entry) in &mut self.directories {
            if entry.path == dir_absolute_path {
                if !entry.entries.contains(&entry_name) {
                    entry.entries.push(entry_name);
                }
                return;
            }
        }
    }

    /// Remove an entry name from a directory's entries list
    pub fn remove_directory_entry(&mut self, dir_absolute_path: &Path, entry_name: &str) {
        for (_, entry) in &mut self.directories {
            if entry.path == dir_absolute_path {
                entry.entries.retain(|n| n != entry_name);
                return;
            }
        }
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

        // Verify base58check encoding in JSON (not hex)
        // The hash should be encoded as base58check, not hex
        assert!(
            !json.contains(HASH_A),
            "JSON should NOT contain hex-encoded hash"
        );
        // Heads should be present as base58check strings
        assert!(json.contains("head"), "JSON should contain head field");

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

        assert!(snapshot.get_file(Path::new("/tmp/test/a.txt")).is_some());
        assert!(snapshot.get_file(Path::new("/tmp/test/b.txt")).is_none());
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
            snapshot
                .get_file(Path::new("/tmp/test/test.txt"))
                .unwrap()
                .head,
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

    #[test]
    fn test_update_file_heads() {
        let mut snapshot = Snapshot::new(PathBuf::from("/tmp/test"), None);

        snapshot.add_file(
            "test.txt".to_string(),
            SnapshotFileEntry {
                path: PathBuf::from("/tmp/test/test.txt"),
                url: test_url(UUID_A),
                head: vec![test_hash(HASH_A)],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );

        // Update heads
        let updated =
            snapshot.update_file_heads(Path::new("/tmp/test/test.txt"), vec![test_hash(HASH_B)]);
        assert!(updated);

        // Verify heads changed
        let entry = snapshot.get_file(Path::new("/tmp/test/test.txt")).unwrap();
        assert_eq!(entry.head, vec![test_hash(HASH_B)]);

        // URL and other fields should be unchanged
        assert_eq!(entry.url.to_string(), test_url(UUID_A).to_string());
        assert_eq!(entry.extension, "txt");
    }

    #[test]
    fn test_update_file_heads_not_found() {
        let mut snapshot = Snapshot::new(PathBuf::from("/tmp/test"), None);

        // Try to update a file that doesn't exist
        let updated = snapshot.update_file_heads(
            Path::new("/tmp/test/nonexistent.txt"),
            vec![test_hash(HASH_A)],
        );
        assert!(!updated);
    }
}
