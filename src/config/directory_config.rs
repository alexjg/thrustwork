use std::path::{Path, PathBuf};

use samod::AutomergeUrl;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Per-directory configuration stored in `.pushwork/config.json`
///
/// This structure matches pushwork's DirectoryConfig for compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DirectoryConfig {
    /// URL of the root directory document
    #[serde(with = "serde_automergeurl")]
    pub root_directory_url: AutomergeUrl,

    /// Whether sync is enabled for this directory
    pub sync_enabled: bool,

    /// Sync server WebSocket URL
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_server: Option<String>,

    /// Sync server storage ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sync_server_storage_id: Option<String>,

    /// Patterns to exclude from syncing (glob patterns)
    pub exclude_patterns: Vec<String>,

    /// Sync-related settings
    pub sync: SyncConfig,
}

/// Sync-related configuration options
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct SyncConfig {
    /// Threshold for Sørensen–Dice coefficient when detecting moves/renames
    pub move_detection_threshold: f64,
}

impl DirectoryConfig {
    /// Create a new config with default values
    pub fn new(root_url: AutomergeUrl) -> Self {
        Self {
            root_directory_url: root_url,
            sync_enabled: true,
            sync_server: Some(super::DEFAULT_SYNC_SERVER.to_string()),
            sync_server_storage_id: Some(super::DEFAULT_SYNC_SERVER_STORAGE_ID.to_string()),
            exclude_patterns: super::default_exclude_patterns(),
            sync: SyncConfig::default(),
        }
    }

    /// Load config from a JSON file
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let content = std::fs::read_to_string(path).map_err(ConfigError::Io)?;
        let config = serde_json::from_str(&content).map_err(ConfigError::Parse)?;
        Ok(config)
    }

    /// Save config to a JSON file
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let content = serde_json::to_string_pretty(self).map_err(ConfigError::Serialize)?;
        std::fs::write(path, content).map_err(ConfigError::Io)?;
        Ok(())
    }

    /// Get the sync server URL, falling back to default
    pub fn sync_server_url(&self) -> &str {
        self.sync_server
            .as_deref()
            .unwrap_or(super::DEFAULT_SYNC_SERVER)
    }
}

mod serde_automergeurl {
    use samod::AutomergeUrl;
    use serde::{Deserialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        url: &AutomergeUrl,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&url.to_string())
    }

    pub(crate) fn deserialize<'de, D>(deserializer: D) -> Result<AutomergeUrl, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        s.parse().map_err(serde::de::Error::custom)
    }
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            move_detection_threshold: super::DEFAULT_MOVE_DETECTION_THRESHOLD,
        }
    }
}

/// Errors that can occur when working with config
#[derive(Debug, Error)]
pub enum ConfigError {
    /// IO error reading/writing config file
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Error parsing config JSON
    #[error("Failed to parse config: {0}")]
    Parse(#[source] serde_json::Error),

    /// Error serializing config to JSON
    #[error("Failed to serialize config: {0}")]
    Serialize(#[source] serde_json::Error),

    #[error("{0} is not a pushwork directory or within a pushwork directory")]
    NotAPushworkDirectory(PathBuf),
}
