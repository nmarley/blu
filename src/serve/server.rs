//! HTTP server for `blu serve`: a local S3-compatible translation
//! layer that presents decrypted files to any S3 client while the real
//! backend holds only opaque encrypted blobs.
//!
//! Stage 3 added `ListObjectsV2` and `ListBuckets` (read-only listing).
//! Stage 4 adds `GetObject` (with byte-range support), `HeadObject`,
//! and the `EncBlobReader` LRU cache for serving chunk data from
//! decrypted blobs. Internal endpoints live under `/_` prefix
//! (e.g., `/_health`) to avoid collision with S3 bucket names.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::body::Bytes;
use axum::extract::{Path, Query};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chrono::NaiveDateTime;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Mutex;

use crate::blob::{BlobBuffer, BlobIndex, EncBlobReader};
use crate::block::{chunk_bytes, ChunkMeta, FileRef, DEFAULT_CHUNK_SIZE};
use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::config::Config;
use crate::dek_provider::DekProvider;
use crate::error::BluError;
use crate::hash::{self, Hash};
use crate::io::Position;
use crate::serve::index_sync;
use crate::serve::redb_store::RedbStore;
use crate::serve::s3xml;
use crate::storage::BackendKind;

/// Default listen address for the serve daemon. Localhost only; the
/// agent daemon is the trust boundary, not the HTTP server.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:7777";

/// State shared across all axum handlers. Stored in an `OnceLock`
/// behind an `Arc`; handlers access it via `OnceLock::get()` which
/// returns `None` until the background index sync completes.
pub struct ServeState {
    /// Local redb index store for path/file/blob/tag lookups.
    redb: Arc<RedbStore>,
    /// S3 bucket name (vault directory basename).
    bucket_name: String,
    /// Index-level `updated_at` timestamp, used as a proxy for
    /// individual object `LastModified` values. The current index
    /// format does not track per-file modification times.
    index_updated_at: NaiveDateTime,
    /// Blob reader for fetching, decrypting, and caching blob data.
    /// Shared across concurrent handlers via `Arc`.
    blob_reader: Arc<EncBlobReader>,
    /// Vault config. Read by the index flush to resolve local index
    /// directory paths and to push encrypted indexes to the backend.
    #[allow(dead_code)]
    cfg: Config,
    /// DEK provider. Used by the write path for envelope encryption
    /// of new blobs and (eventually) by the index flush.
    keys: DekProvider,
    /// Storage backend. Used by the write path for uploading new
    /// blobs and (eventually) by the index flush.
    backend: BackendKind,
    /// Serializes all write-path mutations so redb state stays
    /// consistent and no two writes overlap. PutObject, DeleteObject,
    /// and the index flush all acquire this lock. Read-path handlers
    /// do not need it; they only read redb and the
    /// `EncBlobReader` cache.
    write_mutex: Mutex<()>,
}

impl std::fmt::Debug for ServeState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ServeState")
            .field("bucket_name", &self.bucket_name)
            .field("index_updated_at", &self.index_updated_at)
            .finish_non_exhaustive()
    }
}

/// Wait for SIGTERM or SIGINT, then return. Used as the graceful
/// shutdown signal for `axum::serve`.
async fn shutdown_signal() {
    let mut sigterm = signal(SignalKind::terminate()).expect("failed to install SIGTERM handler");
    let mut sigint = signal(SignalKind::interrupt()).expect("failed to install SIGINT handler");

    tokio::select! {
        _ = sigterm.recv() => info!("received SIGTERM, shutting down"),
        _ = sigint.recv() => info!("received SIGINT, shutting down"),
    }
}

/// Entry point for `blu serve`. Loads the vault config and keys,
/// binds the TCP listener, starts serving HTTP immediately, and runs
/// index sync as a background task. Until sync completes, `/_health`
/// and all S3 endpoints return 503. This gives process supervisors a
/// real readiness signal instead of the port being closed until sync
/// finishes.
pub async fn serve(bind_addr: Option<String>) -> Result<(), BluError> {
    let addr: SocketAddr = bind_addr
        .as_deref()
        .unwrap_or(DEFAULT_BIND_ADDR)
        .parse()
        .expect("invalid bind address");

    info!("loading vault config and keys");
    let (cfg, keys) = load_config_and_keys(&LoadOptions::default())?;

    info!("initializing storage backend");
    let backend = cfg.init_storage_backend().await?;

    // Shared readiness slot: None until sync completes, then Some.
    let ready: Arc<OnceLock<ServeState>> = Arc::new(OnceLock::new());

    let app = Router::new()
        .route("/_health", get(health_handler))
        .route("/", get(list_buckets_handler))
        .route("/{bucket}", get(list_objects_handler))
        .route(
            "/{bucket}/{*key}",
            get(get_object_handler)
                .head(head_object_handler)
                .put(put_object_handler),
        )
        .with_state(ready.clone());

    let listener = TcpListener::bind(addr).await?;
    info!("blu serve listening on http://{}", addr);

    // Run index sync in the background so the HTTP server is
    // immediately available for health checks. The ready slot is
    // populated on success; on failure the error is logged and the
    // slot stays empty, leaving the server in a 503 "not ready"
    // state until SIGTERM/SIGINT.
    let sync_ready = ready.clone();
    tokio::spawn(async move {
        info!("syncing indexes from backend into local redb store");
        match index_sync::sync_from_backend(cfg, keys, backend).await {
            Ok((cfg, keys, backend, store, index_updated_at, blob_reader)) => {
                let bucket_name = cfg
                    .basedir()
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "blu".to_string());

                let state = ServeState {
                    redb: Arc::new(store),
                    bucket_name,
                    index_updated_at,
                    blob_reader: Arc::new(blob_reader),
                    cfg,
                    keys,
                    backend,
                    write_mutex: Mutex::new(()),
                };

                if sync_ready.set(state).is_err() {
                    warn!("serve state already set, sync result discarded");
                }
                info!("index sync complete, server ready");
            }
            Err(e) => {
                error!("index sync failed, server remains not-ready: {}", e);
            }
        }
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;

    info!("blu serve stopped");
    Ok(())
}

/// Health check handler. Returns 503 while the index sync is in
/// progress (the `OnceLock` is empty), and 200 with index stats once
/// the store is loaded and the server is ready to serve traffic.
async fn health_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
) -> impl IntoResponse {
    match state.0.get() {
        Some(s) => {
            let paths = s.redb.path_count().unwrap_or(0);
            let files = s.redb.file_count().unwrap_or(0);
            let chunks = s.redb.blob_count().unwrap_or(0);
            let blocks = s.redb.block_count().unwrap_or(0);
            let tags = s.redb.tag_count().unwrap_or(0);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain")],
                format!(
                    "ok ({} paths, {} files, {} chunks, {} blocks, {} tags)",
                    paths, files, chunks, blocks, tags
                ),
            )
        }
        None => (
            StatusCode::SERVICE_UNAVAILABLE,
            [(header::CONTENT_TYPE, "text/plain")],
            "starting".to_string(),
        ),
    }
}

