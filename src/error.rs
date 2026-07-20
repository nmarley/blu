//! Error types for blu.
//!
//! This module provides a unified error type for the blu crate, enabling
//! better error messages and more precise error handling.

use std::path::PathBuf;
use thiserror::Error;

/// The main error type for blu operations.
#[derive(Error, Debug)]
pub enum BluError {
    // -------------------------------------------------------------------------
    // Configuration errors
    // -------------------------------------------------------------------------
    /// No configuration file found
    #[error("not a blu repository (or any of the parent directories): .blu")]
    NotARepository,

    /// Configuration file is invalid
    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    /// I/O error (file read/write, network, etc.)
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// redb database error
    #[error("redb error: {0}")]
    Redb(String),

    // -------------------------------------------------------------------------
    // Key management errors
    // -------------------------------------------------------------------------
    /// No encryption key configured
    #[error("no encryption key configured (run `blu init` to set up a vault)")]
    NoKeyConfigured,

    /// Key file not found
    #[error("key file not found: {path}")]
    KeyFileNotFound {
        /// The path that was not found
        path: PathBuf,
    },

    /// Invalid key format
    #[error("invalid key format: {0}")]
    InvalidKeyFormat(String),

    /// Passphrase required but not provided
    #[error("passphrase required to decrypt key (use --no-passphrase to skip)")]
    PassphraseRequired,

    /// Wrong passphrase
    #[error("incorrect passphrase")]
    WrongPassphrase,

    // -------------------------------------------------------------------------
    // Encryption/decryption errors
    // -------------------------------------------------------------------------
    /// Encryption failed
    #[error("encryption failed: {0}")]
    EncryptionFailed(String),

    /// Decryption failed
    #[error("decryption failed: {0}")]
    DecryptionFailed(String),

    // -------------------------------------------------------------------------
    // Storage backend errors
    // -------------------------------------------------------------------------
    /// Storage backend error
    #[error("storage error: {0}")]
    StorageError(String),

    /// File not found in storage
    #[error("file not found in storage: {path}")]
    StorageFileNotFound {
        /// The path that was not found
        path: PathBuf,
    },

    /// S3 operation failed
    #[error("S3 error: {0}")]
    S3Error(String),

    /// Object is in an archive tier and must be restored before GET
    #[error(
        "object archived (restore required): {path}{}",
        format_archived_detail(storage_class.as_deref(), access_tier.as_deref())
    )]
    ObjectArchived {
        /// Relative backend path of the archived object
        path: PathBuf,
        /// S3 storage class, when known (e.g. `INTELLIGENT_TIERING`, `DEEP_ARCHIVE`)
        storage_class: Option<String>,
        /// Intelligent-Tiering access tier, when known (e.g. `DEEP_ARCHIVE_ACCESS`)
        access_tier: Option<String>,
    },

    // -------------------------------------------------------------------------
    // Index errors
    // -------------------------------------------------------------------------
    /// Index file not found
    #[error("index not found: {0}")]
    IndexNotFound(String),

    /// Index is corrupted or invalid
    #[error("index corrupted: {0}")]
    IndexCorrupted(String),

    /// Index file could not be loaded (decryption, decompression, or
    /// deserialization failed)
    #[error("failed to load index at {path}: {reason}")]
    IndexLoadFailed {
        /// The path to the index file
        path: PathBuf,
        /// Human-readable description of why the load failed
        reason: String,
    },

    /// File hash not found in index
    #[error("file hash not found in index: {hash}")]
    FileHashNotFound {
        /// The hash that was not found
        hash: String,
    },

    /// Block hash not found in index
    #[error("block not found in blob index: {hash}")]
    BlockNotFound {
        /// The hash that was not found
        hash: String,
    },

    /// Block data does not match its expected hash
    #[error("block hash mismatch: expected {expected}, got {actual}")]
    BlockHashMismatch {
        /// The hash the index claims the block should have
        expected: String,
        /// The hash computed from the actual block data
        actual: String,
    },

    // -------------------------------------------------------------------------
    // File operation errors
    // -------------------------------------------------------------------------
    /// File already exists (for restore)
    #[error("file already exists: {path}")]
    FileAlreadyExists {
        /// The path that already exists
        path: PathBuf,
    },

    /// Path is not a file
    #[error("not a file: {path}")]
    NotAFile {
        /// The path that was expected to be a file
        path: PathBuf,
    },

    /// Path is not a directory
    #[error("not a directory: {path}")]
    NotADirectory {
        /// The path that was expected to be a directory
        path: PathBuf,
    },

    // -------------------------------------------------------------------------
    // Serialization errors
    // -------------------------------------------------------------------------
    /// Serialization failed
    #[error("serialization error: {0}")]
    SerializationError(String),

    /// Deserialization failed
    #[error("deserialization error: {0}")]
    DeserializationError(String),

    // -------------------------------------------------------------------------
    // Generic/other errors
    // -------------------------------------------------------------------------
    /// An internal error occurred
    #[error("internal error: {0}")]
    Internal(String),

    /// Wraps any other error
    #[error("{0}")]
    Other(#[from] Box<dyn std::error::Error + Send + Sync>),
}

