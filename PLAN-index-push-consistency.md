# Fix index push consistency across CLI commands

## Problem

`blu sync` writes blobs to the backend but only pushes encrypted index
files to the backend when `--push` is passed. Every other CLI command
that modifies indexes (`add`, `encrypt-files`, `delete-files`,
`tagger`, `defrag-blobs`) writes indexes locally and never pushes them
to the backend at all. This creates a half-state where the backend has
blobs but stale or missing indexes, which is exactly how data loss
occurred: the local directory was deleted, the S3 backend had blobs,
but no indexes, making the blobs unrecoverable.

`blu serve` already does the right thing: its debounced flush writes
indexes locally AND pushes them to the backend atomically after every
write. The CLI path should follow the same principle.

## Design decisions

Indexes are always written locally. That is unchanged and non-negotiable:
the local index is the primary working artifact that powers search,
listing, status, and the `blu serve` redb population. The remote copy is
an additive, synced backup for recovery and multi-machine access.
`push_indexes` reads the local index files off disk and uploads them, so
local indexes are a prerequisite for pushing, never a replacement.

The change is to make the push automatic instead of opt-in, resolved
against these three decisions:

1. Push failure is a hard error. The backend is the source of truth; a
   silent success when the remote did not receive the index is the exact
   trap that causes orphaned blobs. On failure, the local index is
   already written, so the error must say so clearly, e.g. "Local indexes
   updated, but push to backend `<name>` failed: <reason>. Re-run when the
   backend is reachable." No speculative offline/track-and-retry machinery
   is built now.

2. Every index-modifying command pushes (uniform, not surgical). One
   mental model: after any blu command that changes the vault, the backend
   is current. Index-only uploads are tiny; predictability outweighs the
   cost of `add`/`tagger` touching the network.

3. No standalone push command. With uniform auto-push plus hard-fail
   there is no half-state to reconcile, so a standalone `blu push` has no
   job. It is deliberately omitted.

This is a breaking change (the `--push` flag is removed from `blu sync`).
Per AGENTS.md greenfield rules, breaking changes are welcome when they
produce a cleaner design.

## Stage 1: Add push_indexes to every index-modifying CLI command

Every command that calls `cfg.write_*_index()` must also call
`cfg.push_indexes(&backend)` afterward. The backend is resolved the
same way it is today: `--backend` flag if present, otherwise
`cfg.default_backend`. Push failures surface the hard-fail message from
decision 1.

1a: `blu add` (`src/cli/add.rs`) -- writes plain index, currently
     never pushes. Add backend init + push_indexes after
     write_plain_index.

1b: `blu encrypt-files` (`src/cli/encrypt_files.rs`) -- writes blob
     index, currently never pushes. Add push_indexes after
     write_blob_index.

1c: `blu delete-files` (`src/cli/delete_files.rs`) -- writes all
     three indexes, currently never pushes. It already initializes a
     backend for blob deletion. Add push_indexes after the three
     write_*_index calls. If no backend was initialized (no dead
     blobs to delete), initialize the default backend for the push.

1d: `blu tagger` (`src/cli/tagger.rs`) -- writes tag index, currently
     never pushes. Add backend init + push_indexes after
     write_tag_index.

1e: `blu defrag-blobs` (`src/cli/defrag_blobs.rs`) -- writes blob
     index, currently never pushes. It already initializes a backend.
     Add push_indexes after write_blob_index.

## Stage 2: Remove --push flag from blu sync

2a: Remove the `push` field from `SyncArgs` in `src/cli/clapargs.rs`.

2b: Remove the `if args.push` conditional in `src/cli/sync.rs` and
     make `cfg.push_indexes(&backend)` unconditional after index
     writes. The `backend` variable already exists in scope (used for
     blob writes), so the push targets the same backend that received
     the blobs.

## Stage 3: Push indexes after blu backend mirror

3a: In `src/cli/backend_cmd.rs`, after the mirror operation completes
     successfully, push indexes to the destination backend via
     `cfg.push_indexes(&to_backend)`. This ensures that after
     mirroring blobs to a new backend, that backend also has the
     indexes needed to use them.

## Stage 4: Update tests

4a: Update any tests that assert on the `--push` flag or its absence.

4b: Add a test that verifies `blu sync` (without any flags) pushes
     indexes to the backend by default.

## Stage 5: Update documentation

5a: Update `BLU_SERVE_DESIGN.md` if it references `--push`.

5b: Update `TODO.md` if it references `--push`.

5c: Update `AGENTS.md` if it references `--push`.
