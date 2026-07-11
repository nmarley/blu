//! Open an existing vault from a remote backend.
//!
//! Fresh-machine path: identity already recovered, then `blu open`
//! writes a local config, pulls the UK-wrapped KEK store and indexes,
//! and verifies this identity can unwrap the KEK.

use std::fs;
use std::path::{Path, PathBuf};

use age::Identity;

use crate::cli::clapargs::OpenArgs;
use crate::cli::{global_identity_age_path, load_global_identity};
use crate::config::{self, backend::BackendConfig, EncryptionConfig};
use crate::error::BluError;
use crate::keys;
use crate::keys::kek::KekStore;

/// Resolved inputs for opening a vault (no interactive prompts).
pub struct OpenVaultParams {
    /// Directory that will hold the local `.blu/` tree.
    pub dir: PathBuf,
    /// PQ public key from the global identity.
    pub pq_recipient: String,
    /// Backend name to write into config as default.
    pub backend_name: String,
    /// Backend configuration (S3 or local).
    pub backend: BackendConfig,
}

/// Create a local vault config pointing at an existing backend and pull
/// KEK store + indexes.
///
/// Does not generate a KEK. Fails if `.blu/config.toml` already exists
/// or if the remote has no KEK metadata.
pub async fn open_vault(params: OpenVaultParams) -> Result<PathBuf, BluError> {
    let dir = &params.dir;
    fs::create_dir_all(dir)?;

    let bludir = dir.join(".blu");
    let config_path = bludir.join("config.toml");
    if config_path.exists() {
        return Err(BluError::Internal(format!(
            "vault already exists at {} (remove it or choose another --dir)",
            config_path.display()
        )));
    }

    fs::create_dir_all(&bludir)?;
    fs::create_dir_all(bludir.join("indexes"))?;

    let mut cfg = config::Config::default();
    cfg.set_basedir(
        dir.canonicalize().map_err(|e| {
            BluError::Internal(format!("could not resolve {}: {}", dir.display(), e))
        })?,
    );
    cfg.set_encryption(EncryptionConfig {
        pq_recipient: params.pq_recipient.clone(),
    });
    cfg.backends.clear();
    cfg.backends
        .insert(params.backend_name.clone(), params.backend);
    cfg.default_backend = params.backend_name.clone();
    cfg.save()?;

    // Re-read so basedir/backends match on-disk layout after save.
    let cfg = config::read_config(dir)?;
    let backend = cfg.init_storage_backend().await?;

    println!("Pulling KEK store and indexes from backend...");
    cfg.pull_indexes(&backend).await?;

    let store = KekStore::new(&cfg.bludir());
    if !store.exists() {
        // Leave the partial vault so the user can inspect; they can
        // delete .blu and retry after publishing keys from the old machine.
        return Err(BluError::Internal(
            "backend has no KEK store (keys/kek.toml missing)\n\
             On the original machine, run any command that pushes indexes\n\
             (e.g. `blu backup`) so the KEK store is published, then retry."
                .into(),
        ));
    }

    Ok(config_path)
}

/// CLI entry point for `blu open`.
pub async fn open(args: OpenArgs) -> Result<(), BluError> {
    let dir = Path::new(&args.dir);
    if !dir.exists() {
        fs::create_dir_all(dir)?;
    }
    let abs_path = dir.canonicalize().map_err(|e| {
        BluError::Internal(format!("fatal: could not resolve {}: {}", dir.display(), e))
    })?;

    let global_meta = load_global_identity()?.ok_or_else(|| {
        BluError::Internal(
            "no global identity found\n\
             Run `blu identity recover` (or `blu identity init`) first"
                .into(),
        )
    })?;
    let pq_recipient = global_meta.pq_public_key;

    let backend = match args.backend_type.as_str() {
        "local" => {
            let path = args.path.ok_or_else(|| {
                BluError::InvalidConfig("--path is required for local backends".into())
            })?;
            BackendConfig::Local(config::backend::LocalConfig {
                path: PathBuf::from(path),
            })
        }
        "s3" => {
            let bucket = args.bucket.ok_or_else(|| {
                BluError::InvalidConfig("--bucket is required for S3 backends".into())
            })?;
            BackendConfig::AmazonS3(config::backend::S3Config {
                bucket,
                prefix: args.prefix,
                region: args.region,
            })
        }
        other => {
            return Err(BluError::InvalidConfig(format!(
                "unknown backend type: \"{}\" (expected local or s3)",
                other
            )));
        }
    };

    println!("Opening blu vault in {}", abs_path.display());
    println!("PQ key: {}...", &pq_recipient[..40.min(pq_recipient.len())]);

    let config_path = open_vault(OpenVaultParams {
        dir: abs_path.clone(),
        pq_recipient: pq_recipient.clone(),
        backend_name: args.backend_name,
        backend,
    })
    .await?;

    // Verify this identity can unwrap the pulled KEK.
    let pq_identity = load_pq_identity_for_open(args.no_passphrase)?;
    let store = KekStore::new(&abs_path.join(".blu"));
    let (_kek, version) = store
        .unwrap_current_kek_with(&[&pq_identity as &dyn Identity])
        .map_err(|e| {
            BluError::Internal(format!(
                "KEK store was pulled but this identity cannot unwrap it: {}\n\
                 Confirm you recovered the same mnemonic used on the original machine.",
                e
            ))
        })?;

    println!("Wrote config to {}", config_path.display());
    println!("KEK store verified (version {})", version);
    println!();
    println!("Vault open. Next:");
    println!("  blu unlock");
    println!("  blu ls");
    println!("  blu restore --all --to <dir>");

    Ok(())
}

