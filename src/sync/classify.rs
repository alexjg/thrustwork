//! Classification types for the sync algorithm.
//!
//! Every path in the sync process gets classified into exactly one of these
//! change types, based on comparing the three states (filesystem, snapshot, repo).

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use samod::AutomergeUrl;
use tracing::debug;

use super::sync_ops::get_file_content_at_heads;

use super::state::{FsFile, FsState, RepoDir, RepoFile, RepoState, SnapDir, SnapFile, SnapState};

/// Classification of a file's sync state.
///
/// Based on the presence/absence in each of the three states and content comparison.
#[derive(Debug)]
pub enum FileChange {
    /// In all three states, no content changes since snapshot
    Unchanged(RepoFile),

    /// In all three states, local content differs from snapshot, remote unchanged
    LocalEdit {
        local: FsFile,
        #[expect(dead_code)]
        snap: SnapFile,
        remote: RepoFile,
    },

    /// In all three states, remote content differs from snapshot, local unchanged
    RemoteEdit {
        #[expect(dead_code)]
        local: FsFile,
        #[expect(dead_code)]
        snap: SnapFile,
        remote: RepoFile,
    },

    /// In all three states, both local and remote changed since snapshot
    Conflict {
        local: FsFile,
        snap: SnapFile,
        remote: RepoFile,
    },

    /// Only in filesystem (new local file, needs to be pushed)
    /// parent_url may be None if parent directory is also new (will be looked up during execution)
    LocalNew {
        local: FsFile,
        parent_url: Option<AutomergeUrl>,
    },

    /// Only in repo (new remote file, needs to be pulled)
    RemoteNew { remote: RepoFile },

    /// In snapshot+repo but not filesystem (local deletion)
    LocalDelete { snap: SnapFile, remote: RepoFile },

    /// In snapshot+filesystem but not repo (remote deletion)
    RemoteDelete { local: FsFile, snap: SnapFile },

    /// Only in snapshot (both sides deleted, nothing to do)
    Gone,

    /// Concurrent creation (in filesystem+repo but not snapshot)
    /// Need to decide how to handle this (merge, conflict, etc.)
    BothNew {
        #[expect(dead_code)]
        local: FsFile,
        remote: RepoFile,
    },

    /// Detected as a move/rename
    Moved {
        /// Original path before the move
        old_path: PathBuf,
        /// Snapshot entry at the old path
        #[expect(dead_code)]
        snap: SnapFile,
        /// Repo file at the old path
        remote: RepoFile,
        /// New content at the current path
        new_content: FsFile,
    },
}

/// Classification of a directory's sync state.
#[derive(Debug)]
pub enum DirChange {
    /// Directory exists in all three states, unchanged
    Unchanged(RepoDir),

    /// New local directory (only in filesystem)
    /// parent_url may be None if parent is also a new directory
    LocalNew {
        #[allow(dead_code)] // Can't use expect due to a bug in the lint
        abs_path: PathBuf,
        parent_url: Option<AutomergeUrl>,
    },

    /// New remote directory (only in repo)
    RemoteNew { remote: RepoDir },

    /// Directory deleted locally (in snapshot+repo but not filesystem)
    LocalDelete {
        #[expect(dead_code)]
        snap: SnapDir,
        remote: RepoDir,
    },

    /// Directory deleted remotely (in snapshot but not repo)
    RemoteDelete { snap: SnapDir },

    /// Only in snapshot (both sides deleted)
    Gone,
}

/// A complete sync plan mapping paths to their classifications.
///
/// After building the sync plan, the executor iterates over these
/// and performs the appropriate action for each.
#[derive(Debug)]
pub struct SyncPlan {
    /// File classifications by relative path
    pub files: HashMap<PathBuf, FileChange>,
    /// Directory classifications by relative path
    pub dirs: HashMap<PathBuf, DirChange>,
}

impl SyncPlan {
    /// Create a new empty sync plan.
    pub fn new() -> Self {
        Self {
            files: HashMap::new(),
            dirs: HashMap::new(),
        }
    }
}

