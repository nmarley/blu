//! redb-backed local index store for `blu serve`.
//!
//! The local redb database is the working copy; encrypted CBOR on the
//! backend is the source of truth and the interchange format. redb
//! pages data in and out through the OS page cache, so the daemon does
//! not pin hundreds of megabytes of deserialized HashMaps in resident
//! memory.
//!
//! Table schemas will be defined in Stage 2. This module currently
//! provides the database handle and open/create logic so the server
//! can hold a long-lived connection.

use std::path::Path;

use redb::Database;
use redb::DatabaseError;

/// redb database handle held by the serve daemon for the lifetime of
/// the process.
pub struct RedbStore {
    db: Database,
}

impl RedbStore {
    /// Open an existing redb database, or create one at the given path
    /// if it does not exist. The parent directory must already exist.
    pub fn open(path: &Path) -> Result<Self, DatabaseError> {
        let db = Database::create(path)?;
        Ok(Self { db })
    }

    /// Borrow the underlying redb database handle for table operations.
    pub fn db(&self) -> &Database {
        &self.db
    }
}
