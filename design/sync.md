# Filesystem Sync Design

How do we synchronise the current state of the file system with the automerge
document states?

## Representations

What exactly are we synchronising _between_? There are two things, the current
filesystem state, and the "repo" state. The file system state is hopefully
familiar, it's a hierarchical set of blobs and directories. The interesting part
is the automerge-repo representation. We represent each file as an automerge
document and each directory as another automerge document which contains a list
of "entries", each of which contains the URL of the file the link refers to.

### Files

We represent files as automerge documents with the following schema:

```json
{
  "@patchwork": {
    "type": <"file" | "document">
  },
  "name": <string>,
  "extension": <string>,
  "mimeType": <string>,
  "content": <string | Uint8Array>,
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

### Directories

```json
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

**Automerge URL format**: URLs follow the automerge-repo format: `automerge:<document-id>` where `<document-id>` is a base58-encoded identifier.

## Synchronizing

"synchronizing" as a user level action is kind of about synchronizing the local
filesystem state with the remote repository. At a technical level though, there
are three states, not two:

* The file system state
* The remote repo state
* The local repo state

The reason for this distinction is that we have no ability to capture changes as
they are made to the filesystem, instead we must wait for someone to run some
kind of operation which synchronizes the filesystem state with the local repo
state. Thus, the local repo state can be different to the local file system
state. 

Following `jj`, the first thing we do on any invocation of `thrustwork` is
synchronize the filesystem state with the local repo state. This provides
the illusion to the user that the local repo state is always up to date
because the only way to interact with it is via the `thrustwork` CLI.
This also simplifies the job of interacting with remotes, because
subsequent tasks can work directly with the local repo, rather than the
filesystem.

These problems are unsurprisingly all the problems that Git has, we should
use the solutions Git has arrived at in most cases.

### Synchronizing With Local State

Synchronizing the filesystem with the local repo state is conceptually:

* Scan the current state of the filesystem
* Load the state of the file system from the local repo
* For each file compare the current state with the repo state and update
  the repo state

There's a problem though. The local repo state may have changed arbitrarily
since we last synced. The changes may represent changes made locally but not
reflected in the filesystem, or maybe changes received over the network. If some
document in the local repo _has_ changed, then that means that the changes 
made on the filesystem are _concurrent_ with the changes in the local repo. To
represent this in automerge we need to update the local repo document to match
the filesystem state, but we need to perform the update _from the point when
we last updated the filesystem_. Automerge provides facilities to achieve
this via `Automerge::transaction_at`, but we need to know the heads we used
last time we synced with the file system. Knowing these heads is the purpose
of snapshots.

### Synchronising with Snapshots

Snapshot files record the mapping from automerge docuemnts (IDs and heads)
to the filesystem state so that we can reconcile local changes correctly.
Snapshots are stored in `.pushwork/snapshot.json` and look roughly like this:
(detailed schema in the Appendix).

```json
{
  "timestamp": <integer>,
  "rootPath": <string>,
  "rootDirectoryUrl": <automerge-url | null>,
  "files": [
    {
      "path": <string>,
      "url": <automerge-url>,
      "head": <array of hex strings>,
      "extension": <string>,
      "mimeType": <string>
    },
    ...
  ],
  "directories": [
    {
      "path": <string>,
      "url": <automerge-url>,
      "head": <array of hex strings>,
      "entries": <array of strings>
    },
    ...
  ]
}
```

Let's return to the process for synchronising the local repo with the filesystem
then. The process is now:

* Scan the current state of the filesystem
* Load the snapshot
* For each file in the filesystem:
  * Check if it is present in the snapshot
  * If it is present check if the filesystem contents match the repo content
  * If the contents match, do nothing
  * If the contents do not match, we need to determine what has happened
    * We load the local repo state as at the snapshot heads, call this the snapshot state
    * Now we have three possibilities
      * The file system has not changed, but the local repo has new changes
        * This is the case if the snapshot heads are not equal to the repo heads
          but the snapshot state matches the filesystem state
        * In this case we need to load the local repo state and update the local filesystem state
      * The file system has changed, but the local repo state has not changed
        * This is the case if the snapshot state does not match the filesystem
          state but the snapshot heads match the local repo heads
        * In this case we need to update the local repo state with the new filesystem state
      * The file system has changed and the remote has changed
        * This is the case if the snapshot state does not match the filesystem
          and the snapshot heads do not match the local repo heads
        * In this case we first apply an update to the local repo state as at
          the snapshot heads (using Automerge::transaction_at) updating the
          document to match the current state
        * Then, we read the merged state (which is now the state of the heads of
          the document) and update the filesystem to match