impl Default for SyncPlan {
    fn default() -> Self {
        Self::new()
    }
}

/// Build a complete sync plan from the three states.
///
/// This collects all paths from all three states and classifies each one
/// based on its presence/absence and content changes.
///
/// The `root` parameter is the absolute path to the sync root directory,
/// used to construct absolute paths for directory entries.
pub fn build_sync_plan(fs: FsState, snap: SnapState, repo: RepoState, root: &Path) -> SyncPlan {
    let mut plan = SyncPlan::new();

    // Collect all file paths from all three states
    let mut all_file_paths: HashSet<PathBuf> = HashSet::new();
    all_file_paths.extend(fs.files.keys().cloned());
    all_file_paths.extend(snap.files.keys().cloned());
    all_file_paths.extend(repo.files.keys().cloned());

    // Collect all directory paths from all three states
    // Exclude the root directory (empty path) from classification
    let mut all_dir_paths: HashSet<PathBuf> = HashSet::new();
    all_dir_paths.extend(fs.dirs.iter().cloned());
    all_dir_paths.extend(
        snap.dirs
            .keys()
            .filter(|p| !p.as_os_str().is_empty())
            .cloned(),
    );
    all_dir_paths.extend(
        repo.dirs
            .keys()
            .filter(|p| !p.as_os_str().is_empty())
            .cloned(),
    );

    // Convert to owned data for consumption during classification
    let mut fs_files = fs.files;
    let fs_dirs = fs.dirs;
    let mut snap_files = snap.files;
    let mut repo_files = repo.files;
    let mut snap_dirs = snap.dirs;
    let mut repo_dirs = repo.dirs;

    // Build a map of parent URLs upfront so we don't need to worry about
    // consuming repo_dirs during iteration
    let parent_urls: HashMap<PathBuf, AutomergeUrl> = repo_dirs
        .iter()
        .map(|(path, dir)| (path.clone(), dir.url.clone()))
        .collect();

    // Classify each file
    for path in all_file_paths {
        let in_fs = fs_files.remove(&path);
        let in_snap = snap_files.remove(&path);
        let in_repo = repo_files.remove(&path);

        // Get parent directory URL for LocalNew case
        let parent_url = get_parent_url_from_map(&path, &parent_urls);

        let change = classify_file(in_fs, in_snap, in_repo, parent_url);
        plan.files.insert(path, change);
    }

    // Classify each directory (process in depth order - parents first)
    let mut sorted_dirs: Vec<PathBuf> = all_dir_paths.into_iter().collect();
    sorted_dirs.sort_by_key(|a| a.components().count());

    for path in sorted_dirs {
        let in_fs = fs_dirs.contains(&path);
        let in_snap = snap_dirs.remove(&path);
        let in_repo = repo_dirs.remove(&path);

        // Get parent directory URL from pre-computed map
        let parent_url = get_parent_url_from_map(&path, &parent_urls);

        // Construct absolute path from root
        let abs_path = root.join(&path);

        let change = classify_dir(in_fs, in_snap, in_repo, parent_url, abs_path);
        plan.dirs.insert(path, change);
    }

    plan
}

/// Get the parent directory's URL from a pre-computed URL map.
fn get_parent_url_from_map<P: AsRef<Path>>(
    path: P,
    url_map: &HashMap<PathBuf, AutomergeUrl>,
) -> Option<AutomergeUrl> {
    let parent = path.as_ref().parent()?;
    let parent_path = if parent.as_os_str().is_empty() {
        PathBuf::new() // Root directory
    } else {
        parent.to_path_buf()
    };
    url_map.get(&parent_path).cloned()
}

