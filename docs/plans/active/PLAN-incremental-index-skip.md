# PLAN: Incremental plain-index skip (mtime + size)

## Does this make sense?

Yes. Efficiency win is large and real for the common case.

| Path | Cost on ~187G media tree |
|------|---------------------------|
| Today | Full content hash every file every `blu backup` (hours, disk-bound) |
| With skip | `readdir` + `stat` per path; hash only new/changed (seconds if tree is static) |

The plain index is already serialized and loaded. The missing piece is a
**per-path** cache of filesystem identity so the walk can skip open/hash when
nothing changed. That is standard backup-tool practice (rsync, restic-style
assumptions), not a shortcut that undermines content-addressing: the content
hash remains the catalog key; mtime+size is only a **local short-circuit**.

False-negative skip (same size+mtime, different bytes) is the known tradeoff.
Mitigate with `--force-rehash` (or equivalent). Worth it for archival media
trees that almost never rewrite in place.

**Not worth doing:** “just check serialization” alone. Deserializing `index.dat`
is already cheap; the cost is re-reading file bytes.

## Problem

`PlainIndex::hash_and_add_file` always streams and hashes every file
(`src/block/index.rs`). Backup always calls it for the whole tree
(`src/cli/backup.rs`). Progress pre-scan already has sizes via
`walk_files_with_sizes`, but never compares to stored metadata.

`FileRef` is content-hash keyed:

```text
files: content_hash -> FileRef { chunkmetas, paths: HashSet<PathBuf> }
```

Paths carry no FS mtime/size. `file_times` is LWW merge clock (when content
was added to the index), not filesystem mtime. Do not overload it.

Backup `add` is also sticky: if content at a path changes, the path is added
to the new hash but not removed from the old `FileRef`. Skip work should pair
with **path unlink on content change** so the catalog stays accurate.

## Goal

On `blu backup` index phase:

1. `stat` each path (mtime + size).
2. If path is already in the index with matching stored mtime+size, **skip**
   open/hash; treat as still present.
3. If new or metadata mismatch, full hash as today; record fresh PathStat;
   if content hash changed, remove path from the previous FileRef.
4. Optional force path that always rehashes.

Greenfield: breaking index schema change is fine (bump version, no migration).

## Design

### PathStat on FileRef

Replace `FileRef.paths: HashSet<PathBuf>` with:

```rust
paths: HashMap<PathBuf, PathStat>

struct PathStat {
    mtime_secs: i64,  // Unix seconds from metadata modified time
    size: u64,        // st_size
}
```

Bump `CURRENT_INDEX_VERSION` (e.g. `0.2.1` -> `0.3.0`). Merge requires matching
versions already; peers on old schema pull/push will need a re-index once
(acceptable greenfield).

Leave `file_times` / `deleted_files` unchanged (multi-device LWW).

### Skip algorithm (`hash_and_add_file` or successor)

```text
stat(path) -> (mtime, size)   // error => surface as today
path_index = build_path_index() or maintained reverse map
if !force_rehash
   and path_index[path] = Some(old_hash)
   and files[old_hash].paths[path] = Some(stat)
   and stat.mtime == mtime and stat.size == size
   and files[old_hash].total_size() == size   // cheap sanity
then
   // skip: already indexed and unchanged
   report progress as skipped (bytes optional: count as done without reading)
   return
else
   full hash -> new_hash
   if Some(old_hash) = path_index[path] and old_hash != new_hash
      remove path from files[old_hash]
      if files[old_hash].paths empty: drop FileRef + scrub blocks / tombstone policy
   insert/update files[new_hash].paths[path] = PathStat { mtime, size }
   update blocks / file_times as today
```

Only skip when **local** stat equals **local** stored PathStat. Remote-merged
PathStat values are not a substitute for a local stat match (if a peer never
saw the file, there is no local entry; if merge brought a path in, first local
backup still stats and may rehash once to confirm).

### Progress / UX

- `plan_index_work` can stay size-based for overall bar, or split
  “to hash” vs “to stat” later. Minimum: emit skipped vs hashed counts so the
  UI does not look hung on a fast second run.
- Summary line: `N files hashed, M skipped unchanged` (or similar).
- CLI: `--force-rehash` on `BackupArgs` (name TBD) to bypass skip.

### Call sites to update

- `FileRef` + all constructors/tests using `HashSet` paths
- `PlainIndex::build_path_index`, merge (`index_merge`), status, search, rm,
  restore path listing, serve dump/load if they touch path sets
- CBOR round-trip tests; bump version assertions
- `blu backup` orchestration + progress events if needed

### Correctness notes

- Same size+mtime, different content: false skip (document; `--force-rehash`).
- `touch` only: rehash, same content hash, refresh PathStat (fine).
- Copy that preserves mtime+size: may skip incorrectly if path is new but
  identical metadata as another machine’s ghost entry — only applies if the
  **same path string** already has PathStat; new paths always hash.
- Hard links: two paths, two PathStats; content hash shared (already true).

## Stages

Stage 1: PathStat model + version bump
  1a: Add `PathStat`, change `FileRef.paths` to `HashMap<PathBuf, PathStat>`
  1b: Bump `CURRENT_INDEX_VERSION` to `0.3.0`
  1c: Fix compile across crate (merge, status, tests, fixtures)
  1d: Unit tests for FileRef serde round-trip with PathStat

Stage 2: Path unlink on content change
  2a: When rehash yields a new content hash for an existing path, remove path
      from the old FileRef; drop empty FileRefs cleanly (block scrub consistent
      with existing tombstone/remove helpers)
  2b: Tests: edit file in place (same path, new bytes) leaves one live FileRef
      for that path

Stage 3: Skip in hash_and_add_file
  3a: Stat + compare PathStat; skip full read on match
  3b: On hash path, write fresh PathStat from the stat taken at start (or
      re-stat after hash if we want post-write accuracy; prefer single stat
      at start for speed, document race)
  3c: Tests: second add of unchanged tree does not re-read (injectable reader
      counter or temp files + chmod/mtime freeze); changed size or mtime forces
      rehash

Stage 4: Backup CLI + progress
  4a: `--force-rehash` on backup
  4b: Progress/summary distinguishes hashed vs skipped
  4c: Smoke: init/backup twice on a small tree; second run near-instant index

Stage 5: Verify
  5a: `cargo test`, clippy, fmt
  5b: Optional live: after media backup completes, second `blu backup` on zeal
      should skip almost all files in index phase

## Non-goals

- Storing device/inode (can add later for move detection)
- Bitrot scrubber schedule (use `--force-rehash` or a future `blu scrub`)
- Streaming index I/O / mmap (separate TODO)
- Changing blob_index finalize/upload pipeline
- Migrating old `0.2.1` indexes in place (re-backup / re-index is fine)
- Fixing additive delete detection for removed files (nice follow-up; not
  required for skip). Optional note: `PlainIndex::update` already diffs trees
  but backup does not use it.

## Efficiency claim (why ship this)

For a static 187G vault after first successful index:

- Index phase drops from full disk read to metadata walk (orders of magnitude).
- Encrypt/upload still only processes chunks missing from `blob_index` (already
  true). Together, a no-op second backup becomes “stat tree + empty encrypt +
  push indexes if needed,” which is what an archival CLI should feel like.

If the tree changes often (active project dir), skip still helps unchanged
files; only dirty paths pay full hash. Still a win.

## Notes

- Do not confuse overall backup progress (index bytes + encrypt bytes) with
  “upload size.” Unchanged after this work.
- Pair with the completed S3 upload backpressure work: first full backup is
  still long; every later backup should not re-inflict the index tax.
