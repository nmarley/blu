# Multi-Backend Architecture Specification

This document is the implementation specification for named, multi-backend
storage in blu. It supersedes the exploratory notes in `MULTI-BACKEND.md`
with concrete decisions, detailed phases, and per-stage implementation
guidance grounded in the current codebase.

## Problem

blu supports exactly one storage backend, configured as `[backend]` in
`config.toml`. Transitioning from local to S3 (or vice versa) has no
supported path. Data stored on the old backend becomes orphaned, and the
user must manually copy blobs and hand-edit TOML to switch.

## Driving Use Case

A vault has real data on local storage. The user wants to shift to S3.
Today the options are: (a) manually copy `.blu/data/` contents into the
S3 bucket with the right prefix, then hand-edit config.toml, or (b) start
fresh on S3 and re-sync everything. Neither is acceptable.

## Design Decisions

These decisions were reached after a full exploration of the codebase.
Each maps to one of the open questions from the original design brief.

### D1. Config format: named map

Use `[backends.<name>]` tables with a top-level `default_backend` key.

    [default_backend]
    name = "s3-prod"

    [backends.local]
    type = "local"
    path = ".blu/data"

    [backends.s3-prod]
    type = "s3"
    bucket = "my-bucket"
    region = "us-east-1"

Rationale: a named map deserializes directly into
`HashMap<String, BackendConfig>` via serde. Avoids the ordering
ambiguity of `[[backends]]` arrays and reads more naturally in TOML.

### D2. Write strategy: default-only

Writes target the default backend only. No implicit fan-out. The user
copies data between backends explicitly via `blu backend mirror`. This
avoids surprising behavior where writes silently target backends the
user may have forgotten about.

### D3. Read strategy: default-only with override

Reads target the default backend. `blu restore --backend <name>` allows
an explicit override. No automatic fallback between backends. Fallback
introduces implicit coupling that makes debugging difficult.

### D4. Migration command: mirror

`blu backend mirror --from X --to Y` copies all blobs present in the
source but missing from the target. This is the core operation for the
driving use case.

### D5. Consistency: user's responsibility (MVP)

Divergence between backends is the user's responsibility to resolve
via `mirror`. A `blu backend diff` command that reports blob-count
differences is desirable but not MVP.

### D6. Index sync: one index, push to any backend

One index across all backends. Indexes are content-addressed and
backend-agnostic. The blob index stores hash-derived relative paths,
not absolute paths, so the same index is valid regardless of which
backend holds the blobs. `--push` and `pull` accept a `--backend`
flag to target a specific backend.

### D7. Backward compatibility: auto-promote old format

If `[backend]` (singular, no name) is present and `[backends]` is
absent, treat it as a sole backend named `"default"` and set
`default_backend = "default"`. Emit a deprecation notice on stderr
suggesting the user migrate the config format.

### D8. Error handling: per-item reporting for mirror

For write-to-default-only, standard `Result` propagation applies.
For `mirror`, copy errors are reported per-blob with a summary at the
end (like rsync), not fail-fast. The user can re-run `mirror` to
retry failures.

## Codebase Findings

Summary of the current architecture relevant to this work.

### Single-backend assumption is pervasive but shallow

Only 4 CLI commands touch the backend: `sync` (`src/cli/sync.rs`),
`encrypt-files` (`src/cli/encrypt_files.rs`), `restore-files`
(`src/cli/restore_files.rs`), and `pull` (`src/cli/pull.rs`). All go
through one factory method: `Config::init_storage_backend()`
(`src/config.rs:199`).

### StorageBackend trait is incomplete

The trait (`src/storage.rs:45`) has four methods: `read_data`,
`write_data`, `exists`, `delete`. It lacks any method for writing to
a known path (as opposed to a hash-derived path). This forces
`push_indexes` and `pull_indexes` in `src/config.rs` to match directly
on the `BackendConfig` enum (3 match sites at lines 202, 287, 319),
bypassing the trait abstraction.

### S3 index push writes to hash-derived paths

