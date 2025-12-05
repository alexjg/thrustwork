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

**Testing Philosophy**
- Prefer integration tests over unit tests or manual testing
- Integration tests verify actual sync behavior end-to-end with a real sync server
- Add new integration tests in `tests/integration/main.rs` using the `TestHarness`
- The harness provides `create_client()` to get isolated test directories with helper
  methods like `init()`, `sync()`, `clone()`, `write_file()`, `read_file()`, etc.

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

## Phase 13: Move Detection

**Goal**: Detect file moves/renames and preserve document identity.

**Deliverable**: Rename a file locally, sync updates the name rather than delete+create.

**Architecture Context**:
In `process_sync_directory`, we already detect both local deletions (in snapshot, not on disk)
and local additions (on disk, not in snapshot/remote). Move detection identifies when these
pairs represent the same file that was renamed. The key insight: a deleted file and a new file
with similar content should be treated as a move, not delete+create.

From pushwork: uses Sørensen–Dice coefficient with 70% similarity threshold.

---

### Task 13.1: Implement String Similarity

Add a function to compute Sørensen–Dice coefficient between two byte slices.

- [ ] Create `src/move_detector.rs` module
- [ ] Implement `sorensen_dice_similarity(a: &[u8], b: &[u8]) -> f64`
  - Returns 0.0 to 1.0 (1.0 = identical)
  - Use bigrams (2-character sequences) for comparison
  - Handle edge cases: empty strings, single characters
- [ ] Add unit tests for similarity function

**Algorithm**:
```
sorensen_dice(a, b) = 2 * |bigrams(a) ∩ bigrams(b)| / (|bigrams(a)| + |bigrams(b)|)
```

---

### Task 13.2: Detect Move Candidates in Sync

Modify `process_sync_directory` to identify potential moves before processing deletions.

- [ ] Before processing deletions and new files, collect:
  - `deleted_files`: Files in snapshot but not on disk (with their snapshot content/URL)
  - `new_local_files`: Files on disk but not in snapshot or remote (with their content)
- [ ] For each (deleted, new) pair, compute similarity
- [ ] If similarity ≥ threshold (0.7), mark as move candidate
- [ ] A file can only be in one move pair (highest similarity wins if ambiguous)

**Note**: Content comparison requires reading file content. For binary files, compare raw bytes.
For text files, compare the text content. Large files may need size-based pre-filtering.

---

### Task 13.3: Handle Move as Rename Operation

When a move is detected, update the directory entry and file document instead of delete+create.

- [ ] For detected moves, instead of spawning `DeleteRemoteFile` + `PushNewFile`:
  - Update the directory entry's `name` field to the new name
  - Update the file document's `name` field
  - Update the snapshot to reflect the new path (same URL)
- [ ] Add `SyncResult::Moved { old_path, new_path }` variant
- [ ] Update `SyncSummary` to track moves

---

### Task 13.4: Update File Document Name

Implement updating the `name` field in a file document for renames.

- [ ] Add `update_file_name(handle, new_name)` to sync_ops.rs
- [ ] Update the `name` field in the file document
- [ ] Also update `extension` if it changed
- [ ] Return new heads after the update

---

### Task 13.5: Handle Cross-Directory Moves

Extend move detection to handle files moved between directories.

- [ ] Move detection currently happens per-directory; cross-directory moves are
  harder because the deleted and new file are in different `process_sync_directory` calls
- [ ] Option A: Defer cross-directory moves to a future phase (simpler)
- [ ] Option B: Collect all deletions and additions at the top level, then match (complex)
- [ ] Document the chosen approach

**Decision needed**: Start with same-directory moves only (Option A), or implement full
cross-directory move detection?

---

### Task 13.6: Configuration for Move Threshold

Allow configuring the similarity threshold.

- [ ] Add `move_threshold` to config (default: 0.7)
- [ ] Pass threshold to move detection logic
- [ ] Allow disabling move detection with threshold of 0 or 1.0+

---

### Task 13.7: Integration Tests

Add tests for move detection scenarios.

- [ ] Test: rename file in place, sync, verify same document URL
- [ ] Test: move file to subdirectory (if cross-directory supported), verify behavior
- [ ] Test: rename with content changes, verify move still detected if similar enough
- [ ] Test: two files with different content renamed, verify treated as separate delete+create
- [ ] Test: threshold configuration affects detection

---

### Phase 13 Completion Checklist

- [ ] Same-directory renames detected and synced as moves
- [ ] Document URL preserved after rename
- [ ] Directory entry updated (not deleted+recreated)
- [ ] File document name field updated
- [ ] Snapshot updated with new path, same URL
- [ ] Integration tests pass
- [ ] Cross-directory move handling documented (even if deferred)

**Phase 13 complete when renames preserve document identity.**

---

