use std::path::{Path, PathBuf};

/// Paths for the pushwork directory structure
#[derive(Debug, Clone)]
pub struct PushworkPaths {
    /// Root directory being synced
    pub root: PathBuf,
    /// The .pushwork directory
    pub pushwork_dir: PathBuf,
    /// The automerge storage directory
    pub automerge_dir: PathBuf,
    /// The config file path
    pub config_file: PathBuf,
}

/// Name of the pushwork directory
pub const PUSHWORK_DIR: &str = ".pushwork";

/// Name of the automerge storage subdirectory
const AUTOMERGE_DIR: &str = "automerge";

/// Name of the config file
const CONFIG_FILENAME: &str = "config.json";

impl PushworkPaths {
    /// Create paths for a given root directory
    pub fn new(root: &Path) -> Self {
        let pushwork_dir = root.join(PUSHWORK_DIR);
        Self {
            root: root.to_path_buf(),
            automerge_dir: pushwork_dir.join(AUTOMERGE_DIR),
            config_file: pushwork_dir.join(CONFIG_FILENAME),
            pushwork_dir,
        }
    }

    /// Check if the directory is already initialized
    pub fn is_initialized(&self) -> bool {
        self.pushwork_dir.exists()
    }

    /// Find a .pushwork directory by walking up from the given path.
    ///
    /// Returns `Some(PushworkPaths)` if found, `None` if we reach the root without finding it.
    pub fn find_from(start: &Path) -> Option<Self> {
        let mut current = start.to_path_buf();
        loop {
            let paths = Self::new(&current);
            if paths.is_initialized() {
                return Some(paths);
            }
            if !current.pop() {
                return None;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use crate::{PushworkPaths, config::Config};

    #[test]
    fn test_pushwork_paths() {
        let root = Path::new("/tmp/test");
        let paths = PushworkPaths::new(root);

        assert_eq!(paths.root, PathBuf::from("/tmp/test"));
        assert_eq!(paths.pushwork_dir, PathBuf::from("/tmp/test/.pushwork"));
        assert_eq!(
            paths.automerge_dir,
            PathBuf::from("/tmp/test/.pushwork/automerge")
        );
        assert_eq!(
            paths.config_file,
            PathBuf::from("/tmp/test/.pushwork/config.json")
        );
    }

    #[tokio::test]
    async fn test_find_from() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // Not initialized - should return None
        assert!(PushworkPaths::find_from(root).is_none());

        // Initialize and try again
        Config::init(root, false).await.unwrap();
        let found = PushworkPaths::find_from(root);
        assert!(found.is_some());
        assert_eq!(found.unwrap().root, root);

        // Create a subdirectory and search from there
        let subdir = root.join("subdir");
        std::fs::create_dir(&subdir).unwrap();
        let found = PushworkPaths::find_from(&subdir);
        assert!(found.is_some());
        assert_eq!(found.unwrap().root, root);

        // Create a nested subdirectory
        let nested = subdir.join("nested");
        std::fs::create_dir(&nested).unwrap();
        let found = PushworkPaths::find_from(&nested);
        assert!(found.is_some());
        assert_eq!(found.unwrap().root, root);
    }
}