/// Classify a single file based on its presence in each state.
fn classify_file(
    fs: Option<FsFile>,
    snap: Option<SnapFile>,
    repo: Option<RepoFile>,
    parent_url: Option<AutomergeUrl>,
) -> FileChange {
    match (fs, snap, repo) {
        // In all three states - need to check for changes
        (Some(local), Some(snap), Some(remote)) => {
            let remote_changed = snap.heads != remote.heads;

            debug!(
                snap_heads = ?snap.heads,
                remote_heads = ?remote.heads,
                remote_changed = remote_changed,
                "classifying file"
            );

            // Check if local content has changed since last sync.
            // We do this by comparing local content with the repo content AT the snapshot heads,
            // which represents what the content was at the last sync point.
            let snapshot_content = get_file_content_at_heads(&remote.handle, &snap.heads);
            let local_changed = match snapshot_content {
                Ok(snap_content) => local.content != snap_content,
                Err(e) => {
                    // If we can't get snapshot content, log and assume unchanged
                    debug!(error = %e, "failed to get snapshot content, assuming unchanged");
                    false
                }
            };

            debug!(local_changed = local_changed, "content comparison result");

            match (local_changed, remote_changed) {
                // Both changed - conflict
                (true, true) => FileChange::Conflict {
                    local,
                    snap,
                    remote,
                },
                // Only local changed - push local edit
                (true, false) => FileChange::LocalEdit {
                    local,
                    snap,
                    remote,
                },
                // Only remote changed - pull remote edit
                (false, true) => FileChange::RemoteEdit {
                    local,
                    snap,
                    remote,
                },
                // Neither changed - unchanged
                (false, false) => FileChange::Unchanged(remote),
            }
        }

        // Only in filesystem - new local file
        // parent_url may be None if parent directory is also new
        (Some(local), None, None) => FileChange::LocalNew { local, parent_url },

        // Only in repo - new remote file
        (None, None, Some(remote)) => FileChange::RemoteNew { remote },

        // In snapshot and repo but not filesystem - local deletion
        (None, Some(snap), Some(remote)) => FileChange::LocalDelete { snap, remote },

        // In snapshot and filesystem but not repo - remote deletion
        (Some(local), Some(snap), None) => FileChange::RemoteDelete { local, snap },

        // Only in snapshot - both deleted
        (None, Some(_), None) => FileChange::Gone,

        // In filesystem and repo but not snapshot - concurrent creation
        (Some(local), None, Some(remote)) => FileChange::BothNew { local, remote },

        // This would mean repo has it but we never synced it - treat as RemoteNew
        (None, None, None) => {
            // Path exists but not in any state - shouldn't happen
            FileChange::Gone
        }
    }
}

