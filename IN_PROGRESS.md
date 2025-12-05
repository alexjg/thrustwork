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

**Autosurgeon Key Attribute** (discovered during Phase 14):
- When reconciling `Vec<T>` where items have identity (like directory entries),
  use `#[key]` on the identifying field
- Without `#[key]`, autosurgeon does position-based reconciliation which causes
  CRDT merge issues with concurrent modifications
- With `#[key]`, autosurgeon tracks items by identity, enabling correct merges

---

## Phase 15: Remaining CLI Commands

**Goal**: Complete the CLI interface with status reporting and improved UX.

**Status**: In Progress

**Deliverable**: `status` and `url` commands working, `--verbose` flag for detailed output.

### Task 1: Add `url` Command
- [ ] Add `Url` variant to `Commands` enum in `main.rs`
- [ ] Implement handler that:
  - Loads config from `.pushwork/config.json`
  - Prints the `rootDirectoryUrl` if present
  - Prints helpful error if not initialized
- [ ] Add integration test `test_url_command`

**Stop for review after completing Task 1**

### Task 2: Add `status` Command
- [ ] Add `Status` variant to `Commands` enum
- [ ] Create `src/status.rs` module with `execute()` function
- [ ] Reuse `scan_directory_for_changes()` from `sync_tasks.rs` to collect changes
- [ ] Print summary without applying changes:
  - Files to push (new local files)
  - Files to pull (new remote files)
  - Files modified locally
  - Files modified remotely
  - Files deleted locally
  - Files deleted remotely
  - Detected moves
- [ ] Add integration test `test_status_shows_pending_changes`

**Stop for review after completing Task 2**

### Task 3: Add `--verbose` Flag
- [ ] Add `#[arg(long, short, global = true)] verbose: bool` to `Cli` struct
- [ ] Pass verbose flag through to sync/status operations
- [ ] When verbose:
  - Print each file being processed during sync
  - Print similarity scores for move detection
  - Print document URLs for synced files
- [ ] Update existing print statements to respect verbose flag (quiet by default)

**Stop for review after completing Task 3**

### Task 4: Improve Error Messages
- [ ] Review all `eprintln!` and `expect()` calls
- [ ] Add context to error messages (file paths, URLs, operation being performed)
- [ ] Ensure non-zero exit codes for all error conditions
- [ ] Add `--help` examples for each command

**Stop for review after completing Task 4**

### Task 5: Clean Up Development Commands
- [ ] Consider removing or hiding `create-test` and `read-dir` commands
  - These were for development/debugging
  - Could move behind a `--dev` flag or remove entirely
- [ ] Update `--help` output to be user-friendly

**Verification**:
- `thrustwork url` prints the root URL
- `thrustwork status` shows pending changes without syncing
- `thrustwork sync --verbose` shows detailed output
- All existing tests still pass

---