/// `GET /` -- ListBuckets. Returns a single bucket named after the
/// vault directory. This is what `aws s3 ls` (without a bucket name)
/// calls. Returns 503 if the index sync has not completed yet.
async fn list_buckets_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
) -> impl IntoResponse {
    let s = match state.0.get() {
        Some(s) => s,
        None => {
            return s3xml::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "NotReady",
                "index sync in progress",
            )
        }
    };

    let xml = s3xml::list_all_my_buckets(
        &s.bucket_name,
        &s.index_updated_at
            .format("%Y-%m-%dT%H:%M:%S.000Z")
            .to_string(),
    );
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        xml,
    )
}

/// `GET /{bucket}` -- ListObjectsV2 (or ListObjects V1). Dispatches
/// based on the `list-type` query parameter. This is what `aws s3 ls
/// s3://bucket/` calls. Returns 503 if the index sync has not
/// completed yet.
async fn list_objects_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
    Path(bucket): Path<String>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    let s = match state.0.get() {
        Some(s) => s,
        None => {
            return s3xml::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "NotReady",
                "index sync in progress",
            )
        }
    };

    if bucket != s.bucket_name {
        return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchBucket", &bucket);
    }

    let list_type = params.get("list-type").map(String::as_str);
    if list_type != Some("2") {
        return s3xml::error_response(
            StatusCode::NOT_IMPLEMENTED,
            "NotImplemented",
            "ListObjects V1 is not supported; use list-type=2 for ListObjectsV2",
        );
    }

    let prefix = params.get("prefix").cloned().unwrap_or_default();
    let delimiter = params.get("delimiter").cloned();
    let max_keys: usize = params
        .get("max-keys")
        .and_then(|s| s.parse().ok())
        .unwrap_or(1000)
        .min(1000);
    let continuation_token = params.get("continuation-token").cloned();
    let start_after = params.get("start-after").cloned();

    // Resolve the cursor: continuation-token takes priority over
    // start-after. If neither is present, listing begins at the prefix.
    let start_after_key: Option<String> = if let Some(token) = continuation_token.as_ref() {
        s3xml::decode_continuation_token(token)
    } else {
        start_after.clone()
    };

    // Fetch one extra row beyond max_keys to determine IsTruncated.
    let fetch_count = max_keys.saturating_add(1);
    let results = match s
        .redb
        .list_paths(&prefix, start_after_key.as_deref(), fetch_count)
    {
        Ok(r) => r,
        Err(e) => {
            return s3xml::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            );
        }
    };

    let is_truncated = results.len() > max_keys;
    let entries: Vec<(String, crate::hash::Hash)> = if is_truncated {
        results.into_iter().take(max_keys).collect()
    } else {
        results
    };

    // Group by delimiter into Contents and CommonPrefixes.
    let (contents, common_prefixes, next_cursor) =
        s3xml::group_by_delimiter(&entries, &prefix, delimiter.as_deref());

    // Build the next continuation token if truncated.
    let next_continuation_token = if is_truncated {
        next_cursor.map(|c| s3xml::encode_continuation_token(&c))
    } else {
        None
    };

    let xml = s3xml::list_bucket_result(
        &s.bucket_name,
        &prefix,
        delimiter.as_deref(),
        max_keys,
        &continuation_token,
        &start_after,
        params.contains_key("start-after"),
        is_truncated,
        next_continuation_token.as_deref(),
        &contents,
        &common_prefixes,
        &s.index_updated_at,
        &s.redb,
    );

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        xml,
    )
}

/// Resolve a virtual path to its `FileRef` via the redb index.
///
/// Returns `Ok(Some(fileref, file_hash))` if the path exists, or
/// `Ok(None)` if no file matches. Errors are returned for index
/// corruption or redb failures.
fn resolve_path(
    redb: &RedbStore,
    path: &str,
) -> Result<Option<(FileRef, crate::hash::Hash)>, BluError> {
    let file_hash = match redb.get_file_hash_by_path(path)? {
        Some(h) => h,
        None => return Ok(None),
    };
    let fileref = match redb.get_fileref(&file_hash)? {
        Some(f) => f,
        None => return Ok(None),
    };
    Ok(Some((fileref, file_hash)))
}

