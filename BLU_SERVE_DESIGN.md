# `blu serve` Design: Encrypted Storage Platform

## 1. Vision

blu is an encrypted storage platform. The archival CLI (`blu sync`,
`blu restore-files`, etc.) is one frontend. `blu serve` is a second
frontend: a local translation layer that presents decrypted,
de-obfuscated files to any client while the real backend (S3, GCS,
Azure, Ceph, or any object store) holds only opaque, uniform,
content-addressed encrypted blobs.

The threat model: a state-level attacker who compromises or subpoenas
the storage provider sees only uniformly-sized opaque blobs with
content-addressed names. They learn nothing about file count, file
sizes, file types, or file contents. All metadata, all indexes, and
all data live in the same bucket, encrypted with the same key
hierarchy. The only information needed to recover everything is the
BIP39 mnemonic.

### Design constraints (non-negotiable)

- Content-defined chunking (~512 KiB) with deduplication is preserved
- Chunks are packed into uniform ~64 MiB blobs
- Blob sizes remain uniform regardless of source file sizes (small
  files aggregate, large files split)
- No per-file objects on the backend; everything is opaque blobs
- All metadata (indexes) is stored in the same bucket, encrypted the
  same way
- Single key hierarchy (UK wraps KEK wraps DEK) for everything
- Backend-agnostic: S3 is one implementation, others follow the same
  `BackendKind` enum

## 2. Architecture

```
Any S3 client           Any filesystem client
(rclone, aws cli,       (Finder, ls, cp,
 Cyberduck, Jellyfin)    VLC, ffmpeg)
       |                       |
       v                       v
  S3-compat API           FUSE mount (future)
       |                       |
       +-----------+-----------+
                   |
                   v
           blu serve (local daemon)
             - virtual namespace (path -> file metadata)
             - chunk resolver (file -> chunks -> blobs)
             - blob cache (LRU, decrypted + decompressed)
             - write pipeline (chunk -> dedup -> pack -> encrypt -> upload)
                   |
                   v
           blu agent (key daemon)
             - KEK in mlock'd memory
             - DEK wrap/unwrap over Unix socket
                   |
                   v
           BackendKind (S3 / Local / GCS / Azure / Ceph / ...)
             - only ciphertext crosses this boundary
             - all blobs are ~64 MiB, content-addressed, opaque
```

`blu serve` is a long-running local process (like the agent daemon).
It holds the decrypted indexes in memory, maintains an LRU blob cache,
and serves client requests by resolving virtual paths through the
chunk and blob layers.

## 3. Index Strategy

### Current state

Three encrypted indexes (CBOR, gzipped, v2 envelope):

| Index | Maps | Purpose |
|-------|------|---------|
| `PlainIndex` | file_hash -> FileRef{chunkmetas, paths} | What files exist, their chunks in order |
| `BlobIndex` | chunk_hash -> BlobBlockLocation{blob_path, offset, size} | Where each chunk lives in encrypted storage |
| `TagIndex` | tag -> set of file_hashes | User-defined tags |

### For `blu serve`

On startup:

1. Pull all index files from the backend (`cfg.pull_indexes`)
2. Decrypt and deserialize into memory (existing `EncryptedSerializable`
   path)
3. Build a path index (`PlainIndex::build_path_index()` already exists,
   returns `HashMap<PathBuf, Hash>`)
4. Optionally persist decrypted indexes to a local B-tree (SQLite or
   redb) for fast restarts

On writes:

1. Update in-memory indexes
2. Periodically flush to local disk (encrypted)
3. Push updated indexes to backend

The indexes live in the same bucket as the data. On a fresh machine,
you pull everything from one bucket and have full state. The decrypted
local copy is a cache, not a source of truth.

### Scaling consideration

For a personal/small-team vault, the full index fits in memory easily.
A vault with 1 million files at ~200 bytes per file entry is ~200 MB
of index data. This is fine. If the index ever outgrows memory, a
local B-tree (redb is a good Rust-native option) would handle billions
of entries without changing the backend format.

