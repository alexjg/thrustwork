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

### Task 11.1: Detect Local File Deletion

Extend `process_sync_directory` to detect files that exist in snapshot but not on disk.

- [ ] In `process_sync_directory`, after gathering local/remote/snapshot sets:
  - Find files in snapshot that are NOT in local filesystem
  - These are candidates for local deletion
- [ ] For each locally-deleted file:
  - If file also exists in remote → it was deleted locally, needs remote removal
  - If file doesn't exist in remote → already deleted remotely, just clean snapshot
- [ ] Add `SyncResult::Deleted { path: String }` variant for reporting

**Notes**: Be careful not to treat "new remote file" as "locally deleted". A file
in remote but not in snapshot is new, not deleted.

---

### Task 11.2: Remove Entry from Remote Directory

Implement removing a file entry from a directory document.

- [ ] Add `remove_file_from_directory(handle, filename)` to sync_ops.rs
- [ ] Remove the entry from the directory's `files` map
- [ ] Wait for sync confirmation (`they_have_our_changes`)
- [ ] Add test for this function

---

### Task 11.3: Handle Local File Deletion in Sync

Wire up local deletion detection to actually remove from remote.

- [ ] When local deletion detected, call `remove_file_from_directory`
- [ ] Remove the file from snapshot
- [ ] Report `SyncResult::Deleted`
- [ ] Handle the case where remote file was also modified (conflict - remote wins?)

**Decision (from pushwork)**: Remote modification wins - restore file locally with remote content.
Rationale: In CRDT systems, modifications win over deletions to prevent data loss.
If someone edited the file, they want it to exist.

---

### Task 11.4: Detect Remote File Deletion

Extend `process_sync_directory` to detect files deleted on remote.

- [ ] Find files in snapshot that are NOT in remote directory document
- [ ] These were deleted remotely
- [ ] If file exists locally → delete it
- [ ] If file doesn't exist locally → just clean snapshot (already deleted both sides)

---

### Task 11.5: Delete Local File

Implement deleting a local file when remote deletion is detected.

- [ ] Delete the file from local filesystem
- [ ] Remove from snapshot
- [ ] Report `SyncResult::DeletedLocally { path }` or similar
- [ ] Handle errors gracefully (file already gone, permission denied)

---

### Task 11.6: Handle Directory Deletion

Extend deletion handling to directories.

- [ ] Detect directory in snapshot but not locally → local deletion
- [ ] Detect directory in snapshot but not in remote → remote deletion
- [ ] For local deletion of directory:
  - Remove folder entry from parent directory document
  - The directory document itself can remain (orphaned but harmless)
- [ ] For remote deletion of directory:
  - Delete local directory (must be empty or recursive delete?)
  - Remove from snapshot
- [ ] Add `remove_folder_from_directory(handle, foldername)` to sync_ops.rs

**Decision (from pushwork)**: Yes, recursively delete non-empty directories.
Pushwork uses `fs.rm(path, { recursive: true })` - no empty-only requirement.
We'll use `std::fs::remove_dir_all()` in Rust.

---

### Task 11.7: Update Snapshot Cleanup

Ensure snapshot stays consistent after deletions.

- [ ] Remove deleted files from snapshot immediately after deletion confirmed
- [ ] Remove deleted directories from snapshot
- [ ] Handle nested deletions (directory deleted → children implicitly gone)
- [ ] Verify snapshot is saved even if sync is interrupted mid-deletion

---

### Task 11.8: Verification Tests

Test deletion flows end-to-end.

- [ ] Local file deletion: sync file, delete locally, sync, verify remote removal
- [ ] Remote file deletion: sync file, delete from another client, sync, verify local removal
- [ ] Local directory deletion: sync directory with files, delete locally, sync
- [ ] Remote directory deletion: sync directory, delete from another client, sync
- [ ] Edge case: delete file that was never synced (should just disappear)
- [ ] Edge case: delete file that's being modified remotely (conflict handling)

---

### Phase 11 Completion Checklist

- [ ] Local file deletion detected and synced
- [ ] Remote file deletion detected and applied
- [ ] Directory deletion works in both directions
- [ ] Snapshot updated correctly after deletions
- [ ] Conflict case handled (delete vs modify)
- [ ] All verification tests pass

**Phase 11 complete. Proceed to Phase 13 (Move Detection).**

---

