# Parallel I/O for Backend Operations

Three commands in `src/cli/backend_cmd.rs` do sequential network round-trips
per blob, which is heinously slow over S3:

| Command        | Line | Pattern                                  | Impact                         |
|----------------|------|------------------------------------------|--------------------------------|
| `mirror`       | 278  | `for path { exists + read + hash + write }` | ~3s per blob, 30s+ for 12     |
| `diff`         | 410  | `for path { exists(from) + exists(to) }`    | 2 serial round-trips per blob |
| `list --stats` | 114  | `for path { exists }`                       | 1 round-trip per blob per BE  |

All backend methods are already async (Tokio), and `tokio = { features =
["full"] }` gives us `JoinSet` and `Semaphore` with zero new deps. The
backends (`Local`, `AmazonS3`) are composed of `Send + Sync` fields, so
spawning tasks is straightforward once we can clone them.

## Stage 1: Make `BackendKind` cloneable

- 1a: Derive `Clone` on `Local` (just holds a `PathBuf`)
- 1b: Derive `Clone` on `AmazonS3` (all fields are Clone; `aws_sdk_s3::Client`
  is Arc-backed internally, cheap to clone)
- 1c: Derive `Clone` on `BackendKind`

## Stage 2: Add `--jobs` / `-j` flag

- 2a: Add `jobs: usize` to `BackendMirrorArgs` with `default_value = "16"`
- 2b: Add `jobs: usize` to `BackendDiffArgs` with `default_value = "16"`
- Skip for `list --stats`; hardcode a sensible default there.

## Stage 3: Parallelize `mirror`

- 3a: Define a small `MirrorResult` enum (`Copied(u64)`, `Skipped`,
  `Failed(String)`) for clean per-task return values
- 3b: Replace the `for` loop with `JoinSet` + `Arc<Semaphore>` bounded
  concurrency. Each spawned task: acquire permit, exists check, read from
  source, re-hash, write to dest, return `MirrorResult`
- 3c: Aggregate results after all tasks drain from the JoinSet

## Stage 4: Parallelize `diff`

- 4a: Same `JoinSet` + `Semaphore` pattern
- 4b: Within each task, use `tokio::join!` to check both backends concurrently
  (two independent exists calls per blob; no reason they should be serial
  either)

## Stage 5: Parallelize `list --stats`

- 5a: Same `JoinSet` + `Semaphore` pattern (hardcoded concurrency of 16)

## Design decisions

**No new crate dependencies.** `JoinSet` + `Semaphore` from Tokio is the
canonical pattern for bounded task-parallel I/O. `futures::buffer_unordered`
would be an alternative but adds a dep for no gain.

**No shared utility function across the three call sites.** They each have
different per-task logic (mirror does read+hash+write, diff does
double-exists, list does single-exists). A generic `run_concurrent` helper
would over-abstract and obscure intent.

**`Clone` on the backends is the right primitive (not `Arc` wrapping).** The
types are designed for it (`aws_sdk_s3::Client` is `Arc` internally), and
cloning into each task is explicit and idiomatic.

**Progress logging:** S3 already emits `info!()` per write; concurrent tasks
will interleave those lines, which is fine and expected. Error output uses
`eprintln!` with the path, so it is self-identifying.

**Default 16 concurrent tasks:** AWS S3 handles this easily (AWS CLI itself
defaults to 10 for `s3 sync`). Configurable via `--jobs` for the user to tune.

## Addendum: Progress bars

Add `indicatif` (the standard Rust progress bar crate) as a new dependency.
This is a small lift that slots in naturally alongside the JoinSet pattern,
since each completed task just increments the bar.

**Dependency:** `indicatif = "0.17"` in `Cargo.toml`.

**Integration with logging:** `indicatif` provides `ProgressBar::suspend()`
which lets `log` / `simplelog` output coexist without corrupting the bar.
Alternatively, use `indicatif-log-bridge` to wire them together, but that
adds another dep; `suspend()` is simple enough.

**Where bars appear:**

- `mirror`: `ProgressBar` with total = blob count. Each task increments on
  completion. Message shows copied/skipped/failed tallies. Bytes transferred
  shown via `ProgressStyle` with `binary_bytes` for throughput.
- `diff`: `ProgressBar` with total = blob count, increment per blob checked.
- `list --stats`: `ProgressBar` with total = blob count per backend.

**Dry-run mode (`mirror --dry-run`):** Still show the bar (the exists-check
phase is the slow part anyway). Style it differently or use a spinner to make
it visually distinct.

**When to implement:** Fold into Stages 3, 4, and 5 respectively. The bar is
created before the JoinSet loop, and `inc(1)` is called in the
result-aggregation loop as each `JoinHandle` resolves. No channels needed;
the progress bar's internal state is `Arc<Mutex<...>>` and safe to share, but
we do not even need to share it since all `inc()` calls happen on the main
task that drains the JoinSet.
