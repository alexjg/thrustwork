//! Integration tests for thrustwork
//!
//! These tests spin up a local sync server and run the thrustwork binary
//! against it to verify end-to-end functionality.

mod harness;

use harness::TestHarness;

/// Basic test: init creates .pushwork directory
#[tokio::test]
async fn test_init_creates_pushwork_directory() {
    let harness = TestHarness::new().await;
    let client = harness.create_client("client-a").await;

    client.init().await.expect("init should succeed");

    assert!(client.path().join(".pushwork").exists());
    assert!(client.path().join(".pushwork/config.json").exists());
    // Note: snapshot.json is only created after first sync, not during init
}

/// Basic test: push single file, clone to second client
#[tokio::test]
async fn test_push_and_clone_single_file() {
    let harness = TestHarness::new().await;

    // Client A creates and pushes a file
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.expect("init should succeed");

    client_a.write_file("hello.txt", "Hello, World!").await;
    client_a.sync().await.expect("sync should succeed");

    // Get the root URL from client A
    let root_url = client_a.root_url().await.expect("should have root URL");

    // Client B clones
    let client_b = harness.create_client("client-b").await;
    client_b
        .clone(&root_url)
        .await
        .expect("clone should succeed");

    // Verify content matches
    let content = client_b.read_file("hello.txt").await;
    assert_eq!(content, "Hello, World!");
}

/// Test: push multiple files, clone, verify all present
#[tokio::test]
async fn test_push_and_clone_multiple_files() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.expect("init should succeed");

    client_a.write_file("file1.txt", "Content 1").await;
    client_a.write_file("file2.txt", "Content 2").await;
    client_a.write_file("file3.txt", "Content 3").await;
    client_a.sync().await.expect("sync should succeed");

    let root_url = client_a.root_url().await.expect("should have root URL");

    let client_b = harness.create_client("client-b").await;
    client_b
        .clone(&root_url)
        .await
        .expect("clone should succeed");

    assert_eq!(client_b.read_file("file1.txt").await, "Content 1");
    assert_eq!(client_b.read_file("file2.txt").await, "Content 2");
    assert_eq!(client_b.read_file("file3.txt").await, "Content 3");
}

/// Test: push nested directory structure, clone, verify structure
#[tokio::test]
async fn test_push_and_clone_nested_directories() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.expect("init should succeed");

    client_a.write_file("root.txt", "root content").await;
    client_a
        .write_file("subdir/nested.txt", "nested content")
        .await;
    client_a
        .write_file("subdir/deep/deeper.txt", "deep content")
        .await;
    client_a.sync().await.expect("sync should succeed");

    let root_url = client_a.root_url().await.expect("should have root URL");

    let client_b = harness.create_client("client-b").await;
    client_b
        .clone(&root_url)
        .await
        .expect("clone should succeed");

    assert_eq!(client_b.read_file("root.txt").await, "root content");
    assert_eq!(
        client_b.read_file("subdir/nested.txt").await,
        "nested content"
    );
    assert_eq!(
        client_b.read_file("subdir/deep/deeper.txt").await,
        "deep content"
    );
}

/// Test: modify file on client A, sync both, verify B has changes
#[tokio::test]
async fn test_modification_syncs_to_other_client() {
    let harness = TestHarness::new().await;

    // Setup: A creates file, B clones
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("test.txt", "original").await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // A modifies the file
    client_a.write_file("test.txt", "modified by A").await;
    client_a.sync().await.unwrap();

    // B syncs and should see the change
    client_b.sync().await.unwrap();
    assert_eq!(client_b.read_file("test.txt").await, "modified by A");
}

/// Test: modify file on client B, sync both, verify A has changes
#[tokio::test]
async fn test_modification_from_clone_syncs_back() {
    let harness = TestHarness::new().await;

    // Setup: A creates file, B clones
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("test.txt", "original").await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // B modifies the file
    client_b.write_file("test.txt", "modified by B").await;
    client_b.sync().await.unwrap();

    // A syncs and should see the change
    client_a.sync().await.unwrap();
    assert_eq!(client_a.read_file("test.txt").await, "modified by B");
}

