use automerge::Automerge;
use autosurgeon::{hydrate, reconcile};
use clap::{Parser, Subcommand};
use samod::{storage::TokioFilesystemStorage, AutomergeUrl, ConnDirection, Repo};
use tokio_tungstenite::connect_async;

mod changes;
mod clone;
mod config;
mod documents;
mod files;
mod init;
mod scanner;
mod snapshot;
mod sync;
mod sync_ops;

use documents::{DirectoryDocument, DirectoryEntry, FileDocument};

const SYNC_SERVER_URL: &str = "wss://sync3.automerge.org";

#[derive(Parser)]
#[command(name = "thrustwork")]
#[command(about = "A Rust implementation of pushwork - sync files via Automerge")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize a new thrustwork directory
    Init,

    /// Sync local changes with the remote
    Sync,

    /// Clone a remote pushwork directory
    Clone {
        /// Automerge URL of the directory to clone
        url: String,
    },

    /// Create test file+directory for pushwork interop testing
    CreateTest,

    /// Read a pushwork directory and its files
    ReadDir {
        /// Automerge URL of the directory document
        url: String,
    },
}

/// Connect to the sync server and return the repo
async fn connect_to_sync_server(repo: &Repo) {
    println!("Connecting to sync server: {}", SYNC_SERVER_URL);

    let (ws_stream, _response) = match connect_async(SYNC_SERVER_URL).await {
        Ok(conn) => conn,
        Err(e) => {
            eprintln!("Failed to connect to sync server: {}", e);
            std::process::exit(1);
        }
    };

    println!("WebSocket connected, starting sync protocol...");

    // Set up the connection - returns immediately with a Connection handle
    let conn = match repo.connect_tungstenite(ws_stream, ConnDirection::Outgoing) {
        Ok(conn) => conn,
        Err(_) => {
            eprintln!("Failed to set up connection: repo stopped");
            std::process::exit(1);
        }
    };

    conn.handshake_complete()
        .await
        .unwrap_or_else(|_| {
            eprintln!("Connection handshake failed");
            std::process::exit(1);
        });

    println!("Connected to sync server");
}

/// Create a test file and directory document for interop testing
async fn create_test(repo: &Repo) {
    // Create a file document
    let file = FileDocument::new(
        "test.txt".to_string(),
        "txt".to_string(),
        "text/plain".to_string(),
        "Hello from thrustwork!\n\nThis is a test file created for interop testing.",
        644,
    );

    let mut file_doc = Automerge::new();
    file_doc
        .transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &file).expect("reconcile file failed");
            Ok(())
        })
        .expect("file transaction failed");

    let file_handle = repo
        .create(file_doc)
        .await
        .expect("Failed to create file document");
    let file_url = file_handle.url().to_string();
    println!("Created file document: {}", file_url);

    // Create a directory document containing the file
    let dir = DirectoryDocument::with_entries(vec![DirectoryEntry::file(
        "test.txt".to_string(),
        file_url.clone(),
    )]);

    let mut dir_doc = Automerge::new();
    dir_doc
        .transact::<_, _, automerge::AutomergeError>(|txn| {
            reconcile(txn, &dir).expect("reconcile dir failed");
            Ok(())
        })
        .expect("dir transaction failed");

    let dir_handle = repo
        .create(dir_doc)
        .await
        .expect("Failed to create directory document");
    let dir_url = dir_handle.url().to_string();
    println!("Created directory document: {}", dir_url);

    // Wait for sync
    println!("Waiting for sync...");
    tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;

    println!("\n=== Interop Test Instructions ===");
    println!("To verify with pushwork, run:");
    println!("  mkdir /tmp/pushwork-test && cd /tmp/pushwork-test");
    println!("  pushwork clone {}", dir_url);
    println!("  cat test.txt");
    println!("Expected content: \"Hello from thrustwork!...\"");
}

/// Read a directory document and its contents
async fn read_dir(repo: &Repo, url: &str) {
    println!("Looking up directory: {}", url);

    // Give time for sync protocol to establish
    println!("Waiting for sync protocol to establish...");
    tokio::time::sleep(tokio::time::Duration::from_secs(1)).await;

    // Parse the URL
    let automerge_url: AutomergeUrl = url.parse().expect("Invalid automerge URL");

    let dir_handle = repo
        .find(automerge_url.doc_id().clone())
        .await
        .expect("Repo stopped")
        .expect("Directory document not found");

    // Hydrate the directory document
    let dir: DirectoryDocument = dir_handle.with_document(|doc| {
        hydrate(doc).expect("Failed to hydrate directory document")
    });

    println!("\nDirectory contents:");
    println!("  Type: {}", dir.patchwork.doc_type_str());
    if let Some(ts) = dir.last_sync_at {
        println!("  Last sync: {}", ts);
    }
    println!("  Entries: {}", dir.docs.len());

    for entry in &dir.docs {
        println!("    - {} ({})", entry.name_str(), entry.entry_type_str());

        // If it's a file, try to load and display its content
        if entry.entry_type_str() == "file" {
            let file_url: AutomergeUrl = entry
                .url_str()
                .parse()
                .expect("Invalid file automerge URL");

            if let Some(file_handle) = repo.find(file_url.doc_id().clone()).await.expect("Repo stopped") {
                let file: FileDocument = file_handle.with_document(|doc| {
                    hydrate(doc).expect("Failed to hydrate file document")
                });

                println!("      MIME type: {}", file.mime_type_str());
                println!("      Permissions: {}", file.metadata.permissions);
                let content = file.content_string();
                let preview = if content.len() > 100 {
                    format!("{}...", &content[..100])
                } else {
                    content
                };
                println!("      Content: {:?}", preview);
            } else {
                println!("      (file document not found)");
            }
        }
    }
}

