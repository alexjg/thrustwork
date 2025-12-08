//! State types for the three views of the world during sync.
//!
//! The sync algorithm compares three states:
//! - Filesystem (FsState): what's on disk right now
//! - Snapshot (SnapState): what we knew at the last sync
//! - Repo (RepoState): current Automerge documents from the sync server

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use automerge::ChangeHash;
use glob::Pattern;
use samod::{AutomergeUrl, ConnectionId, DocHandle, Repo};
use walkdir::WalkDir;

use autosurgeon::hydrate;

use super::sync_ops::get_document_heads;
use crate::documents::DirectoryDocument;
use crate::files::{get_file_permissions, guess_mime_type, is_text_mime_type};
use crate::snapshot::Snapshot;

/// Filesystem state: what's currently on disk.
#[derive(Debug, Clone)]
pub struct FsState {
    /// Files by relative path
    pub files: HashMap<PathBuf, FsFile>,
    /// Directories by relative path (just tracks existence)
    pub dirs: HashSet<PathBuf>,
}

impl FsState {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            dirs: HashSet::new(),
        }
    }

    /// Load filesystem state by scanning the directory tree.
    ///
    /// Walks the directory tree starting at `root`, respecting the exclude patterns.
    /// Reads file contents and metadata for each file found.
    pub fn load(root: &Path, excludes: &[String]) -> std::io::Result<Self> {
        let patterns = compile_patterns(excludes);
        let mut state = Self::new();

        for entry in WalkDir::new(root)
            .follow_links(false)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();

            // Skip the root itself
            if path == root {
                continue;
            }

            // Skip excluded paths
            if is_path_excluded(path, root, &patterns) {
                continue;
            }

            // Get relative path
            let relative = path
                .strip_prefix(root)
                .map(|p| p.to_path_buf())
                .unwrap_or_default();

            if relative.as_os_str().is_empty() {
                continue;
            }

            if path.is_dir() {
                state.dirs.insert(relative);
            } else if path.is_file() {
                let content = std::fs::read(path)?;
                let mime_type = guess_mime_type(path);
                let is_text = is_text_mime_type(&mime_type);
                let permissions = get_file_permissions(path).unwrap_or(0o644);
                let extension = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_string();

                state.files.insert(
                    relative,
                    FsFile {
                        abs_path: path.to_path_buf(),
                        content,
                        is_text,
                        permissions,
                        extension,
                        mime_type,
                    },
                );
            }
        }

        Ok(state)
    }
}

impl Default for FsState {
    fn default() -> Self {
        Self::new()
    }
}

/// A file on disk.
#[derive(Debug, Clone)]
pub struct FsFile {
    /// Absolute path to the file
    #[expect(dead_code)]
    pub abs_path: PathBuf,
    /// File contents
    pub content: Vec<u8>,
    /// Whether this is a text file (vs binary)
    pub is_text: bool,
    /// Unix permissions (e.g., 0o644)
    pub permissions: u32,
    /// File extension without dot
    pub extension: String,
    /// MIME type
    pub mime_type: String,
}

/// Snapshot state: what we recorded at the last successful sync.
#[derive(Debug, Clone)]
pub struct SnapState {
    /// Files by relative path
    pub files: HashMap<PathBuf, SnapFile>,
    /// Directories by relative path
    pub dirs: HashMap<PathBuf, SnapDir>,
}

impl SnapState {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            dirs: HashMap::new(),
        }
    }

    /// Load snapshot state from a snapshot file.
    ///
    /// Returns `None` if the file doesn't exist or can't be parsed.
    pub fn load(path: &Path) -> Option<Self> {
        let snapshot = Snapshot::load(path).ok()?;
        let mut state = Self::new();

        // Convert file entries
        for (relative_path, entry) in snapshot.files {
            state.files.insert(
                PathBuf::from(relative_path),
                SnapFile {
                    url: entry.url,
                    heads: entry.head,
                },
            );
        }

        // Convert directory entries
        for (relative_path, entry) in snapshot.directories {
            state.dirs.insert(
                PathBuf::from(relative_path),
                SnapDir {
                    url: entry.url,
                    heads: entry.head,
                    entries: entry.entries.into_iter().collect(),
                },
            );
        }

        Some(state)
    }
}

