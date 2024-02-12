use std::error::Error;
use std::fmt::{self, Display, Formatter};
use std::path::PathBuf;

#[derive(Debug)]
pub enum StorageError {
    ConnectionError,
    NotFound(PathBuf),
    PermissionDenied(PathBuf),
    ReadError(PathBuf),
    WriteError(PathBuf),
    ServerError,
    // TODO: remove this error variant and replace with TokioIoError (rename
    // TokioIoError to IoError)
    IoError(std::io::Error),
    TokioIoError(tokio::io::Error),
    // AwsSdkError(aws_sdk_s3::Error),
    HashError(multihash::Error),
    // Add more errors as needed
}

impl Display for StorageError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            StorageError::ConnectionError => write!(f, "Connection error"),
            StorageError::NotFound(path) => write!(f, "File not found: {}", path.display()),
            StorageError::PermissionDenied(path) => {
                write!(f, "Permission denied: {}", path.display())
            }
            StorageError::ReadError(path) => write!(f, "Read error: {}", path.display()),
            StorageError::WriteError(path) => write!(f, "Write error: {}", path.display()),
            StorageError::ServerError => write!(f, "Server error"),
            StorageError::IoError(err) => write!(f, "I/O error: {}", err),
            StorageError::TokioIoError(err) => write!(f, "Tokio I/O error: {}", err),
            StorageError::HashError(err) => write!(f, "Hash error: {}", err),
        }
    }
}

impl Error for StorageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            StorageError::IoError(err) => Some(err),
            _ => None,
        }
    }
}

impl From<tokio::io::Error> for StorageError {
    fn from(err: tokio::io::Error) -> Self {
        StorageError::TokioIoError(err)
    }
}
