# Implementation Plan

This document outlines the implementation plan for thrustwork, a Rust port of
the pushwork library. The work is organized into vertical slices - each phase
delivers working, testable functionality.

See DESIGN.md for the high-level design and technical specifications.

## Implementation Phases

### Phase 1: Connect to Sync Server

**Goal**: Verify we can use samod to connect to the Automerge sync server and
create/read documents. This validates our core dependency before building
anything else.

**Deliverable**: A minimal binary that connects to the sync server, creates a
document, and prints its URL.

**Work**:
- Set up Cargo.toml with samod, automerge, and tokio
- Write a main.rs that:
  - Initializes a samod Repo with in-memory storage
  - Connects to `wss://sync3.automerge.org` via WebSocket
  - Creates a simple document and writes a test value
  - Prints the document URL
  - Waits briefly for sync, then shuts down

**验证**: Run the binary, copy the URL, run again with that URL to verify the
document synced.

---

### Phase 2: Read and Write Pushwork Documents

**Goal**: Create documents that match the pushwork schema and verify
interoperability.

**Deliverable**: Can create file and directory documents that pushwork can
read, and can read documents created by pushwork.

**Work**:
- Define minimal types for file and directory documents (just enough to
  serialize the schema)
- Implement creating a file document with the correct structure:
  - `@patchwork.type: "file"`
  - `name`, `extension`, `mimeType`
  - `content` as Text or Bytes
  - `metadata.permissions`
- Implement creating a directory document:
  - `@patchwork.type: "folder"`
  - `docs` array with entries
- Implement reading file content from a document
- Test by creating documents with thrustwork and reading with pushwork (and
  vice versa)

**Verification**: Create a file document with thrustwork, sync, verify pushwork
can read it. Create a directory with pushwork, verify thrustwork can traverse
it.

---

### Phase 3: Initialize a Directory

**Goal**: Implement the `init` command to set up a directory for syncing.

**Deliverable**: `thrustwork init` creates the `.pushwork` folder, config file,
and root directory document.

**Work**:
- Add clap for CLI parsing (just `init` command for now)
- Implement minimal config structure and serialization
- Create `.pushwork/` directory
- Create `.pushwork/config.json` with root directory URL and defaults
- Create root directory document (empty `docs` array)
- Use filesystem storage for the repo (in `.pushwork/repo/`)
- Implement basic error handling for this path

**Verification**: Run `thrustwork init` in a test directory. Verify config file
exists and contains a valid Automerge URL. Run pushwork against the same URL
and verify it sees an empty directory.

---

### Phase 4: Push a Single File

**Goal**: Sync a single local file to the remote.

**Deliverable**: After `init`, creating a file and running `thrustwork sync`
pushes it to the remote.

**Work**:
- Implement minimal snapshot structure (just enough to track one file)
- Implement reading a file from disk (detect text vs binary)
- Implement MIME type detection for the file
- Create file document from local file content
- Add entry to root directory document
- Save snapshot with the new file entry
- Wait for sync to complete

**Verification**: Init, create a text file, run sync. Use pushwork (or another
thrustwork instance) to clone the URL and verify the file appears.

---

### Phase 5: Pull a Single File

**Goal**: Sync a remote file to the local filesystem.

**Deliverable**: Clone a pushwork directory containing one file.

**Work**:
- Add `clone` command that takes an Automerge URL
- Load root directory document from URL
- Traverse directory to find file entries
- Read file document content
- Write content to local filesystem
- Create snapshot with the pulled file

**Verification**: Create a directory with pushwork containing one file. Run
`thrustwork clone <url>`. Verify the file appears locally.

---

### Phase 6: Detect and Sync Local Changes

**Goal**: Detect when a tracked file has changed and push the update.

**Deliverable**: Modify a synced file, run sync, change is pushed.

**Work**:
- Implement snapshot loading
- Compare local file content against snapshot (by reading the document at
  snapshot heads)
- Detect LOCAL_ONLY change type
- Update existing file document with new content
- Update snapshot with new heads

**Verification**: Clone a file, modify it locally, run sync. Verify the change
appears on another client.

---

### Phase 7: Detect and Apply Remote Changes

**Goal**: Detect when a remote file has changed and pull the update.

**Deliverable**: When a file changes remotely, sync pulls it.

**Work**:
- Compare document heads against snapshot heads to detect remote changes
- Detect REMOTE_ONLY change type
- Read updated content from document
- Write to local filesystem
- Update snapshot

**Verification**: Sync a file on two clients. Modify on client A, sync. Sync
on client B, verify it receives the change.

---

### Phase 8: Handle Multiple Files

**Goal**: Sync directories with multiple files.

**Deliverable**: Init/clone/sync works with multiple files in the root
directory.

**Work**:
- Extend filesystem scanning to list all files (with exclusion patterns)
- Process multiple files in change detection
- Create multiple file documents when pushing
- Pull multiple files when cloning
- Update snapshot with all files