/// Load the global PQ identity for KEK verification.
fn load_pq_identity_for_open(no_passphrase: bool) -> Result<crate::keys::pq::PqIdentity, BluError> {
    let age_path = global_identity_age_path()?;
    let seed = if no_passphrase {
        keys::load_pq_seed(&age_path, None)?
    } else {
        match keys::load_pq_seed(&age_path, None) {
            Ok(s) => s,
            Err(BluError::PassphraseRequired) => {
                let pass =
                    keys::prompt_passphrase("Enter passphrase for global identity: ", false)?;
                keys::load_pq_seed(&age_path, Some(&pass))?
            }
            Err(e) => return Err(e),
        }
    };
    Ok(crate::keys::pq::PqIdentity::new(seed))
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::cli::{init_vault, InitVaultParams};
    use crate::keys::mnemonic;
    use crate::storage::BackendKind;

    fn test_pq_recipient() -> String {
        let m = mnemonic::parse_mnemonic(mnemonic::TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        mnemonic::derive_pq_recipient(&seed).unwrap().to_string()
    }

    fn test_pq_identity() -> crate::keys::pq::PqIdentity {
        let m = mnemonic::parse_mnemonic(mnemonic::TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        mnemonic::derive_pq_identity(&seed).unwrap()
    }

    #[tokio::test]
    async fn open_vault_pulls_kek_and_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let source = tmp.path().join("source");
        let backend_data = tmp.path().join("backend");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&backend_data).unwrap();

        let pq_recipient = test_pq_recipient();
        init_vault(
            &source,
            InitVaultParams {
                pq_recipient: pq_recipient.clone(),
            },
        )
        .unwrap();

        // Point source vault at the shared local backend and push.
        let mut src_cfg = config::read_config(&source).unwrap();
        src_cfg.backends.clear();
        src_cfg.backends.insert(
            "remote".into(),
            BackendConfig::Local(config::backend::LocalConfig {
                path: backend_data.clone(),
            }),
        );
        src_cfg.default_backend = "remote".into();
        src_cfg.save().unwrap();
        let src_cfg = config::read_config(&source).unwrap();
        let backend = src_cfg.init_storage_backend().await.unwrap();
        // Seed a non-empty index file so pull has something to fetch.
        fs::write(src_cfg.idxdir().join("index.dat"), b"index-bytes").unwrap();
        src_cfg.push_indexes(&backend).await.unwrap();

        open_vault(OpenVaultParams {
            dir: dest.clone(),
            pq_recipient: pq_recipient.clone(),
            backend_name: "default".into(),
            backend: BackendConfig::Local(config::backend::LocalConfig {
                path: backend_data.clone(),
            }),
        })
        .await
        .unwrap();

        assert!(dest.join(".blu/config.toml").exists());
        assert!(dest.join(".blu/keys/kek.toml").exists());
        assert!(dest.join(".blu/keys/kek_v0/wrapped.age").exists());
        assert_eq!(
            fs::read(dest.join(".blu/indexes/index.dat")).unwrap(),
            b"index-bytes"
        );

        let store = KekStore::new(&dest.join(".blu"));
        let pq_identity = test_pq_identity();
        let (_kek, version) = store
            .unwrap_current_kek_with(&[&pq_identity as &dyn Identity])
            .unwrap();
        assert_eq!(version, 0);
    }

    #[tokio::test]
    async fn open_vault_fails_when_remote_has_no_kek() {
        let tmp = tempfile::tempdir().unwrap();
        let backend_data = tmp.path().join("backend");
        let dest = tmp.path().join("dest");
        fs::create_dir_all(backend_data.join("indexes")).unwrap();
        fs::write(backend_data.join("indexes/index.dat"), b"legacy").unwrap();

        let err = open_vault(OpenVaultParams {
            dir: dest,
            pq_recipient: test_pq_recipient(),
            backend_name: "default".into(),
            backend: BackendConfig::Local(config::backend::LocalConfig { path: backend_data }),
        })
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("no KEK store"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn open_vault_refuses_existing_vault() {
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("vault");
        fs::create_dir_all(dir.join(".blu")).unwrap();
        fs::write(dir.join(".blu/config.toml"), "blu_version = \"0.7.0\"\n").unwrap();

        let err = open_vault(OpenVaultParams {
            dir,
            pq_recipient: test_pq_recipient(),
            backend_name: "default".into(),
            backend: BackendConfig::Local(config::backend::LocalConfig {
                path: tmp.path().join("data"),
            }),
        })
        .await
        .unwrap_err();

        assert!(
            err.to_string().contains("already exists"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn open_vault_backend_is_reachable() {
        // Sanity: BackendKind local can be constructed for the open path.
        let tmp = tempfile::tempdir().unwrap();
        let _ = BackendKind::Local(crate::storage::Local::new(tmp.path()));
    }
}
