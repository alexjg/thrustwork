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

- [ ] Update Cargo.toml with dependencies:
  - `samod = "0.3"`
  - `automerge = "0.5"`
  - `tokio = { version = "1", features = ["full"] }`
- [ ] Verify the project compiles with `cargo build`

**Notes:**
<!-- Add any notes about version issues or dependency conflicts here -->

---

### Task 1.2: Initialize a Samod Repo

Write minimal code to create a samod Repo with in-memory storage.

- [ ] Create a tokio async main function
- [ ] Initialize a samod Repo using `Repo::build_tokio()` or equivalent
- [ ] Use in-memory storage (no filesystem persistence yet)
- [ ] Verify it compiles and runs without crashing

**Notes:**
<!-- Document the actual samod API if it differs from expectations -->

---

### Task 1.3: Connect to Sync Server

Connect the repo to the public Automerge sync server.

- [ ] Connect to `wss://sync3.automerge.org` via WebSocket
- [ ] Handle connection errors gracefully (print error and exit)
- [ ] Verify connection succeeds (no errors on startup)

**Notes:**
<!-- Note any connection issues or API differences -->

---

### Task 1.4: Create and Sync a Document

Create a simple document and sync it to the server.

- [ ] Create a new document using the repo
- [ ] Write a simple test value to the document (e.g., a string field)
- [ ] Print the document's Automerge URL to stdout
- [ ] Wait briefly for sync (e.g., 1-2 seconds)
- [ ] Shut down cleanly

**Notes:**
<!-- Document the actual API for document creation and URL retrieval -->

---

### Task 1.5: Verify Round-Trip

Verify that documents actually sync by reading back a document by URL.

- [ ] Modify the program to optionally accept a URL as a command-line argument
- [ ] If URL provided: find the document and print its contents
- [ ] If no URL: create a new document (existing behavior)
- [ ] Test by running twice: once to create, once to read back

**Verification:**
```
# First run - creates document
$ cargo run
Created document: automerge:abc123...

# Second run - reads it back
$ cargo run -- automerge:abc123...
Document contents: { test: "value" }
```

**Notes:**
<!-- Record whether round-trip worked, any issues encountered -->

---

### Phase 1 Completion Checklist

- [ ] All tasks above completed
- [ ] Can create a document and print its URL
- [ ] Can read back a document by URL
- [ ] Sync server connection works reliably
- [ ] Code is clean enough to build on

**Phase 1 complete. Proceed to Phase 2 in IMPLEMENTATION_PLAN.md.**

---