* Finally, we save a new snapshot with the current heads of the local repo state
  for each file

This is great for synchronizing files which already exist, but it doesn't help
with changes in the file system structure itself - such as creating a file or
deleting a directory.

### Structural Changes: Creation and Deletion

The algorithm above handles the case where a file exists in both the filesystem
and the snapshot. However, we also need to handle four additional cases:

1. A file or directory exists in the filesystem but not in the snapshot (local creation)
2. A file or directory exists in the snapshot but not in the filesystem (local deletion)
3. A file or directory exists in the repo but not in the snapshot (remote creation)
4. A file or directory was removed from the repo since the snapshot (remote deletion)

These cases interact with each other to produce conflicts. For example, if the
filesystem shows a deletion but the repo shows the file was modified, we have a
conflict that requires user intervention.

#### Detecting Structural Changes

To detect structural changes we need to compare three sets of entries for each
directory:

1. **Filesystem entries**: the current contents of the directory on disk
2. **Snapshot entries**: the `entries` array from the directory's snapshot record
3. **Repo entries**: the current `docs` array from the directory's automerge document

For each entry name that appears in any of these three sets, we can classify
its state:

| Filesystem | Snapshot | Repo | Interpretation |
|------------|----------|------|----------------|
| ✓ | ✓ | ✓ | Entry exists everywhere - use content sync algorithm |
| ✓ | ✗ | ✗ | Created locally on filesystem |
| ✗ | ✓ | ✓ | Deleted locally on filesystem |
| ✓ | ✓ | ✗ | Deleted remotely (removed from repo since snapshot) |
| ✗ | ✗ | ✓ | Created remotely (added to repo since snapshot) |
| ✓ | ✗ | ✓ | Concurrent creation - both sides created the same name |
| ✗ | ✓ | ✗ | Deleted on both sides - no action needed |

The case where an entry exists in the filesystem and snapshot but not in the
repo deserves special attention: this means the entry was deleted from the repo
after the snapshot was taken. If the filesystem content matches the snapshot
state, this is a clean remote deletion and we should remove the file from the
filesystem. If the filesystem content differs from the snapshot, we have a
conflict: the user modified a file that was concurrently deleted.

#### Handling File Creation

**Local creation** (file exists on filesystem, not in snapshot or repo):

1. Create a new automerge document for the file with the appropriate schema
2. Add an entry to the parent directory's `docs` array with the new document's URL
3. Record the new file in the snapshot

**Remote creation** (file exists in repo, not in snapshot or filesystem):

1. Read the file content from the automerge document
2. Write the file to the filesystem at the appropriate path
3. Record the file in the snapshot with the current document heads

**Concurrent creation** (file exists on filesystem and in repo, but not in snapshot):

This is a conflict. Both sides independently created a file with the same name.
The fundamental problem here is that we now have two different automerge
documents representing the same logical file - the one in the repo (which the
remote created) and one we would create locally for the filesystem version.
Unlike content conflicts which automerge can merge automatically, we cannot
merge two different documents into one. We must choose which document URL
becomes the canonical one for this path.

The resolution strategy is:

1. **Adopt the remote document as canonical.** The document URL in the repo's
   directory entry becomes the authoritative document for this path. This
   ensures all peers agree on which document to use.