## 4. Read Path: Serving Files from Packed Blobs

When a client requests a file (e.g., `GET /movies/inception.mkv`):

1. **Path lookup**: find the path in the in-memory path index, get
   file_hash
2. **File metadata**: look up `FileRef` in `PlainIndex`, get ordered
   `Vec<ChunkMeta>` with sizes; compute total file size from sum of
   chunk sizes
3. **Chunk resolution**: for each `ChunkMeta`, look up
   `BlobBlockLocation` in `BlobIndex`, get blob_path + offset + size
   within decompressed blob
4. **Blob fetch**: for each unique blob needed, check LRU cache first;
   on miss, fetch from backend, decrypt envelope (via agent),
   decompress, cache
5. **Chunk extraction**: slice each chunk from its cached decompressed
   blob at `[offset..offset+size]`
6. **Serve**: concatenate chunks in order, write to client

This is exactly what `restore_files` does today, minus writing to
disk. The `EncBlobReader` with its LRU cache already implements
steps 4-5.

### Byte-range requests

For streaming (video seek, HTTP Range headers), the client requests a
byte range like `bytes=50000000-54000000`. The translation layer:

1. Computes cumulative chunk offsets from the `Vec<ChunkMeta>` sizes
   (e.g., chunk 0 covers bytes 0-524287, chunk 1 covers
   524288-1048575, etc.)
2. Identifies which chunks overlap the requested range (binary search
   on cumulative offsets)
3. Fetches only those chunks (which means fetching their parent blobs,
   but the LRU cache makes sequential access fast)
4. Slices the exact requested byte range from the reassembled chunk
   data
5. Returns with `Content-Range` header

For sequential streaming (normal video playback), the LRU cache works
beautifully. A 2 GB movie is ~4000 chunks across ~30 blobs. Sequential
playback reads chunks in order, and since chunks from the same movie
are likely packed into the same blobs (they were added at the same
time), each blob fetch serves ~128 chunks before the next blob is
needed. The 10-blob cache holds ~1.3 GB of decompressed data, so
playback has a comfortable buffer.

For random seeks, the worst case is fetching a new 64 MiB blob to
serve a single 512 KiB chunk. On a decent connection, that is a few
seconds of latency per seek. Not ideal, but workable for personal use.
The segmented AEAD optimization (section 5) addresses this for the
future.

## 5. Segmented AEAD

### The problem with whole-blob encryption

Currently, each blob is encrypted as a single AEAD ciphertext:

```
compress(chunk1 || chunk2 || ... || chunkN) -> encrypt_as_one_unit -> blob
```

Poly1305 authentication covers the entire ciphertext. You cannot:

- Authenticate a partial read (the tag covers all-or-nothing)
- Do a byte-range fetch from S3 and decrypt just that range
- Avoid downloading 64 MiB when you need 512 KiB

For archival this is fine (you always restore full files). For
streaming, it means every cache miss costs a full 64 MiB download.

### What segmented AEAD is

Instead of encrypting the blob as one unit, split it into fixed-size
segments, each independently encrypted and authenticated:

```
Segment 0: nonce(12) || encrypt(plaintext_segment_0) || tag(16)
Segment 1: nonce(12) || encrypt(plaintext_segment_1) || tag(16)
...
Segment N: nonce(12) || encrypt(plaintext_segment_N) || tag(16)
```

Each segment has its own ChaCha20-Poly1305 nonce and tag. You can:

- Fetch a byte range from S3 covering only the segments you need
- Authenticate and decrypt each segment independently
- Never download more data than necessary (within segment granularity)

The segment size is a tuning knob. Larger segments (e.g., 1 MiB) mean
less overhead but coarser granularity. Smaller segments (e.g., 64 KiB)
mean more overhead but finer random access. A reasonable default is
512 KiB (matching chunk size), so each chunk gets its own
authenticated segment.

### Per-chunk AEAD within blobs

A natural design for blu: instead of
`compress(all_chunks) -> encrypt`, do:

