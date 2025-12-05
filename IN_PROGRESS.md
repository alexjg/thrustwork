# Implementation Progress Tracker

This document tracks the step-by-step implementation of features. It's organized into discrete chunks of work that are each small enough to review independently. When working on this codebase with an LLM, use this document to understand what has been completed and what needs to be done next.

The overall design we are working on is described in DESIGN.md and the separate phases of development are described in IMPLEMENTATION_PLAN.md. This document always tracks the development of one phase.

**How to use this document:**
- Each task block represents a reviewable unit of work
- **IMPORTANT: Stop for review after completing each task block before proceeding to the next**
- Mark tasks as [x] when completed
- Add notes about deviations or important decisions under each section
- When resuming work, scan for the first uncomplected task

**General Instructions**
- Read DESIGN.md and IMPLEMENTATION_PLAN.md for context

**Code Style**
- Organise functions in modules with entry points at the top and helper/leaf functions
  at the bottom. This makes it easy to understand the module by reading top-to-bottom.

**Technical Decisions**
- Using `autosurgeon` crate for mapping Rust structs to Automerge documents
  - Provides `Reconcile` (write) and `Hydrate` (read) derive macros
  - Use `#[autosurgeon(rename = "...")]` for field renaming (e.g., `@patchwork`, `mimeType`)
  - With samod: use `doc.transact(|txn| reconcile(txn, &value))` since samod's
    `with_document` provides `&mut Automerge`, but `reconcile` needs `Transactable`
  - For reading: `hydrate(&doc)` works directly on `Automerge` (no transaction needed)

**Pushwork Schema Compatibility Notes** (discovered during Phase 2):
- Metadata fields (`@patchwork.type`, `name`, `extension`, `mimeType`, directory entry
  fields) must be `autosurgeon::Text` (collaborative text objects)
- File `content` must be `String` (scalar string = ImmutableString in pushwork)
- `permissions` is stored as `i64` (JavaScript number -> Automerge Int)
- `lastSyncAt` may be absent, use `#[autosurgeon(missing = "Default::default")]`
- Snapshot heads are base58check encoded (using `bs58` crate with check feature)

---

## Phase 10: Handle Subdirectories (Parallel Sync Architecture)

**Goal**: Support nested directory structures with efficient parallel syncing.

**Deliverable**: Sync works with files in subdirectories, processing directories
and files in parallel as they become available.

**Architecture Overview**:
Instead of gathering all changes upfront and then processing them, we use a
streaming/parallel approach with a work queue (`FuturesUnordered`). This allows:
- Parallel fetching and syncing of documents
- Incremental discovery of nested directories
- Efficient handling of deep hierarchies
- Natural handling of new remote directories

```
SyncTask enum:
- SyncDirectory { path, handle }     // Compare local vs remote, spawn child tasks
- SyncFile { path, handle, entry }   // Compare content, push/pull/merge
- FetchDirectory { path, url }       // Fetch new remote directory
- FetchFile { path, url }            // Fetch new remote file
- CreateDirectory { path, entries }  // Create new local directory doc
```

---

### Task 10.1: Define SyncTask Enum and Result Types

Create the core types for the parallel sync system.

- [x] Define `SyncTask` enum with variants for different sync operations
- [x] Define `SyncResult` to report what happened (for summary)
- [x] Define `SyncContext` struct to hold shared state (repo, conn_id, snapshot, root path)

**Done:** Created `src/sync_tasks.rs` with SyncTask, SyncResult, SyncContext, and SyncSummary types.

**Notes:**
```rust
enum SyncTask {
    /// Sync a directory: load doc, compare entries, spawn child tasks
    SyncDirectory {
        relative_path: PathBuf,  // "" for root
        url: AutomergeUrl,
    },
    /// Sync a tracked file: compare local/remote, push/pull/merge
    SyncFile {
        relative_path: PathBuf,
        url: AutomergeUrl,
        snapshot_heads: Vec<ChangeHash>,
    },
    /// Fetch a new remote file (not in snapshot)
    FetchNewFile {
        relative_path: PathBuf,
        url: AutomergeUrl,
    },
    /// Push a new local file (not in remote)
    PushNewFile {
        relative_path: PathBuf,
        absolute_path: PathBuf,
    },
    /// Push a new local directory
    PushNewDirectory {
        relative_path: PathBuf,
        absolute_path: PathBuf,
    },
}

enum SyncResult {
    Pushed { path: String },
    Pulled { path: String },
    Merged { path: String },
    Created { path: String },
    NoChange,
    Error { path: String, message: String },
}
```

