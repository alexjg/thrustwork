# Implementation Plan

This document outlines the implementation plan for thrustwork, a Rust port of
the pushwork library. The work is organized into vertical slices - each phase
delivers working, testable functionality.

See DESIGN.md for the high-level design and technical specifications.

## Implementation Phases

### Phase 1: Connect to Sync Server ✓

**Goal**: Verify we can use samod to connect to the Automerge sync server and
create/read documents. This validates our core dependency before building
anything else.

**Deliverable**: A minimal binary that connects to the sync server, creates a
document, and prints its URL.

**Status**: Complete

---

### Phase 2: Read and Write Pushwork Documents ✓

**Goal**: Create documents that match the pushwork schema and verify
interoperability.

**Deliverable**: Can create file and directory documents that pushwork can
read, and can read documents created by pushwork.

**Status**: Complete

---

### Phase 3: Initialize a Directory ✓

**Goal**: Implement the `init` command to set up a directory for syncing.

**Deliverable**: `thrustwork init` creates the `.pushwork` folder, config file,
and root directory document.

**Status**: Complete

---

### Phase 4: Push a Single File ✓

**Goal**: Sync a single local file to the remote.

**Deliverable**: After `init`, creating a file and running `thrustwork sync`
pushes it to the remote.

**Status**: Complete

---

### Phase 5: Pull a Single File ✓

**Goal**: Sync a remote file to the local filesystem.

**Deliverable**: Clone a pushwork directory containing one file.

**Status**: Complete

---

### Phase 6: Detect and Sync Local Changes ✓

**Goal**: Detect when a tracked file has changed and push the update.

**Deliverable**: Modify a synced file, run sync, change is pushed.

**Status**: Complete

---

### Phase 7: Binary File Support ✓

**Goal**: Support syncing binary files (images, PDFs, etc.) in addition to text.

**Deliverable**: Binary files can be pushed, pulled, and synced like text files.

**Status**: Complete

---

### Phase 8: Detect and Apply Remote Changes ✓

**Goal**: Detect when a remote file has changed and pull the update.

**Deliverable**: When a file changes remotely, sync pulls it.

**Status**: Complete (includes CRDT merge for conflicts)

---

### Phase 9: Handle Multiple Files ✓

**Goal**: Sync directories with multiple files.

**Deliverable**: Init/clone/sync works with multiple files in the root
directory.

**Status**: Complete

---

### Phase 10: Handle Subdirectories ✓

**Goal**: Support nested directory structures with efficient parallel syncing.

**Deliverable**: Sync works with files in subdirectories, processing directories
and files in parallel as they become available.

**Status**: Complete

**Architecture Note**: This phase introduced a parallel task-based architecture
using `FuturesUnordered` that significantly simplified the codebase. The new
architecture:
- Processes sync operations as parallel tasks
- Discovers nested directories incrementally
- Handles new remote files automatically (originally Phase 12)
- Implements two-phase sync implicitly (originally Phase 14)
- Updates snapshot incrementally during sync

---

### Phase 11: Handle File Deletion ✓

**Goal**: Sync file deletions in both directions.

**Deliverable**: Delete a file locally, sync removes it remotely (and vice
versa).

**Status**: Complete

---

### Phase 12: Integration Test Suite

**Goal**: Build automated integration tests to verify sync behavior without
manual testing. This is essential before further feature work.

**Deliverable**: A test suite that:
- Compiles the thrustwork binary
- Runs a local automerge-repo sync server
- Executes test scenarios against the local server
- Verifies correct behavior programmatically

**Work**:
- Set up test infrastructure:
  - Build binary as part of test setup
  - Start/stop local sync server (automerge-repo-sync-server or equivalent)
  - Create temporary directories for test clients
  - Helper functions for common operations (init, clone, sync, file operations)
- Implement test scenarios:
  - Basic: init, push file, clone to second client, verify content
  - Bidirectional: push from A, pull to B, push from B, pull to A
  - Modifications: edit file on A, sync, verify B gets update
  - Conflicts: concurrent edits, verify CRDT merge
  - Deletions: delete locally → syncs remotely, delete remotely → syncs locally
  - Directories: nested directories, directory deletion
  - Binary files: push/pull binary content
- Consider using Rust's built-in test framework with `#[test]` or a dedicated
  integration test binary

**Technical Notes**:
- The sync server can be `automerge-repo-sync-server` (npm package) or a Rust
  equivalent if available
- Tests should be isolated (each test gets fresh directories and server state)
- Tests should clean up after themselves
- Consider parallel test execution with isolated server ports

**Verification**: `cargo test` runs all integration tests successfully.

---

### ~~Phase 12 (old): Handle New Remote Files~~ (Merged into Phase 10)

**Status**: Complete - handled by `FetchNewFile` task in the parallel sync
architecture. When `SyncDirectory` finds a remote file not in our snapshot,
it automatically fetches it.