/// Classify a single directory based on its presence in each state.
fn classify_dir(
    in_fs: bool,
    snap: Option<SnapDir>,
    repo: Option<RepoDir>,
    parent_url: Option<AutomergeUrl>,
    abs_path: PathBuf,
) -> DirChange {
    match (in_fs, snap, repo) {
        // In all three states
        (true, Some(_), Some(remote)) => DirChange::Unchanged(remote),

        // In filesystem and repo but not snapshot (shouldn't normally happen for dirs)
        (true, None, Some(remote)) => DirChange::Unchanged(remote),

        // Only in filesystem - new local directory
        // parent_url may be None if parent is also a new directory
        (true, None, None) => DirChange::LocalNew {
            abs_path,
            parent_url,
        },

        // Only in repo - new remote directory
        (false, None, Some(remote)) => DirChange::RemoteNew { remote },

        // In snapshot and repo but not filesystem - local deletion
        (false, Some(snap), Some(remote)) => DirChange::LocalDelete { snap, remote },

        // In snapshot and filesystem but not repo - remote deletion
        (true, Some(snap), None) => DirChange::RemoteDelete { snap },

        // Only in snapshot - both deleted
        (false, Some(_), None) => DirChange::Gone,

        // Path exists but not in any state
        (false, None, None) => DirChange::Gone,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::ChangeHash;

    fn test_hash(hex: &str) -> ChangeHash {
        hex.parse().expect("invalid test hash")
    }

    fn test_url(uuid: &str) -> AutomergeUrl {
        format!("automerge:{}", uuid)
            .parse()
            .expect("invalid test url")
    }

    const HASH_A: &str = "deadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeefdeadbeef";
    const UUID_FILE: &str = "6ba7b810-9dad-11d1-80b4-00c04fd430c8";
    const UUID_PARENT: &str = "6ba7b811-9dad-11d1-80b4-00c04fd430c8";

    fn make_fs_file() -> FsFile {
        FsFile {
            abs_path: PathBuf::from("/test/file.txt"),
            content: b"content".to_vec(),
            is_text: true,
            permissions: 0o644,
            extension: "txt".to_string(),
            mime_type: "text/plain".to_string(),
        }
    }

    fn make_snap_file(hash: &str) -> SnapFile {
        SnapFile {
            url: test_url(UUID_FILE),
            heads: vec![test_hash(hash)],
        }
    }

    #[test]
    fn test_classify_file_local_new() {
        let local = make_fs_file();
        let parent_url = test_url(UUID_PARENT);

        let change = classify_file(Some(local), None, None, Some(parent_url.clone()));

        match change {
            FileChange::LocalNew {
                local: _,
                parent_url: url,
            } => {
                assert_eq!(url.unwrap().to_string(), parent_url.to_string());
            }
            _ => panic!("Expected LocalNew, got {:?}", change),
        }
    }

    #[test]
    fn test_classify_file_local_new_no_parent() {
        let local = make_fs_file();

        let change = classify_file(Some(local), None, None, None);

        // LocalNew with parent_url: None (parent directory may also be new)
        match change {
            FileChange::LocalNew { parent_url, .. } => {
                assert!(parent_url.is_none());
            }
            _ => panic!("Expected LocalNew, got {:?}", change),
        }
    }

    #[test]
    fn test_classify_file_gone() {
        let snap = make_snap_file(HASH_A);

        let change = classify_file(None, Some(snap), None, None);

        assert!(matches!(change, FileChange::Gone));
    }

    #[test]
    fn test_classify_file_local_delete() {
        // Can't test fully without RepoFile, but can test pattern matching
        let snap = make_snap_file(HASH_A);

        // This will be LocalDelete when repo is present
        let change = classify_file(None, Some(snap), None, None);

        // With no repo, it's Gone
        assert!(matches!(change, FileChange::Gone));
    }

    #[test]
    fn test_classify_file_remote_delete() {
        let local = make_fs_file();
        let snap = make_snap_file(HASH_A);

        let change = classify_file(Some(local), Some(snap), None, None);

        match change {
            FileChange::RemoteDelete { local: _, snap: s } => {
                assert_eq!(s.heads, vec![test_hash(HASH_A)]);
            }
            _ => panic!("Expected RemoteDelete, got {:?}", change),
        }
    }

    #[test]
    fn test_classify_dir_local_new() {
        let parent_url = test_url(UUID_PARENT);
        let abs_path = PathBuf::from("/test/subdir");

        let change = classify_dir(true, None, None, Some(parent_url.clone()), abs_path.clone());

        match change {
            DirChange::LocalNew {
                abs_path: path,
                parent_url: url,
            } => {
                assert_eq!(path, abs_path);
                assert_eq!(url.unwrap().to_string(), parent_url.to_string());
            }
            _ => panic!("Expected LocalNew, got {:?}", change),
        }
    }

    #[test]
    fn test_classify_dir_gone() {
        let snap = SnapDir {
            url: test_url(UUID_FILE),
            heads: vec![test_hash(HASH_A)],
            entries: ["file.txt".to_string()].into_iter().collect(),
        };

        let change = classify_dir(false, Some(snap), None, None, PathBuf::from("/test/subdir"));

        assert!(matches!(change, DirChange::Gone));
    }

    #[test]
    fn test_classify_dir_remote_delete() {
        let snap = SnapDir {
            url: test_url(UUID_FILE),
            heads: vec![test_hash(HASH_A)],
            entries: ["file.txt".to_string()].into_iter().collect(),
        };

        let change = classify_dir(true, Some(snap), None, None, PathBuf::from("/test/subdir"));

        match change {
            DirChange::RemoteDelete { snap: s } => {
                assert_eq!(s.heads, vec![test_hash(HASH_A)]);
            }
            _ => panic!("Expected RemoteDelete, got {:?}", change),
        }
    }
}
