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

- [ ] Add function to read current content from a document (not at specific heads)
- [ ] Return bytes (works for both text and binary)
- [ ] Reuse existing hydration logic from FileDocument

**Notes:**
- Similar to `get_file_content_at_heads()` but uses current state
- May be able to reuse `content_bytes()` method directly

---

### Task 8.3: Write Remote Changes to Disk

Apply remote file changes to the local filesystem.

- [ ] Write new content to the file path from snapshot
- [ ] Preserve or update file permissions from document
- [ ] Handle errors (permission denied, disk full, etc.)

**Notes:**
- Use `std::fs::write()` which works for both text and binary
- May need to handle file that was deleted locally but exists remotely

---

### Task 8.4: Integrate Remote Changes into Sync

Wire up remote change detection and application in the sync command.

- [ ] After pushing local changes, detect remote changes
- [ ] For each remote change, read content and write to disk
- [ ] Update snapshot with new heads after applying
- [ ] Print summary of pulled files

**Notes:**
- This completes the two-way sync: push local, then pull remote
- Order matters: push first, then pull (to avoid overwriting local changes)

---

### Task 8.5: Handle BOTH_CHANGED Scenario

Detect and handle files changed both locally and remotely.

- [ ] Detect when a file has both local and remote changes
- [ ] For now: prefer remote (pull overwrites local)
- [ ] Print warning when this happens
- [ ] Future: merge text files, prefer remote for binary

**Notes:**
- This is a conflict scenario
- Phase 14 will implement proper merging
- For now, simple "remote wins" policy is acceptable

---

### Task 8.6: Verification Test

Test the full remote change flow.

- [ ] Set up two directories syncing the same root
- [ ] Modify a file in directory A, sync
- [ ] Sync in directory B, verify file is updated
- [ ] Test with both text and binary files

**Verification:**
```
# Terminal 1: Create and sync
mkdir /tmp/client-a && cd /tmp/client-a
thrustwork init
echo "original" > test.txt
thrustwork sync
# Note the root URL

# Terminal 2: Clone
mkdir /tmp/client-b && cd /tmp/client-b
thrustwork clone <url>
cat test.txt  # Should show "original"

# Terminal 1: Modify and sync
echo "modified by A" > test.txt
thrustwork sync

# Terminal 2: Pull changes
thrustwork sync
cat test.txt  # Should show "modified by A"
```

---

### Phase 8 Completion Checklist

- [ ] Remote changes detected by comparing document heads to snapshot
- [ ] Remote file content can be read from documents
- [ ] Remote changes are written to local filesystem
- [ ] Sync command pulls remote changes after pushing local
- [ ] BOTH_CHANGED scenario handled (remote wins for now)
- [ ] Two-client sync verified working

**Phase 8 complete when all items checked. Proceed to Phase 9.**

---

