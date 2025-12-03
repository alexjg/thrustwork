# Design

This project is a port of the `pushwork` (`../pushwork`) library to Rust. The
primary reason for porting is to be easier to deploy, but also to take
advantage of the higher performance of the Rust language.

## Overview

`pushwork` is a JavaScript library which uses the `@automerge/automerge-repo`
library to represent a local filesystem folder as a set of Automerge documents
and synchronize changes to that folder with a remote server. Thrustwork
implements the same functionality. Specifically it does the following:

* Parses the local config file in the `.pushwork` folder
* Provides a `sync` command to detect changes in the local file system and
  push them to the remote server and likewise detect changes on the remote
  server and merge them with the local filesystem state

This latter step is achieved using the `samod` library which implements the
same wire and storage protocol as the automerge-repo library.

## Pushwork Architecture

Pushwork is a bidirectional file synchronization library built on Automerge
CRDTs. It synchronizes files across peers via a sync server, with automatic
conflict resolution provided by the underlying Automerge documents.

### Document Model

Pushwork represents the state of the filesystem as a collection of Automerge
documents. There are two types of document: one for files and one for
directories.

A **file document** contains the file's name, extension, MIME type, and
content. The content is stored either as an Automerge text object (for text
files, enabling character-level merging) or as raw bytes (for binary files).
File documents also track Unix permissions in a metadata field.

A **directory document** represents a folder in the filesystem. It contains an
array of entries, where each entry has a name, a type (either "file" or
"folder"), and an Automerge URL pointing to the child document. Directory
documents also track a `lastSyncAt` timestamp indicating when they were last
modified during a sync operation.

This structure forms a tree: the root directory document contains references to
its children, which may themselves be directory documents containing further
references. The entire filesystem state can be traversed starting from the root
directory URL.

### Local State Tracking

To detect what has changed between syncs, pushwork maintains a local snapshot
file (`.pushwork/snapshot.json`). This snapshot records:

- The Automerge URL and document "heads" for each tracked file and directory
- The timestamp of the last sync
- The root directory URL

The "heads" are Automerge's way of identifying a specific version of a
document. By comparing the current document heads against the snapshot heads,
pushwork can determine whether a document has changed remotely since the last
sync. Similarly, by comparing filesystem contents against what was last synced,
it can detect local changes.

### Change Detection

When detecting changes, pushwork compares three states for each file:

1. **Last known state**: What was recorded in the snapshot at the last sync
2. **Current local state**: What's currently on the filesystem
3. **Current remote state**: The current Automerge document content

This three-way comparison allows pushwork to classify each file into one of
four categories:

- **No change**: The file is the same locally and remotely
- **Local only**: The file changed locally but not remotely
- **Remote only**: The file changed remotely but not locally
- **Both changed**: The file changed in both places (conflict)

For conflicts, Automerge's CRDT properties ensure that concurrent edits to text
files are automatically merged. Binary files cannot be merged, so the remote
version takes precedence.

### Move Detection

Pushwork includes a move detector that identifies when files have been renamed
or moved rather than deleted and recreated. It works by comparing the content
of deleted files against newly created files using the Sørensen–Dice
coefficient for string similarity. If two files are sufficiently similar
(default threshold: 70%), they're treated as a move operation rather than
separate delete and create operations. This preserves the Automerge document
identity across renames.

### Two-Phase Sync

The sync operation runs in two phases:

**Phase 1 (Push)**: Local changes are written to Automerge documents. This
includes creating new file documents, updating existing documents with new
content, deleting documents for removed files, and updating directory documents
to reflect structural changes. Move operations update the directory entries and
file names without creating new documents.

After pushing, pushwork waits for the changes to sync to the server. There's
also a short delay (200ms) to allow any peer changes to propagate back through
the WebSocket connection.

**Phase 2 (Pull)**: After the network sync, pushwork re-detects changes to get
a fresh view of the remote state. Any remote-only changes are then written to
the local filesystem. Changes are processed in order of path depth (parents
before children) to ensure directories exist before their contents are written.

Finally, the snapshot is updated with the new document heads and saved.

### Configuration

Pushwork uses a cascading configuration system:

- **Default config**: Built-in defaults (sync server URL, exclude patterns)
- **Global config**: User-level settings in `~/.pushwork/config.json`
- **Local config**: Directory-level settings in `.pushwork/config.json`

Each level overrides the previous. Configuration includes the sync server URL,
patterns for files to exclude from syncing (like `.git` and `node_modules`),
and the move detection threshold.

## Samod Library

