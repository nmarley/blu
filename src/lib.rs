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

//! Blu is an encrypted and de-duplicated file archival system.
//!
//! > "Not your keys, not your secrets ..."
//!
//! Based on directories in the typical \*nix hierarchical file system (HFS), this will read all
//! files in the directory, and encrypt, de-duplicate and archive to any of several configurable
//! backends, including locally and cloud object storage such as Amazon s3.
//!
//! All encryption in the project uses [rage](https://github.com/str4d/rage), based on age by
//! [@FiloSottile](https://twitter.com/FiloSottile) and
//! [@Benjojo12](https://twitter.com/Benjojo12).

#[macro_use]
extern crate log;

/// age handles all encryption and decryption
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
/// error types for blu
pub mod error;
/// format contains a format fn for datetime (chrono/serde)
pub mod format;
/// wrapper around Vec<u8> for cryptographic hashes
pub mod hash;
/// serialization + compression + encryption for indexes
pub mod io;
/// key management (generation, loading, storage)
pub mod keys;
/// search index for filenames
pub mod search;
/// storage backends and hash to path translation methods
pub mod storage;
/// tag index, probably should rename this
pub mod tag;
/// v2 file format: envelope encryption with KEK/DEK hierarchy
pub mod v2format;
