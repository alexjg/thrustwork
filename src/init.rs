//! Directory initialization logic for thrustwork.
//!
//! Handles creating the `.pushwork` directory structure and config files.

use std::path::{Path, PathBuf};

use automerge::Automerge;
use autosurgeon::reconcile;
use samod::{storage::TokioFilesystemStorage, ConnDirection, Repo};
use thiserror::Error;
use tokio_tungstenite::connect_async;

use crate::config::{ConfigError, DirectoryConfig};
use crate::documents::DirectoryDocument;

/// Name of the pushwork directory
pub const PUSHWORK_DIR: &str = ".pushwork";

/// Name of the automerge storage subdirectory
const AUTOMERGE_DIR: &str = "automerge";

/// Name of the config file
const CONFIG_FILENAME: &str = "config.json";

/// Errors that can occur during initialization
#[derive(Debug, Error)]
pub enum InitError {
    /// Directory is already initialized
    #[error("Directory is already initialized (use --force to reinitialize)")]
    AlreadyInitialized,

    /// Failed to create directory
    #[error("Failed to create directory '{path}': {source}")]
    CreateDir {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    /// Failed to save config
    #[error("Failed to save config: {0}")]
    Config(#[from] ConfigError),

    /// Failed to connect to sync server
    #[error("Failed to connect to sync server '{url}': {source}")]
    SyncConnect {
        url: String,
        #[source]
        source: tokio_tungstenite::tungstenite::Error,
    },

    /// Failed to create document in repo
    #[error("Failed to create document: {0}")]
    CreateDocument(String),

    /// Repo was stopped
    #[error("Repository was stopped")]
    RepoStopped,

    /// Connection failed during handshake
    #[error("Connection failed during handshake")]
    ConnectionFailed,
}

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
}

/// Create the .pushwork directory structure
///
/// Creates:
/// - `.pushwork/`
/// - `.pushwork/automerge/`
/// - `.pushwork/config.json` (with default config, no root URL yet)
///
/// Returns the paths and initial config.
pub fn create_directory_structure(
    root: &Path,
    force: bool,
) -> Result<(PushworkPaths, DirectoryConfig), InitError> {
    let paths = PushworkPaths::new(root);

    // Check if already initialized
    if paths.is_initialized() && !force {
        return Err(InitError::AlreadyInitialized);
    }

    // Create .pushwork directory
    std::fs::create_dir_all(&paths.pushwork_dir).map_err(|source| InitError::CreateDir {
        path: paths.pushwork_dir.clone(),
        source,
    })?;

    // Create automerge storage directory
    std::fs::create_dir_all(&paths.automerge_dir).map_err(|source| InitError::CreateDir {
        path: paths.automerge_dir.clone(),
        source,
    })?;

    // Create initial config (without root URL - that comes after document creation)
    let config = DirectoryConfig::default();
    config.save(&paths.config_file)?;

    Ok((paths, config))
}

/// Create the root directory document and sync it
///
/// This function:
/// 1. Initializes a samod Repo with filesystem storage
/// 2. Connects to the sync server
/// 3. Creates an empty DirectoryDocument
/// 4. Waits for sync to complete
/// 5. Updates the config file with the root URL
///
/// Returns the root directory URL.
pub async fn create_root_document(
    paths: &PushworkPaths,
    config: &mut DirectoryConfig,
) -> Result<String, InitError> {
    // Initialize repo with filesystem storage
    let storage = TokioFilesystemStorage::new(&paths.automerge_dir);
    let repo = Repo::build_tokio()
        .with_storage(storage)
        .load()
        .await;

    // Connect to sync server
    let sync_url = config.sync_server_url();
    let (ws_stream, _response) = connect_async(sync_url)
        .await
        .map_err(|source| InitError::SyncConnect {
            url: sync_url.to_string(),
            source,
        })?;

    // Set up the connection - this returns immediately with a Connection handle
    let conn = repo
        .connect_tungstenite(ws_stream, ConnDirection::Outgoing)
        .map_err(|_| InitError::RepoStopped)?;

    // Wait for the handshake to complete before proceeding
    conn.handshake_complete()
        .await
        .map_err(|_| InitError::ConnectionFailed)?;

    // Create an empty root directory document
    let dir = DirectoryDocument::new();

    let mut doc = Automerge::new();
    doc.transact::<_, _, automerge::AutomergeError>(|txn| {
        reconcile(txn, &dir).expect("reconcile failed");
        Ok(())
    })
    .expect("transaction failed");

    // Create the document in the repo
    let handle = repo
        .create(doc)
        .await
        .map_err(|e| InitError::CreateDocument(format!("{:?}", e)))?;

    let root_url = handle.url().to_string();

    // Wait for sync server to acknowledge our changes
    handle.they_have_our_changes(conn.id()).await;

    // Update config with the root URL
    config.root_directory_url = Some(root_url.clone());
    config.save(&paths.config_file)?;

    Ok(root_url)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

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

    #[test]
    fn test_create_directory_structure() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let (paths, _config) = create_directory_structure(root, false).unwrap();

        // Verify directories exist
        assert!(paths.pushwork_dir.exists());
        assert!(paths.automerge_dir.exists());
        assert!(paths.config_file.exists());

        // Verify config was saved
        let loaded_config = DirectoryConfig::load(&paths.config_file).unwrap();
        assert!(loaded_config.root_directory_url.is_none());
        assert!(loaded_config.sync_enabled);
    }

    #[test]
    fn test_already_initialized_error() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // First init should succeed
        create_directory_structure(root, false).unwrap();

        // Second init should fail
        let result = create_directory_structure(root, false);
        assert!(matches!(result, Err(InitError::AlreadyInitialized)));
    }

    #[test]
    fn test_force_reinitialize() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // First init
        create_directory_structure(root, false).unwrap();

        // Force reinit should succeed
        let result = create_directory_structure(root, true);
        assert!(result.is_ok());
    }

    #[test]
    fn test_is_initialized() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let paths = PushworkPaths::new(root);

        assert!(!paths.is_initialized());

        create_directory_structure(root, false).unwrap();

        assert!(paths.is_initialized());
    }
}