/// Run the delete cascade for the given `file_hash` against the redb
/// index. This is the index-only half: it removes the FileRef, its
/// path entries, decrements BlockRef references (removing chunks that
/// become unreferenced from the blob index), and strips the file from
/// any tag_index entries. Blob file deletion from the backend is
/// deferred to the index flush, which serializes redb
/// state via `RedbStore::dump_to_indexes` and drains dead blob
/// paths via `BlobIndex::drain_paths_to_delete`.
///
/// `PutObject` calls this when overwriting an existing path so the
/// old file's chunks are reclaimed before the new FileRef is written.
/// `DeleteObject` will call this directly.
///
/// Caller must already hold `ServeState::write_mutex`. Returns the
/// [`DeleteStats`] produced by [`RedbStore::delete_object_index`]. A
/// `None` return indicates the file_hash was not present, which is
/// treated as a success with zero stats.
async fn delete_file_cascade(
    state: &ServeState,
    file_hash: &crate::hash::Hash,
) -> Result<crate::serve::redb_store::DeleteStats, BluError> {
    state.redb.delete_object_index(file_hash)
}

/// Fetch all chunk data for a `FileRef`, in chunk order, and
/// concatenate into a single `Vec<u8>`.
///
/// This is the serve read path: path -> FileRef -> chunks ->
/// BlobBlockLocation -> EncBlobReader::get_bytes -> concatenate.
/// The `EncBlobReader` LRU cache makes sequential reads efficient
/// (chunks from the same blob share a cache entry).
async fn fetch_file_bytes(
    fileref: &FileRef,
    redb: &RedbStore,
    blob_reader: &EncBlobReader,
) -> Result<Vec<u8>, BluError> {
    let total = fileref.total_size() as usize;
    let mut buf = Vec::with_capacity(total);
    for chunkmeta in &fileref.chunkmetas {
        let location =
            redb.get_blob_location(&chunkmeta.hash)?
                .ok_or_else(|| BluError::BlockNotFound {
                    hash: chunkmeta.hash.to_string(),
                })?;
        let chunk = blob_reader.get_bytes(&location).await?;
        buf.extend_from_slice(&chunk);
    }
    Ok(buf)
}

/// Fetch only the byte range `[start, end)` (0-indexed, end exclusive)
/// from a file, fetching only the chunks that overlap the range.
///
/// Computes cumulative chunk offsets to find overlapping chunks via
/// binary search, fetches each, and slices the requested bytes from
/// the reassembled data.
async fn fetch_range_bytes(
    fileref: &FileRef,
    redb: &RedbStore,
    blob_reader: &EncBlobReader,
    start: u64,
    end: u64,
) -> Result<Vec<u8>, BluError> {
    let mut buf = Vec::with_capacity((end - start) as usize);
    let mut offset: u64 = 0;
    for chunkmeta in &fileref.chunkmetas {
        let chunk_start = offset;
        let chunk_end = offset + chunkmeta.size as u64;
        offset = chunk_end;

        // Skip chunks entirely before the requested range.
        if chunk_end <= start {
            continue;
        }
        // Stop once we're past the requested range.
        if chunk_start >= end {
            break;
        }

        let location =
            redb.get_blob_location(&chunkmeta.hash)?
                .ok_or_else(|| BluError::BlockNotFound {
                    hash: chunkmeta.hash.to_string(),
                })?;
        let chunk = blob_reader.get_bytes(&location).await?;

        // Slice the overlapping portion.
        let slice_start = start.saturating_sub(chunk_start) as usize;
        let slice_end = (end - chunk_start) as usize;
        let slice_end = slice_end.min(chunk.len());
        buf.extend_from_slice(&chunk[slice_start..slice_end]);
    }
    Ok(buf)
}

/// Parsed byte range from an HTTP `Range` header.
struct ByteRange {
    /// Inclusive start offset (0-indexed).
    start: u64,
    /// Exclusive end offset (0-indexed, i.e., one past the last byte).
    end: u64,
}

/// Parse an HTTP `Range: bytes=...` header.
///
/// Supports three forms:
/// - `bytes=start-end` (inclusive end)
/// - `bytes=start-` (to end of file)
/// - `bytes=-suffix` (last N bytes)
///
/// Returns `Ok(None)` if no Range header is present. Returns `Err` if
/// the header is present but malformed.
fn parse_range_header(headers: &HeaderMap, total_size: u64) -> Result<Option<ByteRange>, String> {
    let raw = match headers.get(header::RANGE) {
        Some(v) => v
            .to_str()
            .map_err(|e| format!("invalid Range header: {}", e))?,
        None => return Ok(None),
    };

    let rest = raw
        .strip_prefix("bytes=")
        .ok_or_else(|| "Range header must start with 'bytes='".to_string())?;

    let (start_s, end_s) = rest
        .split_once('-')
        .ok_or_else(|| "Range header missing '-' separator".to_string())?;

    if start_s.is_empty() {
        // Suffix range: bytes=-N (last N bytes)
        let suffix: u64 = end_s
            .parse()
            .map_err(|_| "invalid suffix length in Range header".to_string())?;
        if suffix == 0 {
            return Err("suffix range of 0 bytes is unsatisfiable".to_string());
        }
        let start = total_size.saturating_sub(suffix);
        return Ok(Some(ByteRange {
            start,
            end: total_size,
        }));
    }

    let start: u64 = start_s
        .parse()
        .map_err(|_| "invalid start offset in Range header".to_string())?;

    if end_s.is_empty() {
        // Open-ended range: bytes=start-
        if start >= total_size {
            return Err("range start beyond file size".to_string());
        }
        return Ok(Some(ByteRange {
            start,
            end: total_size,
        }));
    }

    // Closed range: bytes=start-end (end is inclusive in HTTP)
    let end_inclusive: u64 = end_s
        .parse()
        .map_err(|_| "invalid end offset in Range header".to_string())?;
    if start > end_inclusive {
        return Err("range start is greater than end".to_string());
    }
    let end = end_inclusive + 1; // Convert to exclusive
    if start >= total_size {
        return Err("range start beyond file size".to_string());
    }
    // Clamp end to file size (S3 allows end >= file size, returns up to EOF)
    let end = end.min(total_size);

    Ok(Some(ByteRange { start, end }))
}

