# Implementation Progress Tracker

This document tracks the step-by-step implementation of features. It's organized into discrete chunks of work that are each small enough to review independently. When working on this codebase with an LLM, use this document to understand what has been completed and what needs to be done next.

The overall design we are working on is described in design/README.md and the separate chunks of development are described in IMPLEMENTATION_PLAN.md. This document always tracks the development of one chunk. Each previous chunk should be briefly marked as complete in this file, then the rest of the file should be more detailed steps for the current chunk. The workflow should be:

* Start work on a new chunk
* Break the chunk down into smaller chunks, write each chunk up as a separate task in this document
* Work through the tasks one by one marking each task as [x] when completed
* Once the whole chunk is complete, write a summary of the completed chunk
* Go back to the start with the next chunk

When resuming work, scan for the first uncompleted task.

**General Instructions**
- Read REFACTOR_DESIGN.md and IMPLEMENTATION_PLAN.md for context

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

**Pushwork Schema Compatibility Notes**:
- Metadata fields (`@patchwork.type`, `name`, `extension`, `mimeType`, directory entry
  fields) must be `autosurgeon::Text` (collaborative text objects)
- File `content` must be `String` (scalar string = ImmutableString in pushwork)
- `permissions` is stored as `i64` (JavaScript number -> Automerge Int)
- `lastSyncAt` may be absent, use `#[autosurgeon(missing = "Default::default")]`
- Snapshot heads are base58check encoded (using `bs58` crate with check feature)
- **All integers in Automerge are `i64`** - use `i64` not `u64` for fields like `lastSyncAt`

**Autosurgeon Key Attribute**:
- When reconciling `Vec<T>` where items have identity (like directory entries),
  use `#[key]` on the identifying field
- Without `#[key]`, autosurgeon does position-based reconciliation which causes
  CRDT merge issues with concurrent modifications
- With `#[key]`, autosurgeon tracks items by identity, enabling correct merges

**Automerge View/Transaction at Heads**:
- We use an unreleased version of automerge with `Automerge::view_at(heads)` method
- `view_at` returns a `ReadDoc` view of the document at specific heads
- This allows using autosurgeon's `hydrate` to read document state at snapshot heads
- Combined with `Automerge::transaction_at(heads)` for writes, we can operate on
  historical document state without forking

---