`write_index_to_backend` (`src/config.rs:295-301`) writes indexes to
hash-derived paths on S3 instead of known paths. There is a TODO at
line 300 acknowledging this. `pull_indexes` reads from known paths.
This means S3 index push/pull does not round-trip correctly today.

### BlobBuffer and EncBlobReader borrow the backend

`BlobBuffer` (`src/blob.rs:31`) and `EncBlobReader` (`src/blob.rs:183`)
both borrow `&dyn StorageBackend`. A coordinator type that itself
implements `StorageBackend` can be slotted in with zero lifetime or
ownership changes.

### BlobBlockLocation stores relative paths

`BlobBlockLocation.path` (`src/blob.rs:160-166`) stores the
hash-derived relative path returned by `write_data`. These paths are
backend-agnostic (no absolute paths, no bucket names). This means
the blob index does not need to change for multi-backend.

## Implementation Phases

The work is organized into three phases. Each phase is independently
shippable. Phases contain numbered stages; each stage is one atomic,
reviewable unit of change (one commit or PR).

### Phase 1: Trait Cleanup (prerequisite)

Fix the `StorageBackend` trait so that all backend interactions go
through the trait, with no direct matching on the config enum. This
phase has value independent of multi-backend: it fixes the S3 index
push bug and removes leaky abstractions.

#### Stage 1.1: Add path-based methods to StorageBackend

Add two methods to the `StorageBackend` trait in `src/storage.rs`:

    fn write_to_path(&self, path: &Path, data: &[u8])
        -> Result<(), Box<dyn std::error::Error>>;

    fn read_from_path(&self, path: &Path)
        -> Result<Vec<u8>, Box<dyn std::error::Error>>;

These are for index files and any other data that must live at a known
path rather than a hash-derived path.

Files touched:
- `src/storage.rs` (trait definition)
- `src/storage/local.rs` (implement for Local)
- `src/storage/s3.rs` (implement for AmazonS3)

Implementation notes for each backend:

Local: `write_to_path` joins `self.datadir` with the given path,
creates parent dirs, writes. `read_from_path` joins and reads.
Essentially what `write_index_to_backend` does today for the Local
match arm (`src/config.rs:288-293`).

S3: `write_to_path` calls `put_object` with `self.path_to_key(path)`
as the key. `read_from_path` calls `get_object` with the same key
derivation. This fixes the S3 index push bug since indexes will be
written to `indexes/index.dat` (etc.) instead of hash-derived paths.

#### Stage 1.2: Refactor push_indexes and pull_indexes

Replace the 3 `match &self.backend` sites in `src/config.rs` with
calls to the new trait methods.

`write_index_to_backend` (`src/config.rs:275-307`) becomes:

    fn write_index_to_backend(
        &self,
        backend: &dyn StorageBackend,
        data: &[u8],
        path: &Path,
    ) -> Result<(), Box<dyn std::error::Error>> {
        backend.write_to_path(path, data)
    }

Or simply inline the single call at each call site, removing
`write_index_to_backend` entirely.

`pull_indexes` (`src/config.rs:313-379`) becomes a single code path
that calls `backend.exists()` and `backend.read_from_path()` for each
index, regardless of backend type. The Local-vs-S3 branching
disappears.

`init_storage_backend` (`src/config.rs:199-213`) keeps its match on
the config enum, since it is the factory that constructs the correct
impl. This is the one place where matching on the enum is appropriate.

Files touched:
- `src/config.rs`

#### Stage 1.3: Rename StorageBackend to Backend

The existing TODO at `src/storage.rs:9` notes that
`crate::storage::StorageBackend` stutters. Rename to
`crate::storage::Backend`. This is a grep-and-replace across the
codebase.

Files touched:
- `src/storage.rs`
- `src/storage/local.rs`
- `src/storage/s3.rs`
- `src/blob.rs`
- `src/config.rs`
- `src/cli/sync.rs` (if it references the trait directly)

This stage is optional but cheap and improves readability for all
subsequent work.

### Phase 2: Multi-Backend Plumbing

Core config and runtime changes to support N named backends.

#### Stage 2.1: Multi-backend config format