/// Test: delete file locally, sync, verify removed from clone
#[tokio::test]
async fn test_local_deletion_syncs_to_remote() {
    let harness = TestHarness::new().await;

    // Setup: A creates file, B clones
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("deleteme.txt", "to be deleted").await;
    client_a.write_file("keepme.txt", "keep this").await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // Verify B has both files
    assert!(client_b.file_exists("deleteme.txt").await);
    assert!(client_b.file_exists("keepme.txt").await);

    // A deletes the file
    client_a.delete_file("deleteme.txt").await;
    client_a.sync().await.unwrap();

    // B syncs and should see the deletion
    client_b.sync().await.unwrap();
    assert!(!client_b.file_exists("deleteme.txt").await);
    assert!(client_b.file_exists("keepme.txt").await);
}

/// Test: delete file remotely, sync, verify deleted locally
#[tokio::test]
async fn test_remote_deletion_syncs_locally() {
    let harness = TestHarness::new().await;

    // Setup: A creates file, B clones
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("deleteme.txt", "to be deleted").await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // B deletes the file
    client_b.delete_file("deleteme.txt").await;
    client_b.sync().await.unwrap();

    // A syncs and should see the deletion
    client_a.sync().await.unwrap();
    assert!(!client_a.file_exists("deleteme.txt").await);
}

/// Test: delete directory locally, sync, verify removed from clone
#[tokio::test]
async fn test_directory_deletion_syncs() {
    let harness = TestHarness::new().await;

    // Setup: A creates directory with files, B clones
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("subdir/file1.txt", "content 1").await;
    client_a.write_file("subdir/file2.txt", "content 2").await;
    client_a.write_file("keepme.txt", "keep this").await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // Verify B has the directory
    assert!(client_b.file_exists("subdir/file1.txt").await);

    // A deletes the directory
    client_a.delete_dir("subdir").await;
    client_a.sync().await.unwrap();

    // B syncs and should see the deletion
    client_b.sync().await.unwrap();
    assert!(!client_b.file_exists("subdir/file1.txt").await);
    assert!(!client_b.path().join("subdir").exists());
    assert!(client_b.file_exists("keepme.txt").await);
}

/// Test: push binary file, clone, verify content matches
#[tokio::test]
async fn test_binary_file_sync() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();

    // Create a binary file (PNG header + some bytes)
    let binary_content: Vec<u8> = vec![
        0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A, // PNG header
        0x00, 0x00, 0x00, 0x0D, 0x49, 0x48, 0x44, 0x52, // IHDR chunk start
        0xDE, 0xAD, 0xBE, 0xEF, // Some arbitrary bytes
    ];
    client_a
        .write_binary_file("test.png", &binary_content)
        .await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    let received = client_b.read_binary_file("test.png").await;
    assert_eq!(received, binary_content);
}

/// Test: sync with no changes is idempotent
#[tokio::test]
async fn test_sync_no_changes_idempotent() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("test.txt", "content").await;
    client_a.sync().await.unwrap();

    // Sync again with no changes - should succeed without errors
    let result = client_a.sync().await;
    assert!(result.is_ok());

    // Content should be unchanged
    assert_eq!(client_a.read_file("test.txt").await, "content");
}

/// Test: sync empty directory
#[tokio::test]
async fn test_sync_empty_directory() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // Both should have only .pushwork directory
    let entries: Vec<_> = std::fs::read_dir(client_b.path())
        .unwrap()
        .filter_map(|e| e.ok())
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .collect();
    assert!(entries.is_empty());
}

/// Test: rename file locally, sync, verify document URL preserved
#[tokio::test]
async fn test_rename_file_preserves_document_url() {
    let harness = TestHarness::new().await;

    // Setup: A creates and syncs a file
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a
        .write_file(
            "original.txt",
            "This is the original content that should be preserved",
        )
        .await;
    client_a.sync().await.unwrap();

    // Get the document URL before rename
    let url_before = client_a
        .get_file_url("original.txt")
        .await
        .expect("should have URL in snapshot");

    // Rename the file locally
    client_a.rename_file("original.txt", "renamed.txt").await;
    client_a.sync().await.unwrap();

    // Verify: old file gone, new file exists
    assert!(!client_a.file_exists("original.txt").await);
    assert!(client_a.file_exists("renamed.txt").await);

    // Get the document URL after rename - should be the same (document identity preserved)
    let url_after = client_a
        .get_file_url("renamed.txt")
        .await
        .expect("should have URL in snapshot");
    assert_eq!(
        url_before, url_after,
        "Document URL should be preserved after rename"
    );

    // Verify content is unchanged
    assert_eq!(
        client_a.read_file("renamed.txt").await,
        "This is the original content that should be preserved"
    );
}

