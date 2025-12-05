//! Test harness for integration tests
//!
//! Provides utilities for:
//! - Starting a local sync server
//! - Creating test client directories
//! - Running thrustwork commands
//! - Cleaning up after tests

use futures::lock::Mutex;
use samod::Repo;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::Arc;
use tempfile::TempDir;
use tokio::net::TcpListener;
use tokio::process::Command;

/// Path to the thrustwork binary (built by cargo)
fn binary_path() -> PathBuf {
    // The binary is built in target/debug or target/release
    // For tests, cargo builds it in target/debug
    let manifest_dir = env!("CARGO_MANIFEST_DIR");
    PathBuf::from(manifest_dir)
        .join("target")
        .join("debug")
        .join("thrustwork")
}

/// A running test server
struct TestServer {
    port: u16,
    #[allow(dead_code)]
    repo: Repo,
    #[allow(dead_code)]
    connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
}

impl TestServer {
    async fn start() -> Self {
        let repo = Repo::build_tokio().load().await;
        let connections = Arc::new(Mutex::new(Vec::new()));

        let app = axum::Router::new()
            .route("/", axum::routing::get(websocket_handler))
            .with_state((repo.clone(), connections.clone()));

        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("failed to bind test server");
        let port = listener.local_addr().unwrap().port();

        let server = axum::serve(listener, app).into_future();
        tokio::spawn(server);

        TestServer {
            port,
            repo,
            connections,
        }
    }

    fn url(&self) -> String {
        format!("ws://127.0.0.1:{}", self.port)
    }
}

#[allow(clippy::type_complexity)]
async fn websocket_handler(
    ws: axum::extract::ws::WebSocketUpgrade,
    axum::extract::State((repo, connections)): axum::extract::State<(
        Repo,
        Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
    )>,
) -> axum::response::Response {
    ws.on_upgrade(|socket| handle_socket(socket, repo, connections))
}

async fn handle_socket(
    socket: axum::extract::ws::WebSocket,
    repo: Repo,
    connections: Arc<Mutex<Vec<tokio::task::JoinHandle<()>>>>,
) {
    let conn = repo.accept_axum(socket).unwrap();
    let handle = tokio::spawn(async move {
        let _ = conn.finished().await;
    });
    connections.lock().await.push(handle);
}

/// Test harness that manages the server and temp directories
pub struct TestHarness {
    server: TestServer,
    #[allow(dead_code)]
    temp_dir: TempDir,
    base_path: PathBuf,
}

impl TestHarness {
    /// Create a new test harness with a running server
    pub async fn new() -> Self {
        let server = TestServer::start().await;
        let temp_dir = TempDir::new().expect("failed to create temp dir");
        let base_path = temp_dir.path().to_path_buf();

        TestHarness {
            server,
            temp_dir,
            base_path,
        }
    }

    /// Create a new test client in a subdirectory
    pub async fn create_client(&self, name: &str) -> TestClient {
        let path = self.base_path.join(name);
        std::fs::create_dir_all(&path).expect("failed to create client dir");

        TestClient {
            path,
            server_url: self.server.url(),
        }
    }
}

/// A test client that can run thrustwork commands
pub struct TestClient {
    path: PathBuf,
    server_url: String,
}

impl TestClient {
    /// Get the client's directory path
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run thrustwork init
    pub async fn init(&self) -> Result<Output, std::io::Error> {
        let output = self
            .run_command(&["init", "--sync-server", &self.server_url])
            .await?;

        // Check if init succeeded (non-zero exit means failure)
        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "init failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }

        Ok(output)
    }

    /// Run thrustwork clone
    pub async fn clone(&self, url: &str) -> Result<Output, std::io::Error> {
        let output = self
            .run_command(&["clone", url, "--sync-server", &self.server_url])
            .await?;

        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "clone failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }

        Ok(output)
    }

