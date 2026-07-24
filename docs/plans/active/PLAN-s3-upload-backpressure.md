# PLAN: S3 upload backpressure and clearer errors

## Problem

Live media backup (`blackcarrot.de` / `bludata/media`, us-west-2) died mid
`encrypt+upload` with:

```text
S3 error: dispatch failure
```

From the tmux export (`hist`):

- Phase line: `31673/383010 chunks  0/246 blobs  246 in flight`
- Overall: `uploaded 0 B` after ~5m40s of encrypt
- SDK was retrying `TransientError` (standard strategy, max 3 attempts)
- Stalled-stream / minimum-throughput protection was active on puts
- First hard fail: `upload failed (06b80b0): S3 error: dispatch failure`

Encrypt seals ~64 MiB blobs faster than the network can put them.
`BlobBuffer` spawns every put into an unbounded `JoinSet` with no cap, so
ciphertext piles up in memory (~246 x 64 MiB plus an extra `to_vec()` copy
per put in `AmazonS3::write_data`). Concurrent puts starve each other;
AWS SDK classifies the resulting connector failures as transient, retries,
then surfaces `SdkError::DispatchFailure`, whose `Display` is only the
string `"dispatch failure"` (root cause lives on `Error::source()` and is
dropped by `e.to_string()`).

The overall bar reading ~373 GiB against `du -sh` ~187G is expected, not a
second bug: overall work = index hash bytes + encrypt missing-chunk
plaintext bytes (`BackupUiState::overall_total`). Network progress is the
separate `uploaded` counter, which stayed at 0 B.

Backup is still re-runnable: local `blob_index` is only written after
`finalize` succeeds. Content-addressed puts make a re-run safe (same keys
overwrite). Orphan sealed-but-unindexed objects may remain on S3 until
future GC; that is out of scope here.

## Goal

Stop unbounded put fan-out, cut the extra body copy, and print the real
connector cause when S3 fails. Scope is concurrency + errors only (no
multipart, no custom retry policy beyond what the SDK already does).

## Stages

Stage 1: Cap concurrent blob uploads in `BlobBuffer`
  1a: Add `DEFAULT_MAX_INFLIGHT_UPLOADS` (default 8) and `max_inflight` on
      `BlobBuffer`; expose via `with_max_inflight` if useful for tests
  1b: Before spawning a put, await completions until
      `inflight.len() < max_inflight` (backpressure the seal path)
  1c: Make `seal_and_upload` async (or add an async wait helper called
      from `add_chunk` / `finalize`) so the wait can use `join_next`
  1d: Unit test that seals more blobs than the cap and asserts
      `inflight.len()` never exceeds the cap (injectable max; slow or
      parking backend if needed)

Stage 2: Drop the extra S3 body copy
  2a: Change `BackendKind::write_data` / `AmazonS3::write_data` /
      `Local::write_data` to take owned `Vec<u8>` (or `bytes::Bytes`)
  2b: `ByteStream::from(data)` move, no `data.to_vec()`
  2c: Update all call sites (`blob.rs` spawn moves `encrypted`, tests,
      serve, doctor, backend_cmd, thaw)

Stage 3: Richer S3 error strings
  3a: Add `format_s3_error` (or similar) in `src/storage/s3.rs` that
      walks `std::error::Error::source()` and joins the chain
  3b: Use it for every `BluError::S3Error(e.to_string())` path in that
      file (put, get body collect, head, delete, list, restore, IT)
  3c: Unit test for the formatter with a synthetic error chain

Stage 4: Verify
  4a: `cargo test` (and clippy/fmt as usual)
  4b: Re-run the live media backup; confirm phase line stays near the
      cap (e.g. `N in flight` with N <= 8), `uploaded` advances, and a
      forced failure (if any) shows connector detail past "dispatch
      failure"

## Non-goals

- Multipart upload for large blobs
- Custom application-level retry/backoff beyond SDK defaults
- Raising SDK `max_attempts` or disabling stalled-stream protection
- Orphan blob reclaim / doctor GC of partially uploaded runs
- Progress UI redesign (overall % still tracks encrypt bytes sealed, not
  network bytes; fine for now)

## Notes

- Default of 8 concurrent ~64 MiB puts caps upload memory around ~512 MiB
  instead of tens of GiB. Raise toward 16 if the pipe stays underfed.
- `poll_completed` / fail-fast on first put error stays as-is.
- Do not commit the `hist` capture; it may contain AWS debug metadata.
  Prefer deleting or gitignoring it after diagnosis.
