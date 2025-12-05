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

### Task 13.1: Implement String Similarity ✓

Add a function to compute Sørensen–Dice coefficient between two byte slices.

- [x] Create `src/move_detector.rs` module
- [x] Implement `sorensen_dice_similarity(a: &[u8], b: &[u8]) -> f64`
  - Returns 0.0 to 1.0 (1.0 = identical)
  - Use bigrams (2-character sequences) for comparison
  - Handle edge cases: empty strings, single characters
- [x] Add unit tests for similarity function

---

### Task 13.2: Detect Move Candidates in Sync ✓

Modify `process_sync_directory` to identify potential moves before processing deletions.

- [x] Before processing deletions and new files, collect:
  - `deleted_files`: Files in snapshot but not on disk (with their snapshot content/URL)
  - `new_local_files`: Files on disk but not in snapshot or remote (with their content)
- [x] For each (deleted, new) pair, compute similarity
- [x] If similarity ≥ threshold (0.7), mark as move candidate
- [x] A file can only be in one move pair (highest similarity wins if ambiguous)

---

### Task 13.3: Handle Move as Rename Operation ✓

When a move is detected, update the directory entry and file document instead of delete+create.

- [x] For detected moves, instead of spawning `DeleteRemoteFile` + `PushNewFile`:
  - Update the directory entry's `name` field to the new name
  - Update the file document's `name` field
  - Update the snapshot to reflect the new path (same URL)
- [x] Add `SyncResult::Moved { old_path, new_path }` variant
- [x] Update `SyncSummary` to track moves

---

### Task 13.4: Update File Document Name ✓

Implement updating the `name` field in a file document for renames.

- [x] Add `update_file_name(handle, new_name)` to sync_ops.rs
- [x] Update the `name` field in the file document
- [x] Also update `extension` if it changed
- [x] Return new heads after the update

---

### Task 13.5: Handle Cross-Directory Moves

Extend move detection to handle files moved between directories.

- [x] Move detection currently happens per-directory; cross-directory moves are
  harder because the deleted and new file are in different `process_sync_directory` calls
- [x] **Decision: Option A** - Defer cross-directory moves to a future phase

**Note**: Same-directory moves work. Cross-directory moves would require collecting all
deletions and additions at a higher level before matching - more complex.

---

### Task 13.6: Configuration for Move Threshold ✓

Allow configuring the similarity threshold.

- [x] Add `move_threshold` to config (default: 0.7)
- [x] Pass threshold to move detection logic
- [x] Allow disabling move detection with threshold of 0 or 1.0+

---

### Task 13.7: Integration Tests ✓

Add tests for move detection scenarios.

- [x] Test: rename file in place, sync, verify same document URL
- [x] Test: rename with content changes, verify move still detected if similar enough
- [x] Test: two files with different content renamed, verify treated as separate delete+create
- [x] Test: cross-client rename sync

---

### Phase 13 Completion Checklist

- [x] Same-directory renames detected and synced as moves
- [x] Document URL preserved after rename
- [x] Directory entry updated (not deleted+recreated)
- [x] File document name field updated
- [x] Snapshot updated with new path, same URL
- [x] Integration tests pass
- [x] Cross-directory move handling documented (deferred to future phase)

**Phase 13 complete - renames preserve document identity.**

---