---

### Phase 13: Move Detection

**Goal**: Detect file moves/renames and preserve document identity.

**Deliverable**: Rename a file locally, sync updates the name rather than
delete+create.

**Work**:
- In `process_sync_directory`, when we detect both a local deletion and a
  local-only file, check for content similarity
- Implement string similarity (Sørensen–Dice coefficient)
- If similarity exceeds threshold, treat as rename:
  - Update directory entry name instead of delete+create
  - Update file document's `name` field
  - Preserve document URL in snapshot

**Verification**: Sync a file, rename it locally, sync. Verify the document URL
is preserved (same document, new name).

---

### ~~Phase 14: Two-Phase Sync~~ (Merged into Phase 10)

**Status**: Complete - the parallel task architecture implicitly implements
two-phase sync:
1. Each task waits for remote changes (`we_have_their_changes`) before comparing
2. Local changes are pushed and we wait for confirmation (`they_have_our_changes`)
3. Conflicts are resolved via CRDT merge (already implemented in Phase 8)

The architecture ensures we always have the latest remote state before making
decisions, and concurrent edits merge correctly.

---

### Phase 15: Remaining CLI Commands

**Goal**: Complete the CLI interface.

**Deliverable**: All planned commands working.

**Work**:
- `status`: Show pending changes without syncing
  - Run change detection but don't apply changes
  - Print summary of local changes, remote changes, conflicts
- `url`: Print the root directory URL
- Add `--verbose` flag for detailed output
- Improve error messages and user feedback

**Note**: The `commit` command (push without pulling) may not be needed since
the parallel architecture handles this naturally - you can interrupt sync
after pushing completes.

**Verification**: Each command works as expected.

---

### Phase 16: Configuration

**Goal**: Support user configuration.

**Deliverable**: Global and local config files are respected.

**Work**:
- Load global config from `~/.config/thrustwork/config.json`
- Merge with local config (local takes precedence)
- Support custom sync server URL (already works)
- Support custom exclude patterns (already works)
- Support custom move detection threshold

**Verification**: Set a custom exclude pattern, verify files are excluded.

---

### Phase 17: Robustness

**Goal**: Handle edge cases and errors gracefully.

**Deliverable**: Production-ready error handling.

**Work**:
- Graceful handling of network disconnection mid-sync
- Timeout handling for sync wait (don't hang forever)
- Partial sync (continue on individual file errors) - already partially done
- Proper cleanup on shutdown (save snapshot even on interrupt)
- Handle permission errors on file operations
- Validate snapshot integrity on load

**Verification**: Interrupt sync mid-operation, verify no corruption. Simulate
network issues.

---

## Milestone Summary

| Milestone | Phases | Capability |
|-----------|--------|------------|
| M1: Connected | 1-2 | Can create/read pushwork-compatible documents |
| M2: Single file | 3-6 | Init, push one file, clone one file, detect changes |
| M3: Binary + remote | 7-8 | Binary file support, pull remote changes |
| M4: Full tree | 9-11 | Multiple files, nested directories, deletions |
| M5: Test infrastructure | 12 | Automated integration tests |
| M6: Complete sync | 13 | Move detection |
| M7: Production | 15-17 | Full CLI, configuration, robustness |

## Architecture Notes

### Parallel Task-Based Sync (Phase 10)

The sync system uses a work queue (`FuturesUnordered`) with task types:

```
SyncTask:
- SyncDirectory: Compare local/remote/snapshot, spawn child tasks
- SyncFile: Compare content, push/pull/merge as needed
- FetchNewFile: Pull file not in snapshot
- FetchNewDirectory: Pull directory not locally present
```

Key benefits:
1. **Parallel processing**: Independent files/directories sync concurrently
2. **Incremental discovery**: Nested directories discovered as parent syncs
3. **Unified flow**: Push, pull, and merge all happen in one pass
4. **Implicit two-phase**: Wait for remote state before comparing

The `SyncContext` holds shared state including a `Mutex<Snapshot>` for
thread-safe incremental updates.

### Clone Architecture

Clone uses a similar but simpler task set (`CloneTask`) that only fetches:
- `FetchDirectory`: Fetch directory doc, spawn tasks for contents
- `FetchFile`: Fetch file doc, write to disk

No comparison logic needed since there's no existing local state.

## Notes

**Interoperability testing**: Phases 2, 3, 4, and 5 should all be tested
against pushwork to ensure document compatibility. Don't proceed past Phase 5
without confirming interop.

**Incremental complexity**: Each phase adds one concept. If a phase feels too
large, split it. If something is harder than expected, the narrow scope limits
the blast radius.

**Test as you go**: Each phase has a verification step. Write these as
integration tests where practical so they continue to pass as development
proceeds.
