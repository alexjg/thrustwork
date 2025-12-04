//! Clone command implementation for pulling remote directories.

use std::path::{Path, PathBuf};

use autosurgeon::hydrate;
use samod::{AutomergeUrl, DocHandle, Repo};
use thiserror::Error;

use crate::config::DirectoryConfig;
use crate::documents::{DirectoryDocument, FileDocument};
use crate::init::PushworkPaths;
use crate::snapshot::{Snapshot, SnapshotFileEntry};

/// Errors that can occur during clone operations
#[derive(Debug, Error)]
pub enum CloneError {
    #[error("Invalid URL: {0}")]
    InvalidUrl(String),

    #[error("Root directory document not found")]
    RootNotFound,

    #[error("Failed to hydrate directory: {0}")]
    HydrateDirectory(String),

    #[error("Failed to save config: {0}")]
    SaveConfig(String),

    #[error("Failed to save snapshot: {0}")]
    SaveSnapshot(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

/// Result of cloning a directory
pub struct CloneResult {
    pub files_cloned: usize,
    pub files_skipped: usize,
}

/// Execute the full clone operation
pub(crate) async fn execute_clone(
    repo: &Repo,
    url: &str,
    cwd: PathBuf,
    paths: PushworkPaths,
) -> Result<CloneResult, CloneError> {
    // Parse URL and load directory
    let root_url = parse_automerge_url(url)?;
    println!("Loading root directory...");

    let (_dir_handle, dir) = load_directory(repo, &root_url).await?;

    println!("Directory loaded:");
    println!("  Type: {}", dir.patchwork.doc_type_str());
    println!("  Entries: {}", dir.docs.len());

    // Extract files to clone
    let (files_to_clone, dirs_skipped) = extract_files_from_directory(&dir);

    for file in &files_to_clone {
        println!("  Found file: {}", file.name);
    }

    println!("\nFound {} file(s) to clone", files_to_clone.len());
    if dirs_skipped > 0 {
        println!("Skipped {} subdirectory(ies)", dirs_skipped);
    }

    if files_to_clone.is_empty() {
        return Ok(CloneResult {
            files_cloned: 0,
            files_skipped: 0,
        });
    }

    // Load all file documents concurrently
    println!("\nPulling files...");
    let loaded_results = load_files_concurrently(repo, &files_to_clone).await;

    // Filter out failures
    let loaded_files: Vec<LoadedFile> = loaded_results.into_iter().flatten().collect();
    let files_skipped = files_to_clone.len() - loaded_files.len();

    for file in &loaded_files {
        println!("  Loaded: {} ({} bytes)", file.name, file.content.len());
    }

    if loaded_files.is_empty() {
        return Ok(CloneResult {
            files_cloned: 0,
            files_skipped,
        });
    }

    // Write files to disk
    println!("\nWriting files to disk...");
    let written_count = write_files_to_disk(&cwd, &loaded_files);

    // Save config and snapshot
    save_config_and_snapshot(&paths, &root_url, &cwd, &loaded_files)?;
    println!("Config and snapshot saved.");

    Ok(CloneResult {
        files_cloned: written_count,
        files_skipped,
    })
}

/// Save config and snapshot after cloning
fn save_config_and_snapshot(
    paths: &PushworkPaths,
    root_url: &AutomergeUrl,
    cwd: &Path,
    files: &[LoadedFile],
) -> Result<(), CloneError> {
    // Update config with root directory URL
    let mut config = DirectoryConfig::load(&paths.config_file).unwrap_or_default();
    config.root_directory_url = Some(root_url.to_string());
    config
        .save(&paths.config_file)
        .map_err(|e| CloneError::SaveConfig(e.to_string()))?;

    // Create snapshot with cloned files
    let mut snapshot = Snapshot::new(cwd.to_path_buf(), Some(root_url.clone()));

    for file in files {
        let heads = file.handle.with_document(|doc| doc.get_heads());
        let file_url = file.handle.url();

        let entry = SnapshotFileEntry {
            path: cwd.join(&file.name),
            url: file_url,
            head: heads,
            extension: file.extension.clone(),
            mime_type: file.mime_type.clone(),
        };

        snapshot.add_file(file.name.clone(), entry);
    }

    snapshot.update_timestamp();
    let snapshot_path = Snapshot::path_in(&paths.pushwork_dir);
    snapshot
        .save(&snapshot_path)
        .map_err(|e| CloneError::SaveSnapshot(e.to_string()))?;

    Ok(())
}

/// Write loaded files to the local filesystem
///
/// Returns the number of files successfully written.
fn write_files_to_disk(cwd: &Path, files: &[LoadedFile]) -> usize {
    let mut written = 0;

    for file in files {
        let file_path = cwd.join(&file.name);

        // Check if file already exists
        if file_path.exists() {
            eprintln!("  Warning: '{}' already exists, skipping", file.name);
            continue;
        }

        // Write content to file
        if let Err(e) = std::fs::write(&file_path, &file.content) {
            eprintln!("  Error writing '{}': {}", file.name, e);
            continue;
        }

        // Set file permissions (Unix only)
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(file.permissions as u32);
            if let Err(e) = std::fs::set_permissions(&file_path, perms) {
                eprintln!(
                    "  Warning: Failed to set permissions for '{}': {}",
                    file.name, e
                );
            }
        }

        println!("  Wrote: {}", file.name);
        written += 1;
    }

    written
}

/// Load multiple file documents concurrently
async fn load_files_concurrently(repo: &Repo, files: &[FileToClone]) -> Vec<Option<LoadedFile>> {
    let futures: Vec<_> = files
        .iter()
        .map(|file| load_single_file(repo, file))
        .collect();

    futures::future::join_all(futures).await
}

/// Load the root directory document and hydrate it
async fn load_directory(
    repo: &Repo,
    url: &AutomergeUrl,
) -> Result<(DocHandle, DirectoryDocument), CloneError> {
    let handle = repo
        .find(url.doc_id().clone())
        .await
        .expect("Repo stopped")
        .ok_or(CloneError::RootNotFound)?;

    let dir: DirectoryDocument = handle
        .with_document(|doc| hydrate(doc))
        .map_err(|e| CloneError::HydrateDirectory(e.to_string()))?;

    Ok((handle, dir))
}

/// Extract files to clone from a directory document
///
/// Returns a list of files and the count of skipped directories.
fn extract_files_from_directory(dir: &DirectoryDocument) -> (Vec<FileToClone>, usize) {
    let mut files = Vec::new();
    let mut skipped_dirs = 0;

    for entry in &dir.docs {
        let entry_type = entry.entry_type_str();
        let name = entry.name_str().to_string();
        let url_str = entry.url_str();

        if entry_type == "file" {
            match url_str.parse::<AutomergeUrl>() {
                Ok(url) => files.push(FileToClone { name, url }),
                Err(e) => {
                    eprintln!("  Warning: Invalid file URL for '{}': {}", name, e);
                }
            }
        } else if entry_type == "folder" {
            println!(
                "  Skipping directory: {} (subdirectories not yet supported)",
                name
            );
            skipped_dirs += 1;
        }
    }

    (files, skipped_dirs)
}

/// Load a single file document
async fn load_single_file(repo: &Repo, file: &FileToClone) -> Option<LoadedFile> {
    let handle = match repo.find(file.url.doc_id().clone()).await.expect("Repo stopped") {
        Some(h) => h,
        None => {
            eprintln!("  Warning: File document not found for '{}'", file.name);
            return None;
        }
    };

    let file_doc: FileDocument = match handle.with_document(|doc| hydrate(doc)) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("  Warning: Failed to hydrate file '{}': {}", file.name, e);
            return None;
        }
    };

    Some(LoadedFile {
        name: file.name.clone(),
        content: file_doc.content_bytes().to_vec(),
        permissions: file_doc.metadata.permissions,
        extension: file_doc.extension_str().to_string(),
        mime_type: file_doc.mime_type_str().to_string(),
        handle,
    })
}

/// Parse an Automerge URL
fn parse_automerge_url(url: &str) -> Result<AutomergeUrl, CloneError> {
    url.parse()
        .map_err(|e| CloneError::InvalidUrl(format!("{}", e)))
}

/// Information about a file to clone
struct FileToClone {
    name: String,
    url: AutomergeUrl,
}

/// A file that has been loaded from the remote
struct LoadedFile {
    name: String,
    /// Raw bytes - works for both text and binary files
    content: Vec<u8>,
    permissions: i64,
    extension: String,
    mime_type: String,
    handle: DocHandle,
}
