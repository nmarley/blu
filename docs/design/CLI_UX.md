# CLI UX: git-like vault

Canonical design for blu's user-facing command model. Encrypted,
content-addressed archival with a shared catalog and deliberate
plaintext checkout. Not a Dropbox-style sync folder.

## Product model

blu is an encrypted, content-addressed **vault** with:

- A **shared catalog** (encrypted indexes) and **opaque blobs** on a backend
- A local directory that is an optional **checkout** of plaintext, not a
  continuous mirror of the vault
- Multi-device use via the same identity and backend: publish, pull catalog,
  restore what you need

Remote (the default backend) is the vault of record for catalog and
ciphertext. The working tree is never auto-filled on pull. Vaults may be
GB or TB scale; auto-restore is out of scope.

### Day-to-day multi-device

```sh
# Machine A: publish local files into the vault
blu backup path/or/.

# Machine B: refresh catalog, then checkout plaintext deliberately
blu pull
blu status
blu restore --path 'music/*'   # or --all when intentional
```

## Vocabulary

Hard cut. Old command names are removed from the user-facing surface.
No deprecated aliases or muscle-memory shims.

| Command | Role |
|---------|------|
| `blu backup [paths]` | Index paths, encrypt, upload blobs, merge remote indexes, push indexes |
| `blu pull` | Fetch and union-merge remote indexes (catalog only; no plaintext) |
| `blu pull --force` | Hard reset: discard local indexes, take remote only |
| `blu restore` | Materialize plaintext from catalog + blobs |
| `blu rm` | Tombstone + cascade (multi-device safe) |
| `blu status` | Working tree vs catalog vs remote |
| `blu ls` | Catalog listing (not directory listing) |
| `blu open` / `blu init` | Open existing vault / create vault |

Removed from user-facing help (plumbing only, or deleted if unused):

- `sync` (replaced by `backup`)
- `restore-files` (replaced by `restore`)
- `delete-files` (replaced by `rm`)
- `add` (deleted; not a staging area in this model)
- `encrypt-files`, `write-index` (already hidden)

Optional later (not required for this design): `push` as a synonym for
`backup` if git muscle memory is wanted.

`backup` default paths keep "current tree" semantics of the former
`sync` command unless a status-driven "only changed" mode is added later.

## Invariants

1. **Publish completeness.** After `backup`, `rm`, or any command that
   mutates catalog + blobs, either the remote catalog reflects the new
   state or the command fails with a clear error. No silent success with
   blobs uploaded and indexes unpublished. Indexes are never pushed when
   live plain-index chunks lack blob-index ciphertext (catalog without
   content is not a valid durable state).

2. **Merge before publish.** The shared push path always fetch+merges
   remote indexes before upload. That is the only multi-writer path.

3. **Pull never writes plaintext** by default.

4. **Orphan blobs are bugs, not a workflow.** Durable state where blobs
   exist on the backend without a published index entry that references
   them must be detectable (`status` / `doctor`) and prevented by publish
   completeness. Backend `list` + orphan GC is a follow-up when the
   storage API supports it.

## Status shape

`blu status` answers three questions:

1. What local files are not yet in the catalog / not published?
2. What catalog entries are not checked out on disk (count + total size)?
3. Is the local catalog in sync with, ahead of, behind, or diverged from
   the remote?

Illustrative output (wording free to refine in implementation):

```text
On vault …  backend s3://…/prefix
Catalog: N files (size)    Remote: in sync | ahead | behind | diverged
Checkout: P present, M missing (size) — blu restore ���

Unpublished local files:
  …

Not in checkout (in catalog only):
  …
```

`blu pull` success copy should remind that only the catalog changed and
point at `blu restore` when entries are missing on disk.

## Non-goals

- Folder-sync or auto-materialize on pull
- Snapshot/history renames (catalog remains live content-addressed state)
- Crypto or index-merge algorithm changes
- First-class `add` staging area (git index analog)
- FUSE, GUI, Windows
- Soft deprecation aliases for old command names

## Follow-ups

- [x] Doctor orphan blob scan (`blob-orphans` via backend `list_blob_paths`)
- Orphan blob reclaim (explicit destroy; not auto on doctor)
- Multi-device-safe blob GC (grace period / refcount / tombstone-first)
  before any default-on reclaim
- Optional user-facing `backend list-blobs` operator plumbing
- Optional `push` synonym for `backup`

## Success criteria

- Multi-device dogfood needs only: `backup` on the writer, `pull` +
  `restore` on the reader, with `status` making divergence obvious
- No documented path that says "encrypt then somehow upload"
- A failed index push cannot look like a successful backup
- Help text matches the git-like vault model in one screen