/// `HEAD /{bucket}/{*key}` -- HeadObject. Returns metadata headers
/// (Content-Length, Last-Modified, ETag) with no body. 404 if the
/// key does not exist.
async fn head_object_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
    Path((bucket, key)): Path<(String, String)>,
) -> axum::response::Response {
    let s = match state.0.get() {
        Some(s) => s,
        None => {
            return s3xml::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "NotReady",
                "index sync in progress",
            )
            .into_response();
        }
    };

    if bucket != s.bucket_name {
        return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchBucket", &bucket)
            .into_response();
    }

    let (fileref, file_hash) = match resolve_path(&s.redb, &key) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchKey", &key).into_response();
        }
        Err(e) => {
            return s3xml::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            )
            .into_response();
        }
    };

    let total = fileref.total_size();
    let last_modified = s
        .index_updated_at
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();

    let mut headers = HeaderMap::new();
    headers.insert(header::CONTENT_LENGTH, total.to_string().parse().unwrap());
    headers.insert(header::LAST_MODIFIED, last_modified.parse().unwrap());
    headers.insert(
        header::CONTENT_TYPE,
        "application/octet-stream".parse().unwrap(),
    );
    headers.insert(
        header::ETAG,
        format!("\"{}\"", file_hash.dbg_short(16)).parse().unwrap(),
    );

    (StatusCode::OK, headers, String::new()).into_response()
}

/// `GET /{bucket}/{*key}` -- GetObject, with optional `Range` support.
/// Returns the full file (200) or a byte range (206 Partial Content).
/// 404 if the key does not exist, 416 if the range is unsatisfiable.
async fn get_object_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
    Path((bucket, key)): Path<(String, String)>,
    headers: HeaderMap,
) -> axum::response::Response {
    let s = match state.0.get() {
        Some(s) => s,
        None => {
            return s3xml::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "NotReady",
                "index sync in progress",
            )
            .into_response();
        }
    };

    if bucket != s.bucket_name {
        return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchBucket", &bucket)
            .into_response();
    }

    let (fileref, file_hash) = match resolve_path(&s.redb, &key) {
        Ok(Some(pair)) => pair,
        Ok(None) => {
            return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchKey", &key).into_response();
        }
        Err(e) => {
            return s3xml::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &e.to_string(),
            )
            .into_response();
        }
    };

    let total = fileref.total_size();

    // Parse Range header if present.
    let range = match parse_range_header(&headers, total) {
        Ok(r) => r,
        Err(msg) => {
            return s3xml::error_response(StatusCode::RANGE_NOT_SATISFIABLE, "InvalidRange", &msg)
                .into_response();
        }
    };

    let last_modified = s
        .index_updated_at
        .format("%Y-%m-%dT%H:%M:%S.000Z")
        .to_string();
    let etag = format!("\"{}\"", file_hash.dbg_short(16));

    match range {
        None => {
            // Full file.
            let data = match fetch_file_bytes(&fileref, &s.redb, &s.blob_reader).await {
                Ok(d) => d,
                Err(e) => {
                    return s3xml::error_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "InternalError",
                        &e.to_string(),
                    )
                    .into_response();
                }
            };

            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                header::CONTENT_LENGTH,
                data.len().to_string().parse().unwrap(),
            );
            response_headers.insert(
                header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            response_headers.insert(header::LAST_MODIFIED, last_modified.parse().unwrap());
            response_headers.insert(header::ETAG, etag.parse().unwrap());

            (
                StatusCode::OK,
                response_headers,
                axum::body::Body::from(data),
            )
                .into_response()
        }

        Some(r) => {
            let data =
                match fetch_range_bytes(&fileref, &s.redb, &s.blob_reader, r.start, r.end).await {
                    Ok(d) => d,
                    Err(e) => {
                        return s3xml::error_response(
                            StatusCode::INTERNAL_SERVER_ERROR,
                            "InternalError",
                            &e.to_string(),
                        )
                        .into_response();
                    }
                };

            let content_range = format!("bytes {}-{}/{}", r.start, r.end - 1, total);

            let mut response_headers = HeaderMap::new();
            response_headers.insert(
                header::CONTENT_LENGTH,
                data.len().to_string().parse().unwrap(),
            );
            response_headers.insert(
                header::CONTENT_TYPE,
                "application/octet-stream".parse().unwrap(),
            );
            response_headers.insert(header::LAST_MODIFIED, last_modified.parse().unwrap());
            response_headers.insert(header::ETAG, etag.parse().unwrap());
            response_headers.insert(header::CONTENT_RANGE, content_range.parse().unwrap());

            (
                StatusCode::PARTIAL_CONTENT,
                response_headers,
                axum::body::Body::from(data),
            )
                .into_response()
        }
    }
}

