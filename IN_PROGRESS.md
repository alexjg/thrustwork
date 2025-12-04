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

## Phase 6: Detect and Sync Local Changes

**Goal**: Detect when a tracked file has changed and push the update.

**Deliverable**: Modify a synced file, run sync, change is pushed.

---

### Task 6.1: Load Document at Snapshot Heads

Implement reading the file content as it was at the last sync.

- [x] Add function to load a file document at specific heads
- [x] Use `Automerge::fork_at()` or similar to get document state at heads
- [x] Extract content from the historical document state
- [x] Add tests for loading at heads

**Notes:**
- Need to compare current local file against what was synced last
- The snapshot stores heads from the last sync
- This allows detecting if local file changed since last sync

---

### Task 6.2: Detect Local File Changes

Compare local file content against snapshot state.

- [x] For each file in snapshot, check if it still exists on disk
- [x] Read current file content from disk
- [x] Load document content at snapshot heads
- [x] Compare: if different, mark as LOCAL_ONLY change
- [x] Add `modified_files` detection to scanner

**Notes:**
- A file is modified if disk content differs from document-at-snapshot-heads
- This is Phase 6's focus; remote changes come in Phase 7

---

### Task 6.3: Update Existing File Document

Update a file document with new local content.

- [x] Load the existing file document by URL from snapshot
- [x] Update the `content` field with new file content
- [x] Update `metadata.permissions` if changed
- [x] Reconcile changes back to document
- [x] Add `update_file_document()` function to sync_ops

**Notes:**
- Don't create a new document - update the existing one
- This preserves the document URL and history

---

### Task 6.4: Update Snapshot After Push

Update snapshot with new document heads after pushing changes.

- [x] After updating document, get new heads
- [x] Update the file's entry in snapshot with new heads
- [x] Save snapshot to disk

**Notes:**
- The snapshot must reflect the new state after sync
- New heads indicate what version we last synced

---

### Task 6.5: Wire Up Modified File Sync

Integrate change detection into the sync command.

- [x] Load existing snapshot at start of sync
- [x] Run change detection (new files + modified files)
- [x] For new files: create documents (existing behavior)
- [x] For modified files: update documents (new behavior)
- [x] Wait for sync to complete
- [x] Update snapshot with all changes

**Notes:**
- Sync command now handles both new and modified files
- The flow: detect changes → push changes → wait for sync → update snapshot

---

### Task 6.6: Interop Verification

Verify local changes sync correctly with pushwork.

- [ ] Clone a pushwork directory with thrustwork
- [ ] Modify a file locally
- [ ] Run `thrustwork sync`
- [ ] Verify pushwork sees the updated content

**Verification:**
```
# Setup: Create with pushwork
$ mkdir /tmp/pushwork-source && cd /tmp/pushwork-source
$ pushwork init
$ echo "Original content" > test.txt
$ pushwork sync

# Clone with thrustwork
$ mkdir /tmp/thrustwork-clone && cd /tmp/thrustwork-clone
$ thrustwork clone <url>
$ cat test.txt  # "Original content"

# Modify and sync
$ echo "Modified by thrustwork" > test.txt
$ thrustwork sync

# Verify with pushwork
$ cd /tmp/pushwork-source
$ pushwork sync
$ cat test.txt  # Should show "Modified by thrustwork"
```

---

### Phase 6 Completion Checklist

- [ ] Can load document content at specific heads
- [ ] Can detect when local file differs from last sync
- [ ] Can update existing file document with new content
- [ ] Snapshot updated with new heads after push
- [ ] Sync command handles modified files
- [ ] Changes sync correctly with pushwork

**Phase 6 complete. Proceed to Phase 7 in IMPLEMENTATION_PLAN.md.**

---