2. **Merge the local content into the remote document.** We apply the local
   filesystem content as a concurrent edit to the remote document. Since the
   remote document was just created, we use `transaction_at` with empty heads
   (the document's initial state) to apply the local content. Automerge will
   then merge both versions of the content.

3. **Update the filesystem.** After merging, we read the merged state from
   the document and write it to the filesystem.

4. **Record in snapshot.** The snapshot records the remote document's URL
   (now canonical) with the merged heads.

If the contents happen to match exactly, the merge is trivial and produces
identical content. If they differ, automerge's merge semantics apply - for
text content this means both sets of changes are preserved, which may require
user cleanup but ensures no work is lost.

The key principle is that the repo's directory entry is authoritative for
document URL assignment. A local-only file has no URL until we create one
and add it to the repo; if the repo already has a URL for that name, we must
use it.

#### Handling File Deletion

**Local deletion** (file exists in snapshot and repo, not on filesystem):

1. We need to determine if this is a clean deletion or a conflict
2. Load the repo state at the snapshot heads
3. If the current repo heads match the snapshot heads (no remote changes):
   - Remove the entry from the parent directory's `docs` array
   - The automerge document can be left orphaned (or garbage collected later)
4. If the repo has changed since the snapshot:
   - This is a conflict: user deleted locally but someone else modified remotely
   - Options: restore the file with remote changes, or keep it deleted
   - For now, we restore the file to make the remote changes visible

**Remote deletion** (file exists in snapshot and filesystem, not in repo):

The file was removed from the repo since the last snapshot. We unconditionally
delete the local file from the filesystem, regardless of whether it has local
modifications. This matches the behavior of pushwork and keeps the semantics
simple: if a file is removed from the repo, it is removed everywhere.

1. Delete the file from the filesystem
2. Remove the file from the snapshot

Any local changes to the file are lost. Users who need to recover such changes
can use external version control or backup systems.

**Concurrent deletion** (file exists in snapshot only):

Both sides deleted the file. No action needed beyond removing it from the
snapshot.

#### Handling Directory Creation and Deletion

Directory creation and deletion follow the same principles as files, but with
additional considerations for their contents.

**Local directory creation**:

1. Create a new automerge document for the directory with empty `docs` array
2. Recursively process all contents of the directory (creating documents for each)
3. Add entries to the new directory document for each child
4. Add the directory to its parent's `docs` array

**Remote directory creation**:

1. Create the directory on the filesystem
2. Recursively process all entries in the directory document, creating files
   and subdirectories as needed
3. Record the directory and all its contents in the snapshot

**Local directory deletion**:

When a directory is deleted locally, all its contents are implicitly deleted
too. We need to check for conflicts with any file inside the directory tree:

1. Recursively check all entries that were in the snapshot under this directory
2. For each entry, check if the repo has changes since the snapshot
3. If any entry has remote changes, we have a conflict - restore the directory
4. If no entries have remote changes, remove the directory entry from the parent
   and consider all child documents orphaned

**Remote directory deletion**:

Similar to files, but we must handle the case where local files were added:

1. Check if the filesystem directory contains exactly what the snapshot recorded
2. If there are new local files, we have a conflict - re-create the directory
   in the repo with the new local contents
3. If the contents match, recursively delete the directory from the filesystem

#### Move Detection

The creation and deletion cases described above handle the simple scenarios, but
there's an important optimization: detecting moves. When a user renames or moves
a file, the filesystem presents this as a deletion at the old path and a
creation at the new path. Without move detection, we would delete the old
automerge document and create a new one, losing the document's history and
identity. With move detection, we can instead update the document's `name` field
and relocate it in the directory structure, preserving its URL and history.

**When to detect moves**

Move detection applies in two symmetric scenarios:

1. **Local move**: A file was deleted locally and a similar file was created
   locally. That is, the deleted file exists in the snapshot and repo but not
   the filesystem, and the created file exists in the filesystem but not in
   the snapshot or repo.

2. **Remote move**: A file was deleted remotely and a similar file was created
   remotely. That is, the deleted file exists in the snapshot and filesystem
   but not in the repo, and the created file exists in the repo but not in
   the snapshot or filesystem.

In both cases, we're looking for pairs of (deleted, created) entries where
the content is sufficiently similar that they likely represent the same
logical file being moved rather than an unrelated deletion and creation.

**Similarity measurement**

We use the Sørensen-Dice coefficient to measure similarity between files.
For two files A and B, we compute the coefficient over their content
(treating them as sets of bigrams for text files, or as raw byte sequences
for binary files):

```
similarity = 2 * |A ∩ B| / (|A| + |B|)
```

This produces a value between 0 (completely different) and 1 (identical).
A threshold of 0.5 or higher is typically sufficient to identify moves with
reasonable confidence - files that share more than half their content in
common are likely the same file with modifications.

**The move detection algorithm**

For each sync operation, after classifying all entries into the categories
from the table above, we run move detection:

1. Collect all local deletions and local creations into separate lists
2. Collect all remote deletions and remote creations into separate lists
3. For local moves:
   a. For each (deleted, created) pair from the local lists, compute similarity
   b. Find the best matching pairs where similarity exceeds the threshold
   c. Use a greedy matching algorithm: sort pairs by similarity descending,
      then assign each deletion to at most one creation (and vice versa)
4. Repeat step 3 for remote moves using the remote lists

**Handling detected moves**

Once we've identified a move, the sync operation changes:

**Local move** (user moved a file on the filesystem):

Instead of deleting the old document and creating a new one:

1. Remove the entry from the old parent directory's `docs` array
2. Add an entry to the new parent directory's `docs` array, using the
   *existing* document URL from the old location
3. Update the document's `name` field to reflect the new filename
4. If the content also changed, apply the content changes as usual
5. Update the snapshot to record the document at its new path

**Remote move** (file was moved in the repo):

Instead of deleting the local file and creating a new one:

1. Move (or rename) the file on the filesystem from old path to new path
2. If the content also changed, apply the merged content to the filesystem
3. Update the snapshot to record the document at its new path

**Edge cases**

A few complications arise in practice:

- **Cross-directory moves**: The algorithm handles these naturally since we're
  comparing all deletions against all creations, not just within a single
  directory.

- **Move + significant edit**: If the similarity is below threshold due to
  heavy editing, we treat it as a deletion and creation. This is acceptable -
  we lose document history but preserve correctness.

- **Multiple similar files**: The greedy matching ensures each file is matched
  at most once. If file A was deleted and files B and C were created with
  similar content, only the best match becomes a move; the other is a creation.

- **Conflicting moves**: If locally a file was moved from X to Y, but remotely
  the same file was moved from X to Z, we have a conflict. The file cannot
  exist at both Y and Z. We resolve this by applying the same principle used
  elsewhere: the local repo state is authoritative over the filesystem state.
  
  This means the remote move wins. The file ends up at path Z (where the repo
  says it should be), and the filesystem is updated accordingly. Any content
  changes from the local file at Y are merged into the document, so no edits
  are lost - only the local user's choice of destination is overridden.
  
  This resolution is deterministic and order-independent across peers:
  
  1. Automerge's CRDT semantics ensure that after peer-to-peer sync, all peers
     have identical local repo state.
  2. When each peer syncs their filesystem to their local repo, they all apply
     the same rule: the repo's destination wins over the filesystem's.
  3. Therefore all filesystems converge to the same state.
  
  The only ordering dependency is that the first peer to sync their filesystem
  (before receiving updates from others) effectively "sets" the destination in
  the repo. But once that state propagates through peer-to-peer sync, all other
  peers will converge to it when they sync their filesystems.

#### The Complete Sync Algorithm

Putting it all together, the complete algorithm for synchronizing with local
state is:

1. Scan the current state of the filesystem
2. Load the snapshot
3. Load the current state of all directory documents from the repo
4. For each directory, starting from the root:
   a. Compute the three-way diff (filesystem vs snapshot vs repo entries)
   b. For entries that exist everywhere, apply the content sync algorithm
   c. For local creations, create new documents and update parent
   d. For local deletions, check for conflicts and remove or restore
   e. For remote creations, write to filesystem
   f. For remote deletions, check for conflicts and delete or preserve
   g. For concurrent creations/deletions, apply conflict resolution
5. Save a new snapshot reflecting the synchronized state
6. If there are unresolved conflicts, report them to the user


# Appendix

## Snapshot Schema

```json
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

#### Top-Level Fields

| Field              | Type          | Description                                    |
|--------------------|---------------|------------------------------------------------|
| `timestamp`        | integer       | Unix timestamp (ms) when snapshot was saved    |
| `rootPath`         | string        | Absolute path to the synced directory          |
| `rootDirectoryUrl` | string/null   | Automerge URL of the root directory document   |
| `files`            | array         | Array of `[path, entry]` tuples for files      |
| `directories`      | array         | Array of `[path, entry]` tuples for directories|

#### File Entry

```json
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

```json
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