/// `PUT /{bucket}/{*key}` -- PutObject. Stores the request body as a
/// new object at the given virtual path, chunking, deduplicating,
/// encrypting, and uploading new chunks to the storage backend, then
/// updating redb index state atomically.
///
/// Returns 200 with an `ETag` header containing the file's multihash
/// in double quotes (the S3 ETag convention). If the path already
/// exists, the old file is cascade-deleted from redb first; the old
/// blob files themselves are reclaimed lazily by the index flush.
/// Returns 503 if the index sync has not completed, 404
/// `NoSuchBucket` if the bucket does not match, 500 `InternalError`
/// on any index or backend failure.
async fn put_object_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
    Path((bucket, key)): Path<(String, String)>,
    body: Bytes,
) -> axum::response::Response {
    let s = match state.0.get() {
        Some(s) => s,
        None => {
            return s3xml::error_response(
                StatusCode::SERVICE_UNAVAILABLE,
                "NotReady",
                "index sync in progress",
            )
            .into_response();
        }
    };

    if bucket != s.bucket_name {
        return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchBucket", &bucket)
            .into_response();
    }

    // All write-path work happens under the write mutex so redb state
    // stays consistent and no two PutObjects overlap. The lock is
    // held for the full chunk/pack/encrypt/upload/index-update cycle.
    let _guard = state
        .0
        .get()
        .expect("checked Some above")
        .write_mutex
        .lock()
        .await;

    // Overwrite: if the path already exists, cascade-delete the old
    // FileRef before writing the new one. The old file's chunks are
    // decremented in block_index and removed from blob_index when
    // unreferenced; the blob files themselves are reclaimed later by
    // the index flush.
    if let Some(old_file_hash) = match s.redb.get_file_hash_by_path(&key) {
        Ok(Some(h)) => Some(h),
        Ok(None) => None,
        Err(e) => {
            return s3xml::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &format!("path lookup failed: {e}"),
            )
            .into_response();
        }
    } {
        if let Err(e) = delete_file_cascade(s, &old_file_hash).await {
            return s3xml::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &format!("overwrite cascade failed: {e}"),
            )
            .into_response();
        }
    }

    match put_object_inner(s, &key, &body).await {
        Ok(file_hash) => {
            let etag = format!("\"{}\"", file_hash.dbg_short(16));
            let mut headers = HeaderMap::new();
            headers.insert(header::ETAG, etag.parse().unwrap());
            (StatusCode::OK, headers, String::new()).into_response()
        }
        Err(e) => s3xml::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            &e.to_string(),
        )
        .into_response(),
    }
}

/// Inner PutObject pipeline: chunk, dedup, pack, encrypt, upload, then
/// update redb in a single write transaction via
/// [`RedbStore::put_object`]. Returns the file hash (used as the ETag)
/// on success.
///
/// The caller must already hold `ServeState::write_mutex` and must
/// have already run the overwrite cascade if the path previously
/// existed.
async fn put_object_inner(state: &ServeState, key: &str, body: &[u8]) -> Result<Hash, BluError> {
    // Chunk and hash. chunk_bytes returns at least one (possibly
    // empty) chunk for any input, matching S3's acceptance of
    // zero-byte objects.
    let chunks = chunk_bytes(body, DEFAULT_CHUNK_SIZE);
    let chunkmetas: Vec<ChunkMeta> = chunks.iter().map(|c| ChunkMeta::new(c)).collect();

    // Whole-file hash is the multihash of the original bytes -- used
    // as the FileRef identity and the ETag.
    let file_hash = Hash::from(hash::multihash(body).to_bytes());

    // Build a FileRef and record this path on it. The path entry is
    // also written to path_index inside put_object below.
    let mut fileref = FileRef::new(chunkmetas.clone());
    fileref.paths.insert(std::path::PathBuf::from(key));

    // Dedup: for each chunk, check if it already lives in a blob. New
    // chunks go into a fresh BlobBuffer; dedup hits skip the blob
    // pipeline entirely. Track new vs dedup locations so put_object
    // only inserts new entries into blob_index.
    let mut new_blob_locations: Vec<(Hash, crate::blob::BlobBlockLocation)> = Vec::new();
    let mut blob_buf = BlobBuffer::new(&state.backend, state.keys.clone());
    let mut req_blob_index = BlobIndex::new();

    for (chunk_bytes_vec, cm) in chunks.iter().zip(chunkmetas.iter()) {
        if state.redb.get_blob_location(&cm.hash)?.is_some() {
            continue;
        }
        let mut chunk_bytes_mut = chunk_bytes_vec.clone();
        blob_buf
            .add_chunk(&mut chunk_bytes_mut, &mut req_blob_index)
            .await?;
    }
    blob_buf.finalize(&mut req_blob_index).await?;

    // After finalize, the per-request BlobIndex has a location for
    // every freshly-packed chunk. Walk it into the new_blob_locations
    // list. Dedup hits are intentionally skipped here -- their
    // blob_index entries already exist in redb.
    for (chunk_hash, location) in &req_blob_index.map {
        new_blob_locations.push((chunk_hash.clone(), location.clone()));
    }

    // Build the blockref update list: (chunk_hash, file_hash, position)
    // for every chunk in this file. Dedup hits need their existing
    // BlockRef merged with this file_hash -> position; put_object
    // fetches and merges internally so the caller only supplies the
    // new (file_hash, position) pair for each chunk. The position
    // comes from the freshly-written blob for new chunks, or from the
    // existing BlobBlockLocation for dedup hits.
    let mut blockref_updates: Vec<(Hash, Hash, Position)> = Vec::with_capacity(chunkmetas.len());
    for cm in &chunkmetas {
        let location = match req_blob_index.map.get(&cm.hash) {
            Some(loc) => loc.clone(),
            None => match state.redb.get_blob_location(&cm.hash)? {
                Some(loc) => loc,
                None => {
                    return Err(BluError::Internal(format!(
                        "chunk {} has no blob location after put_object_inner",
                        cm.hash.dbg_short(12)
                    )));
                }
            },
        };
        blockref_updates.push((cm.hash.clone(), file_hash.clone(), location.position));
    }

    state.redb.put_object(
        &file_hash,
        &fileref,
        key,
        &new_blob_locations,
        &blockref_updates,
    )?;

    Ok(file_hash)
}

