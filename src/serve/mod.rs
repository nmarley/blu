//! `blu serve` local daemon: a translation layer that presents
//! decrypted, de-obfuscated files to any S3-compatible client while the
//! real backend holds only opaque, content-addressed encrypted blobs.
//!
//! See `BLU_SERVE_DESIGN.md` for the canonical design.

pub mod index_sync;
pub mod redb_store;
pub mod s3xml;
pub mod server;

pub use server::serve;