impl Default for SnapState {
    fn default() -> Self {
        Self::new()
    }
}

/// A file in the snapshot.
#[derive(Debug, Clone)]
pub struct SnapFile {
    /// Automerge URL of the file document
    #[cfg_attr(not(test), expect(dead_code))]
    pub url: AutomergeUrl,
    /// Document heads at last sync
    pub heads: Vec<ChangeHash>,
}

/// A directory in the snapshot.
#[derive(Debug, Clone)]
pub struct SnapDir {
    /// Automerge URL of the directory document
    #[cfg_attr(not(test), expect(dead_code))]
    pub url: AutomergeUrl,
    /// Document heads at last sync
    #[cfg_attr(not(test), expect(dead_code))]
    pub heads: Vec<ChangeHash>,
    /// Names of child entries at last sync
    #[cfg_attr(not(test), expect(dead_code))]
    pub entries: HashSet<String>,
}

/// Repo state: current Automerge documents fetched from the sync server.
#[derive(Debug)]
pub struct RepoState {
    /// Files by relative path
    pub files: HashMap<PathBuf, RepoFile>,
    /// Directories by relative path
    pub dirs: HashMap<PathBuf, RepoDir>,
}

impl RepoState {
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            dirs: HashMap::new(),
        }
    }

    /// Load repo state by walking the directory tree from the sync server.
    ///
    /// Starts from the root directory and recursively fetches all documents,
    /// waiting for remote changes on each.
    pub async fn load(
        repo: &Repo,
        root_url: &AutomergeUrl,
        conn_id: ConnectionId,
    ) -> Result<Self, String> {
        let mut state = Self::new();

        // Process the root directory at the empty path
        load_directory_recursive(repo, root_url, conn_id, PathBuf::new(), &mut state).await?;

        Ok(state)
    }
}

impl Default for RepoState {
    fn default() -> Self {
        Self::new()
    }
}

/// A file document from the repo.
pub struct RepoFile {
    /// Automerge URL of the file document
    pub url: AutomergeUrl,
    /// Document handle for accessing the Automerge document
    pub handle: DocHandle,
    /// Current document heads
    pub heads: Vec<ChangeHash>,
}

impl std::fmt::Debug for RepoFile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoFile")
            .field("url", &self.url)
            .field("heads", &self.heads)
            .finish_non_exhaustive()
    }
}

/// A directory document from the repo.
pub struct RepoDir {
    /// Automerge URL of the directory document
    pub url: AutomergeUrl,
    /// Document handle for accessing the Automerge document
    #[expect(dead_code)]
    pub handle: DocHandle,
    /// Current document heads
    pub heads: Vec<ChangeHash>,
    /// Current directory entries
    pub entries: Vec<RepoDirEntry>,
}

impl std::fmt::Debug for RepoDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RepoDir")
            .field("url", &self.url)
            .field("heads", &self.heads)
            .field("entries", &self.entries)
            .finish_non_exhaustive()
    }
}

/// An entry in a repo directory.
#[derive(Debug, Clone)]
pub struct RepoDirEntry {
    /// Entry name (filename or subdirectory name)
    pub name: String,
    /// Entry type: "file" or "folder"
    pub entry_type: String,
    /// Automerge URL of the child document
    pub url: AutomergeUrl,
}

// Helper functions for repo loading