**Verification**: Create a directory with several files, init, sync. Clone
elsewhere and verify all files appear.

---

### Phase 9: Handle Subdirectories

**Goal**: Support nested directory structures.

**Deliverable**: Sync works with files in subdirectories.

**Work**:
- Create directory documents for subdirectories
- Link parent directory to child directory documents
- Recursive directory traversal for scanning
- Recursive traversal for pulling remote directories
- Path handling for nested files in snapshot

**Verification**: Create nested directory structure, sync. Clone and verify
structure is preserved.

---

### Phase 10: Handle File Deletion

**Goal**: Sync file deletions in both directions.

**Deliverable**: Delete a file locally, sync removes it remotely (and vice
versa).

**Work**:
- Detect local deletion (file in snapshot but not on filesystem)
- Remove entry from parent directory document
- Detect remote deletion (entry removed from directory)
- Delete local file
- Remove from snapshot

**Verification**: Sync a file, delete it locally, sync. Verify remote directory
no longer contains it. Reverse: delete remotely, sync, verify local deletion.

---

### Phase 11: Handle New Remote Files

**Goal**: Discover and pull files added remotely that aren't in our snapshot.

**Deliverable**: When a peer adds a new file, we pull it.

**Work**:
- Traverse remote directory hierarchy to find files not in snapshot
- This completes the change detection algorithm from DESIGN.md Appendix C
- Create local files for new remote entries
- Add to snapshot

**Verification**: Two clients synced. Client A adds a new file, syncs. Client B
syncs and receives the new file.

---

### Phase 12: Move Detection

**Goal**: Detect file moves/renames and preserve document identity.

**Deliverable**: Rename a file locally, sync updates the name rather than
delete+create.

**Work**:
- Implement string similarity (Sørensen–Dice coefficient)
- Identify deleted files with known remote content
- Identify new files not in snapshot
- Match by similarity above threshold
- Update directory entries and file name instead of delete+create

**Verification**: Sync a file, rename it locally, sync. Verify the document URL
is preserved (same document, new name).

---

### Phase 13: Two-Phase Sync

**Goal**: Implement the full two-phase sync algorithm for correctness.

**Deliverable**: Sync handles concurrent changes correctly.

**Work**:
- Phase 1: Push all local changes
- Wait for network sync completion
- Add post-sync delay for peer propagation
- Re-run change detection
- Phase 2: Pull remote changes
- Handle BOTH_CHANGED by merging (text) or preferring remote (binary)

**Verification**: Concurrent edits on two clients. Both sync. Verify both end
up with merged result.

---

### Phase 14: Remaining CLI Commands

**Goal**: Complete the CLI interface.

**Deliverable**: All planned commands working.

**Work**:
- `status`: Show pending changes without syncing
- `commit`: Push local changes without pulling (for offline work)
- `url`: Print the root directory URL
- Add `--verbose` flag for detailed output
- Improve error messages and user feedback

**Verification**: Each command works as expected.

---

### Phase 15: Configuration

**Goal**: Support user configuration.

**Deliverable**: Global and local config files are respected.

**Work**:
- Load global config from `~/.pushwork/config.json`
- Merge with local config
- Support custom sync server URL
- Support custom exclude patterns
- Support custom move detection threshold

**Verification**: Set a custom exclude pattern, verify files are excluded.

---

### Phase 16: Robustness

**Goal**: Handle edge cases and errors gracefully.

**Deliverable**: Production-ready error handling.

**Work**:
- Graceful handling of network disconnection
- Timeout handling for sync wait
- Partial sync (continue on individual file errors)
- Proper cleanup on shutdown
- Handle permission errors on file operations
- Validate snapshot integrity

**Verification**: Interrupt sync mid-operation, verify no corruption. Simulate
network issues.

---

## Milestone Summary

| Milestone | Phases | Capability |
|-----------|--------|------------|
| M1: Connected | 1-2 | Can create/read pushwork-compatible documents |
| M2: Single file | 3-5 | Init, push one file, clone one file |
| M3: Bidirectional | 6-7 | Detect and sync changes both directions |
| M4: Full tree | 8-9 | Multiple files, nested directories |
| M5: Complete sync | 10-13 | Deletions, new files, moves, two-phase |
| M6: Production | 14-16 | Full CLI, configuration, robustness |

## Notes

**Interoperability testing**: Phases 2, 3, 4, and 5 should all be tested
against pushwork to ensure document compatibility. Don't proceed past Phase 5
without confirming interop.

**Incremental complexity**: Each phase adds one concept. If a phase feels too
large, split it. If something is harder than expected, the narrow scope limits
the blast radius.

**Test as you go**: Each phase has a verification step. Write these as
integration tests where practical so they continue to pass as development
proceeds.
