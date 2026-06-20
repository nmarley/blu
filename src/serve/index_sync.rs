//! Index synchronization between the backend and the local redb store.
//!
//! On startup (fresh machine): pull all encrypted index files from the
//! backend, decrypt and deserialize via the existing
//! `EncryptedSerializable` path, and load into local redb tables.
//!
//! On startup (returning machine): open the existing redb database,
//! pull index files from the backend, and re-populate redb with the
//! latest state. The local redb file is a cache that survives restarts;
//! re-population is a full overwrite (delta sync is a future
//! optimization).
//!
//! On writes: update local redb, then periodically serialize redb
//! state to encrypted CBOR and push to the backend (the debounced
//! index flush).

use chrono::NaiveDateTime;

use crate::blob::EncBlobReader;
use crate::config::Config;
use crate::dek_provider::DekProvider;
use crate::error::BluError;
use crate::serve::redb_store::RedbStore;
use crate::storage::BackendKind;

/// Synchronize the local redb store with the backend.
///
/// Pulls encrypted indexes from the backend, decrypts and deserializes
/// them, and populates the redb store. This is the startup path for
/// `blu serve`.
///
/// The redb database is opened (or created) at
/// `.blu/serve.redb` within the vault directory. On a fresh machine,
/// the database is created and populated from scratch. On a returning
/// machine, the existing database is overwritten with fresh state from
/// the backend.
///
/// Takes ownership of `cfg`, `keys`, and `backend` so the caller can
/// thread them back into `ServeState` without re-cloning. Returns
/// them alongside the redb store, the `PlainIndex::updated_at`
/// timestamp (used as a proxy for object `LastModified` values, since
/// individual file modification times are not tracked in the current
/// index format), and an `EncBlobReader` owning its own cloned keys
/// and backend for serving chunk data.
pub async fn sync_from_backend(
    cfg: Config,
    keys: DekProvider,
    backend: BackendKind,
) -> Result<
    (
        Config,
        DekProvider,
        BackendKind,
        RedbStore,
        NaiveDateTime,
        EncBlobReader,
    ),
    BluError,
> {
    let redb_path = cfg.bludir().join("serve.redb");

    info!("pulling indexes from backend");
    cfg.pull_indexes(&backend).await?;

    let plain = cfg.load_plain_index(&keys)?;
    let updated_at = plain.updated_at();
    let blob = cfg.load_blob_index_or_default(&keys);
    let tag = cfg.load_tag_index_or_default(&keys);

    info!("opening redb store at {}", redb_path.display());
    let store = RedbStore::open(&redb_path)?;

    info!("populating redb from indexes");
    store.populate_from_indexes(&plain, &blob, &tag)?;

    info!(
        "redb store ready: {} paths, {} files, {} chunks, {} tags",
        store.path_count()?,
        store.file_count()?,
        store.blob_count()?,
        store.tag_count()?,
    );

    // The blob reader needs its own key/backend handles for the read
    // path. Clone before moving the originals back out to the caller.
    let blob_reader = EncBlobReader::new(keys.clone(), backend.clone());

    Ok((cfg, keys, backend, store, updated_at, blob_reader))
}
