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
It maintains a local redb database as its working index, an LRU blob
cache, and serves client requests by resolving virtual paths through
the chunk and blob layers.

## 3. Index Strategy

### Current state

Three encrypted indexes (CBOR, gzipped, v2 envelope):

| Index | Maps | Purpose |
|-------|------|---------|
| `PlainIndex` | file_hash -> FileRef{chunkmetas, paths} | What files exist, their chunks in order |
| `BlobIndex` | chunk_hash -> BlobBlockLocation{blob_path, offset, size} | Where each chunk lives in encrypted storage |
| `TagIndex` | tag -> set of file_hashes | User-defined tags |

### For `blu serve`

The local index store is a redb database (Rust-native, single-file,
ACID, no FFI or external process). redb is the working copy;
encrypted CBOR on the backend is the source of truth and the
interchange format.

**Why redb rather than in-memory HashMaps:**

- `blu serve` is a long-running daemon. Pinning hundreds of megabytes
  of deserialized HashMaps in resident memory permanently is wasteful
  when redb pages data in and out through the OS page cache as needed.
- Startup time. Deserializing a large CBOR blob into in-memory
  HashMaps takes seconds and scales linearly with index size. redb
  opens in milliseconds regardless of size.
- Crash recovery. If the daemon restarts, in-memory indexes are gone
  and must be re-pulled from the backend. With redb, the local
  database survives restarts. You only re-pull to sync deltas, not
  to recover full state.
- No migration. Building on in-memory first means throwing away that
  code later. redb's API (`insert`, `get`, `range`) is not
  significantly harder than `HashMap::insert` and `HashMap::get`.
  The complexity cost of starting with redb is low.
- redb handles concurrent readers with a single writer, which maps
  cleanly to the serve workload (many concurrent read requests,
  occasional writes).

**On startup (fresh machine):**

1. Pull all encrypted index files from the backend
   (`cfg.pull_indexes`)
2. Decrypt and deserialize (existing `EncryptedSerializable` path)
3. Load into local redb tables

**On startup (returning machine):**

1. Open existing redb database (milliseconds)
2. Pull index files from backend, diff against local state, apply
   deltas

**On writes:**

1. Update local redb
2. Periodically serialize redb state to encrypted CBOR and push to
   backend

The indexes live in the same bucket as the data. On a fresh machine,
you pull everything from one bucket and have full state. The local
redb file is a cache that survives restarts; the backend is
authoritative.

### Scaling

A vault with 1 million files at ~200 bytes per file entry is ~200 MB
of index data. redb handles this without loading it all into resident
memory (the OS page cache manages hot/cold pages). For vaults with
tens of millions of entries, redb scales to the size of the local
disk without changing the backend format or the local API.

## 4. Read Path: Serving Files from Packed Blobs

When a client requests a file (e.g., `GET /movies/inception.mkv`):

1. **Path lookup**: find the path in the local redb path index, get
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

### Metadata leakage constraint

A naive segmented design (per-chunk segments with variable sizes due
to compression, plus a table of contents listing segment offsets)
leaks metadata to the storage provider:

- Number of chunks per blob
- Compressed size of each chunk (which correlates with content type
  and entropy)
- Internal structure of the blob

This violates the core guarantee: a blob, as seen by the storage
provider, must reveal nothing about its internal structure. The blob
must be indistinguishable from random bytes of a predictable,
uniform size.

### Fixed-size segments with no table of contents

The solution is to make every encrypted segment the same size, with
no in-blob metadata:

```
plaintext   = chunk1 || chunk2 || ... || chunkN
compressed  = compress(plaintext)
padded      = pad(compressed, multiple of SEGMENT_SIZE)

segment_0   = encrypt(padded[0..S])
segment_1   = encrypt(padded[S..2S])
...
segment_K   = encrypt(padded[(K-1)*S..K*S])
```

Every segment is exactly `SEGMENT_SIZE + 28` bytes on disk (plaintext
segment + 12-byte nonce + 16-byte tag). An attacker sees a blob of
size `K * (S + 28)` and learns only K (the segment count), which is
the same for all ~64 MiB blobs because blob sizes are uniform. There
is no table of contents, no variable sizes, no internal structure
visible in the ciphertext.

