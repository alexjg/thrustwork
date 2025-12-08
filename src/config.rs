//! Configuration types and management for thrustwork.
//!
//! Matches pushwork's config structure for compatibility.

use automerge::Automerge;
use autosurgeon::reconcile;
pub(crate) use errors::{ConnectError, InitError};
use samod::{AutomergeUrl, Repo, storage::TokioFilesystemStorage};
use std::path::{Path, PathBuf};
use tokio_tungstenite::connect_async;

mod directory_config;
pub(crate) use directory_config::{ConfigError, DirectoryConfig};

use crate::{PushworkPaths, documents::DirectoryDocument};

/// Default sync server URL
pub const DEFAULT_SYNC_SERVER: &str = "wss://sync3.automerge.org";

/// Default sync server storage ID (matches pushwork)
pub const DEFAULT_SYNC_SERVER_STORAGE_ID: &str = "3760df37-a4c6-4f66-9ecd-732039a9385d";

/// Default move detection threshold for rename detection
pub const DEFAULT_MOVE_DETECTION_THRESHOLD: f64 = 0.7;

/// The snapshot file name
pub const SNAPSHOT_FILENAME: &str = "snapshot.json";

fn default_exclude_patterns() -> Vec<String> {
    vec![
        ".git".to_string(),
        "node_modules".to_string(),
        "*.tmp".to_string(),
        ".DS_Store".to_string(),
        ".pushwork".to_string(),
    ]
}

#[derive(Clone, Debug)]
pub(crate) struct Config {
    paths: PushworkPaths,
    config_file: DirectoryConfig,
}

impl Config {
    pub(crate) async fn init<P: AsRef<Path>>(root_dir: P, force: bool) -> Result<Self, InitError> {
        let (paths, config) = create_directory_structure(root_dir.as_ref(), None, force).await?;
        Ok(Self {
            paths,
            config_file: config,
        })
    }

    pub(crate) async fn init_for_clone<P: AsRef<Path>>(
        root_dir: P,
        root_doc_url: AutomergeUrl,
        force: bool,
    ) -> Result<Self, InitError> {
        let (paths, config) =
            create_directory_structure(root_dir.as_ref(), Some(root_doc_url), force).await?;
        Ok(Self {
            paths,
            config_file: config,
        })
    }

    pub(crate) fn load(paths: PushworkPaths) -> Result<Self, ConfigError> {
        let config_file = DirectoryConfig::load(&paths.config_file)?;

        Ok(Self { paths, config_file })
    }

    pub(crate) fn find_from_cwd(cwd: &Path) -> Result<Self, ConfigError> {
        let Some(paths) = PushworkPaths::find_from(cwd) else {
            return Err(ConfigError::NotAPushworkDirectory(cwd.to_path_buf()));
        };
        Self::load(paths)
    }

    pub(crate) fn save(&self) -> Result<(), ConfigError> {
        self.config_file.save(&self.paths.config_file)
    }

    pub async fn repo(&self) -> Repo {
        Repo::build_tokio()
            .with_storage(TokioFilesystemStorage::new(&self.paths.automerge_dir))
            .load()
            .await
    }

    pub async fn sync_server_connection(
        &self,
        repo: &Repo,
    ) -> Result<samod::Connection, ConnectError> {
        let sync_url = self.config_file.sync_server_url();
        let (ws_stream, _response) = connect_async(sync_url).await?;

        // Set up the connection - this returns immediately with a Connection handle
        let connection = repo.connect_tungstenite(ws_stream, samod::ConnDirection::Outgoing)?;

        // Wait for the other end to respond
        connection
            .handshake_complete()
            .await
            .map_err(ConnectError::HandshakeFailed)?;

        Ok(connection)
    }

    pub(crate) fn set_sync_server_url(&mut self, url: String) {
        self.config_file.sync_server = Some(url);
    }

    pub(crate) fn sync_server_url(&self) -> &str {
        self.config_file.sync_server.as_ref().unwrap()
    }

    pub(crate) fn exclude_patterns(&self) -> &Vec<String> {
        &self.config_file.exclude_patterns
    }

    pub(crate) fn move_detection_threshold(&self) -> f64 {
        self.config_file.sync.move_detection_threshold
    }

    pub(crate) fn snapshot_path(&self) -> PathBuf {
        self.paths.pushwork_dir.join(SNAPSHOT_FILENAME)
    }

    pub(crate) fn root_dir(&self) -> &Path {
        &self.paths.root
    }

    pub(crate) fn root_doc_url(&self) -> &AutomergeUrl {
        &self.config_file.root_directory_url
    }
}

/// Create the .pushwork directory structure
///
/// Creates:
/// - `.pushwork/`
/// - `.pushwork/automerge/`
/// - An automerge document containing an empty DirectoryDocument stored in .pushwork/automerge/
/// - `.pushwork/config.json` with the root URL set to the above document
///
/// Returns the paths and initial config.
pub async fn create_directory_structure(
    root: &Path,
    root_doc_url: Option<AutomergeUrl>,
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

    let root_url = if let Some(url) = root_doc_url {
        url
    } else {
        create_root_document(&paths).await?
    };

    let config = DirectoryConfig::new(root_url);
    config.save(&paths.config_file)?;

    Ok((paths, config))
}