---

### Task 10.2: Create Directory Document Function

Add ability to create directory documents (for new local subdirectories).

- [x] Add `create_directory_document()` to sync_ops.rs
- [x] Takes directory name, returns handle and URL
- [x] Creates empty directory doc with proper schema

**Done:** Added `create_directory_document()` and `add_folder_to_directory()` to sync_ops.rs with tests.

---

### Task 10.3: Implement Process Single Task

Create function to process one SyncTask, returning results and new tasks.

- [x] `process_task(task, context) -> (Vec<SyncResult>, Vec<SyncTask>)`
- [x] Handle `SyncDirectory`: load doc, compare with local filesystem and snapshot
- [x] Handle `SyncFile`: existing logic for push/pull/merge
- [x] Handle `FetchNewFile`: pull file not in snapshot
- [x] Handle `PushNewFile`: create doc, push to server
- [x] Handle `PushNewDirectory`: create dir doc, push, return tasks for contents

**Done:** Implemented all task processing functions in sync_tasks.rs.

---

### Task 10.4: Implement Directory Sync Logic

The core logic for `SyncDirectory` task - comparing local and remote state.

- [x] Load directory document, wait for sync (`we_have_their_changes`)
- [x] List local filesystem entries in that directory
- [x] Compare remote entries (from doc) vs local entries vs snapshot
- [x] Spawn appropriate tasks:
  - Remote-only file → `FetchNewFile`
  - Local-only file → `PushNewFile`
  - Both exist → `SyncFile`
  - Remote-only dir → `SyncDirectory` (will fetch)
  - Local-only dir → `PushNewDirectory`
  - Both exist dir → `SyncDirectory`

**Done:** Implemented `process_sync_directory` with full comparison logic.

---

### Task 10.5: Implement Parallel Task Runner

Create the main loop that processes tasks in parallel.

- [x] Use `FuturesUnordered` to run tasks concurrently
- [x] Start with `SyncDirectory` for root
- [x] As tasks complete, add new spawned tasks to the queue
- [x] Collect all results for summary
- [x] Continue until queue is empty

**Done:** Implemented `run_sync()` using `FuturesUnordered` pattern.

---

### Task 10.6: Update Snapshot Incrementally

Modify snapshot to be updated as tasks complete, not at the end.

- [x] Add methods to update snapshot entries during sync
- [x] Track both file and directory entries in snapshot
- [x] Ensure snapshot is saved even if sync is interrupted
- [x] Handle concurrent updates safely (or use single-threaded update)

**Done:** Snapshot is wrapped in `Arc<Mutex<>>` and updated incrementally in each task processor.

---

### Task 10.7: Refactor Sync Command to Use New Architecture

Replace existing sync logic with the parallel task-based approach.

- [x] Remove old `detect_modified_files`, `detect_remote_changes` calls
- [x] Create `SyncContext` with repo, conn_id, snapshot, config
- [x] Call `run_sync()` to process everything
- [x] Update snapshot with results
- [x] Print summary from collected results

**Done:** sync.rs now uses the new task-based architecture.

---

### Task 10.8: Update Clone to Use Task Architecture

Refactor clone to use the same parallel approach.

- [x] Clone becomes: connect, then `SyncDirectory` on root (fetch-only mode)
- [x] Or: keep clone simple, just recursive fetch without comparison
- [x] Ensure clone creates local directories as needed

**Done:** clone.rs uses `run_clone()` with parallel directory fetching.

---

### Task 10.9: Verification Test

Test the full subdirectory flow with parallel sync.

- [x] Create nested directory structure, init, sync
- [x] Clone and verify structure is preserved
- [x] Add file in subdirectory on client A, sync, pull on client B
- [x] Add new subdirectory on client A, sync, pull on client B
- [x] Verify deep nesting works (3+ levels)

**Done:** All verification tests passed:
- Created `src/utils/helpers/helper.rs` (3 levels deep) ✓
- Cloned and verified content ✓
- Added `src/models/user.rs`, synced, and pulled on client B ✓

---

### Phase 10 Completion Checklist

- [x] SyncTask enum and result types defined
- [x] Directory document creation working
- [x] Single task processing implemented
- [x] Directory sync logic compares local/remote/snapshot
- [x] Parallel task runner with FuturesUnordered
- [x] Snapshot updated incrementally
- [x] Sync command refactored to new architecture
- [x] Clone updated (or verified working)
- [x] Nested directory structure verified end-to-end

**Phase 10 complete. Proceed to Phase 11.**

---

