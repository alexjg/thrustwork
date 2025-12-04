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

**Phase 6 Complete.**

---

## Phase 7: Binary File Support

**Goal**: Support syncing binary files (images, PDFs, etc.) in addition to text.

**Deliverable**: Binary files can be pushed, pulled, and synced like text files.

---

### Task 7.1: Create Binary FileDocument Type

Add a new document type for binary files using Automerge Bytes.

- [x] Add `BinaryFileDocument` struct using `autosurgeon::ByteVec` for content
- [x] Or modify `FileDocument` to use an enum for content (text vs binary)
- [x] Add constructor for binary files
- [x] Add tests for binary document serialization roundtrip

**Notes:**
- Pushwork uses the same schema but with `content` as Bytes instead of String
- Use an enum `FileContent { Text(String), Binary(Vec<u8>) }` with custom Reconcile/Hydrate
- Reconcile: write String or Bytes depending on variant
- Hydrate: inspect Automerge value type and construct appropriate variant
- This keeps FileDocument as a single struct, avoids field duplication

**Implementation:** Added `FileContent` enum with custom `Reconcile` and `Hydrate` implementations
in `src/documents.rs`. The enum switches between String and Bytes based on the variant.
`FileDocument` now uses `FileContent` for its content field.

---

### Task 7.2: Add Binary File Reading

**SKIPPED** - This is just a trivial wrapper around `std::fs::read()`. We'll use
`std::fs::read()` directly where needed.

---

### Task 7.3: Create Binary File Documents

Update sync_ops to create documents for binary files.

- [x] Modify `create_file_document()` to handle both text and binary
- [x] Use `FileInfo.is_text` to choose the right content type
- [x] Create binary document with ByteVec content
- [x] Add tests for creating binary file documents

**Notes:**
- Remove the "binary files not yet supported" error
- The function should work for any file type

**Implementation:** Updated `create_file_document()` in `sync_ops.rs` to:
- Read all files as bytes using `std::fs::read()`
- Use `FileDocument::new()` for text files (converting bytes to String)
- Use `FileDocument::new_binary()` for binary files (raw bytes)
- Removed `SyncError::NotTextFile` variant - no longer needed
- Updated test to verify binary document creation works correctly

---

### Task 7.4: Update Binary File Documents

Support updating existing binary file documents.

- [ ] Modify `update_file_document()` to handle binary content
- [ ] Read binary content from disk when updating
- [ ] Add tests for updating binary documents

**Notes:**
- Similar to text update but with raw bytes
- Need to handle mixed scenarios (was text, now binary?)

---

### Task 7.5: Clone Binary Files

Support cloning/pulling binary files from remote.

- [ ] Update clone module to handle binary file documents
- [ ] Write binary content to disk (not as UTF-8 string)
- [ ] Ensure file permissions are set correctly
- [ ] Add tests for cloning binary files

**Notes:**
- Currently clone writes with `fs::write()` which works for both text and binary
- Need to extract bytes from ByteVec content

---

### Task 7.6: Wire Up Binary Support in Sync

Remove binary file skipping and enable full binary support.

- [ ] Remove "Skipping (binary)" logic from sync module
- [ ] Update change detection to work with binary files
- [ ] Ensure snapshot tracks binary files correctly
- [ ] Test end-to-end binary file sync

**Notes:**
- The `FileInfo.is_text` field is still useful for choosing content type
- But we no longer skip binary files

---

### Task 7.7: Interop Verification

Verify binary files sync correctly with pushwork.

- [ ] Create a directory with pushwork containing an image
- [ ] Clone with thrustwork, verify image is byte-for-byte identical
- [ ] Modify image with thrustwork, sync
- [ ] Verify pushwork sees the updated image

**Verification:**
```
# Setup: Create with pushwork including a binary file
$ mkdir /tmp/pushwork-binary && cd /tmp/pushwork-binary
$ pushwork init
$ cp /path/to/test.png .
$ pushwork sync

# Clone with thrustwork
$ mkdir /tmp/thrustwork-binary && cd /tmp/thrustwork-binary
$ thrustwork clone <url>
$ diff test.png /tmp/pushwork-binary/test.png  # Should be identical

# Modify and sync back
$ convert test.png -resize 50% test.png  # or any image edit
$ thrustwork sync

# Verify with pushwork
$ cd /tmp/pushwork-binary
$ pushwork sync
$ diff test.png /tmp/thrustwork-binary/test.png  # Should match
```

---

### Phase 7 Completion Checklist

- [ ] Binary file documents can be created with ByteVec content
- [ ] Binary files can be read from disk
- [ ] Binary files can be pushed (create and update)
- [ ] Binary files can be cloned/pulled
- [ ] No more "binary not supported" skipping
- [ ] Binary files sync correctly with pushwork

**Phase 7 complete when all items checked. Proceed to Phase 8.**

---

