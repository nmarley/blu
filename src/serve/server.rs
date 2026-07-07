//! HTTP server for `blu serve`: a local S3-compatible translation
//! layer that presents decrypted files to any S3 client while the real
//! backend holds only opaque encrypted blobs.
//!
//! Stage 3 added `ListObjectsV2` and `ListBuckets` (read-only listing).
//! Stage 4 adds `GetObject` (with byte-range support), `HeadObject`,
//! and the `EncBlobReader` LRU cache for serving chunk data from
//! decrypted blobs. Internal endpoints live under `/_` prefix
//! (e.g., `/_health`) to avoid collision with S3 bucket names.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::body::Bytes;
use axum::extract::{Path, Query};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chrono::NaiveDateTime;
use rand::RngCore;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

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

/// Index flush debounce interval. After each successful write
/// (PutObject or DeleteObject) a debounce timer is scheduled; when it
/// fires, redb state is dumped to the on-disk indexes and pushed to
/// the backend. Replacing the pending timer on each write coalesces
/// bursts of writes into a single flush.
const FLUSH_DEBOUNCE: std::time::Duration = std::time::Duration::from_secs(5);

/// In-progress multipart upload. Stored in
/// `ServeState::multipart_uploads` keyed by `upload_id`. Parts are
/// buffered in memory; on `CompleteMultipartUpload` they are
/// concatenated and run through the standard PutObject pipeline.
pub(crate) struct MultipartState {
    /// Object key (path) the upload targets.
    path: String,
    /// Buffered part bytes, 0-indexed. S3 part numbers are 1-indexed;
    /// we store parts[N-1] at index N-1.
    parts: Vec<Vec<u8>>,
    /// Creation timestamp, used for staleness reaping if we ever add
    /// a GC pass. Currently informational only.
    created_at: NaiveDateTime,
}

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
    cfg: Config,
    /// DEK provider. Used by the write path for envelope encryption
    /// of new blobs and by the index flush for encrypting index
    /// updates.
    keys: DekProvider,
    /// Storage backend. Used by the write path for uploading new
    /// blobs and by the index flush to push encrypted indexes.
    backend: BackendKind,
    /// Serializes all write-path mutations so redb state stays
    /// consistent and no two writes overlap. PutObject, DeleteObject,
    /// and the index flush all acquire this lock. Read-path handlers
    /// do not need it; they only read redb and the
    /// `EncBlobReader` cache.
    write_mutex: Mutex<()>,
    /// Debounce handle for the index flush. After each successful
    /// write, [`schedule_flush`] resets this timer; when it fires the
    /// flush task acquires `write_mutex` and dumps redb state to the
    /// on-disk indexes and the backend. Aborted on graceful shutdown
    /// in favor of a final inline flush.
    flush_timer: Mutex<Option<JoinHandle<()>>>,
    /// In-progress multipart uploads keyed by `upload_id`. Separate
    /// from `write_mutex` because part uploads do not touch redb or
    /// the blob pipeline until completion.
    multipart_uploads: Mutex<HashMap<String, MultipartState>>,
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
                .put(put_object_handler)
                .post(multipart_post_handler)
                .delete(delete_object_handler),
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
                    flush_timer: Mutex::new(None),
                    multipart_uploads: Mutex::new(HashMap::new()),
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

    // Final best-effort flush so no writes are lost on shutdown.
    // Abort the debounce timer (if any) and run the flush inline.
    if let Some(state) = ready.get() {
        if let Some(handle) = state.flush_timer.lock().await.take() {
            handle.abort();
        }
        match flush_indexes(state).await {
            Ok(()) => info!("final index flush complete"),
            Err(e) => error!("final index flush failed: {}", e),
        }
    }

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
/// index and the storage backend.
///
/// Removes the FileRef, its path entries, decrements BlockRef
/// references (removing chunks that become unreferenced from the blob
/// index), strips the file from any tag_index entries, then deletes
/// any fully-dead blob files from the storage backend.
///
/// `PutObject` calls this when overwriting an existing path so the
/// old file's chunks are reclaimed before the new FileRef is written.
/// `DeleteObject` calls this directly.
///
/// Backend blob deletion is best-effort: if a blob file cannot be
/// deleted (e.g., transient S3 error), a warning is logged and the
/// cascade continues. The redb index is already consistent; an
/// orphaned blob is non-fatal and can be reclaimed by a future
/// defrag pass.
///
/// Caller must already hold `ServeState::write_mutex`. Returns the
/// [`DeleteStats`] produced by [`RedbStore::delete_object_index`].
async fn delete_file_cascade(
    state: &ServeState,
    file_hash: &crate::hash::Hash,
    path: &str,
) -> Result<crate::serve::redb_store::DeleteStats, BluError> {
    let stats = state.redb.delete_object_index(file_hash, path)?;

    // Delete fully-dead blob files from the backend. The redb
    // transaction has already committed, so the index is consistent
    // even if a backend delete fails.
    for blob_path in &stats.blobs_dead {
        if let Err(e) = state.backend.delete(blob_path).await {
            warn!(
                "failed to delete dead blob {} from backend: {} \
                 (orphaned blob, will be reclaimed by defrag)",
                blob_path.display(),
                e,
            );
        }
    }

    Ok(stats)
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

/// `PUT /{bucket}/{*key}` -- PutObject (or UploadPart when the query
/// string contains `partNumber` and `uploadId`). Stores the request
/// body as a new object at the given virtual path, chunking,
/// deduplicating, encrypting, and uploading new chunks to the storage
/// backend, then updating redb index state atomically.
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
    params: Query<HashMap<String, String>>,
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

    // Dispatch multipart part upload when the query string carries
    // `partNumber` and `uploadId`. The plain PutObject path holds
    // below.
    if let (Some(part_number), Some(upload_id)) = (params.get("partNumber"), params.get("uploadId"))
    {
        return upload_part(
            state.0.clone(),
            upload_id.clone(),
            part_number.clone(),
            body,
        )
        .await
        .into_response();
    }

    // All write-path work happens under the write mutex so redb state
    // stays consistent and no two PutObjects overlap. The lock is
    // held for the full chunk/pack/encrypt/upload/index-update cycle.
    let _guard = s.write_mutex.lock().await;
    match put_object_full(s, &key, &body).await {
        Ok(file_hash) => {
            let etag = format!("\"{}\"", file_hash.dbg_short(16));
            let mut headers = HeaderMap::new();
            headers.insert(header::ETAG, etag.parse().unwrap());
            schedule_flush(state.0.clone()).await;
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

/// Full PutObject pipeline: run the overwrite cascade if the path
/// already exists, then chunk, dedup, pack, encrypt, upload, and
/// update redb. Shared by the plain PutObject handler and
/// CompleteMultipartUpload.
///
/// The caller must already hold `ServeState::write_mutex`.
async fn put_object_full(state: &ServeState, key: &str, body: &[u8]) -> Result<Hash, BluError> {
    // Overwrite: if the path already exists, cascade-delete the old
    // file's reference to this path before writing the new one. The
    // old file's chunks are decremented in block_index and removed
    // from blob_index when unreferenced; the blob files themselves
    // are reclaimed later by the index flush. Only the single path
    // is removed from the old FileRef; if another path still
    // references the same content, the old FileRef and blobs survive.
    if let Some(old_file_hash) = state.redb.get_file_hash_by_path(key)? {
        delete_file_cascade(state, &old_file_hash, key).await?;
    }
    put_object_inner(state, key, body).await
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

/// `DELETE /{bucket}/{*key}` -- DeleteObject. Removes the object at
/// the given virtual path from the index and deletes any fully-dead
/// blob files from the storage backend.
///
/// Returns 204 No Content on success (the S3 convention for
/// DeleteObject). Returns 404 `NoSuchKey` if the key does not exist,
/// 404 `NoSuchBucket` if the bucket does not match, 503 if the index
/// sync has not completed, 500 `InternalError` on any index or
/// backend failure.
async fn delete_object_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
    Path((bucket, key)): Path<(String, String)>,
    params: Query<HashMap<String, String>>,
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

    // Dispatch multipart abort when the query string carries
    // `uploadId`. The plain DeleteObject path holds below.
    if let Some(upload_id) = params.get("uploadId") {
        return abort_multipart(state.0.clone(), upload_id.clone())
            .await
            .into_response();
    }

    // All write-path work happens under the write mutex so redb state
    // stays consistent and no two writes overlap.
    let _guard = s.write_mutex.lock().await;

    // Resolve path -> file_hash. 404 if the path does not exist.
    let file_hash = match s.redb.get_file_hash_by_path(&key) {
        Ok(Some(h)) => h,
        Ok(None) => {
            return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchKey", &key).into_response();
        }
        Err(e) => {
            return s3xml::error_response(
                StatusCode::INTERNAL_SERVER_ERROR,
                "InternalError",
                &format!("path lookup failed: {e}"),
            )
            .into_response();
        }
    };

    match delete_file_cascade(s, &file_hash, &key).await {
        Ok(_) => {
            schedule_flush(state.0.clone()).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => s3xml::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            &e.to_string(),
        )
        .into_response(),
    }
}

/// Dump redb state to the on-disk indexes and push them to the
/// backend. Acquires `write_mutex` so it does not race with in-flight
/// writes. Shared by the debounce task ([`schedule_flush`]) and the
/// graceful-shutdown final flush.
///
/// Reads redb (no write transaction needed), constructs
/// `(PlainIndex, BlobIndex, TagIndex)` via
/// [`RedbStore::dump_to_indexes`], writes each to `cfg.idxdir()`, then
/// calls [`Config::push_indexes`] to upload the encrypted files.
async fn flush_indexes(state: &ServeState) -> Result<(), BluError> {
    let _guard = state.write_mutex.lock().await;
    let (plain, blob, tag) = state.redb.dump_to_indexes()?;
    state.cfg.write_plain_index(&plain, &state.keys)?;
    state.cfg.write_blob_index(&blob, &state.keys)?;
    state.cfg.write_tag_index(&tag, &state.keys)?;
    state.cfg.push_indexes(&state.backend).await?;
    Ok(())
}

/// Schedule a debounced index flush. Aborts any pending flush timer
/// and spawns a new task that sleeps for [`FLUSH_DEBOUNCE`] then runs
/// [`flush_indexes`]. Coalesces bursts of writes into a single flush.
///
/// The caller typically still holds `write_mutex` (the handler's
/// `_guard`); that is fine because this helper only locks
/// `flush_timer`, not `write_mutex`. The spawned task takes
/// `write_mutex` only after it sleeps, by which point the handler has
/// returned and dropped its guard.
async fn schedule_flush(ready: Arc<OnceLock<ServeState>>) {
    // Lock the timer slot, abort any pending flush, and install the
    // new handle. If the OnceLock is still empty (sync not complete)
    // there is nothing to schedule.
    let Some(state) = ready.get() else {
        return;
    };
    let mut guard = state.flush_timer.lock().await;
    if let Some(prev) = guard.take() {
        prev.abort();
    }
    let ready_for_task = ready.clone();
    *guard = Some(tokio::spawn(async move {
        tokio::time::sleep(FLUSH_DEBOUNCE).await;
        let Some(s) = ready_for_task.get() else {
            return;
        };
        match flush_indexes(s).await {
            Ok(()) => info!("debounced index flush complete"),
            Err(e) => warn!("debounced index flush failed: {}", e),
        }
    }));
    drop(guard);
}

/// `POST /{bucket}/{*key}` -- CreateMultipartUpload (when the query
/// string contains `uploads`) or CompleteMultipartUpload (when the
/// query string contains `uploadId`). The single POST handler
/// dispatches on query params, matching S3's routing conventions.
///
/// CreateMultipartUpload generates a random 16-byte hex `upload_id`,
/// stores a fresh `MultipartState` in `ServeState::multipart_uploads`,
/// and returns `InitiateMultipartUploadResult` XML.
///
/// CompleteMultipartUpload acquires `write_mutex`, removes the
/// `MultipartState`, concatenates all parts in order into a single
/// byte vector, and runs the full PutObject pipeline
/// ([`put_object_full`]). Returns `CompleteMultipartUploadResult`
/// XML with the final ETag.
async fn multipart_post_handler(
    state: axum::extract::State<Arc<OnceLock<ServeState>>>,
    Path((bucket, key)): Path<(String, String)>,
    params: Query<HashMap<String, String>>,
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

    // CompleteMultipartUpload takes precedence: S3 routes on the
    // presence of `uploadId` first.
    if let Some(upload_id) = params.get("uploadId") {
        return complete_multipart(state.0.clone(), upload_id.clone(), &bucket, &key)
            .await
            .into_response();
    }

    // CreateMultipartUpload: query string contains `uploads`.
    if params.contains_key("uploads") {
        return create_multipart(s, &bucket, &key).await.into_response();
    }

    // No recognized query param: bail with a generic error.
    s3xml::error_response(
        StatusCode::BAD_REQUEST,
        "InvalidRequest",
        "POST without `uploads` or `uploadId` is not supported",
    )
    .into_response()
}

/// CreateMultipartUpload: generate a random `upload_id`, insert a
/// fresh `MultipartState` into the uploads map, and return the S3
/// `InitiateMultipartUploadResult` XML response.
async fn create_multipart(state: &ServeState, bucket: &str, key: &str) -> axum::response::Response {
    let upload_id = generate_upload_id();
    let now = chrono::Utc::now().naive_utc();
    let mpu = MultipartState {
        path: key.to_string(),
        parts: Vec::new(),
        created_at: now,
    };
    state
        .multipart_uploads
        .lock()
        .await
        .insert(upload_id.clone(), mpu);
    let xml = s3xml::initiate_multipart_upload(bucket, key, &upload_id);
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/xml")],
        xml,
    )
        .into_response()
}

/// Generate a random 16-byte hex upload_id (32 ASCII chars).
fn generate_upload_id() -> String {
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

/// UploadPart: append the request body to the multipart upload named
/// by `upload_id` at slot `part_number - 1`. Part numbers are
/// 1-indexed in S3; we store 0-indexed. If the upload_id is not
/// found, returns 404 `NoSuchUpload`. On success returns 200 with an
/// `ETag` header containing the hex hash of the part bytes (quoted).
async fn upload_part(
    ready: Arc<OnceLock<ServeState>>,
    upload_id: String,
    part_number: String,
    body: Bytes,
) -> axum::response::Response {
    let Some(state) = ready.get() else {
        return s3xml::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "NotReady",
            "index sync in progress",
        )
        .into_response();
    };
    let part_idx: usize = match part_number.parse::<usize>() {
        Ok(n) if n >= 1 => n - 1,
        _ => {
            return s3xml::error_response(
                StatusCode::BAD_REQUEST,
                "InvalidArgument",
                "partNumber must be a positive integer",
            )
            .into_response();
        }
    };
    let mut uploads = state.multipart_uploads.lock().await;
    let Some(mpu) = uploads.get_mut(&upload_id) else {
        return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchUpload", &upload_id)
            .into_response();
    };
    // Extend or insert at part_idx. Parts may arrive out of order;
    // fill any gap with empty Vecs so parts[part_idx] is valid.
    while mpu.parts.len() <= part_idx {
        mpu.parts.push(Vec::new());
    }
    mpu.parts[part_idx] = body.to_vec();
    let etag = format!("\"{}\"", hex::encode(hash::sha512(&body)));
    drop(uploads);
    let mut headers = HeaderMap::new();
    headers.insert(header::ETAG, etag.parse().unwrap());
    (StatusCode::OK, headers, String::new()).into_response()
}

/// CompleteMultipartUpload: remove the multipart state, concatenate
/// all parts in order, and run the full PutObject pipeline. Requires
/// the `write_mutex`; part uploads have not touched redb yet, so the
/// flush does not race with in-flight UploadPart calls (the state is
/// already removed before we take the lock).
async fn complete_multipart(
    ready: Arc<OnceLock<ServeState>>,
    upload_id: String,
    bucket: &str,
    key: &str,
) -> axum::response::Response {
    let Some(state) = ready.get() else {
        return s3xml::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "NotReady",
            "index sync in progress",
        )
        .into_response();
    };
    // Remove the upload state before taking write_mutex. Parts must
    // already be buffered; no more UploadPart calls can arrive.
    let mpu = {
        let mut uploads = state.multipart_uploads.lock().await;
        match uploads.remove(&upload_id) {
            Some(m) => m,
            None => {
                return s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchUpload", &upload_id)
                    .into_response();
            }
        }
    };
    // The path recorded at CreateMultipartUpload time is the
    // authoritative key; the URL key should match but we trust the
    // original.
    let write_key = mpu.path.clone();
    debug!(
        "completing multipart upload {} for key {} (created at {})",
        upload_id, write_key, mpu.created_at
    );
    // Concatenate all parts in order. Any empty slots contribute no
    // bytes, matching S3's "ignore missing parts" behavior for
    // out-of-range gaps (real S3 rejects gaps; we tolerate them).
    let mut body = Vec::with_capacity(mpu.parts.iter().map(|p| p.len()).sum());
    for part in &mpu.parts {
        body.extend_from_slice(part);
    }

    let _guard = state.write_mutex.lock().await;
    match put_object_full(state, &write_key, &body).await {
        Ok(file_hash) => {
            schedule_flush(ready.clone()).await;
            let etag = format!("\"{}\"", file_hash.dbg_short(16));
            let location = format!("http://{}/{}", bucket, key);
            let xml = s3xml::complete_multipart_upload(&location, bucket, key, &etag);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/xml")],
                xml,
            )
                .into_response()
        }
        Err(e) => s3xml::error_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            "InternalError",
            &e.to_string(),
        )
        .into_response(),
    }
}

