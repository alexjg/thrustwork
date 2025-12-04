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

## Phase 8: Detect and Apply Remote Changes

**Goal**: Detect when a remote file has changed and pull the update.

**Deliverable**: When a file changes remotely, sync pulls it.

---

### Task 8.1: Detect Remote Changes

Compare document heads against snapshot heads to find files changed remotely.

- [x] Add function to compare current document heads with snapshot heads
- [x] Return list of files where document has newer heads than snapshot
- [x] Handle case where document heads are the same (no change)
- [x] Handle case where document heads are different (remote change)

**Notes:**
- A file has remote changes if `doc.get_heads() != snapshot_entry.head`
- This is the inverse of local change detection (which compares disk content)
- Need to load each tracked document and check its heads

**Implementation:** Added `RemotelyChangedFile` struct and `detect_remote_changes()`
function in `changes.rs`. The function iterates through snapshot files, loads each
document, and compares heads. Returns files where heads differ.

---

### Task 8.2: Read Remote File Content

Extract current content from a remotely-changed document.

- [x] Add function to read current content from a document (not at specific heads)
- [x] Return bytes (works for both text and binary)
- [x] Reuse existing hydration logic from FileDocument

**Notes:**
- Similar to `get_file_content_at_heads()` but uses current state
- May be able to reuse `content_bytes()` method directly

**Implementation:** Initially added `get_file_content()` and `get_file_doc_permissions()`
helper functions in `sync_ops.rs`. These were later removed and inlined into
`write_remote_file_to_disk()` to avoid double-hydration overhead.

---

### Task 8.3: Write Remote Changes to Disk

Apply remote file changes to the local filesystem.

- [x] Write new content to the file path from snapshot
- [x] Preserve or update file permissions from document
- [x] Handle errors (permission denied, disk full, etc.)

**Notes:**
- Use `std::fs::write()` which works for both text and binary
- May need to handle file that was deleted locally but exists remotely

**Implementation:** Added `write_remote_file_to_disk()` function in `sync_ops.rs`. The
function reads content from the document, writes it to the specified path, and sets Unix
permissions. Uses the existing `get_file_content()` and `get_file_doc_permissions()`
helpers. Error handling uses the existing `SyncError` type.

---

### Task 8.4: Integrate Remote Changes into Sync

Wire up remote change detection and application in the sync command.

- [x] After pushing local changes, detect remote changes
- [x] For each remote change, read content and write to disk
- [x] Update snapshot with new heads after applying
- [x] Print summary of pulled files

**Notes:**
- This completes the two-way sync: push local, then pull remote
- Order matters: push first, then pull (to avoid overwriting local changes)

**Implementation:** Restructured `sync.rs` execute function to:
1. Process local changes (new + modified) and push to server
2. After push, call `detect_remote_changes()` to find files with different heads
3. Call `process_remote_changes()` to write content to disk and update snapshot
4. Updated `print_summary()` to show both pushed and pulled counts

---

### Task 8.5: Handle BOTH_CHANGED Scenario

Detect and handle files changed both locally and remotely.

- [x] Detect when a file has both local and remote changes
- [x] Use CRDT merge to combine local and remote changes
- [x] Write merged result to disk
- [x] Print summary showing merged file count

**Notes:**
- Automerge is a CRDT, so we can merge concurrent changes
- For scalar strings (file content), last-writer-wins semantics apply
- Future phases may implement smarter text merging

**Implementation:**
1. Added `merge_local_into_remote()` in `sync_ops.rs` that:
   - Forks document at snapshot heads (common ancestor)
   - Applies local changes to the fork
   - Merges fork back into main document (which has remote changes)
   - CRDT automatically resolves conflicts
2. Modified sync flow in `sync.rs`:
   - Detect remote changes BEFORE pushing
   - Find conflicts (files in both local modified and remote changed)
   - For conflicts: call `merge_local_into_remote()`, write merged result to disk
   - Exclude merged files from normal pull (already handled)
3. Updated `print_summary()` to show merged file count

---

### Task 8.6: Verification Test

Test the full remote change flow.

- [x] Set up two directories syncing the same root
- [x] Modify a file in directory A, sync
- [x] Sync in directory B, verify file is updated
- [x] Test CRDT merge when both clients modify same file
- [x] Test with binary files

**Verification Results:**
1. Push new file (client-a): PASSED
2. Clone pulls files (client-b): PASSED
3. Push modified file (client-a): PASSED
4. Pull remote change (client-b): PASSED - Added `preload_tracked_documents()`
   to wait for sync before checking for remote changes
5. CRDT merge conflict (both changed): PASSED - Both local and remote changes
   merged correctly using fork-and-merge approach
6. Binary file push (client-a): PASSED

**Note:** Detecting new remote files (not in snapshot) requires checking the
directory document - this is a future enhancement for directory sync.

---

### Phase 8 Completion Checklist

- [x] Remote changes detected by comparing document heads to snapshot
- [x] Remote file content can be read from documents
- [x] Remote changes are written to local filesystem
- [x] Sync command pulls remote changes after pushing local
- [x] BOTH_CHANGED scenario handled with CRDT merge
- [x] Two-client sync verified working

**Phase 8 complete. Proceed to Phase 9.**

---