#[tokio::main]
async fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Init => {
            // Get the current working directory
            let cwd = std::env::current_dir().unwrap_or_else(|e| {
                eprintln!("Failed to get current directory: {}", e);
                std::process::exit(1);
            });

            // Check if already initialized
            let paths = init::PushworkPaths::new(&cwd);
            if paths.is_initialized() {
                eprintln!(
                    "Directory is already initialized (use --force to reinitialize)"
                );
                std::process::exit(1);
            }

            // Create the directory structure
            println!("Initializing thrustwork in {:?}...", cwd);
            let (paths, mut config) =
                init::create_directory_structure(&cwd, false).unwrap_or_else(|e| {
                    eprintln!("Failed to initialize directory: {}", e);
                    std::process::exit(1);
                });

            // Create the root directory document and sync it
            println!("Creating root directory document...");
            let root_url = init::create_root_document(&paths, &mut config)
                .await
                .unwrap_or_else(|e| {
                    eprintln!("Failed to create root document: {}", e);
                    std::process::exit(1);
                });

            println!("\nInitialized thrustwork directory");
            println!("Root URL: {}", root_url);
            println!("\nShare this URL to allow others to sync with this directory.");
        }
        Commands::Sync => {
            // Find the .pushwork directory
            let cwd = std::env::current_dir().unwrap_or_else(|e| {
                eprintln!("Failed to get current directory: {}", e);
                std::process::exit(1);
            });

            let paths = init::PushworkPaths::find_from(&cwd).unwrap_or_else(|| {
                eprintln!("Not in a thrustwork directory (no .pushwork found)");
                eprintln!("Run 'thrustwork init' to initialize this directory.");
                std::process::exit(1);
            });

            println!("Syncing directory: {:?}", paths.root);

            // Load the config
            let config = config::DirectoryConfig::load(&paths.config_file).unwrap_or_else(|e| {
                eprintln!("Failed to load config: {}", e);
                std::process::exit(1);
            });

            // Initialize repo with filesystem storage
            let storage = TokioFilesystemStorage::new(&paths.automerge_dir);
            let repo = Repo::build_tokio().with_storage(storage).load().await;

            // Execute sync
            sync::execute(&paths, &config, &repo).await;
        }
        Commands::CreateTest => {
            // Initialize a samod Repo with in-memory storage
            let repo = samod::Repo::build_tokio().load().await;
            println!("Repo initialized (peer ID: {})", repo.peer_id());

            // Connect to sync server
            connect_to_sync_server(&repo).await;

            create_test(&repo).await;
            println!("\nDone!");
        }
        Commands::ReadDir { url } => {
            // Initialize a samod Repo with in-memory storage
            let repo = samod::Repo::build_tokio().load().await;
            println!("Repo initialized (peer ID: {})", repo.peer_id());

            // Connect to sync server
            connect_to_sync_server(&repo).await;

            read_dir(&repo, &url).await;
            println!("\nDone!");
        }
        Commands::Clone { url } => {
            println!("Cloning from: {}", url);

            // Get the current working directory
            let cwd = std::env::current_dir().unwrap_or_else(|e| {
                eprintln!("Failed to get current directory: {}", e);
                std::process::exit(1);
            });

            // Check if already initialized
            let paths = init::PushworkPaths::new(&cwd);
            if paths.is_initialized() {
                eprintln!("Directory is already initialized. Cannot clone into an existing thrustwork directory.");
                std::process::exit(1);
            }

            // Create the .pushwork directory structure
            println!("Creating .pushwork directory...");
            let (paths, _config) =
                init::create_directory_structure(&cwd, false).unwrap_or_else(|e| {
                    eprintln!("Failed to create directory structure: {}", e);
                    std::process::exit(1);
                });

            // Initialize repo with filesystem storage
            let storage = TokioFilesystemStorage::new(&paths.automerge_dir);
            let repo = Repo::build_tokio().with_storage(storage).load().await;

            // Connect to sync server
            println!("Connecting to sync server: {}", SYNC_SERVER_URL);
            let (ws_stream, _response) = connect_async(SYNC_SERVER_URL).await.unwrap_or_else(|e| {
                eprintln!("Failed to connect to sync server: {}", e);
                std::process::exit(1);
            });

            let conn = repo
                .connect_tungstenite(ws_stream, ConnDirection::Outgoing)
                .unwrap_or_else(|_| {
                    eprintln!("Failed to set up connection: repo stopped");
                    std::process::exit(1);
                });

            conn.handshake_complete().await.unwrap_or_else(|_| {
                eprintln!("Connection handshake failed");
                std::process::exit(1);
            });

            println!("Connected to sync server");

            // Execute the clone operation
            match clone::execute_clone(&repo, &url, cwd, paths).await {
                Ok(result) => {
                    println!("\nDone! {} file(s) cloned.", result.files_cloned);
                    if result.files_skipped > 0 {
                        println!("{} file(s) skipped due to errors.", result.files_skipped);
                    }
                }
                Err(e) => {
                    eprintln!("Clone failed: {}", e);
                    std::process::exit(1);
                }
            }
        }
    }
}