async fn create_root_document(paths: &PushworkPaths) -> Result<AutomergeUrl, InitError> {
    let dir = DirectoryDocument::new();

    let mut doc = Automerge::new();
    doc.transact::<_, _, automerge::AutomergeError>(|txn| {
        reconcile(txn, &dir).expect("reconcile failed");
        Ok(())
    })
    .expect("transaction failed");

    let repo = Repo::build_tokio()
        .with_storage(TokioFilesystemStorage::new(paths.automerge_dir.clone()))
        .load()
        .await;

    // Create the document in the repo
    let handle = repo.create(doc).await?;

    // shut the repo down, which flushes the new document to storage
    repo.stop().await;

    Ok(handle.url())
}

mod errors {
    use std::path::PathBuf;

    use thiserror::Error;

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
        Config(#[from] super::ConfigError),

        #[error("failed to create root document as the repo has stopped")]
        CreateRootDocument(#[from] samod::Stopped),
    }

    #[derive(Debug, Error)]
    pub enum ConnectError {
        #[error("failed to connect to sync server")]
        Connect(#[from] tokio_tungstenite::tungstenite::Error),
        #[error(transparent)]
        Transient(#[from] samod::Stopped),
        #[error("failed to handshake with sync server, connection finished with reason: {0:?}")]
        HandshakeFailed(samod::ConnFinishedReason),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::{NamedTempFile, TempDir};

    async fn make_dummy_url() -> AutomergeUrl {
        let repo = Repo::build_tokio().load().await;
        let handle = repo.create(Automerge::new()).await.unwrap();
        handle.url()
    }

    #[tokio::test]
    async fn test_default_config() {
        let root_url = make_dummy_url().await;
        let config = DirectoryConfig::new(root_url.clone());

        assert_eq!(config.root_directory_url.to_string(), root_url.to_string(),);
        assert!(config.sync_enabled);
        assert_eq!(config.sync_server.as_deref(), Some(DEFAULT_SYNC_SERVER));
        assert!(config.exclude_patterns.contains(&".git".to_string()));
        assert!(config.exclude_patterns.contains(&".pushwork".to_string()));
        assert_eq!(config.sync.move_detection_threshold, 0.7);
    }

    #[tokio::test]
    async fn test_config_save_load() {
        let root_url = make_dummy_url().await;
        let config = DirectoryConfig::new(root_url);

        // Create temp file
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();

        // Save
        config.save(&path).unwrap();

        // Verify file contents
        let contents = std::fs::read_to_string(&path).unwrap();
        println!("Saved config:\n{}", contents);

        // Load
        let loaded = DirectoryConfig::load(&path).unwrap();

        assert_eq!(
            loaded.root_directory_url.to_string(),
            config.root_directory_url.to_string()
        );
        assert_eq!(loaded.sync_enabled, config.sync_enabled);
    }

    #[tokio::test]
    async fn test_sync_server_url_default() {
        let root_url = make_dummy_url().await;
        let config = DirectoryConfig::new(root_url);
        assert_eq!(config.sync_server_url(), DEFAULT_SYNC_SERVER);
    }

    #[tokio::test]
    async fn test_create_directory_structure() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let (paths, _config) = create_directory_structure(root, None, false).await.unwrap();

        // Verify directories exist
        assert!(paths.pushwork_dir.exists());
        assert!(paths.automerge_dir.exists());
        assert!(paths.config_file.exists());

        // Verify config was saved
        let loaded_config = DirectoryConfig::load(&paths.config_file).unwrap();
        assert!(loaded_config.sync_enabled);
    }

    #[tokio::test]
    async fn test_root_document_created() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        let (paths, config) = create_directory_structure(root, None, false).await.unwrap();

        // Now load a repo pointing at the storage
        let repo = Repo::build_tokio()
            .with_storage(TokioFilesystemStorage::new(paths.automerge_dir))
            .load()
            .await;

        // Load the root document from storage
        repo.find(config.root_directory_url.document_id().clone())
            .await
            .expect("Root document should exist");
    }

    #[tokio::test]
    async fn test_already_initialized_error() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // First init should succeed
        create_directory_structure(root, None, false).await.unwrap();

        // Second init should fail
        let result = create_directory_structure(root, None, false).await;
        assert!(matches!(result, Err(InitError::AlreadyInitialized)));
    }

    #[tokio::test]
    async fn test_force_reinitialize() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();

        // First init
        create_directory_structure(root, None, false).await.unwrap();

        // Force reinit should succeed
        let result = create_directory_structure(root, None, true).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_is_initialized() {
        let temp_dir = TempDir::new().unwrap();
        let root = temp_dir.path();
        let paths = PushworkPaths::new(root);

        assert!(!paths.is_initialized());

        create_directory_structure(root, None, false).await.unwrap();

        assert!(paths.is_initialized());
    }
}
