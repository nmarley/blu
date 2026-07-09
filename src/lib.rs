#![warn(rust_2018_idioms)]
// #![warn(missing_debug_implementations)]
#![warn(missing_docs)]
//
// https://doc.rust-lang.org/rustc/lints/groups.html
#![allow(clippy::uninlined_format_args)]
#![allow(clippy::needless_lifetimes)]
// I don't agree w/this lint from 1.75.0. Specifically I want to be able to
// define module exports at the end of the file, even after a test body
#![allow(clippy::items_after_test_module)]

//! Blu is an encrypted, deduplicated file archival system.
//!
//! > "Not your keys, not your secrets ..."
//!
//! Files are chunked, content-addressed, and stored as opaque encrypted blobs
//! on a local filesystem or Amazon S3. Key hierarchy:
//!
//! - **User key**: post-quantum hybrid (ML-KEM-768 + X25519) from a BIP39
//!   mnemonic, used only to wrap the vault KEK via age
//! - **KEK**: one per vault, wraps per-blob DEKs
//! - **DEK / bulk data**: ChaCha20-Poly1305 (v3 segmented AEAD for new blobs)
//!
//! See the crate README and `docs/design/ENVELOPE_ENCRYPTION_DESIGN.md`.

#[macro_use]
extern crate log;

/// passphrase-based encryption for identity files
pub mod age;
/// agent daemon for session management (unlock/lock)
pub mod agent;
/// blob handles storage and retrieval of encrypted files
pub mod blob;
/// block handles block-based indexing
pub mod block;
/// cli is the cli and subcommands
pub mod cli;
/// helper functions for (de+)compression
pub mod compression;
/// configuration file and related methods
pub mod config;
/// envelope key provider for DEK wrap/unwrap
pub mod dek_provider;
/// error types for blu
pub mod error;
/// format contains a format fn for datetime (chrono/serde)
pub mod format;
/// wrapper around Vec<u8> for cryptographic hashes
pub mod hash;
/// filesystem walking with `.bluignore` support
pub mod ignore;
/// serialization + compression + encryption for indexes
pub mod io;
/// key management (generation, loading, storage)
pub mod keys;
/// search index for filenames
pub mod search;
/// `blu serve` local daemon (HTTP server, redb index store, index sync)
pub mod serve;
/// storage backends and hash to path translation methods
pub mod storage;
/// tag index, probably should rename this
pub mod tag;
/// v2 file format: envelope encryption with KEK/DEK hierarchy
pub mod v2format;
/// v3 segmented AEAD blob format (fixed-size segments, prefix fetch)
pub mod v3format;