```
blob = header || segment_0 || segment_1 || ... || segment_N

where segment_i = encrypt(compress(chunk_i))
```

Each chunk is independently compressed, then independently encrypted
with its own AEAD nonce and tag. The blob header contains a table of
contents: for each segment, its byte offset within the blob and the
chunk hash it contains.

**What this preserves:**

- Uniform ~64 MiB blob files on the backend (no metadata leakage)
- Content-addressed blob naming
- Chunk-level deduplication (unchanged)
- Same key hierarchy (each segment uses the blob's DEK, just with a
  segment-counter-derived nonce)

**What this enables:**

- Byte-range S3 GET for individual segments
- Authenticate and decrypt a single chunk without touching the rest
- Cache miss cost drops from ~64 MiB to ~512 KiB

**What this changes:**

- Blob format (v2 -> v3): new header format with segment table of
  contents
- Compression ratio: per-chunk compression is slightly worse than
  whole-blob compression (less context for the compressor), but the
  difference is small for 512 KiB chunks
- Slight size overhead: 28 bytes (nonce + tag) per segment instead of
  per blob; for 128 chunks per blob, that is ~3.5 KiB overhead,
  negligible

**Compatibility**: this would be a new format version (v3). Existing
v2 blobs remain readable. New blobs can be written in v3. Migration
is optional (repack via `blu defrag-blobs` with a
`--upgrade-format` flag).

### Recommendation

Segmented AEAD is a future optimization, not a prerequisite for
`blu serve`. Phase 1 launches with whole-blob fetch and LRU caching.
Phase 2 introduces segmented AEAD for reduced latency on random
access. The existing `EncBlobReader` LRU cache makes Phase 1 entirely
usable for sequential streaming (video playback, file downloads).

## 6. Write Path: Ingesting Files Through the Translation Layer

When a client writes a file (e.g., `PUT /documents/report.pdf`):

1. **Receive bytes**: buffer the incoming stream (or spool to a temp
   file for large uploads)
2. **Chunk**: split into ~512 KiB fixed-size chunks (same
   `Chunkerator` logic, but operating on in-memory data or a temp
   file instead of a source path)
3. **Hash**: SHA-512 multihash each chunk and the whole file
4. **Dedup**: check `BlobIndex.has_chunk(chunk_hash)` for each chunk;
   skip chunks that already exist
5. **Pack**: feed new chunks into `BlobBuffer`, which seals and
   uploads blobs when full (~64 MiB)
6. **Update indexes**: add `FileRef` to `PlainIndex`, chunk locations
   to `BlobIndex`
7. **Flush**: write encrypted indexes to local disk, push to backend

This is the same pipeline as `blu sync`, but triggered by an API
request instead of a CLI command. The underlying functions
(`BlobBuffer::add_chunk`, `BlobBuffer::seal_and_upload`,
`PlainIndex::hash_and_add_file`) already exist. The translation
layer adapts the input from "filesystem path" to "byte stream."

### Atomicity

The write is not atomic with respect to crashes. If the process dies
between uploading a blob and updating the index:

- Orphaned blobs exist on the backend (encrypted data with no index
  entry pointing to them)
- This is harmless: orphaned blobs waste space but do not corrupt
  anything
- `blu doctor` (on the roadmap) would detect and optionally clean up
  orphans

For stronger guarantees, a local write-ahead log (WAL) could record
intent before uploading, then confirm after the index is updated. This
is a future enhancement, not a launch blocker.

### Partial blob flush

The current `BlobBuffer` only seals a blob when it is full (~64 MiB).
For `blu serve`, we may want to flush more eagerly (e.g., after a
configurable idle timeout) so that recently-written data is persisted
even if the blob is not full yet. This means the last blob for a write
session might be smaller than 64 MiB, but that is already the case
today (the final blob from `BlobBuffer::finalize` can be any size).

## 7. S3-Compatible API

The primary client interface is a local S3-compatible HTTP server.
This gives maximum compatibility: any tool that speaks S3 works
without modification.

