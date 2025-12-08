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

### Synchronization

The details of how pushwork synchronizes with the filesystem are somewhat
involved and specified in design/sync.md.

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

Thrustwork implements the same functionality as pushwork using Rust and samod.

### CLI Commands

Thrustwork provides the following commands:

- `init`: Initialize a directory for syncing, creating the `.pushwork` folder
- `clone <url>`: Clone from an existing Automerge URL
- `sync`: Run bidirectional sync
- `status`: Show current sync status
- `url`: Display the root directory URL for sharing

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