/// Test: rename file on fresh sync (no other clients involved)
#[tokio::test]
async fn test_rename_file_fresh_sync() {
    let harness = TestHarness::new().await;

    // Client renames file between syncs
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a
        .write_file("original.txt", "Content to be renamed")
        .await;
    client_a.sync().await.unwrap();

    // Rename locally
    client_a.rename_file("original.txt", "newname.txt").await;

    // Sync - should detect as move
    client_a.sync().await.unwrap();

    // Verify file exists with new name
    assert!(!client_a.file_exists("original.txt").await);
    assert!(client_a.file_exists("newname.txt").await);
    assert_eq!(
        client_a.read_file("newname.txt").await,
        "Content to be renamed"
    );
}

/// Test: rename file, sync to other client, verify other client sees rename
#[tokio::test]
async fn test_rename_syncs_to_other_client() {
    let harness = TestHarness::new().await;

    // Setup: A creates file, B clones
    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a
        .write_file("original.txt", "Content to be renamed")
        .await;
    client_a.sync().await.unwrap();

    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // Verify B has the original file
    assert!(client_b.file_exists("original.txt").await);

    // A renames the file
    client_a.rename_file("original.txt", "newname.txt").await;
    client_a.sync().await.unwrap();

    // B syncs and should see the renamed file
    client_b.sync().await.unwrap();

    assert!(!client_b.file_exists("original.txt").await);
    assert!(client_b.file_exists("newname.txt").await);
    assert_eq!(
        client_b.read_file("newname.txt").await,
        "Content to be renamed"
    );
}

/// Test: rename with content modification - if similar enough, still detected as move
#[tokio::test]
async fn test_rename_with_small_content_change() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a.write_file("original.txt", "This is a long piece of content that should be mostly preserved even after a small edit").await;
    client_a.sync().await.unwrap();

    let url_before = client_a
        .get_file_url("original.txt")
        .await
        .expect("should have URL");

    // Delete old file and create new file with slightly modified content
    client_a.delete_file("original.txt").await;
    client_a.write_file("renamed.txt", "This is a long piece of content that should be mostly preserved even after a small edit!").await;
    client_a.sync().await.unwrap();

    let url_after = client_a
        .get_file_url("renamed.txt")
        .await
        .expect("should have URL");

    // Should be detected as a move (>70% similar)
    assert_eq!(
        url_before, url_after,
        "Should detect as move due to high similarity"
    );
}

/// Test: different content should NOT be detected as move
#[tokio::test]
async fn test_different_content_not_detected_as_move() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();
    client_a
        .write_file("file1.txt", "AAAAAAAAAAAAAAAAAAAA")
        .await;
    client_a.sync().await.unwrap();

    let url_before = client_a
        .get_file_url("file1.txt")
        .await
        .expect("should have URL");

    // Delete old file and create completely different new file
    client_a.delete_file("file1.txt").await;
    client_a
        .write_file("file2.txt", "BBBBBBBBBBBBBBBBBBBB")
        .await;
    client_a.sync().await.unwrap();

    let url_after = client_a
        .get_file_url("file2.txt")
        .await
        .expect("should have URL");

    // Should NOT be detected as a move - URLs should be different
    assert_ne!(
        url_before, url_after,
        "Completely different content should not be detected as move"
    );
}