Change `Config` in `src/config.rs` to support both the old singular
`[backend]` format and the new `[backends.<name>]` format.

New fields on `Config`:

    /// Named storage backends.
    #[serde(default)]
    pub backends: HashMap<String, backend::BackendConfig>,

    /// Name of the default backend for reads and writes.
    #[serde(default)]
    pub default_backend: Option<String>,

    /// Legacy singular backend (deprecated).
    /// Populated only when deserializing old-format configs.
    #[serde(default)]
    backend: Option<backend::BackendConfig>,

Add a `Config::resolve_backends()` post-deserialization step (called
from `read_config` after `toml::from_str`) that:

1. If `backend` (singular) is `Some` and `backends` is empty, moves
   the value into `backends` under the key `"default"`, sets
   `default_backend = Some("default")`, and emits a deprecation
   warning to stderr.
2. If `backends` is non-empty and `default_backend` is `None`, returns
   an error ("no default backend specified").
3. If `default_backend` names a key not in `backends`, returns an
   error.

The old `backend` field becomes `Option<BackendConfig>` with
`#[serde(default)]` so that new-format configs (which omit `[backend]`)
deserialize cleanly.

Files touched:
- `src/config.rs` (Config struct, read_config, Default impl)
- `src/config/backend.rs` (no structural changes, but verify serde
  compat)

Testing:
- Unit test: old-format TOML round-trips through resolve_backends and
  produces `backends["default"]`.
- Unit test: new-format TOML with two backends parses correctly.
- Unit test: missing `default_backend` with non-empty `backends` is
  an error.

#### Stage 2.2: Update init_storage_backend for named backends

Change `Config::init_storage_backend()` to look up the default backend
from `self.backends` using `self.default_backend`.

Add `Config::init_named_backend(name: &str)` that looks up a specific
backend by name.

Both methods return `Box<dyn StorageBackend>` (or `Box<dyn Backend>`
if the rename from Stage 1.3 landed).

Files touched:
- `src/config.rs`

#### Stage 2.3: Update CLI commands

The 4 CLI commands that call `init_storage_backend()` continue to work
unchanged (they get the default backend). No functional changes here,
just verify that the call sites compile and tests pass after the config
struct changes.

Files touched (verification only, may need minor signature updates):
- `src/cli/sync.rs`
- `src/cli/encrypt_files.rs`
- `src/cli/restore_files.rs`
- `src/cli/pull.rs`

#### Stage 2.4: Update push_indexes and pull_indexes for named backends

Add a `backend_name: Option<&str>` parameter to `push_indexes` and
`pull_indexes`. If `None`, use the default. This enables
`blu sync --push --backend s3-prod` and `blu pull --backend s3-prod`.

Files touched:
- `src/config.rs`

#### Stage 2.5: Update init command for new config format

When `blu init` creates a new vault, write the new-format config with
`[backends.default]` instead of `[backend]`. New vaults should never
use the deprecated format.

Files touched:
- `src/cli/init.rs`

### Phase 3: Backend Management CLI

User-facing commands for managing named backends.

#### Stage 3.1: Subcommand group skeleton

Add `blu backend` as a subcommand group with stubs for:
`add`, `list`, `remove`, `set-default`, `mirror`.

Register the subcommand group in `src/cli.rs` (or wherever clap args
are defined). Each subcommand gets its own file under `src/cli/`.

Files touched:
- `src/cli/clapargs.rs` (or equivalent) for arg definitions
- `src/cli.rs` for registration
- `src/cli/backend_cmd.rs` (new) for dispatch

#### Stage 3.2: backend add

`blu backend add <name> --type <type> [type-specific flags]`

Reads the config, inserts a new entry into `backends`, writes
config.toml. Errors if the name already exists.

Type-specific flags:
- `--type local --path <path>`
- `--type s3 --bucket <bucket> [--prefix <prefix>] [--region <region>]`

Optionally accepts `--default` to also set this as the default backend.

Files touched:
- `src/cli/backend_add.rs` (new)
- `src/config.rs` (add a `save_config` method if one does not exist)

#### Stage 3.3: backend list