    /// Run thrustwork sync
    pub async fn sync(&self) -> Result<Output, std::io::Error> {
        let output = self
            .run_command(&["sync", "--sync-server", &self.server_url])
            .await?;

        if !output.status.success() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!(
                    "sync failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ),
            ));
        }

        Ok(output)
    }

    /// Get the root URL from the config
    pub async fn root_url(&self) -> Option<String> {
        let config_path = self.path.join(".pushwork/config.json");
        let content = std::fs::read_to_string(config_path).ok()?;
        let config: serde_json::Value = serde_json::from_str(&content).ok()?;
        config
            .get("rootDirectoryUrl")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
    }

    /// Write a text file
    pub async fn write_file(&self, relative_path: &str, content: &str) {
        let path = self.path.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&path, content).expect("failed to write file");
    }

    /// Write a binary file
    pub async fn write_binary_file(&self, relative_path: &str, content: &[u8]) {
        let path = self.path.join(relative_path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::write(&path, content).expect("failed to write file");
    }

    /// Read a text file
    pub async fn read_file(&self, relative_path: &str) -> String {
        let path = self.path.join(relative_path);
        std::fs::read_to_string(&path).expect("failed to read file")
    }

    /// Read a binary file
    pub async fn read_binary_file(&self, relative_path: &str) -> Vec<u8> {
        let path = self.path.join(relative_path);
        std::fs::read(&path).expect("failed to read file")
    }

    /// Check if a file exists
    pub async fn file_exists(&self, relative_path: &str) -> bool {
        self.path.join(relative_path).exists()
    }

    /// Delete a file
    pub async fn delete_file(&self, relative_path: &str) {
        let path = self.path.join(relative_path);
        std::fs::remove_file(&path).expect("failed to delete file");
    }

    /// Delete a directory recursively
    pub async fn delete_dir(&self, relative_path: &str) {
        let path = self.path.join(relative_path);
        std::fs::remove_dir_all(&path).expect("failed to delete directory");
    }

    /// Rename/move a file
    pub async fn rename_file(&self, old_path: &str, new_path: &str) {
        let old = self.path.join(old_path);
        let new = self.path.join(new_path);
        if let Some(parent) = new.parent() {
            std::fs::create_dir_all(parent).expect("failed to create parent dirs");
        }
        std::fs::rename(&old, &new).expect("failed to rename file");
    }

    /// Get the file URL from the snapshot (for verifying document identity)
    /// The snapshot files is serialized as array of [key, entry] pairs
    pub async fn get_file_url(&self, relative_path: &str) -> Option<String> {
        let snapshot_path = self.path.join(".pushwork/snapshot.json");
        let content = std::fs::read_to_string(&snapshot_path).ok()?;

        // Debug: print the snapshot content if TEST_VERBOSE is set
        if std::env::var("TEST_VERBOSE").is_ok() {
            eprintln!("Snapshot content for {}:\n{}", relative_path, content);
        }

        let snapshot: serde_json::Value = serde_json::from_str(&content).ok()?;
        // Files is an array of [key, entry] pairs (HashMap serialized as entries)
        let files = snapshot.get("files")?.as_array()?;

        // Try matching by filename
        let target_filename = std::path::Path::new(relative_path)
            .file_name()?
            .to_str()?;

        for pair in files {
            let pair_arr = pair.as_array()?;
            if pair_arr.len() != 2 {
                continue;
            }
            let key = pair_arr[0].as_str()?;
            let entry = &pair_arr[1];

            let key_filename = std::path::Path::new(key)
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            if key_filename == target_filename || key == relative_path {
                if let Some(url) = entry.get("url").and_then(|v| v.as_str()) {
                    return Some(url.to_string());
                }
            }
        }

        None
    }

    /// Run a thrustwork command
    async fn run_command(&self, args: &[&str]) -> Result<Output, std::io::Error> {
        let output = Command::new(binary_path())
            .args(args)
            .current_dir(&self.path)
            .output()
            .await?;

        // Always print output for debugging during test development
        if !output.status.success() || std::env::var("TEST_VERBOSE").is_ok() {
            eprintln!(
                "Command: thrustwork {}\nstdout: {}\nstderr: {}\nexit: {:?}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr),
                output.status
            );
        }

        Ok(output)
    }
}