/// Test: cross-directory move - file moved from root to subdirectory
#[tokio::test]
async fn test_cross_directory_move_to_subdir() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();

    // Create a file in root and a subdirectory
    client_a
        .write_file("moveme.txt", "This file will be moved to a subdirectory")
        .await;
    client_a.create_dir("subdir").await;
    client_a.sync().await.unwrap();

    let url_before = client_a
        .get_file_url("moveme.txt")
        .await
        .expect("should have URL");

    // Move file from root to subdirectory
    client_a
        .rename_file("moveme.txt", "subdir/moveme.txt")
        .await;
    client_a.sync().await.unwrap();

    // Should be detected as a cross-directory move - URL should be preserved
    let url_after = client_a
        .get_file_url("subdir/moveme.txt")
        .await
        .expect("should have URL");

    assert_eq!(
        url_before, url_after,
        "Cross-directory move should preserve document URL"
    );
    assert!(
        !client_a.file_exists("moveme.txt").await,
        "Original file should be gone"
    );
    assert!(
        client_a.file_exists("subdir/moveme.txt").await,
        "File should be in new location"
    );
}

/// Test: cross-directory move - file moved from subdirectory to root
#[tokio::test]
async fn test_cross_directory_move_from_subdir() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();

    // Create a file in subdirectory
    client_a.create_dir("subdir").await;
    client_a
        .write_file("subdir/moveme.txt", "This file will be moved to root")
        .await;
    client_a.sync().await.unwrap();

    let url_before = client_a
        .get_file_url("subdir/moveme.txt")
        .await
        .expect("should have URL");

    // Move file from subdirectory to root
    client_a
        .rename_file("subdir/moveme.txt", "moveme.txt")
        .await;
    client_a.sync().await.unwrap();

    // Should be detected as a cross-directory move - URL should be preserved
    let url_after = client_a
        .get_file_url("moveme.txt")
        .await
        .expect("should have URL");

    assert_eq!(
        url_before, url_after,
        "Cross-directory move should preserve document URL"
    );
    assert!(
        !client_a.file_exists("subdir/moveme.txt").await,
        "Original file should be gone"
    );
    assert!(
        client_a.file_exists("moveme.txt").await,
        "File should be in new location"
    );
}

/// Test: cross-directory move syncs to other client
#[tokio::test]
async fn test_cross_directory_move_syncs_to_other_client() {
    let harness = TestHarness::new().await;

    let client_a = harness.create_client("client-a").await;
    client_a.init().await.unwrap();

    // Create a file in root and a subdirectory
    client_a
        .write_file("moveme.txt", "Content that will be moved cross-directory")
        .await;
    client_a.create_dir("target").await;
    client_a.sync().await.unwrap();

    // Clone to client B
    let root_url = client_a.root_url().await.unwrap();
    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.unwrap();

    // Verify B has the original file in root
    assert!(client_b.file_exists("moveme.txt").await);
    assert!(!client_b.file_exists("target/moveme.txt").await);

    // A moves the file to subdirectory
    client_a
        .rename_file("moveme.txt", "target/moveme.txt")
        .await;
    client_a.sync().await.unwrap();

    // B syncs and should see the file in its new location
    client_b.sync().await.unwrap();

    assert!(
        !client_b.file_exists("moveme.txt").await,
        "File should be gone from root"
    );
    assert!(
        client_b.file_exists("target/moveme.txt").await,
        "File should be in target dir"
    );
    assert_eq!(
        client_b.read_file("target/moveme.txt").await,
        "Content that will be moved cross-directory"
    );
}

// =============================================================================
// CLI Command Tests
// =============================================================================

/// Test the url command prints the root URL
#[tokio::test]
async fn test_url_command() {
    let harness = TestHarness::new().await;
    let client = harness.create_client("test").await;

    // Initialize the directory
    client.init().await.unwrap();

    // Get root URL from config for comparison
    let expected_url = client
        .root_url()
        .await
        .expect("Should have root URL after init");

    // Run the url command
    let output = client.url_command().await.unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);

    // The output should be the URL followed by a newline
    assert_eq!(stdout.trim(), expected_url);
}

/// Test the url command fails gracefully when not initialized
#[tokio::test]
async fn test_url_command_not_initialized() {
    let harness = TestHarness::new().await;
    let client = harness.create_client("test").await;

    // Don't initialize - just try to run url command
    let output = client.run_command_raw(&["url"]).await.unwrap();

    // Should fail with non-zero exit code
    assert!(
        !output.status.success(),
        "url command should fail when not initialized"
    );

    // Should have helpful error message
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("Not in a thrustwork directory") || stderr.contains(".pushwork"));
}
