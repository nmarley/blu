# PLAN: Backup progress reporting

## Problem

`blu backup` prints nothing until the entire run finishes. Long backups
(large video trees, S3 uploads) look hung. Restore and backend mirror
already use `indicatif`; backup does not.

## Goals

- Default-on progress for `blu backup` (opt out with `--quiet` / non-TTY)
- Overall percentage plus phase line plus capped active rows (no infinite scroll)
- Honest byte-based totals for large files
- Strict separation of concerns: domain emits facts; only a UI task draws
- Live visibility into pipelined blob uploads, with early failure if puts fail
- Interrupt remains safe (backup stays idempotent); progress makes status obvious

## Non-goals

- Full-screen ratatui TUI
- Per-chunk active rows (would flood the terminal)
- Migrating restore/mirror onto the new helper in this work
- Changing encryption, chunking, or backend semantics beyond progress seams
  and early upload-error propagation

## Architecture

Three layers. No `indicatif` types outside the CLI UI consumer.

```
CLI orchestrator (backup.rs)
  plans work, runs phases, handles errors
  emits BackupEvent via BackupProgress sink
        |
        |  tokio::mpsc<BackupEvent>
        v
UI consumer task (tokio::spawn)
  ONLY place that touches MultiProgress / ProgressBar
  quiet or non-TTY: drain events, draw nothing
        ^
        |  optional telemetry Sender (facts only)
Domain (block, blob)
  knows nothing about bars
  IndexReporter / BlobBuffer events only
```

### Event protocol

Telemetry only. No ciphertext, keys, or business payloads on the UI channel.

```rust
enum BackupPhase {
    Index,
    EncryptUpload,
    Finalize,
    PushIndexes,
}

enum BackupEvent {
    Phase(BackupPhase),

    IndexPlan { files: u64, bytes: u64 },
    IndexFileStarted { path: PathBuf, bytes: u64 },
    IndexFileProgress { bytes_delta: u64 },
    IndexFileFinished,

    EncryptPlan { chunks: u64, bytes: u64 },
    ChunkSealed { bytes: u64 },
    BlobSealed { short_id: String, bytes: u64 },
    BlobUploaded { short_id: String, bytes: u64 },
    BlobUploadFailed { short_id: String, error: String },

    PushIndexesFinished,
}
```

### Sink

```rust
trait BackupProgress: Send + Sync {
    fn emit(&self, event: BackupEvent);
}

struct ChannelProgress { tx: mpsc::Sender<BackupEvent> }
struct NullProgress;
```

- Async paths: `send().await` or non-blocking try with backpressure policy
- Sync index path: `blocking_send` on a bounded channel (UI must keep up;
  size the channel so normal runs never stall on draw)
- Tests and library callers use `NullProgress` or `()`

### Overall percentage (honest work units)

```
total = bytes_to_hash + plain_bytes_of_missing_chunks
done  = hashed_bytes + sealed_chunk_bytes
```

Uploads do not double-count overall progress (seal already advanced the
byte counter). Uploads appear on the phase line and in capped active
slots (`N in flight`). After seal work hits 100%, phase becomes
waiting for remaining uploads, then pushing indexes.

### UI layout (TTY, not quiet)

```
Backup  ████████████░░░░░░░░  62%  1.8/2.9 GiB  [04:12]
Phase   encrypt+upload  48/60 blobs  3 in flight  12.4 MiB/s
  ▸ hash  vacation.mp4              812 MiB
  ▸ seal  a1b2c3d...                64.0 MiB
  ▸ put   c3d4e5f...                64.0 MiB
```

- `MultiProgress` with steady tick so the display never looks wedged
- Hard cap on active rows (default 4)
- Active row kinds: hashing file, sealed blob, in-flight put
- `finish_and_clear` before the existing summary `println!` lines
- Styles aligned with restore/mirror templates where practical

### Domain seams

#### Indexing (`PlainIndex`)

Keep `block` free of UI crates. Add a reporter with no-op defaults:

```rust
pub trait IndexReporter {
    fn on_file_start(&mut self, path: &Path, len: u64) {}
    fn on_file_bytes(&mut self, n: u64) {}
    fn on_file_end(&mut self, path: &Path) {}
}
impl IndexReporter for () {}
```

- Call sites inside the chunk loop in `hash_and_add_file` so multi-gig
  files tick bytes, not just one file step
- Existing `add` becomes `add_with_reporter(..., &mut ())` or equivalent
  so current callers stay unchanged
- CLI adapter maps reporter calls to `BackupEvent`
- Pre-scan via existing `walk_files_with_sizes` before indexing to emit
  `IndexPlan` and set the overall denominator

#### Encrypt/upload (`BlobBuffer`)

Today completions are only observed in `finalize` (`Vec<JoinHandle>`).
That hides multi-minute S3 puts and can keep sealing after the backend
is already failing.

Required shape:

1. Optional event sender on `BlobBuffer` (blob-local events; CLI maps to
   `BackupEvent`)
2. Replace bare handle vec with `JoinSet` (or equivalent) plus metadata
   `(short_id, bytes)`
