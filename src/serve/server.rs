//! HTTP server for `blu serve`: a local S3-compatible translation
//! layer that presents decrypted files to any S3 client while the real
//! backend holds only opaque encrypted blobs.
//!
//! Phase 1 (current) exposes only `GET /health`. The S3-compatible API
//! surface (`GetObject`, `HeadObject`, `ListObjectsV2`) is added in
//! Stages 3-4.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;

use crate::cli::helpers::{load_config_and_keys, LoadOptions};
use crate::error::BluError;
use crate::serve::index_sync;
use crate::serve::redb_store::RedbStore;

/// Default listen address for the serve daemon. Localhost only; the
/// agent daemon is the trust boundary, not the HTTP server.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:7777";

/// State shared across all axum handlers.
#[derive(Clone)]
pub struct ServeState {
    /// Local redb index store for path/file/blob/tag lookups.
    redb: Arc<RedbStore>,
}

/// Entry point for `blu serve`. Loads the vault config and keys,
/// syncs indexes from the backend into the local redb store, then
/// binds a TCP listener and serves the HTTP API until interrupted.
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

    info!("syncing indexes from backend into local redb store");
    let store = index_sync::sync_from_backend(&cfg, &keys, &backend).await?;

    let state = ServeState {
        redb: Arc::new(store),
    };

    let app = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    let listener = TcpListener::bind(addr).await?;
    info!("blu serve listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Health check handler. Returns 200 OK with a simple body. Used to
/// verify the daemon is running and the index store is loaded.
async fn health_handler(state: axum::extract::State<ServeState>) -> String {
    let paths = state.redb.path_count().unwrap_or(0);
    let files = state.redb.file_count().unwrap_or(0);
    let chunks = state.redb.blob_count().unwrap_or(0);
    let tags = state.redb.tag_count().unwrap_or(0);
    format!(
        "ok ({} paths, {} files, {} chunks, {} tags)",
        paths, files, chunks, tags
    )
}
