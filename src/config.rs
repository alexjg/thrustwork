//! Configuration types and management for thrustwork.
//!
//! Matches pushwork's config structure for compatibility.

use serde::{Deserialize, Serialize};
use std::path::Path;
use thiserror::Error;

/// Default sync server URL
pub const DEFAULT_SYNC_SERVER: &str = "wss://sync3.automerge.org";

/// Default sync server storage ID (matches pushwork)
pub const DEFAULT_SYNC_SERVER_STORAGE_ID: &str = "3760df37-a4c6-4f66-9ecd-732039a9385d";

/// Default move detection threshold for rename detection
pub const DEFAULT_MOVE_DETECTION_THRESHOLD: f64 = 0.7;

/// Sync-related configuration options
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    /// Threshold for Sørensen–Dice coefficient when detecting moves/renames
    pub move_detection_threshold: f64,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            move_detection_threshold: DEFAULT_MOVE_DETECTION_THRESHOLD,
        }
    }
}

/// Per-directory configuration stored in `.pushwork/config.json`
///
/// This structure matches pushwork's DirectoryConfig for compatibility.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DirectoryConfig {
    /// URL of the root directory document (set after creation)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub root_directory_url: Option<String>,

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

impl Default for DirectoryConfig {
    fn default() -> Self {
        Self {
            root_directory_url: None,
            sync_enabled: true,
            sync_server: Some(DEFAULT_SYNC_SERVER.to_string()),
            sync_server_storage_id: Some(DEFAULT_SYNC_SERVER_STORAGE_ID.to_string()),
            exclude_patterns: default_exclude_patterns(),
            sync: SyncConfig::default(),
        }
    }
}

/// Returns the default exclude patterns matching pushwork
fn default_exclude_patterns() -> Vec<String> {
    vec![
        ".git".to_string(),
        "node_modules".to_string(),
        "*.tmp".to_string(),
        ".DS_Store".to_string(),
        ".pushwork".to_string(),
    ]
}

impl DirectoryConfig {
    /// Create a new config with default values
    pub fn new() -> Self {
        Self::default()
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
        self.sync_server.as_deref().unwrap_or(DEFAULT_SYNC_SERVER)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::NamedTempFile;

    #[test]
    fn test_default_config() {
        let config = DirectoryConfig::default();

        assert!(config.root_directory_url.is_none());
        assert!(config.sync_enabled);
        assert_eq!(
            config.sync_server.as_deref(),
            Some(DEFAULT_SYNC_SERVER)
        );
        assert!(config.exclude_patterns.contains(&".git".to_string()));
        assert!(config.exclude_patterns.contains(&".pushwork".to_string()));
        assert_eq!(config.sync.move_detection_threshold, 0.7);
    }

    #[test]
    fn test_config_roundtrip() {
        let mut config = DirectoryConfig::default();
        config.root_directory_url = Some("automerge:abc123".to_string());

        // Serialize to JSON
        let json = serde_json::to_string_pretty(&config).unwrap();
        println!("Config JSON:\n{}", json);

        // Parse back
        let parsed: DirectoryConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(parsed.root_directory_url, config.root_directory_url);
        assert_eq!(parsed.sync_enabled, config.sync_enabled);
        assert_eq!(parsed.exclude_patterns, config.exclude_patterns);
    }

    #[test]
    fn test_config_save_load() {
        let mut config = DirectoryConfig::default();
        config.root_directory_url = Some("automerge:test123".to_string());

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

        assert_eq!(loaded.root_directory_url, config.root_directory_url);
        assert_eq!(loaded.sync_enabled, config.sync_enabled);
    }

    #[test]
    fn test_sync_server_url_default() {
        let config = DirectoryConfig::default();
        assert_eq!(config.sync_server_url(), DEFAULT_SYNC_SERVER);

        let mut config = DirectoryConfig::default();
        config.sync_server = None;
        assert_eq!(config.sync_server_url(), DEFAULT_SYNC_SERVER);
    }
}
