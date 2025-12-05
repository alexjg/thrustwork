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

## Phase 11: Handle File Deletion

**Goal**: Sync file deletions in both directions.

**Deliverable**: Delete a file locally, sync removes it remotely (and vice versa).

**Architecture Context**:
The parallel sync architecture in `process_sync_directory` already compares:
- Local filesystem entries
- Remote directory document entries
- Snapshot entries

Deletion detection fits naturally: files in snapshot but missing from local or remote
indicate deletion. The key challenge is distinguishing "deleted" from "not yet synced".

---

### Task 11.1: Detect Local File Deletion ✓

Extend `process_sync_directory` to detect files that exist in snapshot but not on disk.

- [x] In `process_sync_directory`, after gathering local/remote/snapshot sets:
  - Find files in snapshot that are NOT in local filesystem
  - These are candidates for local deletion
- [x] For each locally-deleted file:
  - If file also exists in remote → it was deleted locally, needs remote removal
  - If file doesn't exist in remote → already deleted remotely, just clean snapshot
- [x] Add `SyncResult::DeletedRemote`, `DeletedLocal`, `Restored` variants for reporting

**Done**: Added deletion detection in `process_sync_directory`. Gets snapshot files for
the current directory and compares against local/remote sets. Spawns appropriate tasks:
- `DeleteRemoteFile` for locally-deleted files still in remote
- `DeleteLocalFile` for remotely-deleted files still locally

---

### Task 11.2: Remove Entry from Remote Directory ✓

Implement removing a file entry from a directory document.

- [x] Add `remove_entry_from_directory(handle, name)` to sync_ops.rs
- [x] Remove the entry from the directory's `docs` array by name
- [x] Reconcile changes back to Automerge

**Done**: Added `remove_entry_from_directory()` function. Also, entry removal is now
handled directly in `process_sync_directory` via the `entries_to_remove` list.

---

### Task 11.3: Handle Local File Deletion in Sync ✓

Wire up local deletion detection to actually remove from remote.

- [x] When local deletion detected, spawn `DeleteRemoteFile` task
- [x] `process_delete_remote_file` checks for remote modifications
- [x] If remote was modified, restore file locally (remote wins)
- [x] If no remote changes, remove from directory doc and snapshot
- [x] Report appropriate `SyncResult` variant

**Decision (from pushwork)**: Remote modification wins - restore file locally with remote content.
Rationale: In CRDT systems, modifications win over deletions to prevent data loss.
If someone edited the file, they want it to exist.

**Done**: `process_delete_remote_file` compares current document heads with snapshot heads.
If different, writes remote content to disk (restore). If same, removes entry from directory
document via `entries_to_remove`.

---

### Task 11.4: Detect Remote File Deletion ✓

Extend `process_sync_directory` to detect files deleted on remote.

- [x] Find files in snapshot that are NOT in remote directory document
- [x] These were deleted remotely
- [x] If file exists locally → spawn `DeleteLocalFile` task
- [x] If file doesn't exist locally → just clean snapshot (already deleted both sides)

**Done**: Detection added in `process_sync_directory`. Files in snapshot but not in remote
and existing locally trigger `DeleteLocalFile` task.

---

### Task 11.5: Delete Local File ✓

Implement deleting a local file when remote deletion is detected.

- [x] Delete the file from local filesystem using `std::fs::remove_file`
- [x] Remove from snapshot
- [x] Report `SyncResult::DeletedLocal { path }`
- [x] Handle errors gracefully (file already gone → NoChange, permission denied → Error)

**Done**: `process_delete_local_file` handles all cases including file already deleted.

---

### Task 11.6: Handle Directory Deletion ✓

Extend deletion handling to directories.

- [x] Detect directory in snapshot but not locally → local deletion
- [x] Detect directory in snapshot but not in remote → remote deletion
- [x] For local deletion of directory:
  - Remove folder entry from parent directory document (via `entries_to_remove`)
  - The directory document itself can remain (orphaned but harmless)
- [x] For remote deletion of directory:
  - Delete local directory recursively with `std::fs::remove_dir_all()`
  - Remove from snapshot (including nested files/dirs)
- [x] Directory entry removal handled by existing `entries_to_remove` mechanism

**Decision (from pushwork)**: Yes, recursively delete non-empty directories.
Pushwork uses `fs.rm(path, { recursive: true })` - no empty-only requirement.
We use `std::fs::remove_dir_all()` in Rust.

**Done**: Added `DeleteRemoteDirectory` and `DeleteLocalDirectory` task variants.
Detection logic added in `process_sync_directory` parallel to file deletion detection.
`process_delete_local_directory` recursively deletes and cleans up nested entries from snapshot.

---

### Task 11.7: Verification Tests ✓

Test deletion flows end-to-end.

- [x] Local file deletion: sync file, delete locally, sync, verify remote removal
- [x] Remote file deletion: sync file, delete from another client, sync, verify local removal
- [x] Local directory deletion: sync directory with files, delete locally, sync
- [x] Remote directory deletion: sync directory, delete from another client, sync
- [x] Edge case: delete file that was never synced (should just disappear)
- [x] Edge case: delete file that's being modified remotely (conflict handling - remote wins, file restored)

**Bugs Fixed During Testing**:
1. **Spurious file deletion**: Files pushed in same sync were being marked for deletion because
   they weren't yet in the remote directory document. Fixed by tracking `just_pushed_names` and
   excluding them from deletion detection.

2. **Remote deletion not propagating**: Files deleted on one client were being re-pushed by the
   other. Fixed by adding snapshot check in local-only entry processing - if a file exists locally
   and in snapshot but NOT in remote, it was deleted remotely (don't push it back).

3. **Directory re-fetch on local deletion**: When a directory was deleted locally, the sync was
   still spawning `SyncDirectory` task which re-fetched its contents. Fixed by checking for
   local deletion (in snapshot but not locally) before spawning `SyncDirectory`.

4. **Root directory cleanup spam**: The root directory entry ("") was matching the filter for
   subdirectories and triggering spurious "Cleaning up: / (deleted)" messages. Fixed by explicitly
   excluding empty path from directory deletion detection.

---

### Phase 11 Completion Checklist

- [x] Local file deletion detected and synced
- [x] Remote file deletion detected and applied
- [x] Directory deletion works in both directions
- [x] Snapshot updated correctly after deletions
- [x] Conflict case handled (delete vs modify - remote modification wins, file restored)
- [x] All verification tests pass

**Phase 11 complete. Proceed to Phase 13 (Move Detection).**

---