#[cfg(test)]
mod test {
    use std::collections::HashSet;
    use std::path::PathBuf;
    use std::sync::{Arc, OnceLock};

    use axum::body::Body;
    use axum::http::{header, Request, StatusCode};
    use chrono::TimeZone;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;
    use crate::blob::{BlobIndex, EncBlobReader};
    use crate::block::{ChunkMeta, FileRef, PlainIndex};
    use crate::dek_provider::DekProvider;
    use crate::hash;
    use crate::hash::Hash;
    use crate::storage::{BackendKind, Local};
    use crate::tag::TagIndex;

    /// Build a test `DekProvider` holding a freshly generated KEK.
    /// Caller is responsible for leaking any tempdir backing the KEK
    /// if it needs to persist; this helper just generates an
    /// in-memory KEK and wraps it in `DekProvider::Local`.
    fn test_keys() -> DekProvider {
        let kek = crate::keys::kek::Kek::generate();
        DekProvider::Local {
            kek,
            kek_version: 0,
        }
    }

    /// Build a test `BackendKind::Local` backed by a leaked tempdir.
    /// The tempdir is leaked so the path survives for the test's
    /// lifetime; the OS cleans up temp files on process exit.
    fn test_backend() -> BackendKind {
        let tmp = tempfile::tempdir().unwrap();
        let backend = BackendKind::Local(Local::new(tmp.path()));
        std::mem::forget(tmp);
        backend
    }

    /// Build a test `EncBlobReader` with a fresh KEK and an empty
    /// local backend. The blob reader is not exercised by read-only
    /// listing tests; it is only needed so `ServeState` is fully
    /// populated.
    fn test_blob_reader() -> EncBlobReader {
        EncBlobReader::new(test_keys(), test_backend())
    }

    fn test_state(paths: &[&str]) -> ServeState {
        let tmp = tempfile::tempdir().unwrap();
        let store = RedbStore::open(&tmp.path().join("test.redb")).unwrap();

        let mut plain = PlainIndex::new_empty();
        let dummy_chunk = ChunkMeta {
            hash: Hash::from("1340aaaa"),
            size: 4096,
        };

        for (i, path) in paths.iter().enumerate() {
            let fileref = FileRef {
                chunkmetas: vec![dummy_chunk.clone()],
                paths: HashSet::from([PathBuf::from(path)]),
            };
            let file_hash = Hash::from(format!("1340{:028x}", i).as_str());
            plain.files.insert(file_hash.clone(), fileref);
        }

        store
            .populate_from_indexes(&plain, &BlobIndex::default(), &TagIndex::new())
            .unwrap();

        // Leak the tempdir so the redb file survives for test.
        // The OS cleans up temp files on process exit.
        std::mem::forget(tmp);

        ServeState {
            redb: Arc::new(store),
            bucket_name: "testvault".to_string(),
            index_updated_at: chrono::Utc
                .timestamp_opt(1718774400, 0)
                .unwrap()
                .naive_utc(),
            blob_reader: Arc::new(test_blob_reader()),
            cfg: Config::default(),
            keys: test_keys(),
            backend: test_backend(),
            write_mutex: Mutex::new(()),
        }
    }

    /// Build a router with a ready (populated) state.
    fn test_router(state: ServeState) -> Router {
        let lock = Arc::new(OnceLock::new());
        lock.set(state).unwrap();
        Router::new()
            .route("/_health", get(health_handler))
            .route("/", get(list_buckets_handler))
            .route("/{bucket}", get(list_objects_handler))
            .route(
                "/{bucket}/{*key}",
                get(get_object_handler)
                    .head(head_object_handler)
                    .put(put_object_handler),
            )
            .with_state(lock)
    }

    /// Build a router with an empty (not-yet-ready) state slot.
    fn not_ready_router() -> Router {
        let lock: Arc<OnceLock<ServeState>> = Arc::new(OnceLock::new());
        Router::new()
            .route("/_health", get(health_handler))
            .route("/", get(list_buckets_handler))
            .route("/{bucket}", get(list_objects_handler))
            .route(
                "/{bucket}/{*key}",
                get(get_object_handler)
                    .head(head_object_handler)
                    .put(put_object_handler),
            )
            .with_state(lock)
    }

    async fn body_string(body: Body) -> String {
        let bytes = body
            .collect()
            .await
            .expect("failed to read body")
            .to_bytes();
        String::from_utf8(bytes.to_vec()).expect("body is not UTF-8")
    }

    /// Read response body as raw bytes (for binary content tests).
    async fn body_bytes(body: Body) -> Vec<u8> {
        body.collect()
            .await
            .expect("failed to read body")
            .to_bytes()
            .to_vec()
    }

    /// Build a `ServeState` with real blob data. Writes `file_data`
    /// through `BlobBuffer` (chunking, encrypting, uploading to a
    /// local backend), then populates redb from the resulting
    /// indexes. The file is accessible at `path` in the virtual
    /// namespace.
    ///
    /// Returns the state and the original file data for assertions.
    async fn data_state(path: &str, file_data: &[u8]) -> (ServeState, Vec<u8>) {
        let tmp = tempfile::tempdir().unwrap();
        let redb_path = tmp.path().join("test.redb");
        let backend = BackendKind::Local(Local::new(tmp.path().join("data")));

        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };

        // Write file_data as a single chunk through BlobBuffer.
        let mut blob_idx = BlobIndex::new();
        let mut blob_buf = crate::blob::BlobBuffer::new(&backend, keys.clone());
        let mut chunk_data = file_data.to_vec();
        blob_buf
            .add_chunk(&mut chunk_data, &mut blob_idx)
            .await
            .unwrap();
        blob_buf.finalize(&mut blob_idx).await.unwrap();

        // Build a PlainIndex with a FileRef pointing at the chunk.
        let chunk_hash = crate::hash::multihash(file_data);
        let chunk_hash = Hash::from(chunk_hash.to_bytes());
        let chunk_meta = ChunkMeta {
            hash: chunk_hash.clone(),
            size: file_data.len(),
        };
        let fileref = FileRef {
            chunkmetas: vec![chunk_meta],
            paths: HashSet::from([PathBuf::from(path)]),
        };
        let file_hash = Hash::from(hash::multihash(b"file_hash_placeholder").to_bytes());
        let mut plain = PlainIndex::new_empty();
        plain.files.insert(file_hash, fileref);

        // Populate redb from both indexes.
        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(&plain, &blob_idx, &TagIndex::new())
            .unwrap();

        // Build the blob reader with the same keys and backend; the
        // state fields take clones so the write path has its own
        // handles.
        let blob_reader = EncBlobReader::new(keys.clone(), backend.clone());

        let state = ServeState {
            redb: Arc::new(store),
            bucket_name: "testvault".to_string(),
            index_updated_at: chrono::Utc
                .timestamp_opt(1718774400, 0)
                .unwrap()
                .naive_utc(),
            blob_reader: Arc::new(blob_reader),
            cfg: Config::default(),
            keys,
            backend,
            write_mutex: Mutex::new(()),
        };

        // Leak the tempdir so the backend and redb files survive.
        std::mem::forget(tmp);