**Where the internal mapping lives**: the client-side encrypted index
(stored in redb locally, pushed to the backend as encrypted CBOR)
records which compressed-byte range each chunk occupies, and
therefore which segments must be fetched to recover a given chunk.
This mapping is never visible to the storage provider.

**What this preserves:**

- Uniform ~64 MiB blob files on the backend (no metadata leakage)
- Content-addressed blob naming
- Chunk-level deduplication (unchanged)
- Same key hierarchy (each segment uses the blob's DEK, with a
  segment-counter-derived nonce)
- Blobs are opaque and structureless to the storage provider

**What this enables:**

- Byte-range S3 GET for individual segments
- Authenticate and decrypt a subset of the blob without downloading
  all of it
- Cache miss cost drops from ~64 MiB to `N * SEGMENT_SIZE` where N
  is the number of segments spanning the requested chunk

**What this changes:**

- Blob format (v2 -> v3): segments replace the single sealed box
- `BlobIndex` gains a compressed-byte-offset field per chunk, so the
  client can compute which segments to fetch
- Compression is still whole-blob (preserving cross-chunk context),
  but the compressed output is split into fixed-size segments before
  encryption
- Slight size overhead: 28 bytes (nonce + tag) per segment instead
  of per blob, plus padding to align the compressed output to a
  segment boundary. For 128 segments per blob, overhead is ~3.5 KiB
  for nonce/tag plus up to one segment of padding, negligible
  relative to 64 MiB

**Segment size**: a reasonable default is 512 KiB (matching chunk
size). Larger segments (1 MiB) reduce per-segment overhead but
increase the minimum fetch granularity. Smaller segments (64 KiB)
improve random-access granularity at the cost of more nonce/tag
bytes. The segment size is a configuration knob, not a format
constant; it can be stored in the v3 header.

**Compatibility**: this would be a new format version (v3). Existing
v2 blobs remain readable. New blobs can be written in v3. Migration
is optional (repack via `blu defrag-blobs --upgrade-format`).

### Known limitation: temporal metadata from the object catalog

Individual blob files reveal nothing about their internal structure.
In both v2 (single sealed AEAD box) and v3 (fixed-size segments,
no table of contents), the ciphertext is indistinguishable from
random bytes. An attacker who downloads a blob learns nothing
without the decryption keys.

However, an attacker with access to the S3 bucket itself (via
compromise, subpoena, or insider access at the provider) can
inspect the object catalog. This reveals:

- The number of blob objects in the bucket
- Object creation timestamps (when each blob was uploaded)
- Total storage consumed

From this, an attacker can infer the approximate rate of data
ingestion over time (e.g., "this user stored roughly 5 GiB in
June and 20 GiB in July"). They learn nothing about what the
data is, how many source files it represents, file types, or
content. Just volume over time.

This is inherent to any third-party object store. You cannot hide
the existence of S3 objects from someone who controls the S3
account. The blob format cannot address this; mitigation would
require operational measures (e.g., dummy blob uploads to obscure
ingestion patterns, or using a storage provider with stronger
access controls).

### Recommendation

Segmented AEAD is a future optimization, not a prerequisite for
`blu serve`. Phase 1 launches with whole-blob fetch and LRU caching
(current v2 format, zero metadata leakage, full-blob download on
cache miss). Phase 3 introduces fixed-size segmented AEAD for
reduced latency on random access without compromising the metadata
guarantees. The existing `EncBlobReader` LRU cache makes Phase 1
entirely usable for sequential streaming (video playback, file
downloads).

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
| `ListObjectsV2` | Query local redb path index; return virtual file listings with pagination |
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
- Local redb index store (pull from backend on first run, open
  existing on subsequent runs)
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

- Fixed-size segments with no in-blob metadata (as described in
  section 5)
- Compressed-byte-offset field added to `BlobIndex` entries
- Byte-range S3 GET for individual segments
- v3 format writer + reader, v2 backward compat
- `blu defrag-blobs --upgrade-format` for migration
- Dramatic improvement in random-access latency with zero metadata
  leakage to the storage provider

### Phase 4: Additional interfaces

- FUSE mount (Linux first, macOS if FUSE-T stabilizes)
- WebDAV (simpler than S3, some clients prefer it)
- NFS loopback (no kernel extension needed, works everywhere)