/// Recursively load a directory and all its children from the repo.
async fn load_directory_recursive(
    repo: &Repo,
    url: &AutomergeUrl,
    conn_id: ConnectionId,
    relative_path: PathBuf,
    state: &mut RepoState,
) -> Result<(), String> {
    // Find the directory document
    let handle = repo
        .find(url.document_id().clone())
        .await
        .map_err(|_| "Repo stopped")?
        .ok_or_else(|| format!("Directory document not found: {}", url))?;

    // Wait for remote changes
    handle.we_have_their_changes(conn_id).await;

    // Get directory entries and heads
    let (entries, heads) = handle
        .with_document(|doc| {
            let heads = doc.get_heads();
            let dir_doc: DirectoryDocument = hydrate(doc).ok()?;

            let entries: Vec<RepoDirEntry> = dir_doc
                .docs
                .iter()
                .filter_map(|entry| {
                    let entry_url = entry.url_str().parse::<AutomergeUrl>().ok()?;
                    Some(RepoDirEntry {
                        name: entry.name_str().to_string(),
                        entry_type: entry.entry_type_str().to_string(),
                        url: entry_url,
                    })
                })
                .collect();

            Some((entries, heads))
        })
        .ok_or("Failed to hydrate directory document")?;

    // Add this directory to state (except for root which has empty path)
    if !relative_path.as_os_str().is_empty() {
        state.dirs.insert(
            relative_path.clone(),
            RepoDir {
                url: url.clone(),
                handle: handle.clone(),
                heads: heads.clone(),
                entries: entries.clone(),
            },
        );
    } else {
        // For root, store with empty path (or we could skip it)
        // The root is special - we need to track it but with empty path
        // Actually, let's store it to have the handle available
        state.dirs.insert(
            PathBuf::new(),
            RepoDir {
                url: url.clone(),
                handle: handle.clone(),
                heads: heads.clone(),
                entries: entries.clone(),
            },
        );
    }

    // Process each entry
    for entry in &entries {
        let child_path = if relative_path.as_os_str().is_empty() {
            PathBuf::from(&entry.name)
        } else {
            relative_path.join(&entry.name)
        };

        if entry.entry_type == "folder" {
            // Recursively process subdirectory
            Box::pin(load_directory_recursive(
                repo, &entry.url, conn_id, child_path, state,
            ))
            .await?;
        } else {
            // Load file document
            load_file(repo, &entry.url, conn_id, child_path, state).await?;
        }
    }

    Ok(())
}

/// Load a file document and add it to the state.
async fn load_file(
    repo: &Repo,
    url: &AutomergeUrl,
    conn_id: ConnectionId,
    relative_path: PathBuf,
    state: &mut RepoState,
) -> Result<(), String> {
    // Find the file document
    let handle = repo
        .find(url.document_id().clone())
        .await
        .map_err(|_| "Repo stopped")?
        .ok_or_else(|| format!("File document not found: {}", url))?;

    // Wait for remote changes
    handle.we_have_their_changes(conn_id).await;

    // Get heads
    let heads = get_document_heads(&handle);

    // Add to state
    state.files.insert(
        relative_path,
        RepoFile {
            url: url.clone(),
            handle,
            heads,
        },
    );

    Ok(())
}

// Helper functions for filesystem scanning

/// Compile exclude patterns into glob patterns.
fn compile_patterns(patterns: &[String]) -> Vec<Pattern> {
    patterns
        .iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect()
}

