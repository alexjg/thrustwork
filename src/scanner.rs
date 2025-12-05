//! Directory scanner for finding files to sync.

use std::path::{Path, PathBuf};

use glob::Pattern;
use walkdir::WalkDir;

use crate::files::FileInfo;
use crate::snapshot::Snapshot;

/// A file that needs to be synced
#[derive(Debug, Clone)]
pub struct FileToSync {
    /// Absolute path to the file
    pub absolute_path: PathBuf,
    /// Path relative to the root directory
    pub relative_path: String,
    /// File information (extension, mime type, etc.)
    pub info: FileInfo,
}

/// Result of scanning a directory
#[derive(Debug)]
pub struct ScanResult {
    /// Files that are new (not in snapshot)
    pub new_files: Vec<FileToSync>,
    /// Files that have been modified (in snapshot but changed)
    pub modified_files: Vec<FileToSync>,
    /// Files that have been deleted (in snapshot but not on disk)
    pub deleted_files: Vec<String>,
}

impl ScanResult {
    /// Check if there are any changes
    pub fn has_changes(&self) -> bool {
        !self.new_files.is_empty() || !self.modified_files.is_empty() || !self.deleted_files.is_empty()
    }

    /// Total number of changes
    pub fn total_changes(&self) -> usize {
        self.new_files.len() + self.modified_files.len() + self.deleted_files.len()
    }
}

/// Compile exclude patterns into glob patterns
fn compile_patterns(patterns: &[String]) -> Vec<Pattern> {
    patterns
        .iter()
        .filter_map(|p| Pattern::new(p).ok())
        .collect()
}

/// Check if a filename matches any of the exclude patterns (public API)
///
/// This is a simpler version that just checks a single filename against patterns.
pub fn is_excluded(name: &str, patterns: &[String]) -> bool {
    for pattern in patterns {
        if let Ok(p) = Pattern::new(pattern) {
            if p.matches(name) {
                return true;
            }
        }
    }
    false
}

/// Check if a path matches any of the exclude patterns (internal)
fn is_path_excluded(path: &Path, root: &Path, patterns: &[Pattern]) -> bool {
    // Get the relative path from root
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

/// Scan a directory for files, respecting exclude patterns
///
/// Returns all files that are not excluded, with their relative paths.
pub fn scan_directory(
    root: &Path,
    exclude_patterns: &[String],
) -> std::io::Result<Vec<FileToSync>> {
    let patterns = compile_patterns(exclude_patterns);
    let mut files = Vec::new();

    for entry in WalkDir::new(root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();

        // Skip directories
        if path.is_dir() {
            continue;
        }

        // Skip excluded paths
        if is_path_excluded(path, root, &patterns) {
            continue;
        }

        // Get relative path
        let relative_path = path
            .strip_prefix(root)
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        // Skip empty relative paths (shouldn't happen, but be safe)
        if relative_path.is_empty() {
            continue;
        }

        let info = FileInfo::from_path(path);

        files.push(FileToSync {
            absolute_path: path.to_path_buf(),
            relative_path,
            info,
        });
    }

    Ok(files)
}

/// Scan a directory and compare against a snapshot to find changes
///
/// For Phase 4, we only care about new files (snapshot is empty).
/// Later phases will handle modifications and deletions.
pub fn scan_for_changes(
    root: &Path,
    exclude_patterns: &[String],
    snapshot: &Snapshot,
) -> std::io::Result<ScanResult> {
    let all_files = scan_directory(root, exclude_patterns)?;

    let mut new_files = Vec::new();
    let modified_files = Vec::new(); // TODO: Phase 5+ will detect modifications
    let deleted_files = Vec::new(); // TODO: Phase 5+ will detect deletions

    for file in all_files {
        // Check if file is in snapshot
        if snapshot.get_file(&file.absolute_path).is_none() {
            new_files.push(file);
        }
        // TODO: Check for modifications by comparing heads
    }

    // TODO: Check for deleted files by comparing snapshot entries against disk

    Ok(ScanResult {
        new_files,
        modified_files,
        deleted_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn test_compile_patterns() {
        let patterns = vec![".git".to_string(), "*.tmp".to_string()];
        let compiled = compile_patterns(&patterns);
        assert_eq!(compiled.len(), 2);
    }

    #[test]
    fn test_is_excluded_simple() {
        let patterns = compile_patterns(&[".git".to_string(), "node_modules".to_string()]);
        let root = Path::new("/project");

        assert!(is_path_excluded(Path::new("/project/.git"), root, &patterns));
        assert!(is_path_excluded(Path::new("/project/.git/config"), root, &patterns));
        assert!(is_path_excluded(
            Path::new("/project/node_modules/foo"),
            root,
            &patterns
        ));
        assert!(!is_path_excluded(Path::new("/project/src/main.rs"), root, &patterns));
    }

    #[test]
    fn test_is_excluded_glob() {
        let patterns = compile_patterns(&["*.tmp".to_string()]);
        let root = Path::new("/project");

        assert!(is_path_excluded(Path::new("/project/file.tmp"), root, &patterns));
        assert!(is_path_excluded(
            Path::new("/project/subdir/other.tmp"),
            root,
            &patterns
        ));
        assert!(!is_path_excluded(Path::new("/project/file.txt"), root, &patterns));
    }

    #[test]
    fn test_scan_directory() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create some files
        fs::write(root.join("file1.txt"), "content1").unwrap();
        fs::write(root.join("file2.rs"), "content2").unwrap();

        // Create a subdirectory with files
        fs::create_dir(root.join("subdir")).unwrap();
        fs::write(root.join("subdir/file3.txt"), "content3").unwrap();

        // Create an excluded directory
        fs::create_dir(root.join(".git")).unwrap();
        fs::write(root.join(".git/config"), "git config").unwrap();

        let exclude_patterns = vec![".git".to_string()];
        let files = scan_directory(root, &exclude_patterns).unwrap();

        // Should find 3 files, not the .git/config
        assert_eq!(files.len(), 3);

        let paths: Vec<_> = files.iter().map(|f| f.relative_path.as_str()).collect();
        assert!(paths.contains(&"file1.txt"));
        assert!(paths.contains(&"file2.rs"));
        assert!(paths.contains(&"subdir/file3.txt") || paths.contains(&"subdir\\file3.txt"));
        assert!(!paths.iter().any(|p| p.contains(".git")));
    }

    #[test]
    fn test_scan_for_changes_new_files() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Create some files
        fs::write(root.join("new_file.txt"), "content").unwrap();

        // Empty snapshot - all files are new
        let snapshot = Snapshot::new(root.to_path_buf(), None);

        let result = scan_for_changes(root, &[], &snapshot).unwrap();

        assert_eq!(result.new_files.len(), 1);
        assert_eq!(result.new_files[0].relative_path, "new_file.txt");
        assert!(result.modified_files.is_empty());
        assert!(result.deleted_files.is_empty());
        assert!(result.has_changes());
    }

    #[test]
    fn test_scan_result_no_changes() {
        let result = ScanResult {
            new_files: vec![],
            modified_files: vec![],
            deleted_files: vec![],
        };

        assert!(!result.has_changes());
        assert_eq!(result.total_changes(), 0);
    }

    #[test]
    fn test_file_info_populated() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        fs::write(root.join("test.txt"), "hello").unwrap();

        let files = scan_directory(root, &[]).unwrap();

        assert_eq!(files.len(), 1);
        assert_eq!(files[0].info.extension, "txt");
        assert_eq!(files[0].info.mime_type, "text/plain");
        assert!(files[0].info.is_text);
    }
}
