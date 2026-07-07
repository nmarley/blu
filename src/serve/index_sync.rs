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

use crate::blob::{EncBlobReader, BLOB_CACHE_CAPACITY};
use crate::config::Config;
use crate::dek_provider::DekProvider;
use crate::error::BluError;
use crate::serve::redb_store::RedbStore;
use crate::storage::BackendKind;

/// Maximum number of attempts for the startup index pull before
/// giving up and leaving the daemon in the not-ready (503) state.
const SYNC_MAX_ATTEMPTS: usize = 5;

/// Base delay for exponential backoff between index-pull retries.
/// Each successive retry sleeps `base * 2^(attempt-1)`: 1s, 2s, 4s, 8s.
const SYNC_BACKOFF_BASE: std::time::Duration = std::time::Duration::from_secs(1);

/// Run `op` with a bounded number of attempts and exponential
/// backoff. Returns `Ok` as soon as `op` succeeds, or the last
/// `BluError` after `max_attempts` failures. Each failed attempt is
/// logged at `warn` level. When `base_delay` is zero the sleeps
/// return immediately, which keeps the unit tests fast.
async fn retry_with_backoff<F, Fut, T>(
    mut op: F,
    max_attempts: usize,
    base_delay: std::time::Duration,
) -> Result<T, BluError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T, BluError>>,
{
    let mut last_err: Option<BluError> = None;
    for attempt in 1..=max_attempts {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                warn!("sync attempt {}/{} failed: {}", attempt, max_attempts, e);
                last_err = Some(e);
                if attempt < max_attempts {
                    let delay = base_delay * 2u32.pow((attempt - 1) as u32);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }
    // `max_attempts >= 1` guarantees the loop body ran at least once
    // and populated `last_err`.
    Err(last_err.expect("max_attempts >= 1 guarantees an error"))
}

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
///
/// `cache_blobs` overrides the default decrypted-blob LRU cache size
/// (`BLOB_CACHE_CAPACITY`); `None` uses the default.
pub async fn sync_from_backend(
    cfg: Config,
    keys: DekProvider,
    backend: BackendKind,
    cache_blobs: Option<usize>,
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
    retry_with_backoff(
        || async { cfg.pull_indexes(&backend).await },
        SYNC_MAX_ATTEMPTS,
        SYNC_BACKOFF_BASE,
    )
    .await?;

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
    let blob_cap = cache_blobs.unwrap_or(BLOB_CACHE_CAPACITY);
    let blob_reader = EncBlobReader::with_capacity(keys.clone(), backend.clone(), blob_cap);

    Ok((cfg, keys, backend, store, updated_at, blob_reader))
}

#[cfg(test)]
mod test {
    use super::*;

    /// A closure fails the first two attempts then succeeds; the
    /// retry helper must keep calling until `Ok`, and the attempt
    /// counter must reflect exactly three invocations. `base_delay`
    /// is zero so the backoff sleeps return immediately.
    #[tokio::test]
    async fn retry_with_backoff_succeeds_after_transient_failure() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);

        let result = retry_with_backoff(
            || {
                let count = attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    if count < 2 {
                        Err(BluError::StorageError("transient backend".to_string()))
                    } else {
                        Ok(())
                    }
                }
            },
            5,
            std::time::Duration::ZERO,
        )
        .await;

        assert!(result.is_ok(), "expected eventual success, got {result:?}");
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    /// When every attempt fails, the helper returns the last error
    /// and stops after exactly `max_attempts` tries.
    #[tokio::test]
    async fn retry_with_backoff_exhausts_attempts_returns_last_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);

        let result = retry_with_backoff(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async move {
                    Result::<(), BluError>::Err(BluError::StorageError("persistent".to_string()))
                }
            },
            3,
            std::time::Duration::ZERO,
        )
        .await;

        assert!(result.is_err());
        assert_eq!(attempts.load(Ordering::SeqCst), 3);
        assert!(matches!(result.unwrap_err(), BluError::StorageError(_)));
    }

    /// A first-try success must not retry.
    #[tokio::test]
    async fn retry_with_backoff_succeeds_first_try() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let attempts = AtomicUsize::new(0);

        let result = retry_with_backoff(
            || {
                attempts.fetch_add(1, Ordering::SeqCst);
                async move { Ok(42u8) }
            },
            5,
            std::time::Duration::ZERO,
        )
        .await;

        assert_eq!(result.unwrap(), 42);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }
}