/// Check if a path should be excluded based on the patterns.
fn is_path_excluded(path: &Path, root: &Path, patterns: &[Pattern]) -> bool {
    let relative = match path.strip_prefix(root) {
        Ok(r) => r,
        Err(_) => return false,
    };

    // Check each component of the path against patterns
    for component in relative.components() {
        let component_str = component.as_os_str().to_string_lossy();
        for pattern in patterns {
            if pattern.matches(&component_str) {
                return true;
            }
        }
    }

    // Also check the full relative path
    let relative_str = relative.to_string_lossy();
    for pattern in patterns {
        if pattern.matches(&relative_str) {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::{SnapshotDirectoryEntry, SnapshotFileEntry};
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_fs_state_load_empty_directory() {
        let temp_dir = TempDir::new().unwrap();
        let state = FsState::load(temp_dir.path(), &[]).unwrap();

        assert!(state.files.is_empty());
        assert!(state.dirs.is_empty());
    }

    #[test]
    fn test_fs_state_load_with_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create some files
        fs::write(root.join("file1.txt"), "content1").unwrap();
        fs::write(root.join("file2.rs"), "fn main() {}").unwrap();

        let state = FsState::load(root, &[]).unwrap();

        assert_eq!(state.files.len(), 2);
        assert!(state.files.contains_key(Path::new("file1.txt")));
        assert!(state.files.contains_key(Path::new("file2.rs")));

        let file1 = &state.files[Path::new("file1.txt")];
        assert_eq!(file1.content, b"content1");
        assert_eq!(file1.extension, "txt");
        assert!(file1.is_text);
    }

    #[test]
    fn test_fs_state_load_with_directories() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a subdirectory with a file
        fs::create_dir(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/nested.txt"), "nested content").unwrap();

        let state = FsState::load(root, &[]).unwrap();

        assert_eq!(state.dirs.len(), 1);
        assert!(state.dirs.contains(Path::new("subdir")));
        assert_eq!(state.files.len(), 1);
        assert!(state.files.contains_key(Path::new("subdir/nested.txt")));
    }

    #[test]
    fn test_fs_state_load_excludes_patterns() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create files including excluded ones
        fs::write(root.join("good.txt"), "good").unwrap();
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "git config").unwrap();
        fs::write(root.join("temp.tmp"), "temp").unwrap();

        let excludes = vec![".git".to_string(), "*.tmp".to_string()];
        let state = FsState::load(root, &excludes).unwrap();

        assert_eq!(state.files.len(), 1);
        assert!(state.files.contains_key(Path::new("good.txt")));
        assert!(!state.files.contains_key(Path::new("temp.tmp")));
        assert!(state.dirs.is_empty()); // .git should be excluded
    }

    #[test]
    fn test_fs_state_load_binary_file() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create a binary file (PNG header bytes)
        let binary_content: Vec<u8> = vec![0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
        fs::write(root.join("image.png"), &binary_content).unwrap();

        let state = FsState::load(root, &[]).unwrap();

        assert_eq!(state.files.len(), 1);
        let file = &state.files[Path::new("image.png")];
        assert_eq!(file.content, binary_content);
        assert_eq!(file.extension, "png");
        assert!(!file.is_text);
    }

    // Helper functions for SnapState tests
    fn test_hash(hex: &str) -> automerge::ChangeHash {
        hex.parse().expect("invalid test hash")
    }

    fn test_url(uuid: &str) -> AutomergeUrl {
        format!("automerge:{}", uuid)
            .parse()
            .expect("invalid test url")
    }

    const HASH_A: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    const UUID_ROOT: &str = "550e8400-e29b-41d4-a716-446655440000";
    const UUID_FILE: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    const UUID_DIR: &str = "6ba7b811-9dad-11d1-80b4-00c04fd430c8";

    #[test]
    fn test_snap_state_load_nonexistent() {
        let state = SnapState::load(Path::new("/nonexistent/snapshot.json"));
        assert!(state.is_none());
    }

    #[test]
    fn test_snap_state_load_with_files() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_path = temp_dir.path().join("snapshot.json");

        // Create a snapshot with a file
        let mut snapshot = Snapshot::new(temp_dir.path(), Some(test_url(UUID_ROOT)));
        snapshot.add_file(
            "test.txt".to_string(),
            SnapshotFileEntry {
                path: temp_dir.path().join("test.txt"),
                url: test_url(UUID_FILE),
                head: vec![test_hash(HASH_A)],
                extension: "txt".to_string(),
                mime_type: "text/plain".to_string(),
            },
        );
        snapshot.save(&snapshot_path).unwrap();

        let state = SnapState::load(&snapshot_path).unwrap();

        assert_eq!(state.files.len(), 1);
        let file = &state.files[Path::new("test.txt")];
        assert_eq!(file.url.to_string(), test_url(UUID_FILE).to_string());
        assert_eq!(file.heads, vec![test_hash(HASH_A)]);
    }

    #[test]
    fn test_snap_state_load_with_directories() {
        let temp_dir = TempDir::new().unwrap();
        let snapshot_path = temp_dir.path().join("snapshot.json");

        // Create a snapshot with a directory
        let mut snapshot = Snapshot::new(temp_dir.path(), Some(test_url(UUID_ROOT)));
        snapshot.add_directory(
            "subdir".to_string(),
            SnapshotDirectoryEntry {
                path: temp_dir.path().join("subdir"),
                url: test_url(UUID_DIR),
                head: vec![test_hash(HASH_A)],
                entries: vec!["file1.txt".to_string(), "file2.txt".to_string()],
            },
        );
        snapshot.save(&snapshot_path).unwrap();

        let state = SnapState::load(&snapshot_path).unwrap();

        assert_eq!(state.dirs.len(), 1);
        let dir = &state.dirs[Path::new("subdir")];
        assert_eq!(dir.url.to_string(), test_url(UUID_DIR).to_string());
        assert_eq!(dir.heads, vec![test_hash(HASH_A)]);
        assert!(dir.entries.contains("file1.txt"));
        assert!(dir.entries.contains("file2.txt"));
    }
}
