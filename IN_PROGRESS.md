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

---

## Phase 3: Initialize a Directory

**Goal**: Implement the `init` command to set up a directory for syncing.

**Deliverable**: `thrustwork init` creates the `.pushwork` folder, config file,
and root directory document.

---

### Task 3.1: Add clap for CLI Parsing

Set up proper CLI argument parsing with clap, replacing the current ad-hoc args handling.

- [x] Add `clap` dependency with derive feature
- [x] Define a `Cli` struct with subcommands enum
- [x] Add `Init` subcommand (no arguments for now)
- [x] Keep existing `create-test` and `read-dir` as subcommands for testing
- [x] Update `main()` to use clap parsing

**Notes:**
- Used `#[derive(Parser)]` for the main struct
- Used `#[derive(Subcommand)]` for the commands enum
- Commands: `init`, `create-test`, `read-dir <url>`

---

### Task 3.2: Define Config Structure

Create the configuration types that will be saved to `.pushwork/config.json`.

- [x] Create a `config` module
- [x] Define `DirectoryConfig` struct with serde derives:
  - `root_directory_url: Option<String>` (set after creating root doc)
  - `sync_server: Option<String>` (default: "wss://sync3.automerge.org")
  - `sync_server_storage_id: Option<String>` (matches pushwork)
  - `sync_enabled: bool` (default: true)
  - `exclude_patterns: Vec<String>` (default: common patterns)
  - `sync: SyncConfig` (move_detection_threshold)
- [x] Implement `Default` for `DirectoryConfig`
- [x] Add functions to load/save config from/to JSON file
- [x] Add `ConfigError` enum for error handling

**Notes:**
- Matches pushwork's DirectoryConfig structure for compatibility
- Default exclude patterns: `.git`, `node_modules`, `*.tmp`, `.DS_Store`, `.pushwork`
- Uses `skip_serializing_if = "Option::is_none"` for optional fields

---

### Task 3.3: Create .pushwork Directory Structure

Implement the directory initialization logic.

- [ ] Create a function `init_directory(path: &Path) -> Result<()>`
- [ ] Create `.pushwork/` directory
- [ ] Create `.pushwork/automerge/` for repo storage
- [ ] Handle error if `.pushwork` already exists (with --force option later)
- [ ] Write initial config file (without root URL yet)

**Notes:**
- Use `std::fs` for directory/file operations
- Return descriptive errors for common failure cases

---

### Task 3.4: Create Root Directory Document

Create the root directory document and update config with its URL.

- [ ] Initialize samod Repo with filesystem storage in `.pushwork/automerge/`
- [ ] Connect to sync server
- [ ] Create empty `DirectoryDocument`
- [ ] Reconcile to a new Automerge document
- [ ] Create document in repo and get URL
- [ ] Update config file with `root_directory_url`
- [ ] Wait for sync to complete

**Notes:**
- Use `samod::Repo::build_tokio().fs_storage(path).load().await`
- The root directory should have empty `docs` array initially

---

### Task 3.5: Implement init Subcommand

Wire up the init command to use the initialization logic.

- [ ] Implement `init` command handler
- [ ] Get current working directory (or accept path argument)
- [ ] Check if already initialized (`.pushwork` exists)
- [ ] Call initialization functions
- [ ] Print success message with root URL

**Notes:**
- Print helpful error if already initialized
- Show the URL so user can share it

---

### Task 3.6: Interop Verification

Verify the initialized directory works with pushwork.

- [ ] Run `thrustwork init` in a test directory
- [ ] Verify `.pushwork/config.json` exists and contains valid URL
- [ ] Run `pushwork clone <url>` in another directory
- [ ] Verify pushwork sees an empty directory
- [ ] Optionally: run `pushwork status` to verify it recognizes the directory

**Verification:**
```
$ mkdir /tmp/test-init && cd /tmp/test-init
$ thrustwork init
Initialized thrustwork directory
Root URL: automerge:xxx

$ mkdir /tmp/test-clone && cd /tmp/test-clone
$ pushwork clone automerge:xxx
$ ls -la
(should show empty directory with .pushwork)
```

**Notes:**
- This is a manual integration test
- Record any compatibility issues discovered

---

### Phase 3 Completion Checklist

- [ ] clap CLI parsing implemented
- [ ] Config structure defined and can be saved/loaded
- [ ] `.pushwork/` directory structure created correctly
- [ ] Root directory document created and synced
- [ ] `thrustwork init` command works end-to-end
- [ ] Pushwork can clone the initialized directory

**Phase 3 complete. Proceed to Phase 4 in IMPLEMENTATION_PLAN.md.**

---
