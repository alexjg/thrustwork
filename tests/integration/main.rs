//! Integration tests for thrustwork
//!
//! These tests spin up a local sync server and run the thrustwork binary
//! against it to verify end-to-end functionality.

mod harness;

use harness::TestHarness;
use std::time::Duration;

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
    client_b.clone(&root_url).await.expect("clone should succeed");

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
    client_b.clone(&root_url).await.expect("clone should succeed");

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
    client_a.write_file("subdir/nested.txt", "nested content").await;
    client_a
        .write_file("subdir/deep/deeper.txt", "deep content")
        .await;
    client_a.sync().await.expect("sync should succeed");

    let root_url = client_a.root_url().await.expect("should have root URL");

    let client_b = harness.create_client("client-b").await;
    client_b.clone(&root_url).await.expect("clone should succeed");

    assert_eq!(client_b.read_file("root.txt").await, "root content");
    assert_eq!(client_b.read_file("subdir/nested.txt").await, "nested content");
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
    client_a.write_binary_file("test.png", &binary_content).await;
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