/// Convenient type alias for Results using BluError.
pub type Result<T> = std::result::Result<T, BluError>;

fn format_archived_detail(storage_class: Option<&str>, access_tier: Option<&str>) -> String {
    match (storage_class, access_tier) {
        (Some(sc), Some(tier)) => format!(" (class={sc}, tier={tier})"),
        (Some(sc), None) => format!(" (class={sc})"),
        (None, Some(tier)) => format!(" (tier={tier})"),
        (None, None) => String::new(),
    }
}

// Convenience conversions from common error types

impl From<toml::de::Error> for BluError {
    fn from(err: toml::de::Error) -> Self {
        BluError::InvalidConfig(err.to_string())
    }
}

impl From<toml::ser::Error> for BluError {
    fn from(err: toml::ser::Error) -> Self {
        BluError::SerializationError(err.to_string())
    }
}

impl From<serde_json::Error> for BluError {
    fn from(err: serde_json::Error) -> Self {
        BluError::SerializationError(err.to_string())
    }
}

impl From<age::EncryptError> for BluError {
    fn from(err: age::EncryptError) -> Self {
        BluError::EncryptionFailed(err.to_string())
    }
}

impl From<age::DecryptError> for BluError {
    fn from(err: age::DecryptError) -> Self {
        BluError::DecryptionFailed(err.to_string())
    }
}

impl From<multihash::Error> for BluError {
    fn from(err: multihash::Error) -> Self {
        BluError::Internal(err.to_string())
    }
}

impl From<std::path::StripPrefixError> for BluError {
    fn from(err: std::path::StripPrefixError) -> Self {
        BluError::Internal(err.to_string())
    }
}

impl From<tokio::task::JoinError> for BluError {
    fn from(err: tokio::task::JoinError) -> Self {
        BluError::Internal(format!("task join failed: {}", err))
    }
}

impl From<redb::Error> for BluError {
    fn from(err: redb::Error) -> Self {
        BluError::Redb(err.to_string())
    }
}

impl From<redb::DatabaseError> for BluError {
    fn from(err: redb::DatabaseError) -> Self {
        BluError::Redb(err.to_string())
    }
}

impl From<redb::TableError> for BluError {
    fn from(err: redb::TableError) -> Self {
        BluError::Redb(err.to_string())
    }
}

impl From<redb::TransactionError> for BluError {
    fn from(err: redb::TransactionError) -> Self {
        BluError::Redb(err.to_string())
    }
}

impl From<redb::CommitError> for BluError {
    fn from(err: redb::CommitError) -> Self {
        BluError::Redb(err.to_string())
    }
}

impl From<redb::StorageError> for BluError {
    fn from(err: redb::StorageError) -> Self {
        BluError::Redb(err.to_string())
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn error_display() {
        let err = BluError::NotARepository;
        assert_eq!(
            err.to_string(),
            "not a blu repository (or any of the parent directories): .blu"
        );

        let err = BluError::NoKeyConfigured;
        assert_eq!(
            err.to_string(),
            "no encryption key configured (run `blu init` to set up a vault)"
        );

        let err = BluError::FileHashNotFound {
            hash: "1e20abc...".to_string(),
        };
        assert_eq!(err.to_string(), "file hash not found in index: 1e20abc...");

        let err = BluError::ObjectArchived {
            path: PathBuf::from("d/dd4/blob"),
            storage_class: Some("INTELLIGENT_TIERING".into()),
            access_tier: Some("DEEP_ARCHIVE_ACCESS".into()),
        };
        assert_eq!(
            err.to_string(),
            "object archived (restore required): d/dd4/blob (class=INTELLIGENT_TIERING, tier=DEEP_ARCHIVE_ACCESS)"
        );
    }
}