3. Upload task completion emits uploaded / failed
4. Poll completions during `add_chunk` (`try_join_next`) so:
   - in-flight count is accurate
   - backend errors abort early mid-loop
   - `finalize` only drains the tail
5. `seal_and_upload` emits sealed when ciphertext is handed to the
   background put

Still no `indicatif` in `blob.rs`.

#### Orchestrator (`backup.rs`)

```
spawn UI consumer
pre-scan -> IndexPlan
index with reporter
compute missing chunks -> EncryptPlan
encrypt loop (domain emits seal/upload; poll inflight)
finalize (drain remaining uploads)
write blob index
push indexes (phase line)
drop sender -> UI exits cleanly
existing summary println!
```

### Quiet and non-TTY

- `--quiet` / `-q` on `BackupArgs`
- Non-TTY stderr: same as quiet for drawing (indicatif draw target / explicit check)
- Event emission may still run; renderer is a no-op drain
- Headless e2e and tests must not require a TTY

### Error handling

- `BlobUploadFailed` (or join error) fails the backup with a clear
  `BluError`; UI suspends/clears bars before the error path prints
- Index and encrypt integrity checks stay as they are (hash mismatch etc.)
- Ctrl-C: no new signal handling required; backup remains re-runnable

### Module layout

- `src/cli/progress.rs`: shared bar style helpers + MultiProgress slot pool
  (backup uses first; restore/mirror migration is out of scope)
- `src/cli/backup.rs`: orchestration, `BackupEvent`, sink, UI consumer
  (keep event types next to the only consumer unless they grow)
- `src/block/index.rs`: `IndexReporter` + wire-up in hash loop
- `src/blob.rs`: optional events + JoinSet completion polling

Prefer the smallest public surface that preserves SoC. Do not invent a
crate-wide progress framework beyond what backup needs.

## Stages

Stage 1: Progress primitives and event protocol
  1a: Add `src/cli/progress.rs` with MultiProgress helpers, capped active
      slots, quiet/non-TTY no-op path, shared bar templates
  1b: Define `BackupEvent`, `BackupPhase`, `BackupProgress`,
      `ChannelProgress`, `NullProgress` (in backup module or progress module)
  1c: Unit-test null sink and slot cap behavior where practical without a TTY

Stage 2: Index reporter seam
  2a: Add `IndexReporter` trait with no-op default impl for `()`
  2b: Thread reporter through `add` / `hash_and_add_file`; emit start,
      per-chunk bytes, end
  2c: Keep existing callers compiling via `()` reporter
  2d: Tests that a reporter observes expected file/byte callbacks on a
      small fixture tree

Stage 3: BlobBuffer upload visibility and early failure
  3a: Optional blob event sender on `BlobBuffer`
  3b: JoinSet (or equivalent) with seal/upload metadata
  3c: Emit sealed on handoff; uploaded/failed on task completion
  3d: Poll completions inside `add_chunk` / between chunks; propagate
      upload errors immediately
  3e: `finalize` drains remaining tasks and surfaces errors
  3f: Extend blob tests for event emission and failed-upload propagation
      (local backend is enough)

Stage 4: Wire `blu backup`
  4a: Add `--quiet` / `-q` to `BackupArgs`
  4b: Spawn UI consumer; use `ChannelProgress` unless quiet/non-TTY
  4c: Pre-scan paths with `walk_files_with_sizes` -> `IndexPlan`
  4d: Index phase with reporter adapter -> index events
  4e: Build missing-chunk plan -> `EncryptPlan`; encrypt loop with
      `ChunkSealed` ticks; map blob events into `BackupEvent`
  4f: Finalize + write blob index + `push_indexes_or_fail` with phase events
  4g: Drop sink, await UI task, print existing summary lines
  4h: Ensure smoke/e2e paths stay quiet-friendly (no TTY required)

Stage 5: Verify
  5a: `cargo test`
  5b: `cargo clippy` and `cargo fmt -- --check`
  5c: Manual TTY check: `blu backup` on a multi-hundred-MB tree shows
      continuous overall/phase/active updates and a clean summary
  5d: Manual quiet check: `blu backup -q` prints summary only
  5e: Confirm interrupt + re-run still completes (idempotent)

## Implementation notes

- Bound the event channel (start around 256); UI must never be the
  throughput bottleneck, but neither may events allocate without bound
- Short blob ids: existing hash dbg_short style (about 7 hex chars)
- Human sizes via existing `crate::format::human_bytes`
- Do not log secrets, passphrases, or plaintext paths beyond what the
  CLI already shows for backup paths
- `encrypt_files` may keep working via default `BlobBuffer::new` with no
  sender; optional follow-up to share progress, not required here
- Commit boundaries: one atomic commit per completed stage (or smaller
  logical units if a stage is large); no plan-file edits mixed into code
  commits

## Success criteria

- A multi-gigabyte `blu backup` to S3 never sits mute for minutes
- Overall % moves during hash and during seal; uploads visible as active
  rows and in-flight count
- Backend put failure fails backup promptly with bars cleared
- `--quiet` and non-TTY runs produce no progress bars
- All tests green; no new clippy/fmt debt
