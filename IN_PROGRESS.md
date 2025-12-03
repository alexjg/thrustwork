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

---

## Phase 1: Connect to Sync Server

**Goal**: Verify we can use samod to connect to the Automerge sync server and
create/read documents. This validates our core dependency before building
anything else.

**Deliverable**: A minimal binary that connects to the sync server, creates a
document, and prints its URL.

---

### Task 1.1: Project Setup

Set up the basic Cargo project with required dependencies.

- [x] Update Cargo.toml with dependencies:
  - `samod = "0.3"`
  - `automerge = "0.5"`
  - `tokio = { version = "1", features = ["full"] }`
- [x] Verify the project compiles with `cargo build`

**Notes:**
- Using edition 2024
- samod 0.5.1 with `tokio` and `tungstenite` features enabled
- automerge 0.7.2 (pulled in transitively by samod)
- tokio 1.48.0 with full features
- Removed explicit automerge dependency - samod brings in the correct version

---

### Task 1.2: Initialize a Samod Repo

Write minimal code to create a samod Repo with in-memory storage.

- [x] Create a tokio async main function
- [x] Initialize a samod Repo using `Repo::build_tokio()` or equivalent
- [x] Use in-memory storage (no filesystem persistence yet)
- [x] Verify it compiles and runs without crashing

**Notes:**
- `samod::Repo::build_tokio().load().await` works as documented
- In-memory storage is the default (no explicit configuration needed)
- Repo provides a `peer_id()` method that returns a unique peer identifier

---

### Task 1.3: Connect to Sync Server

Connect the repo to the public Automerge sync server.

- [x] Connect to `wss://sync3.automerge.org` via WebSocket
- [x] Handle connection errors gracefully (print error and exit)
- [x] Verify connection succeeds (no errors on startup)

**Notes:**
- Added `tokio-tungstenite` dependency with `native-tls` feature for WebSocket client
- samod's `connect_websocket` takes an already-established stream, not a URL
- Use `tokio_tungstenite::connect_async(url)` to establish the WebSocket
- Then pass the stream to `repo.connect_tungstenite(ws_stream, ConnDirection::Outgoing)`
- The connection handler must be spawned as a background task

---

### Task 1.4: Create and Sync a Document

Create a simple document and sync it to the server.

- [x] Create a new document using the repo
- [x] Write a simple test value to the document (e.g., a string field)
- [x] Print the document's Automerge URL to stdout
- [x] Wait briefly for sync (e.g., 1-2 seconds)
- [x] Shut down cleanly

**Notes:**
- Added `automerge = "0.7"` as direct dependency
- Need to import `automerge::transaction::Transactable` trait to use `put()` method
- Create document: `Automerge::new()`, then `doc.transact(|txn| { txn.put(...) })`
- Pass to repo: `repo.create(doc).await`
- Get URL: `doc_handle.url()` returns `AutomergeUrl` which implements Display

---

### Task 1.5: Verify Round-Trip

Verify that documents actually sync by reading back a document by URL.

- [x] Modify the program to optionally accept a URL as a command-line argument
- [x] If URL provided: find the document and print its contents
- [x] If no URL: create a new document (existing behavior)
- [x] Test by running twice: once to create, once to read back

**Verification:**
```
# First run - creates document
$ cargo run
Created document: automerge:2ss1TDGDXtYLYVu4JWVdaxeRUNaf

# Second run - reads it back
$ cargo run -- automerge:2ss1TDGDXtYLYVu4JWVdaxeRUNaf
Found document!
Document keys: ["test"]
Document contents: test = Scalar(Str("hello from thrustwork"))
```

**Notes:**
- Parse URL by stripping `automerge:` prefix, then `DocumentId::from_str()`
- Use `repo.find(doc_id).await` to look up document from sync server
- Need brief delay (1s) after connecting before `find()` to let sync protocol establish
- Use `ReadDoc` trait to access `doc.keys()` and `doc.get()`
- `with_document(|doc| ...)` provides access to the underlying Automerge document

---

### Phase 1 Completion Checklist

- [x] All tasks above completed
- [x] Can create a document and print its URL
- [x] Can read back a document by URL
- [x] Sync server connection works reliably
- [x] Code is clean enough to build on

**Phase 1 complete. Proceed to Phase 2 in IMPLEMENTATION_PLAN.md.**

---