`blu backend list`

Reads the config, prints each backend name, type, and whether it is
the default. Similar to `git remote -v`.

Example output:

    local       local  path=.blu/data
    s3-prod     s3     bucket=my-bucket region=us-east-1  (default)

Files touched:
- `src/cli/backend_list.rs` (new)

#### Stage 3.4: backend remove

`blu backend remove <name>`

Removes the named backend from `backends` in config.toml. Errors if
the name is the current default (user must `set-default` to another
backend first). Does not delete any blob data; that is a separate
cleanup step the user performs manually.

Files touched:
- `src/cli/backend_remove.rs` (new)

#### Stage 3.5: backend set-default

`blu backend set-default <name>`

Updates `default_backend` in config.toml. Errors if the name does not
exist in `backends`.

Files touched:
- `src/cli/backend_set_default.rs` (new)

#### Stage 3.6: backend mirror

`blu backend mirror --from <name> --to <name>`

The core migration operation. Iterates the blob index
(`BlobIndex.path_index` keys are blob file paths). For each blob path:

1. Calls `to_backend.exists(&path)`. If true, skip.
2. Calls `from_backend.read_data(&path)`.
3. Calls `to_backend.write_data(&hash, &data)`, where the hash is
   extracted from the path via `storage::hash_from_path`.

Reports progress per-blob (count, bytes transferred). On error, logs
the failed blob path and continues. Prints a summary at the end with
success/failure counts.

This requires `init_named_backend` from Stage 2.2 to construct both
the source and destination backends.

Files touched:
- `src/cli/backend_mirror.rs` (new)
- May need to make `BlobIndex.path_index` accessible (it is currently
  `pub` per `src/blob.rs:236`)

#### Stage 3.7: --backend flag on existing commands

Add `--backend <name>` to `sync`, `restore`, and `pull` commands. When
provided, use `init_named_backend(name)` instead of
`init_storage_backend()`.

Files touched:
- `src/cli/clapargs.rs` (add flag to SyncArgs, RestoreFilesArgs,
  PullArgs)
- `src/cli/sync.rs`
- `src/cli/restore_files.rs`
- `src/cli/pull.rs`

## Migration Workflow

With all phases complete, the driving use case is resolved:

    blu backend add s3-prod --type s3 --bucket my-bucket --region us-east-1
    blu backend mirror --from default --to s3-prod
    blu backend set-default s3-prod
    blu backend remove default        # optional, when confident

## Testing Strategy

### Unit tests

- Config deserialization: old format, new format, backward-compat
  promotion, error cases (missing default, unknown default name).
- `write_to_path` / `read_from_path`: Local impl round-trips data at
  a known path. S3 impl requires either localstack or mocking (existing
  S3 test pattern).
- `init_named_backend`: returns correct impl for each name, errors on
  unknown name.

### Integration tests

- Full sync/restore cycle with two Local backends (second backend
  pointing to a different temp directory). Mirror from one to the other,
  set-default, restore from the new default.
- `push_indexes` / `pull_indexes` with the new trait methods (Local
  backend, verify indexes land at known paths).

### Manual validation

- S3 index push/pull round-trip (fixes the current bug).
- `backend list` output formatting.
- Deprecation warning when loading an old-format config.

## Scope Exclusions

- New backend types (Azure Blob Storage, Google Cloud Storage,
  DigitalOcean Spaces). These are independent and can be added once
  the multi-backend plumbing exists.
- Automatic fan-out writes. Explicitly out of scope per D2.
- Automatic read fallback between backends. Explicitly out of scope
  per D3.
- `backend diff` command. Desirable but not MVP per D5.
- Backend-level encryption differences (e.g., different keys per
  backend). All backends share the vault's encryption config.

## Open Items

- Should `backend mirror` support `--dry-run`? Probably yes, but can
  be added after the initial implementation.
- Should `backend mirror` support filtering (e.g., mirror only blobs
  referenced by a specific tag)? Not for MVP.
- Should `backend list` show blob counts per backend? Requires listing
  objects in the backend, which may be slow for S3. Not for MVP.
