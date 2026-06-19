//! Index synchronization between the backend and the local redb store.
//!
//! On startup (fresh machine): pull all encrypted index files from the
//! backend, decrypt and deserialize via the existing
//! `EncryptedSerializable` path, and load into local redb tables.
//!
//! On startup (returning machine): open the existing redb database,
//! pull index files from the backend, diff against local state, and
//! apply deltas.
//!
//! On writes: update local redb, then periodically serialize redb
//! state to encrypted CBOR and push to the backend.
//!
//! The full sync logic will be implemented in Stage 2. This module
//! currently provides the entry point so the server can call it.
