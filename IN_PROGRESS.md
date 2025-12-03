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
  - Use `autosurgeon::Text` for text content (enables CRDT merging)
  - Requires `AutoCommit` (not `Automerge`) for `reconcile()` calls

---

## Phase 2: Read and Write Pushwork Documents

**Goal**: Create documents that match the pushwork schema and verify
interoperability.

**Deliverable**: Can create file and directory documents that pushwork can
read, and can read documents created by pushwork.

---

### Task 2.1: Define Document Types and Create a File Document

Define Rust types for file and directory documents using autosurgeon, then create
a file document matching the pushwork schema (see DESIGN.md Appendix A).

- [ ] Define `FileDocument` struct with autosurgeon derives:
  - `@patchwork` map with `type: "file"` (use nested struct + rename)
  - `name`, `extension`, `mimeType` (String fields with appropriate renames)
  - `content` (use `autosurgeon::Text` for now - binary handling later)
  - `metadata` map with `permissions` (nested struct)
- [ ] Define `DirectoryDocument` struct with autosurgeon derives:
  - `@patchwork` map with `type: "folder"`
  - `docs` array of `DirectoryEntry` structs
  - `lastSyncAt` (Option<u64>)
- [ ] Move types to a `documents` module
- [ ] Write a test that creates a file document and verifies the keys

**Notes:**
- Prototype types already tested in `test_autosurgeon.rs`
- Use `#[autosurgeon(rename = "@patchwork")]` for the patchwork field
- Use `#[autosurgeon(rename = "type")]` for type discriminator (reserved word)

---

### Task 2.2: Create and Link Directory with File Entry

Create a directory document and link it to a file document.

- [ ] Write a test that:
  - Creates a `FileDocument` and reconciles it to an AutoCommit doc
  - Creates a `DirectoryDocument` with an entry pointing to the file
  - Reconciles the directory to another AutoCommit doc
  - Verifies both documents have correct structure
- [ ] Verify the directory's `docs` array contains the entry with correct fields

**Notes:**
- Directory types already defined in Task 2.1
- The `url` field stores the automerge URL as a string

---

### Task 2.3: Read File Content from Document (Hydrate)

Verify that file documents can be hydrated back to Rust types.

- [ ] Write a test that:
  - Creates a `FileDocument`, reconciles it
  - Hydrates it back to a `FileDocument`
  - Verifies all fields match (name, extension, mimeType, content, permissions)
- [ ] Ensure `Text` content round-trips correctly

**Notes:**
- Use `autosurgeon::hydrate()` to read documents
- `Text::to_string()` extracts the string value

---

### Task 2.4: Interop Test - Thrustwork to Pushwork

Verify pushwork can read documents created by thrustwork.

- [ ] Add a CLI subcommand (e.g., `create-test`) that:
  - Creates a file document with sample content
  - Creates a directory document containing the file
  - Syncs both to the server
  - Prints both URLs
- [ ] Use pushwork to clone the directory URL
- [ ] Verify pushwork sees the file with correct content

**Verification:**
```
# Create with thrustwork
$ cargo run -- create-test
Created directory: automerge:xxx
Created file: automerge:yyy

# Read with pushwork (in another directory)
$ pushwork clone automerge:xxx
$ cat test.txt
(should show content)
```

**Notes:**
- This is a manual integration test requiring pushwork installed
- Record any schema compatibility issues discovered

---

### Task 2.5: Interop Test - Pushwork to Thrustwork

Verify thrustwork can read documents created by pushwork.

- [ ] Use pushwork to create a synced directory with a file
- [ ] Get the root directory URL from pushwork
- [ ] Add a CLI subcommand (e.g., `read-dir <url>`) that:
  - Loads the directory document
  - Hydrates it to `DirectoryDocument`
  - For each entry, loads and hydrates the file document
  - Prints the directory structure and file contents

**Verification:**
```
# Create with pushwork
$ mkdir test-dir && cd test-dir
$ echo "hello from pushwork" > test.txt
$ pushwork init
$ pushwork sync
$ pushwork url
automerge:xxx

# Read with thrustwork
$ cargo run -- read-dir automerge:xxx
Directory contents:
  - test.txt (file)
Content: "hello from pushwork"
```

**Notes:**
- This verifies hydration works with real pushwork documents
- Record any issues reading pushwork documents

---

### Phase 2 Completion Checklist

- [ ] All tasks above completed
- [ ] Document types defined with autosurgeon derives
- [ ] Can create file documents with correct schema (reconcile)
- [ ] Can create directory documents with correct schema (reconcile)
- [ ] Can read documents back to Rust types (hydrate)
- [ ] Thrustwork documents readable by pushwork (interop verified)
- [ ] Pushwork documents readable by thrustwork (interop verified)
- [ ] Clean up: remove `test_autosurgeon.rs`, move types to proper module

**Phase 2 complete. Proceed to Phase 3 in IMPLEMENTATION_PLAN.md.**

---