[samod](https://crates.io/crates/samod) is a Rust library for managing
Automerge documents. It is wire-compatible with the JavaScript automerge-repo
library, meaning thrustwork can sync with pushwork peers.

The key abstractions in samod are:

- **Repo**: The main entry point for managing documents and peer connections
- **DocHandle**: A reference to a specific Automerge document, used for reading
  and modifying document content
- **Storage trait**: An abstraction for document persistence, allowing different
  backends (in-memory, filesystem, etc.)

Samod handles the networking and sync protocol automatically. When documents
are modified through a DocHandle, changes are propagated to connected peers.
When remote changes arrive, they're automatically merged into the local
document state.

## Thrustwork Design

Thrustwork will implement the same functionality as pushwork using Rust and
samod. The high-level architecture mirrors pushwork:

### Core Components

**SyncEngine**: Orchestrates the two-phase sync process. Coordinates between
the change detector, move detector, and samod repo to push local changes and
pull remote changes.

**ChangeDetector**: Scans the filesystem and compares against the snapshot and
remote document state to classify changes. Uses glob patterns to exclude
configured files and directories.

**MoveDetector**: Analyzes deleted and created files to identify moves using
string similarity comparison.

**SnapshotManager**: Handles loading, saving, and updating the local snapshot
file. Tracks document URLs and heads for change detection.

**ConfigManager**: Loads and merges configuration from default, global, and
local sources.

### CLI Commands

Thrustwork will provide the following commands:

- `init`: Initialize a directory for syncing, creating the `.pushwork` folder
- `clone <url>`: Clone from an existing Automerge URL
- `sync`: Run bidirectional sync
- `commit`: Push local changes without pulling remote changes
- `status`: Show current sync status
- `url`: Display the root directory URL for sharing

### Key Implementation Considerations

**Async runtime**: Thrustwork will use tokio, leveraging samod's built-in tokio
support for async document operations and WebSocket connections.

**Text vs binary handling**: Files are classified as text or binary based on
MIME type detection. Text files use Automerge's text type for character-level
merging; binary files are stored as raw bytes.

**Head-based change detection**: The snapshot stores document heads (as
hex-encoded strings) which are compared against current heads to detect remote
changes. This is more reliable than timestamp-based detection.

**Error handling**: Operations are designed to be recoverable where possible.
Sync errors for individual files don't abort the entire sync; instead, errors
are collected and reported at the end.

### Dependencies

The main dependencies will be:

- `samod`: Automerge document management and sync
- `automerge`: The underlying CRDT library
- `tokio`: Async runtime
- `clap`: Command-line argument parsing
- `serde`/`serde_json`: Configuration and snapshot serialization
- `glob`: File pattern matching for exclusions
- `mime_guess`: MIME type detection for text/binary classification

---

## Appendix A: Document Schemas

This appendix describes the precise structure of the Automerge documents used
by pushwork.

### File Document Schema

```
{
  "@patchwork": {
    "type": "file"
  },
  "name": <string>,
  "extension": <string>,
  "mimeType": <string>,
  "content": <text | bytes>,
  "metadata": {
    "permissions": <integer>
  }
}
```

| Field                | Type            | Description                                      |
|----------------------|-----------------|--------------------------------------------------|
| `@patchwork.type`    | string          | Always `"file"`. Discriminator for document type |
| `name`               | string          | The filename (e.g., `"README.md"`)               |
| `extension`          | string          | File extension without dot (e.g., `"md"`)        |
| `mimeType`           | string          | MIME type (e.g., `"text/markdown"`)              |
| `content`            | text or bytes   | File contents (see note below)                   |
| `metadata.permissions` | integer       | Unix permissions as decimal (e.g., `644`)        |

**Content field**: For text files, this is an Automerge `Text` object, which
enables character-level CRDT merging. For binary files, this is raw bytes
stored as an Automerge `Bytes` value, which does not support merging (last
write wins at the document level).

### Directory Document Schema

```
{
  "@patchwork": {
    "type": "folder"
  },
  "docs": [
    {
      "name": <string>,
      "type": <"file" | "folder">,
      "url": <automerge-url>
    },
    ...
  ],
  "lastSyncAt": <integer | undefined>
}
```

| Field              | Type            | Description                                        |
|--------------------|-----------------|----------------------------------------------------|
| `@patchwork.type`  | string          | Always `"folder"`. Discriminator for document type |
| `docs`             | array           | Array of directory entries                         |
| `docs[].name`      | string          | Entry name (filename or subdirectory name)         |
| `docs[].type`      | string          | Either `"file"` or `"folder"`                      |
| `docs[].url`       | automerge-url   | URL of the child document                          |
| `lastSyncAt`       | integer or null | Unix timestamp (ms) of last sync, if any           |

**Automerge URL format**: URLs follow the automerge-repo format:
`automerge:<document-id>` where `<document-id>` is a base58-encoded identifier.

---

## Appendix B: Snapshot File Format

The snapshot file (`.pushwork/snapshot.json`) tracks the state at the last
successful sync. It is stored as JSON with the following structure:

```
{
  "timestamp": <integer>,
  "rootPath": <string>,
  "rootDirectoryUrl": <automerge-url | null>,
  "files": [
    [<relative-path>, <file-entry>],
    ...
  ],
  "directories": [
    [<relative-path>, <directory-entry>],
    ...
  ]
}
```

### Top-Level Fields

| Field              | Type          | Description                                    |
|--------------------|---------------|------------------------------------------------|
| `timestamp`        | integer       | Unix timestamp (ms) when snapshot was saved    |
| `rootPath`         | string        | Absolute path to the synced directory          |
| `rootDirectoryUrl` | string/null   | Automerge URL of the root directory document   |
| `files`            | array         | Array of `[path, entry]` tuples for files      |
| `directories`      | array         | Array of `[path, entry]` tuples for directories|

### File Entry

```
{
  "path": <string>,
  "url": <automerge-url>,
  "head": <array of hex strings>,
  "extension": <string>,
  "mimeType": <string>
}
```

| Field       | Type          | Description                                    |
|-------------|---------------|------------------------------------------------|
| `path`      | string        | Full filesystem path                           |
| `url`       | string        | Automerge URL of the file document             |
| `head`      | array         | Document heads at last sync (hex-encoded)      |
| `extension` | string        | File extension                                 |
| `mimeType`  | string        | MIME type                                      |

### Directory Entry

```
{
  "path": <string>,
  "url": <automerge-url>,
  "head": <array of hex strings>,
  "entries": <array of strings>
}
```

| Field     | Type          | Description                                    |
|-----------|---------------|------------------------------------------------|
| `path`    | string        | Full filesystem path                           |
| `url`     | string        | Automerge URL of the directory document        |
| `head`    | array         | Document heads at last sync (hex-encoded)      |
| `entries` | array         | Names of child entries at last sync            |

**Document heads**: The `head` field contains an array of Automerge change
hashes encoded as hexadecimal strings. These identify the exact version of the
document at the time of the last sync. Comparing current heads against snapshot
heads reveals whether a document has changed remotely.

---

## Appendix C: Change Detection Algorithm

The change detection algorithm compares three states to classify each tracked
file:

1. **Snapshot state (S)**: The content and heads recorded at the last sync
2. **Local state (L)**: The current content on the filesystem
3. **Remote state (R)**: The current content in the Automerge document

### Algorithm Overview

```
For each file in (snapshot ∪ local filesystem ∪ remote documents):

  1. Retrieve:
     - L = local filesystem content (null if file doesn't exist locally)
     - S = content at snapshot head (null if not in snapshot)
     - R = current remote document content (null if not in remote)

  2. Compute:
     - local_changed  = (L ≠ S)
     - remote_changed = (R ≠ S)

  3. Classify:
     - If NOT local_changed AND NOT remote_changed → NO_CHANGE
     - If local_changed AND NOT remote_changed     → LOCAL_ONLY
     - If NOT local_changed AND remote_changed     → REMOTE_ONLY
     - If local_changed AND remote_changed         → BOTH_CHANGED
```

### Detecting New Files

Files not present in the snapshot require special handling:

- **New local file**: File exists on filesystem but not in snapshot. If also
  not in remote directory hierarchy → `LOCAL_ONLY`. If exists in remote →
  `BOTH_CHANGED`.

- **New remote file**: File exists in remote directory hierarchy but not in
  snapshot. Discovered by traversing directory documents from root. If not on
  local filesystem → `REMOTE_ONLY`. If exists locally → `BOTH_CHANGED`.

### Detecting Deleted Files

- **Local deletion**: File in snapshot but not on filesystem. Check if remote
  has also changed since snapshot. If remote unchanged → `LOCAL_ONLY`. If
  remote changed → `BOTH_CHANGED`.

- **Remote deletion**: File in snapshot but removed from remote directory
  listing (even if document still exists). If local file still exists →
  `REMOTE_ONLY` (will delete local). If local also deleted → no change needed.

### Remote Change Detection via Heads

Remote changes are detected by comparing document heads rather than content:

```
current_heads = document.heads()
snapshot_heads = snapshot.files[path].head

if current_heads ≠ snapshot_heads:
    # Document has changed remotely since last sync
```

This is more efficient than content comparison and correctly handles cases
where content might round-trip to the same value through different edit paths.

### Content Comparison

Content equality checks must handle:

- **String comparison**: Direct equality check
- **Binary comparison**: Byte-by-byte equality
- **Null handling**: `null` represents absence (deleted or non-existent),
  distinct from empty content (`""` or `[]`)
