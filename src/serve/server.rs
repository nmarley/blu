//! HTTP server for `blu serve`: a local S3-compatible translation
//! layer that presents decrypted files to any S3 client while the real
//! backend holds only opaque encrypted blobs.
//!
//! Stage 3 adds `ListObjectsV2` and `ListBuckets` (read-only listing).
//! `GetObject`, `HeadObject`, and byte-range support are added in
//! Stage 4.

use std::net::SocketAddr;
use std::sync::{Arc, OnceLock};

use axum::extract::{Path, Query};
use axum::http::{header, StatusCode};
use axum::response::IntoResponse;
use axum::routing::get;
use axum::Router;
use chrono::NaiveDateTime;
use tokio::net::TcpListener;
use tokio::signal::unix::{signal, SignalKind};

use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;
use crate::serve::index_sync;
use crate::serve::redb_store::RedbStore;
use crate::serve::s3xml;

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
/// index sync as a background task. Until sync completes, `/health`
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
        .route("/health", get(health_handler))
        .route("/", get(list_buckets_handler))
        .route("/{bucket}", get(list_objects_handler))
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
        match index_sync::sync_from_backend(&cfg, &keys, &backend).await {
            Ok((store, index_updated_at)) => {
                let bucket_name = cfg
                    .basedir()
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| "blu".to_string());

                let state = ServeState {
                    redb: Arc::new(store),
                    bucket_name,
                    index_updated_at,
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
            let tags = s.redb.tag_count().unwrap_or(0);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/plain")],
                format!(
                    "ok ({} paths, {} files, {} chunks, {} tags)",
                    paths, files, chunks, tags
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
    use crate::blob::BlobIndex;
    use crate::block::{ChunkMeta, FileRef, PlainIndex};
    use crate::hash::Hash;
    use crate::tag::TagIndex;

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
        }
    }

    /// Build a router with a ready (populated) state.
    fn test_router(state: ServeState) -> Router {
        let lock = Arc::new(OnceLock::new());
        lock.set(state).unwrap();
        Router::new()
            .route("/health", get(health_handler))
            .route("/", get(list_buckets_handler))
            .route("/{bucket}", get(list_objects_handler))
            .with_state(lock)
    }

    /// Build a router with an empty (not-yet-ready) state slot.
    fn not_ready_router() -> Router {
        let lock: Arc<OnceLock<ServeState>> = Arc::new(OnceLock::new());
        Router::new()
            .route("/health", get(health_handler))
            .route("/", get(list_buckets_handler))
            .route("/{bucket}", get(list_objects_handler))
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
                    .uri("/health")
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
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = body_string(response.into_body()).await;
        assert!(body.starts_with("ok ("));
    }
}
