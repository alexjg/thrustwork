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

## Phase 4: Push a Single File

**Goal**: Sync a single local file to the remote.

**Deliverable**: After `init`, creating a file and running `thrustwork sync`
pushes it to the remote.

---

### Task 4.1: Add sync Subcommand Skeleton

Add the `sync` command to the CLI with basic structure.

- [x] Add `Sync` variant to `Commands` enum
- [x] Add basic handler that finds the `.pushwork` directory
- [x] Load the config to get the root directory URL
- [x] Connect to sync server and load the root directory document
- [x] Print basic status (e.g., "Syncing directory: <path>")

**Notes:**
- Added `PushworkPaths::find_from()` to walk up from cwd to find `.pushwork`
- Sync command loads config, connects to sync server, loads root directory document
- Uses `TokioFilesystemStorage` to persist documents locally
- 15 tests passing (including new `test_find_from`)

---

### Task 4.2: Define Snapshot Structure

Create the snapshot types for tracking sync state.

- [x] Create `snapshot` module
- [x] Define `Snapshot` struct with serde derives:
  - `timestamp: u64`
  - `root_path: PathBuf`
  - `root_directory_url: Option<String>`
  - `files: Vec<FileEntry>`
  - `directories: Vec<DirectoryEntry>`
- [x] Define `FileEntry` struct (path, url, head, extension, mime_type)
- [x] Define `DirectoryEntry` struct (path, url, head)
- [x] Add load/save functions for `.pushwork/snapshot.json`
- [x] Add tests for serialization roundtrip

**Notes:**
- Created `src/snapshot.rs` with `Snapshot`, `SnapshotFileEntry`, `SnapshotDirectoryEntry`
- Uses `#[serde(rename_all = "camelCase")]` for pushwork compatibility
- Files and directories stored as `Vec<(String, Entry)>` tuples (relative path, entry)
- Added helper methods: `add_file`, `add_directory`, `get_file`, `get_directory`
- `head` fields use `Vec<ChangeHash>` with custom serde module for hex serialization
- `url` fields use `samod::AutomergeUrl` with custom serde module for string serialization
- 21 tests passing (6 new snapshot tests)

---

### Task 4.3: Implement File Reading and MIME Detection

Add utilities for reading local files and detecting their type.

- [ ] Add `mime_guess` crate dependency
- [ ] Create function to read file content from disk
- [ ] Create function to detect MIME type from filename/extension
- [ ] Create function to extract file extension from path
- [ ] Determine text vs binary based on MIME type
- [ ] Add tests for common file types

**Notes:**
- Text files: text/*, application/json, application/javascript, etc.
- Binary files: everything else (images, executables, etc.)
- For now, focus on text files only

---

### Task 4.4: Scan Directory for Untracked Files

Implement scanning to find files that need to be pushed.

- [ ] Create function to scan directory recursively
- [ ] Filter out excluded patterns from config
- [ ] Compare against empty snapshot (all files are new)
- [ ] Return list of files to push with their paths

**Notes:**
- Use `walkdir` or `std::fs::read_dir` recursively
- Respect `.pushwork/config.json` exclude patterns
- For Phase 4, we only care about new files (not modifications)

---

### Task 4.5: Create File Document from Local File

Implement creating an Automerge file document from a local file.

- [ ] Read file content from disk
- [ ] Get file permissions (Unix mode)
- [ ] Detect MIME type and extension
- [ ] Create `FileDocument` with the content
- [ ] Reconcile to Automerge document
- [ ] Create document in repo and get URL

**Notes:**
- Reuse `FileDocument` from `documents.rs`
- For text files, content is the string content
- Permissions should be read from filesystem metadata

---

### Task 4.6: Update Root Directory Document

Add the new file entry to the root directory.

- [ ] Load the root directory document from repo
- [ ] Hydrate to `DirectoryDocument`
- [ ] Add new `DirectoryEntry` for the file
- [ ] Reconcile back to Automerge
- [ ] Wait for sync to complete

**Notes:**
- The entry needs: name, type ("file"), url
- Use `they_have_our_changes` to wait for sync

---

### Task 4.7: Save Snapshot After Sync

Update the snapshot file with the new file entry.

- [ ] Create or load existing snapshot
- [ ] Add `FileEntry` for the new file (path, url, heads)
- [ ] Update timestamp
- [ ] Save snapshot to `.pushwork/snapshot.json`

**Notes:**
- Get document heads using `doc.get_heads()`
- Heads are hex-encoded change hash strings

---

### Task 4.8: Wire Up Sync Command

Connect all the pieces in the sync command handler.

- [ ] Scan for untracked files
- [ ] For each new file:
  - Create file document
  - Add to root directory
- [ ] Wait for all syncs to complete
- [ ] Save snapshot
- [ ] Print summary of synced files

**Notes:**
- For Phase 4, only handle new files (not updates/deletes)
- Print progress as files are synced

---

### Task 4.9: Interop Verification

Verify the pushed file can be seen by pushwork.

- [ ] Initialize a directory with `thrustwork init`
- [ ] Create a text file (e.g., `hello.txt`)
- [ ] Run `thrustwork sync`
- [ ] Clone with pushwork in another directory
- [ ] Verify the file appears with correct content

**Verification:**
```
$ mkdir /tmp/test-push && cd /tmp/test-push
$ thrustwork init
$ echo "Hello from thrustwork!" > hello.txt
$ thrustwork sync
Syncing directory: /tmp/test-push
  Pushing: hello.txt
Done! 1 file synced.

$ mkdir /tmp/test-pull && cd /tmp/test-pull
$ pushwork clone <url>
$ cat hello.txt
Hello from thrustwork!
```

---

### Phase 4 Completion Checklist

- [ ] `sync` command implemented
- [ ] Snapshot structure defined and can be saved/loaded
- [ ] File reading and MIME detection working
- [ ] Directory scanning finds untracked files
- [ ] File documents created correctly
- [ ] Root directory updated with new entries
- [ ] Snapshot saved after sync
- [ ] Pushwork can clone and see pushed files

**Phase 4 complete. Proceed to Phase 5 in IMPLEMENTATION_PLAN.md.**

---