### API surface (minimal viable subset)

| S3 Operation | Translation |
|---|---|
| `ListBuckets` | List configured vaults |
| `ListObjectsV2` | Query decrypted `PlainIndex` path index; return virtual file listings with pagination |
| `GetObject` | Resolve path -> chunks -> blobs -> fetch/cache/decrypt -> serve bytes |
| `GetObject` with `Range` | Same, but compute chunk overlap with byte range and serve only requested slice |
| `HeadObject` | Resolve path -> compute size from chunk sizes, return metadata |
| `PutObject` | Receive bytes -> chunk -> dedup -> pack -> encrypt -> upload -> update index |
| `DeleteObject` | Remove from indexes -> trigger delete cascade (same as `blu delete-files`) |
| `CreateMultipartUpload` | Allocate upload state, return upload ID |
| `UploadPart` | Buffer part, chunk incrementally |
| `CompleteMultipartUpload` | Finalize chunking, pack remaining, update indexes |

### What we skip (initially)

- S3 auth signatures (localhost only; the agent daemon is the auth
  boundary)
- Bucket creation/deletion (use `blu init` and vault config)
- Object versioning (future: snapshot model)
- ACLs, policies, lifecycle rules
- Server-side encryption (we do our own)

### Implementation

Build on `axum` (already a transitive dependency via tokio). Parse S3
request XML/headers, translate to index lookups, respond with
S3-compatible XML/headers. There are existing Rust crates (e.g., `s3s`)
that provide S3 API scaffolding, though rolling a minimal
implementation for the subset above is also reasonable.

## 8. FUSE Mount (Future)

A FUSE mount would present the decrypted vault as a regular filesystem
directory. Any application works transparently. This is the most
user-friendly interface but has platform challenges:

- macOS: requires macFUSE (kernel extension, notarization issues) or
  FUSE-T (user-space, newer). Both add friction.
- Linux: FUSE is first-class via `libfuse` or `fuser` crate.

The internal read/write paths would be identical to the S3 API; only
the interface layer differs. If/when this is pursued, the same
`blu serve` daemon could expose both an S3 endpoint and a FUSE mount
simultaneously.

## 9. Multi-User

Multi-user access (v0.2.0 roadmap) integrates naturally. Each user
has their own PQ hybrid identity (from their BIP39 mnemonic). The
KEK is wrapped separately for each authorized user (envelope
encryption design, sections 5-6). When a user starts `blu serve`,
their agent daemon unlocks their copy of the KEK, and they can
read/write the shared vault.

Access control at the file level (user A can see these files, user B
cannot) would require per-file or per-directory KEK scoping. This is
a future design decision, not a prerequisite for `blu serve`.

## 10. Phased Implementation

### Phase 1: Read-only `blu serve` with LRU cache

- `blu serve` subcommand: starts local HTTP server
- Pull and decrypt indexes on startup, hold in memory
- Implement `GetObject` (with `Range`), `HeadObject`, `ListObjectsV2`
- LRU blob cache (existing `EncBlobReader` pattern, expanded capacity)
- Whole-blob fetch (existing v2 format, no changes)
- No auth (localhost only, agent daemon is trust boundary)

### Phase 2: Write support

- Implement `PutObject`, `DeleteObject`, multipart upload
- Adapt existing write pipeline (`BlobBuffer`, `PlainIndex` updates)
  to accept byte streams
- Index flush strategy (periodic + on-demand)
- Push updated indexes to backend

### Phase 3: Segmented AEAD (v3 format)

- Per-chunk AEAD within blobs (as described in section 5)
- Byte-range S3 GET for individual segments
- v3 format writer + reader, v2 backward compat
- `blu defrag-blobs --upgrade-format` for migration
- Dramatic improvement in random-access latency

### Phase 4: Additional interfaces

- FUSE mount (Linux first, macOS if FUSE-T stabilizes)
- WebDAV (simpler than S3, some clients prefer it)
- NFS loopback (no kernel extension needed, works everywhere)