/// AbortMultipartUpload: remove the multipart state and return 204.
/// Discards all buffered part data. If the upload_id is not found,
/// returns 404 `NoSuchUpload`.
async fn abort_multipart(
    ready: Arc<OnceLock<ServeState>>,
    upload_id: String,
) -> axum::response::Response {
    let Some(state) = ready.get() else {
        return s3xml::error_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "NotReady",
            "index sync in progress",
        )
        .into_response();
    };
    let mut uploads = state.multipart_uploads.lock().await;
    if uploads.remove(&upload_id).is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        s3xml::error_response(StatusCode::NOT_FOUND, "NoSuchUpload", &upload_id).into_response()
    }
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
    use crate::block::{BlockRef, ChunkMeta, FileRef, PlainIndex};
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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
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
                    .put(put_object_handler)
                    .post(multipart_post_handler)
                    .delete(delete_object_handler),
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
                    .put(put_object_handler)
                    .post(multipart_post_handler)
                    .delete(delete_object_handler),
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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
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

    /// Deterministic low-compressibility bytes so the blob's compressed
    /// stream spans several 512 KiB v3 segments.
    fn pseudo_random_bytes(seed: u64, len: usize) -> Vec<u8> {
        let mut out = Vec::with_capacity(len);
        let mut state = seed | 1;
        while out.len() < len {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            out.extend_from_slice(&state.to_le_bytes());
        }
        out.truncate(len);
        out
    }

    /// Stage 6g.4: End-to-end serve range read over a multi-segment v3
    /// blob. Build a file from several incompressible chunks (one v3
    /// blob, many segments), GET an early byte window, and assert the
    /// bytes are correct AND the backend served strictly fewer bytes
    /// than the whole blob. This proves the prefix-fetch win reaches
    /// the HTTP layer, not just the reader.
    #[tokio::test]
    async fn get_early_range_fetches_prefix_not_whole_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let redb_path = tmp.path().join("test.redb");
        let local = Local::new(tmp.path().join("data"));
        let backend = BackendKind::Local(local.clone());

        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };

        // Eight ~256 KiB incompressible chunks packed into one v3 blob.
        let chunk_len = 256 * 1024;
        let chunks: Vec<Vec<u8>> = (0..8)
            .map(|i| pseudo_random_bytes(0x5eed_0000 ^ i as u64, chunk_len))
            .collect();
        let file_data: Vec<u8> = chunks.iter().flatten().copied().collect();

        let mut blob_idx = BlobIndex::new();
        let mut blob_buf = crate::blob::BlobBuffer::new(&backend, keys.clone());
        let mut chunkmetas = Vec::new();
        for c in &chunks {
            let mut data = c.clone();
            let hash = Hash::from(crate::hash::multihash(c).to_bytes());
            chunkmetas.push(ChunkMeta {
                hash,
                size: c.len(),
            });
            blob_buf.add_chunk(&mut data, &mut blob_idx).await.unwrap();
        }
        blob_buf.finalize(&mut blob_idx).await.unwrap();

        // Confirm a single multi-segment v3 blob was written.
        assert_eq!(blob_idx.count_blob_files(), 1);
        let blob_path = blob_idx
            .map
            .get(&chunkmetas[0].hash)
            .map(|loc| loc.blob_path().clone())
            .unwrap();
        let whole_blob_len = backend.read_data(&blob_path).await.unwrap().len() as u64;

        let fileref = FileRef {
            chunkmetas,
            paths: HashSet::from([PathBuf::from("video.bin")]),
        };
        let file_hash = Hash::from(hash::multihash(b"file_hash_placeholder").to_bytes());
        let mut plain = PlainIndex::new_empty();
        plain.files.insert(file_hash, fileref);

        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(&plain, &blob_idx, &TagIndex::new())
            .unwrap();

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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };
        std::mem::forget(tmp);
        let app = test_router(state);

        // GET an early window that lives entirely within the first
        // chunk, so only the front segment prefix must be fetched.
        let baseline = local.bytes_read();
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/video.bin")
                    .header("Range", "bytes=0-999")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        let body = body_bytes(response.into_body()).await;
        assert_eq!(body, &file_data[0..1000], "early range bytes mismatch");

        let fetched = local.bytes_read() - baseline;
        assert!(
            fetched < whole_blob_len,
            "early range fetched {} bytes, expected strictly less than whole blob {}",
            fetched,
            whole_blob_len
        );
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

    /// Build a `ServeState` with real blob data and proper BlockRef
    /// entries in redb. Unlike `data_state`, this populates
    /// `plain.blocks` so the delete cascade can find and decrement
    /// BlockRef references.
    ///
    /// Writes `file_data` as a single chunk through `BlobBuffer`, then
    /// builds a PlainIndex with a FileRef and a BlockRef pointing at
    /// the chunk. The file is accessible at `path` in the virtual
    /// namespace.
    ///
    /// Returns the state, the original file data, and the blob path
    /// (so tests can assert on backend deletion).
    async fn data_state_with_blocks(
        path: &str,
        file_data: &[u8],
    ) -> (ServeState, Vec<u8>, std::path::PathBuf) {
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

        // Extract the blob path from the blob index.
        let chunk_hash = crate::hash::multihash(file_data);
        let chunk_hash = Hash::from(chunk_hash.to_bytes());
        let blob_path = blob_idx
            .map
            .get(&chunk_hash)
            .map(|loc| loc.blob_path().clone())
            .unwrap();

        // Build a PlainIndex with FileRef and BlockRef.
        let chunk_meta = ChunkMeta {
            hash: chunk_hash.clone(),
            size: file_data.len(),
        };
        let fileref = FileRef {
            chunkmetas: vec![chunk_meta],
            paths: HashSet::from([PathBuf::from(path)]),
        };
        let file_hash = Hash::from(hash::multihash(b"file_hash_placeholder").to_bytes());

        let mut blockref = BlockRef::new();
        blockref.references.insert(
            file_hash.clone(),
            blob_idx.map.get(&chunk_hash).unwrap().position.clone(),
        );

        let mut plain = PlainIndex::new_empty();
        plain.files.insert(file_hash.clone(), fileref);
        plain.blocks.insert(chunk_hash, blockref);

        // Populate redb from both indexes.
        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(&plain, &blob_idx, &TagIndex::new())
            .unwrap();

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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };

        // Leak the tempdir so the backend and redb files survive.
        std::mem::forget(tmp);

        (state, file_data.to_vec(), blob_path)
    }

    /// Build a `ServeState` with two files that share a single chunk
    /// (same content, different paths). Both files reference the same
    /// blob, so deleting one file must NOT delete the blob.
    ///
    /// Returns the state and the blob path.
    async fn shared_chunk_state(
        path1: &str,
        path2: &str,
        file_data: &[u8],
    ) -> (ServeState, std::path::PathBuf) {
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

        let chunk_hash = crate::hash::multihash(file_data);
        let chunk_hash = Hash::from(chunk_hash.to_bytes());
        let blob_path = blob_idx
            .map
            .get(&chunk_hash)
            .map(|loc| loc.blob_path().clone())
            .unwrap();
        let position = blob_idx.map.get(&chunk_hash).unwrap().position.clone();

        // Two distinct file hashes, both referencing the same chunk.
        let file_hash_1 = Hash::from(hash::multihash(b"file_hash_1").to_bytes());
        let file_hash_2 = Hash::from(hash::multihash(b"file_hash_2").to_bytes());

        let chunk_meta = ChunkMeta {
            hash: chunk_hash.clone(),
            size: file_data.len(),
        };
        let fileref_1 = FileRef {
            chunkmetas: vec![chunk_meta.clone()],
            paths: HashSet::from([PathBuf::from(path1)]),
        };
        let fileref_2 = FileRef {
            chunkmetas: vec![chunk_meta],
            paths: HashSet::from([PathBuf::from(path2)]),
        };

        let mut blockref = BlockRef::new();
        blockref
            .references
            .insert(file_hash_1.clone(), position.clone());
        blockref.references.insert(file_hash_2.clone(), position);

        let mut plain = PlainIndex::new_empty();
        plain.files.insert(file_hash_1, fileref_1);
        plain.files.insert(file_hash_2, fileref_2);
        plain.blocks.insert(chunk_hash, blockref);

        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(&plain, &blob_idx, &TagIndex::new())
            .unwrap();

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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };

        std::mem::forget(tmp);

        (state, blob_path)
    }

    #[tokio::test]
    async fn delete_object_returns_204() {
        let file_data: Vec<u8> = (0..256u32).map(|i| i as u8).collect();
        let (state, _) = data_state("delme.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/delme.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let body = body_bytes(response.into_body()).await;
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn delete_object_then_get_404() {
        let file_data: Vec<u8> = vec![0x01, 0x02, 0x03, 0x04];
        let (state, _) = data_state("temp.txt", &file_data).await;
        let app = test_router(state);

        // DELETE the file.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/temp.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // GET returns 404.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/temp.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = body_string(response.into_body()).await;
        assert!(body.contains("NoSuchKey"));

        // HEAD returns 404.
        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/testvault/temp.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn delete_object_nonexistent_404() {
        let file_data: Vec<u8> = vec![0x01];
        let (state, _) = data_state("exists.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
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
    async fn delete_object_wrong_bucket_404() {
        let file_data: Vec<u8> = vec![0x01];
        let (state, _) = data_state("exists.txt", &file_data).await;
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/wrongbucket/exists.txt")
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
    async fn delete_object_returns_503_when_not_ready() {
        let app = not_ready_router();

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
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
    async fn delete_object_removes_blob_from_backend() {
        let file_data: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();
        let (state, _, blob_path) = data_state_with_blocks("unique.bin", &file_data).await;

        // The blob should exist before deletion.
        assert!(
            state.backend.exists(&blob_path).await.unwrap(),
            "blob should exist before delete"
        );

        // Clone the backend before the state is moved into the router.
        let backend = state.backend.clone();
        let app = test_router(state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/unique.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The blob should be gone from the backend after deletion,
        // because the file was the only consumer of its chunk.
        assert!(
            !backend.exists(&blob_path).await.unwrap(),
            "blob should be gone from backend after delete"
        );
    }

    #[tokio::test]
    async fn delete_object_preserves_shared_blob() {
        let file_data: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();
        let (state, blob_path) = shared_chunk_state("file_a.bin", "file_b.bin", &file_data).await;

        // The blob should exist before deletion.
        assert!(
            state.backend.exists(&blob_path).await.unwrap(),
            "blob should exist before delete"
        );

        // We need the backend handle after the router consumes state.
        let backend = state.backend.clone();
        let app = test_router(state);

        // Delete file_a.bin.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/file_a.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // file_b.bin should still be accessible.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/file_b.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // The blob should still exist because file_b.bin still
        // references the shared chunk.
        assert!(
            backend.exists(&blob_path).await.unwrap(),
            "blob should still exist after deleting one of two sharing files"
        );

        // Now delete file_b.bin too.
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/file_b.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Now the blob should be gone, as no files reference it.
        assert!(
            !backend.exists(&blob_path).await.unwrap(),
            "blob should be gone after deleting both sharing files"
        );
    }

    #[tokio::test]
    async fn put_object_overwrite_deletes_old_blob() {
        let original_data: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();
        let (state, _, blob_path) = data_state_with_blocks("overwrite.bin", &original_data).await;

        // The blob should exist before overwrite.
        assert!(
            state.backend.exists(&blob_path).await.unwrap(),
            "blob should exist before overwrite"
        );

        // We need the backend and blob_path after the router consumes
        // state.
        let backend = state.backend.clone();
        let app = test_router(state);

        // PUT different content to the same path.
        let new_data: Vec<u8> = vec![0xFF; 300];
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/overwrite.bin")
                    .body(Body::from(new_data.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // GET should return the new content.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/overwrite.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_bytes(response.into_body()).await;
        assert_eq!(body, new_data);

        // The old blob should be deleted from the backend because
        // the old file was the only consumer of its chunk and the
        // overwrite cascade should have cleaned it up.
        assert!(
            !backend.exists(&blob_path).await.unwrap(),
            "old blob should be gone after overwrite"
        );
    }

    /// Stage 5f.6: After a write, calling `flush_indexes` directly
    /// (skipping the debounce timer for determinism) must produce
    /// the three encrypted index files in `cfg.idxdir()` and push
    /// them to the backend so a subsequent `sync_from_backend` can
    /// recover them.
    #[tokio::test]
    async fn flush_indexes_writes_local_files_and_pushes_to_backend() {
        let tmp = tempfile::tempdir().unwrap();
        let redb_path = tmp.path().join("test.redb");
        let backend = BackendKind::Local(Local::new(tmp.path().join("data")));

        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };

        // Write one chunk through BlobBuffer so redb has something to dump.
        let file_data = b"flush me to disk and backend\n".to_vec();
        let mut blob_idx = BlobIndex::new();
        let mut blob_buf = crate::blob::BlobBuffer::new(&backend, keys.clone());
        let mut chunk_data = file_data.clone();
        blob_buf
            .add_chunk(&mut chunk_data, &mut blob_idx)
            .await
            .unwrap();
        blob_buf.finalize(&mut blob_idx).await.unwrap();

        let chunk_hash = crate::hash::multihash(&file_data);
        let chunk_hash = Hash::from(chunk_hash.to_bytes());
        let chunk_meta = ChunkMeta {
            hash: chunk_hash,
            size: file_data.len(),
        };
        let fileref = FileRef {
            chunkmetas: vec![chunk_meta],
            paths: HashSet::from([PathBuf::from("flush.bin")]),
        };
        let file_hash = Hash::from(hash::multihash(b"flush_file_hash").to_bytes());
        let mut plain = PlainIndex::new_empty();
        plain.files.insert(file_hash, fileref);

        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(&plain, &blob_idx, &TagIndex::new())
            .unwrap();

        // Build a Config whose basedir is the tempdir so idxdir()
        // resolves to tmp/.blu/indexes/ and does not pollute the
        // repo working directory.
        let mut cfg = Config::default();
        cfg.set_basedir(tmp.path().to_path_buf());
        std::fs::create_dir_all(cfg.idxdir()).unwrap();

        let state = ServeState {
            redb: Arc::new(store),
            bucket_name: "testvault".to_string(),
            index_updated_at: chrono::Utc
                .timestamp_opt(1718774400, 0)
                .unwrap()
                .naive_utc(),
            blob_reader: Arc::new(EncBlobReader::new(keys.clone(), backend.clone())),
            cfg,
            keys: keys.clone(),
            backend: backend.clone(),
            write_mutex: Mutex::new(()),
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };

        // Run the flush inline. No debounce timer wait.
        flush_indexes(&state).await.unwrap();

        // All three index files must exist in the local idxdir.
        let idxdir = state.cfg.idxdir();
        let plain_path = idxdir.join("index.dat");
        let blob_path = idxdir.join("blob_index.dat");
        let tag_path = idxdir.join("tags.dat");
        assert!(plain_path.exists(), "plain index not written");
        assert!(blob_path.exists(), "blob index not written");
        assert!(tag_path.exists(), "tag index not written");

        // Each local file must be non-empty (encrypted CBOR).
        let plain_bytes = std::fs::read(&plain_path).unwrap();
        assert!(!plain_bytes.is_empty(), "plain index empty");
        let blob_bytes = std::fs::read(&blob_path).unwrap();
        assert!(!blob_bytes.is_empty(), "blob index empty");
        let tag_bytes = std::fs::read(&tag_path).unwrap();
        assert!(!tag_bytes.is_empty(), "tag index empty");

        // The push must have uploaded the same bytes to the backend
        // at the `indexes/` prefix. Verify via read_from_path.
        let remote_plain = state
            .backend
            .read_from_path(&std::path::Path::new("indexes").join("index.dat"))
            .await
            .unwrap();
        assert_eq!(remote_plain, plain_bytes, "pushed plain index mismatch");
        let remote_blob = state
            .backend
            .read_from_path(&std::path::Path::new("indexes").join("blob_index.dat"))
            .await
            .unwrap();
        assert_eq!(remote_blob, blob_bytes, "pushed blob index mismatch");
        let remote_tag = state
            .backend
            .read_from_path(&std::path::Path::new("indexes").join("tags.dat"))
            .await
            .unwrap();
        assert_eq!(remote_tag, tag_bytes, "pushed tag index mismatch");
    }

    /// Stage 5f.5a: End-to-end multipart upload. CreateMultipartUpload
    /// -> UploadPart x3 -> CompleteMultipartUpload, then GET the
    /// object and verify byte-for-byte equality with the
    /// concatenation of all parts.
    #[tokio::test]
    async fn multipart_upload_round_trip() {
        // Use an empty state so the multipart upload is the only
        // write. Build the state from scratch so the same backend
        // wires into both `state.backend` and `state.blob_reader`.
        let tmp = tempfile::tempdir().unwrap();
        let redb_path = tmp.path().join("test.redb");
        let backend = BackendKind::Local(Local::new(tmp.path().join("data")));

        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };
        let blob_reader = EncBlobReader::new(keys.clone(), backend.clone());

        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(
                &PlainIndex::new_empty(),
                &BlobIndex::new(),
                &TagIndex::new(),
            )
            .unwrap();

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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };
        std::mem::forget(tmp);
        let app = test_router(state);

        // Step 1: CreateMultipartUpload via POST with `?uploads`.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/testvault/multipart.bin?uploads")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let xml = body_string(response.into_body()).await;
        assert!(xml.contains("InitiateMultipartUploadResult"), "got: {xml}");
        // Extract the UploadId from the XML.
        let upload_id = extract_xml_value(&xml, "UploadId").expect("UploadId not found in XML");
        assert!(!upload_id.is_empty());

        // Step 2: UploadPart x3 via PUT with `?partNumber=N&uploadId=X`.
        let parts: [Vec<u8>; 3] = [
            b"hello ".to_vec(),
            b"brave new ".to_vec(),
            b"world\n".to_vec(),
        ];
        for (i, part) in parts.iter().enumerate() {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(format!(
                            "/testvault/multipart.bin?partNumber={}&uploadId={}",
                            i + 1,
                            upload_id
                        ))
                        .body(Body::from(part.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK, "part {} failed", i + 1);
            assert!(response.headers().get(header::ETAG).is_some());
        }

        // Step 3: CompleteMultipartUpload via POST with `?uploadId=X`.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/testvault/multipart.bin?uploadId={}", upload_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let xml = body_string(response.into_body()).await;
        assert!(xml.contains("CompleteMultipartUploadResult"), "got: {xml}");
        assert!(xml.contains("<Bucket>testvault</Bucket>"));
        assert!(xml.contains("<Key>multipart.bin</Key>"));

        // Step 4: GET the object and verify the concatenation.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/multipart.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_bytes(response.into_body()).await;
        let mut expected = Vec::new();
        for part in &parts {
            expected.extend_from_slice(part);
        }
        assert_eq!(body, expected, "multipart round-trip mismatch");
    }

    /// Stage 5f.5b: AbortMultipartUpload cleans up state. After
    /// abort, a subsequent UploadPart for the same upload_id returns
    /// 404 NoSuchUpload. The uploads map is empty after abort.
    #[tokio::test]
    async fn multipart_abort_cleans_up_state() {
        let tmp = tempfile::tempdir().unwrap();
        let redb_path = tmp.path().join("test.redb");
        let backend = BackendKind::Local(Local::new(tmp.path().join("data")));
        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };
        let blob_reader = EncBlobReader::new(keys.clone(), backend.clone());
        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(
                &PlainIndex::new_empty(),
                &BlobIndex::new(),
                &TagIndex::new(),
            )
            .unwrap();

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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };
        std::mem::forget(tmp);

        let app = test_router(state);

        // CreateMultipartUpload to get an upload_id.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/testvault/abort.bin?uploads")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let xml = body_string(response.into_body()).await;
        let upload_id = extract_xml_value(&xml, "UploadId").expect("UploadId not found in XML");

        // AbortMultipartUpload via DELETE with `?uploadId=X`.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!("/testvault/abort.bin?uploadId={}", upload_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // UploadPart with the aborted upload_id should now 404.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!(
                        "/testvault/abort.bin?partNumber=1&uploadId={}",
                        upload_id
                    ))
                    .body(Body::from(b"test".to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let xml = body_string(response.into_body()).await;
        assert!(xml.contains("NoSuchUpload"), "got: {xml}");
    }

    /// Build a `ServeState` backed by an empty redb and a fresh local
    /// backend. The tempdir is leaked so the redb file and backend
    /// files survive for the test's lifetime. Returns the state and
    /// the tempdir path (so the flush test can set `cfg.basedir` to
    /// it).
    fn empty_state() -> ServeState {
        let tmp = tempfile::tempdir().unwrap();
        let redb_path = tmp.path().join("test.redb");
        let backend = BackendKind::Local(Local::new(tmp.path().join("data")));

        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };
        let blob_reader = EncBlobReader::new(keys.clone(), backend.clone());

        let store = RedbStore::open(&redb_path).unwrap();
        store
            .populate_from_indexes(
                &PlainIndex::new_empty(),
                &BlobIndex::new(),
                &TagIndex::new(),
            )
            .unwrap();

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
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };

        std::mem::forget(tmp);
        state
    }

    /// Build a `ServeState` with `cfg.basedir` set to a fresh tempdir
    /// (leaked) so `idxdir()` resolves inside the temp tree. Used by
    /// the flush test, which needs to assert on local index files
    /// without polluting the repo working directory.
    fn empty_state_with_basedir() -> ServeState {
        let mut state = empty_state();
        let tmp = tempfile::tempdir().unwrap();
        state.cfg.set_basedir(tmp.path().to_path_buf());
        std::fs::create_dir_all(state.cfg.idxdir()).unwrap();
        std::mem::forget(tmp);
        state
    }

    /// Stage 5f.1: PutObject then GetObject round-trip from a fresh
    /// (empty) redb. PUT a file via the router, GET it back, assert
    /// byte-for-byte equality. Verify redb now has path, file, blob,
    /// and block entries.
    #[tokio::test]
    async fn put_object_round_trip_from_empty() {
        let state = empty_state();
        let app = test_router(state);

        let file_data: Vec<u8> = (0..1024u32).map(|i| (i % 256) as u8).collect();

        // PUT the file.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/data.bin")
                    .body(Body::from(file_data.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::ETAG).is_some());

        // GET it back.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/data.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_bytes(response.into_body()).await;
        assert_eq!(body, file_data, "round-trip data mismatch");

        // HEAD should also succeed and report the right content-length.
        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/testvault/data.bin")
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
    }

    /// Stage 5f.1 (assertion): After a PUT via the router, redb must
    /// contain path, file, blob, and block entries. This is a
    /// standalone test so the assertion runs even if the round-trip
    /// test above does not check counts.
    #[tokio::test]
    async fn put_object_populates_redb_tables() {
        let state = empty_state();
        let redb = state.redb.clone();
        let app = test_router(state);

        let file_data: Vec<u8> = vec![0xAB; 300];

        // Before PUT, all tables are empty.
        assert_eq!(redb.path_count().unwrap(), 0);
        assert_eq!(redb.file_count().unwrap(), 0);
        assert_eq!(redb.blob_count().unwrap(), 0);
        assert_eq!(redb.block_count().unwrap(), 0);

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/blob.bin")
                    .body(Body::from(file_data))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // After PUT, all four tables have exactly one entry.
        assert_eq!(redb.path_count().unwrap(), 1);
        assert_eq!(redb.file_count().unwrap(), 1);
        assert_eq!(redb.blob_count().unwrap(), 1);
        assert_eq!(redb.block_count().unwrap(), 1);

        // The path resolves to a file_hash.
        assert!(redb.get_file_hash_by_path("blob.bin").unwrap().is_some());
    }

    /// Stage 5f.3: Dedup via the router. PUT the same content to two
    /// different paths. The second PUT must not create a new blob
    /// (blob_count stays at 1) because the chunk is already in the
    /// blob index. Both paths must GET back the same bytes and resolve
    /// to the same file_hash.
    #[tokio::test]
    async fn put_object_dedup_same_content_two_paths() {
        let state = empty_state();
        let redb = state.redb.clone();
        let app = test_router(state);

        let file_data: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();

        // PUT the first copy.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/original.bin")
                    .body(Body::from(file_data.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        let blob_count_after_first = redb.blob_count().unwrap();
        assert_eq!(blob_count_after_first, 1);

        // PUT the same content to a second path.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/duplicate.bin")
                    .body(Body::from(file_data.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);

        // blob_count must not increase: the chunk is deduped.
        let blob_count_after_second = redb.blob_count().unwrap();
        assert_eq!(
            blob_count_after_second, blob_count_after_first,
            "blob count must not increase on dedup PUT"
        );

        // Both paths resolve to the same file_hash (same content =
        // same multihash).
        let hash_a = redb.get_file_hash_by_path("original.bin").unwrap().unwrap();
        let hash_b = redb
            .get_file_hash_by_path("duplicate.bin")
            .unwrap()
            .unwrap();
        assert_eq!(hash_a, hash_b, "both paths must resolve to same file hash");

        // Both GETs return identical bytes.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/original.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_a = body_bytes(response.into_body()).await;

        let response = app
            .oneshot(
                Request::builder()
                    .uri("/testvault/duplicate.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body_b = body_bytes(response.into_body()).await;

        assert_eq!(body_a, body_b, "both GETs must return identical bytes");
        assert_eq!(body_a, file_data, "GET must match original data");
    }

    /// Two paths with identical content (same file_hash) share a
    /// single FileRef. Deleting one path must NOT delete the FileRef,
    /// the blob, or the other path's ability to GET the content.
    /// Deleting the second (last) path must then cascade and drop the
    /// blob from the backend.
    #[tokio::test]
    async fn delete_dedup_path_preserves_other_path_then_last_drops_blob() {
        let state = empty_state();
        let redb = state.redb.clone();
        let app = test_router(state);

        let file_data: Vec<u8> = (0..512u32).map(|i| (i % 256) as u8).collect();

        // PUT the same content to two paths.
        for path in &["/testvault/original.bin", "/testvault/duplicate.bin"] {
            let response = app
                .clone()
                .oneshot(
                    Request::builder()
                        .method("PUT")
                        .uri(*path)
                        .body(Body::from(file_data.clone()))
                        .unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::OK);
        }

        // Both paths resolve to the same file_hash.
        let hash_a = redb.get_file_hash_by_path("original.bin").unwrap().unwrap();
        let hash_b = redb
            .get_file_hash_by_path("duplicate.bin")
            .unwrap()
            .unwrap();
        assert_eq!(hash_a, hash_b, "both paths must share one file hash");

        // The FileRef must have two paths.
        let fileref = redb.get_fileref(&hash_a).unwrap().unwrap();
        assert_eq!(
            fileref.paths.len(),
            2,
            "FileRef must have two paths before delete"
        );

        // Delete one path.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/original.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The deleted path is gone.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/original.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // The surviving path still resolves and returns correct data.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/duplicate.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let body = body_bytes(response.into_body()).await;
        assert_eq!(body, file_data, "surviving path must return correct data");

        // The FileRef must still exist with one path.
        let surviving_ref = redb.get_fileref(&hash_a).unwrap().unwrap();
        assert_eq!(
            surviving_ref.paths.len(),
            1,
            "FileRef must have one path after deleting one of two"
        );

        // The blob location must still exist.
        assert!(
            redb.blob_count().unwrap() > 0,
            "blob index must not be empty after deleting one of two paths"
        );

        // Delete the last path.
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/testvault/duplicate.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // The FileRef is now gone.
        assert!(
            redb.get_fileref(&hash_a).unwrap().is_none(),
            "FileRef must be gone after deleting last path"
        );

        // The blob index is now empty (cascade deleted the chunk).
        assert_eq!(
            redb.blob_count().unwrap(),
            0,
            "blob index must be empty after deleting last path"
        );
    }

    /// Stage 5f.6 (full): Flush after a router-driven PUT. PUT a file
    /// via the router, then call `flush_indexes` directly (skipping
    /// the debounce timer for determinism). Verify the three encrypted
    /// index files appear in `cfg.idxdir()` and are pushed to the
    /// backend via `read_from_path`.
    #[tokio::test]
    async fn flush_after_put_object() {
        let state = empty_state_with_basedir();
        let redb = state.redb.clone();
        let backend = state.backend.clone();
        let cfg_idxdir = state.cfg.idxdir();
        let app = test_router(state);

        let file_data: Vec<u8> = b"flush after put\n".to_vec();

        // PUT via the router so the full write path runs.
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/flushme.bin")
                    .body(Body::from(file_data))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(redb.path_count().unwrap(), 1);

        // The state was consumed by the router. Reconstruct a minimal
        // state for the flush call. We need the same redb, cfg, keys,
        // and backend. Recover them from the clones we kept and the
        // OnceLock that the router still holds. Since test_router
        // moves the state into an Arc<OnceLock>, we cannot get it
        // back. Instead, build a fresh state sharing the same redb,
        // keys, backend, and cfg.
        let kek = crate::keys::kek::Kek::generate();
        let keys = DekProvider::Local {
            kek,
            kek_version: 0,
        };
        let blob_reader = EncBlobReader::new(keys.clone(), backend.clone());
        let mut cfg = Config::default();
        cfg.set_basedir(cfg_idxdir.parent().unwrap().parent().unwrap().to_path_buf());

        let flush_state = ServeState {
            redb,
            bucket_name: "testvault".to_string(),
            index_updated_at: chrono::Utc
                .timestamp_opt(1718774400, 0)
                .unwrap()
                .naive_utc(),
            blob_reader: Arc::new(blob_reader),
            cfg,
            keys,
            backend: backend.clone(),
            write_mutex: Mutex::new(()),
            flush_timer: Mutex::new(None),
            multipart_uploads: Mutex::new(HashMap::new()),
        };

        // Run the flush inline.
        flush_indexes(&flush_state).await.unwrap();

        // All three index files must exist in the local idxdir.
        let plain_path = cfg_idxdir.join("index.dat");
        let blob_path = cfg_idxdir.join("blob_index.dat");
        let tag_path = cfg_idxdir.join("tags.dat");
        assert!(plain_path.exists(), "plain index not written");
        assert!(blob_path.exists(), "blob index not written");
        assert!(tag_path.exists(), "tag index not written");

        // Each local file must be non-empty (encrypted CBOR).
        let plain_bytes = std::fs::read(&plain_path).unwrap();
        assert!(!plain_bytes.is_empty(), "plain index empty");
        let blob_bytes = std::fs::read(&blob_path).unwrap();
        assert!(!blob_bytes.is_empty(), "blob index empty");
        let tag_bytes = std::fs::read(&tag_path).unwrap();
        assert!(!tag_bytes.is_empty(), "tag index empty");

        // The push must have uploaded the same bytes to the backend
        // at the `indexes/` prefix.
        let remote_plain = backend
            .read_from_path(&std::path::Path::new("indexes").join("index.dat"))
            .await
            .unwrap();
        assert_eq!(remote_plain, plain_bytes, "pushed plain index mismatch");

        let remote_blob = backend
            .read_from_path(&std::path::Path::new("indexes").join("blob_index.dat"))
            .await
            .unwrap();
        assert_eq!(remote_blob, blob_bytes, "pushed blob index mismatch");

        let remote_tag = backend
            .read_from_path(&std::path::Path::new("indexes").join("tags.dat"))
            .await
            .unwrap();
        assert_eq!(remote_tag, tag_bytes, "pushed tag index mismatch");
    }

    /// Edge case: PUT an empty body (zero-byte object), then GET it
    /// back. S3 accepts zero-byte objects. `chunk_bytes` returns an
    /// empty Vec for empty input, so `put_object_inner` writes a
    /// FileRef with zero chunks and no blob_index entry. The GET path
    /// must return 200 with an empty body.
    #[tokio::test]
    async fn put_object_empty_body() {
        let state = empty_state();
        let redb = state.redb.clone();
        let app = test_router(state);

        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/testvault/empty.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert!(response.headers().get(header::ETAG).is_some());

        // redb must have a path and file entry, but no blob or block
        // entries (zero chunks).
        assert_eq!(redb.path_count().unwrap(), 1);
        assert_eq!(redb.file_count().unwrap(), 1);
        assert_eq!(redb.blob_count().unwrap(), 0);
        assert_eq!(redb.block_count().unwrap(), 0);

        // GET must return 200 with an empty body.
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri("/testvault/empty.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "0");
        let body = body_bytes(response.into_body()).await;
        assert!(body.is_empty(), "empty body must round-trip as empty");

        // HEAD must also return 200 with content-length 0.
        let response = app
            .oneshot(
                Request::builder()
                    .method("HEAD")
                    .uri("/testvault/empty.bin")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers().get(header::CONTENT_LENGTH).unwrap(), "0");
    }

    /// Extract the first inner text of a tag from an XML string.
    /// Used in multipart tests to pull `UploadId` out of S3 XML
    /// responses. Returns `None` if the tag is missing or empty.
    fn extract_xml_value(xml: &str, tag: &str) -> Option<String> {
        let open = format!("<{tag}>");
        let close = format!("</{tag}>");
        let start = xml.find(&open)? + open.len();
        let end = xml[start..].find(&close)? + start;
        let value = xml[start..end].to_string();
        if value.is_empty() {
            None
        } else {
            Some(value)
        }
    }
}
