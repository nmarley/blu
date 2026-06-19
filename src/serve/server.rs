//! HTTP server for `blu serve`: a local S3-compatible translation
//! layer that presents decrypted files to any S3 client while the real
//! backend holds only opaque encrypted blobs.
//!
//! Phase 1 (Stage 1 skeleton) exposes only `GET /health`. The
//! S3-compatible API surface (`GetObject`, `HeadObject`,
//! `ListObjectsV2`) is added in Stages 3-4.

use std::net::SocketAddr;

use axum::routing::get;
use axum::Router;
use tokio::net::TcpListener;

use crate::error::BluError;

/// Default listen address for the serve daemon. Localhost only; the
/// agent daemon is the trust boundary, not the HTTP server.
const DEFAULT_BIND_ADDR: &str = "127.0.0.1:7777";

/// State shared across all axum handlers. Populated in later stages
/// with the redb store, blob reader, backend handle, and DekProvider.
#[derive(Clone)]
pub struct ServeState {
    // Placeholder: redb store, EncBlobReader, BackendKind, DekProvider
    // are added in Stages 2-4.
}

/// Entry point for `blu serve`. Binds a TCP listener on the configured
/// address and serves the HTTP API until interrupted.
pub async fn serve(bind_addr: Option<String>) -> Result<(), BluError> {
    let addr: SocketAddr = bind_addr
        .as_deref()
        .unwrap_or(DEFAULT_BIND_ADDR)
        .parse()
        .expect("invalid bind address");

    let state = ServeState {};

    let app = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    let listener = TcpListener::bind(addr).await?;
    info!("blu serve listening on http://{}", addr);

    axum::serve(listener, app).await?;

    Ok(())
}

/// Health check handler. Returns 200 OK with an empty body. Used to
/// verify the daemon is running.
async fn health_handler() -> &'static str {
    "ok"
}