        (state, file_data.to_vec())
    }

    #[tokio::test]
    async fn list_buckets_returns_xml() {
        let state = test_state(&[]);
        let app = test_router(state);

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/xml"
        );

        let body = body_string(response.into_body()).await;
        assert!(body.contains("ListAllMyBucketsResult"));
        assert!(body.contains("<Name>testvault</Name>"));
    }

    #[tokio::test]
    async fn list_objects_v2_basic() {
        let state = test_state(&["docs/readme.txt", "docs/api/intro.md", "photos/img.jpg"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("ListBucketResult"));
        assert!(body.contains("<Name>testvault</Name>"));
        assert!(body.contains("<KeyCount>3</KeyCount>"));
        assert!(body.contains("<Key>docs/api/intro.md</Key>"));
        assert!(body.contains("<Key>docs/readme.txt</Key>"));
        assert!(body.contains("<Key>photos/img.jpg</Key>"));
        assert!(body.contains("<IsTruncated>false</IsTruncated>"));
    }

    #[tokio::test]
    async fn list_objects_v2_with_prefix() {
        let state = test_state(&["docs/readme.txt", "docs/api/intro.md", "photos/img.jpg"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2&prefix=docs/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("<Prefix>docs/</Prefix>"));
        assert!(body.contains("<KeyCount>2</KeyCount>"));
        assert!(body.contains("<Key>docs/api/intro.md</Key>"));
        assert!(body.contains("<Key>docs/readme.txt</Key>"));
        assert!(!body.contains("photos/img.jpg"));
    }

    #[tokio::test]
    async fn list_objects_v2_with_delimiter() {
        let state = test_state(&[
            "docs/readme.txt",
            "docs/api/intro.md",
            "photos/img.jpg",
            "readme.md",
        ]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2&delimiter=/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("<Delimiter>/</Delimiter>"));
        assert!(body.contains("<Key>readme.md</Key>"));
        assert!(body.contains("<CommonPrefixes>"));
        assert!(body.contains("<Prefix>docs/</Prefix>"));
        assert!(body.contains("<Prefix>photos/</Prefix>"));
        assert!(body.contains("<KeyCount>3</KeyCount>"));
    }

    #[tokio::test]
    async fn list_objects_v2_pagination() {
        let state = test_state(&["a.txt", "b.txt", "c.txt"]);
        let app = test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2&max-keys=1")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("<IsTruncated>true</IsTruncated>"));
        assert!(body.contains("<KeyCount>1</KeyCount>"));
        assert!(body.contains("<Key>a.txt</Key>"));
        assert!(body.contains("<NextContinuationToken>"));

        let token_start =
            body.find("<NextContinuationToken>").unwrap() + "<NextContinuationToken>".len();
        let token_end = body[token_start..]
            .find("</NextContinuationToken>")
            .unwrap()
            + token_start;
        let token = &body[token_start..token_end];

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/testvault?list-type=2&max-keys=1&continuation-token={}",
                        urlencoding(token)
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("<Key>b.txt</Key>"));
        assert!(body.contains("<IsTruncated>true</IsTruncated>"));
    }

    #[tokio::test]
    async fn list_objects_v2_empty_prefix_no_match() {
        let state = test_state(&["docs/readme.txt"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2&prefix=nonexistent/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("<KeyCount>0</KeyCount>"));
        assert!(body.contains("<IsTruncated>false</IsTruncated>"));
        assert!(!body.contains("<Contents>"));
    }

    #[tokio::test]
    async fn list_objects_wrong_bucket_404() {
        let state = test_state(&["a.txt"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/wrongbucket?list-type=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NoSuchBucket"));
    }

    #[tokio::test]
    async fn list_objects_v1_not_implemented() {
        let state = test_state(&["a.txt"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_IMPLEMENTED);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NotImplemented"));
    }

    #[tokio::test]
    async fn list_objects_v2_max_keys_zero() {
        let state = test_state(&["a.txt", "b.txt"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2&max-keys=0")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("<KeyCount>0</KeyCount>"));
        assert!(body.contains("<MaxKeys>0</MaxKeys>"));
        assert!(body.contains("<IsTruncated>true</IsTruncated>"));
        assert!(!body.contains("<Contents>"));
    }

    fn urlencoding(s: &str) -> String {
        let mut out = String::with_capacity(s.len());
        for c in s.chars() {
            match c {
                '+' => out.push_str("%2B"),
                '/' => out.push_str("%2F"),
                '=' => out.push_str("%3D"),
                _ => out.push(c),
            }
        }
        out
    }

    #[tokio::test]
    async fn health_returns_503_when_not_ready() {
        let app = not_ready_router();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/_health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(response.into_body()).await;
        assert_eq!(body, "starting");
    }

    #[tokio::test]
    async fn list_buckets_returns_503_when_not_ready() {
        let app = not_ready_router();

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NotReady"));
    }

    #[tokio::test]
    async fn list_objects_returns_503_when_not_ready() {
        let app = not_ready_router();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault?list-type=2")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NotReady"));
    }

    #[tokio::test]
    async fn health_returns_200_when_ready() {
        let state = test_state(&["a.txt"]);
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/_health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.starts_with("ok ("));
    }

    #[tokio::test]
    async fn get_object_returns_full_file() {
        let file_data: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();
        let (state, original) = data_state("report.pdf", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/report.pdf")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "1024"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/octet-stream"
        );
        assert!(response.headers().get(header::LAST_MODIFIED).is_some());
        assert!(response.headers().get(header::ETAG).is_some());

        let body = body_bytes(response.into_body()).await;
        assert_eq!(body, original);
    }

    #[tokio::test]
    async fn head_object_returns_metadata_no_body() {
        let file_data: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();
        let (state, _) = data_state("doc.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/testvault/doc.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "512"
        );
        assert!(response.headers().get(header::LAST_MODIFIED).is_some());
        assert!(response.headers().get(header::ETAG).is_some());

        let body = body_bytes(response.into_body()).await;
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn get_object_with_range_closed() {
        let file_data: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();
        let (state, _) = data_state("data.bin", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/data.bin")
                    .header("Range", "bytes=100-199")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_LENGTH).unwrap(),
            "100"
        );
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 100-199/1024"
        );

        let body = body_bytes(response.into_body()).await;
        assert_eq!(body.len(), 100);
        assert_eq!(body, &file_data[100..200]);
    }

    #[tokio::test]
    async fn get_object_with_range_open_ended() {
        let file_data: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();
        let (state, _) = data_state("data.bin", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/data.bin")
                    .header("Range", "bytes=900-")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 900-1023/1024"
        );

        let body = body_bytes(response.into_body()).await;
        assert_eq!(body.len(), 124);
        assert_eq!(body, &file_data[900..]);
    }

    #[tokio::test]
    async fn get_object_with_range_suffix() {
        let file_data: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();
        let (state, _) = data_state("data.bin", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/data.bin")
                    .header("Range", "bytes=-50")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 974-1023/1024"
        );

        let body = body_bytes(response.into_body()).await;
        assert_eq!(body.len(), 50);
        assert_eq!(body, &file_data[974..]);
    }

    #[tokio::test]
    async fn get_object_range_beyond_eof_416() {
        let file_data: Vec<u8> = vec![0xAB; 100];
        let (state, _) = data_state("small.bin", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/small.bin")
                    .header("Range", "bytes=200-300")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("InvalidRange"));
    }

    #[tokio::test]
    async fn get_object_nonexistent_key_404() {
        let file_data: Vec<u8> = vec![0x01, 0x02, 0x03];
        let (state, _) = data_state("exists.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/nope.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NoSuchKey"));
    }

    #[tokio::test]
    async fn head_object_nonexistent_key_404() {
        let file_data: Vec<u8> = vec![0x01, 0x02, 0x03];
        let (state, _) = data_state("exists.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/testvault/nope.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn get_object_wrong_bucket_404() {
        let file_data: Vec<u8> = vec![0x01, 0x02, 0x03];
        let (state, _) = data_state("file.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/wrongbucket/file.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NoSuchBucket"));
    }

    #[tokio::test]
    async fn get_object_returns_503_when_not_ready() {
        let app = not_ready_router();

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/anything.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NotReady"));
    }

    #[tokio::test]
    async fn get_object_range_start_at_zero() {
        let file_data: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let (state, _) = data_state("range.bin", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/range.bin")
                    .header("Range", "bytes=0-9")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 0-9/256"
        );

        let body = body_bytes(response.into_body()).await;
        assert_eq!(body.len(), 10);
        assert_eq!(body, &file_data[0..10]);
    }

    #[tokio::test]
    async fn get_object_range_clamps_end_past_eof() {
        let file_data: Vec<u8> = vec![0xAB; 100];
        let (state, _) = data_state("clamp.bin", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/clamp.bin")
                    .header("Range", "bytes=50-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        // S3 clamps end to EOF rather than returning 416
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(
            response.headers().get(header::CONTENT_RANGE).unwrap(),
            "bytes 50-99/100"
        );

        let body = body_bytes(response.into_body()).await;
        assert_eq!(body.len(), 50);
        assert_eq!(body, &file_data[50..]);
    }
}
