use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

use age::x25519::Identity;

use crate::block::PlainIndex;
use crate::cli::clapargs::InitArgs;
use crate::cli::{
    check_outfile_writable, global_identity_age_path, load_global_identity, write_index_file,
};
use crate::config::{self, EncryptionConfig};
use crate::keys::{self, IDENTITY_FILENAME};

/// Resolved inputs for vault initialization.
///
/// This struct captures everything needed to create a vault, with no
/// interactive prompts or global state reads. The CLI `init()` function
/// resolves user input into this struct, then passes it to
/// `init_vault()` for the actual work.
///
/// The identity (private key) lives at `~/.blu/identity.age` and is not
/// stored in the vault. Pass it separately for the initial index write.
pub struct InitVaultParams {
    /// The age X25519 identity (private key), used only to write the
    /// initial empty index. Not persisted in the vault.
    pub identity: Identity,
    /// The X25519 public key string (age1...).
    pub recipient: String,
    /// The PQ public key string (age1pq...), if available.
    pub pq_recipient: Option<String>,
}

/// Initialize a vault at the given directory.
///
/// Pure logic: creates .blu/, config.toml, KEK store (when PQ is
/// available), and empty indexes. No interactive prompts, no global
/// state reads, no println output. The identity is used only to
/// encrypt the initial empty index; it is not persisted in the vault.
pub fn init_vault(
    dir: &Path,
    params: InitVaultParams,
) -> Result<InitVaultResult, Box<dyn std::error::Error>> {
    let bludir = dir.join(".blu/");
    fs::create_dir_all(&bludir)?;

    // Create config with encryption settings
    let mut cfg = config::Config::default();
    cfg.set_encryption(EncryptionConfig {
        recipient: params.recipient.clone(),
        pq_recipient: params.pq_recipient.clone(),
    });

    // Write config file
    let config_path = bludir.join("config.toml");
    let mut file = fs::File::create(&config_path)?;
    let cfg_bytes = toml::to_string_pretty(&cfg)?.into_bytes();
    file.write_all(&cfg_bytes)?;

    // Initialize KEK store when PQ recipient is available.
    //
    // The age spec (C2SP v1.1.0) forbids mixing PQ recipients
    // (which carry the "postquantum" label) with classical X25519
    // recipients in the same age file. New vaults therefore wrap the
    // KEK to PQ only. The user_strings metadata records both keys
    // for documentation, but only the PQ recipient is used for the
    // actual age encryption.
    //
    // Consequences for the passphrase-only unlock path: it cannot
    // derive the PQ seed (no mnemonic available), so it cannot
    // unwrap PQ-wrapped KEKs. Biometric unlock or mnemonic recovery
    // is required for vaults with PQ KEKs. This is acceptable
    // because the PQ stanza protects against harvest-now-decrypt-
    // later attacks on the KEK blob at rest.
    let has_kek = if let Some(ref pq_str) = params.pq_recipient {
        let pq_recipient = crate::keys::pq::parse_pq_recipient(pq_str)?;

        let recipients: Vec<&dyn age::Recipient> = vec![&pq_recipient as &dyn age::Recipient];
        let user_strings = vec![pq_str.clone(), params.recipient.clone()];

        let store = crate::keys::kek::KekStore::new(&bludir);
        store.init_with(&recipients, &user_strings)?;
        true
    } else {
        false
    };

    // Create indexes directory and write empty index
    let indexes_dir = bludir.join("indexes");
    fs::create_dir_all(&indexes_dir)?;

    let index_path = indexes_dir.join("index.dat");
    check_outfile_writable(&index_path)?;

    let bbox = keys::blackbox_from_identity(params.identity);
    let index = PlainIndex::new_empty();
    write_index_file(&index, &bbox, &index_path)?;

    Ok(InitVaultResult {
        config_path,
        has_kek,
    })
}

/// Result of a successful vault initialization.
pub struct InitVaultResult {
    /// Path to the config file.
    pub config_path: std::path::PathBuf,
    /// Whether a KEK store was created (PQ + X25519).
    pub has_kek: bool,
}

/// CLI entry point for `blu init`.
///
/// Resolves user input (global identity, key file, passphrase prompts)
/// then delegates to `init_vault()`.
pub fn init(args: InitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let dir = Path::new(&args.dir);
    let abs_path = match std::fs::canonicalize(dir) {
        Ok(dir) => dir,
        Err(e) => {
            return Err(format!("fatal: {}", e).into());
        }
    };

    if config::read_config(dir).is_ok() {
        println!(
            "Reinitialized existing blu repository in {}",
            abs_path.display()
        );
        return Ok(());
    }

    println!("Initializing blu repository in {}", abs_path.display());

    // Resolve the identity. Two paths:
    //   1. Global identity (~/.blu/identity.toml) provides both X25519
    //      and PQ keys. This is the default for new vaults.
    //   2. Legacy --key-file import: X25519 only, no KEK store, no PQ.
    let global_meta = load_global_identity()?;

    let (identity, recipient_str, pq_recipient_str) = if let Some(ref key_file) = args.key_file {
        // Legacy key import path
        let identity = import_key_file(key_file)?;
        let recipient_str = identity.to_public().to_string();

        if global_meta.is_some() {
            println!("Note: --key-file overrides global identity for this vault");
        }
        println!("Warning: --key-file creates an X25519-only vault (no PQ protection)");

        (identity, recipient_str, None)
    } else if let Some(ref meta) = global_meta {
        // Read identity from global ~/.blu/identity.age
        let global_age_path = global_identity_age_path()?;
        let identity = load_global_identity_age(&global_age_path, &args)?;
        let recipient_str = meta.public_key.clone();
        let pq_str = meta.pq_public_key.clone();

        // Verify the loaded identity matches the metadata
        let derived_recipient = identity.to_public().to_string();
        if derived_recipient != recipient_str {
            return Err(format!(
                "identity mismatch: identity.age public key ({}) does not match identity.toml ({})",
                &derived_recipient[..20],
                &recipient_str[..20],
            )
            .into());
        }

        println!("Using global identity from ~/.blu/identity.toml");
        println!("Public key: {}", recipient_str);
        if let Some(ref pq) = pq_str {
            println!("PQ key:     {}...", &pq[..40.min(pq.len())]);
        }

        (identity, recipient_str, pq_str)
    } else {
        return Err(
            "no global identity found\n\
             Run `blu identity init` to create one, or use `--key-file` for legacy X25519-only mode"
                .into(),
        );
    };

    let params = InitVaultParams {
        identity,
        recipient: recipient_str,
        pq_recipient: pq_recipient_str.clone(),
    };

    let result = init_vault(&abs_path, params)?;

    // Print summary
    println!("Wrote config to {}", result.config_path.display());
    if result.has_kek {
        println!("Created KEK store with PQ + X25519 recipients");
    }
    println!("\nInitialized empty blu repository.");
    if pq_recipient_str.is_some() {
        println!("Vault is protected with post-quantum hybrid encryption.");
    }

    Ok(())
}

/// Load the global identity age file, trying without passphrase
/// first, then prompting if needed.
fn load_global_identity_age(
    path: &std::path::Path,
    args: &InitArgs,
) -> Result<Identity, Box<dyn std::error::Error>> {
    if args.no_passphrase {
        return keys::load_identity(path, None).map_err(|e| e.into());
    }

    // Try without passphrase first (unencrypted key)
    match keys::load_identity(path, None) {
        Ok(id) => return Ok(id),
        Err(crate::error::BluError::PassphraseRequired) => {}
        Err(e) => return Err(e.into()),
    }

    // Prompt for passphrase to decrypt global identity
    let pass = keys::prompt_passphrase("Enter passphrase for global identity: ", false)?;
    keys::load_identity(path, Some(&pass)).map_err(|e| e.into())
}

/// Import an X25519 identity from a key file (legacy path).
fn import_key_file(key_file: &str) -> Result<Identity, Box<dyn std::error::Error>> {
    println!("Importing key from {}", key_file);
    let key_contents = fs::read_to_string(key_file)
        .map_err(|e| format!("failed to read key file '{}': {}", key_file, e))?;
    let key_str = key_contents
        .lines()
        .find(|line| line.starts_with("AGE-SECRET-KEY-"))
        .ok_or_else(|| format!("no AGE-SECRET-KEY found in '{}'", key_file))?
        .trim();
    Identity::from_str(key_str)
        .map_err(|e| format!("invalid age key in '{}': {}", key_file, e).into())
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::keys::kek::KekStore;
    use crate::keys::mnemonic;
    use age::Identity;
    use tempfile::tempdir;

    const TEST_MNEMONIC: &str = "abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon abandon abandon \
                                  abandon abandon abandon abandon abandon art";

    fn test_identity_and_recipients() -> (age::x25519::Identity, String, String) {
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let identity = mnemonic::derive_x25519_identity(&seed).unwrap();
        let recipient = mnemonic::public_key_from_identity(&identity);
        let pq_recipient = mnemonic::derive_pq_recipient(&seed).unwrap();
        (identity, recipient, pq_recipient.to_string())
    }

    fn test_x25519_only() -> (age::x25519::Identity, String) {
        let identity_str = include_str!("../../test/blu_secrets/blu.key").trim();
        let identity = age::x25519::Identity::from_str(identity_str).unwrap();
        let recipient = identity.to_public().to_string();
        (identity, recipient)
    }

    #[test]
    fn init_vault_with_pq_creates_kek_store() {
        let tmp = tempdir().unwrap();
        let (identity, recipient, pq_recipient) = test_identity_and_recipients();

        let result = init_vault(
            tmp.path(),
            InitVaultParams {
                identity,
                recipient: recipient.clone(),
                pq_recipient: Some(pq_recipient.clone()),
            },
        )
        .unwrap();

        assert!(result.has_kek);
        assert!(result.config_path.exists());

        // Verify config.toml has both recipients
        let cfg = config::read_config(tmp.path()).unwrap();
        let enc = cfg.encryption.unwrap();
        assert_eq!(enc.recipient, recipient);
        assert_eq!(enc.pq_recipient.unwrap(), pq_recipient);

        // Verify KEK store exists and is decryptable by PQ identity
        let blu_dir = tmp.path().join(".blu");
        let store = KekStore::new(&blu_dir);
        assert!(store.exists());

        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let pq_identity = mnemonic::derive_pq_identity(&seed).unwrap();
        let (_, version) = store
            .unwrap_current_kek_with(&[&pq_identity as &dyn Identity])
            .unwrap();
        assert_eq!(version, 0);

        // X25519-only identity cannot unwrap PQ-wrapped KEK (this is
        // expected; the age spec forbids mixing PQ and classical
        // recipients, so new vaults wrap PQ-only).
        let x25519_identity = mnemonic::derive_x25519_identity(&seed).unwrap();
        let result = store.unwrap_current_kek_with(&[&x25519_identity as &dyn Identity]);
        assert!(result.is_err());

        // Metadata records both user keys for documentation
        let meta = store.load_metadata().unwrap();
        assert_eq!(meta.versions[0].users.len(), 2);
    }

    #[test]
    fn init_vault_without_pq_skips_kek_store() {
        let tmp = tempdir().unwrap();
        let (identity, recipient) = test_x25519_only();

        let result = init_vault(
            tmp.path(),
            InitVaultParams {
                identity,
                recipient: recipient.clone(),
                pq_recipient: None,
            },
        )
        .unwrap();

        assert!(!result.has_kek);

        // Verify config has no pq_recipient
        let cfg = config::read_config(tmp.path()).unwrap();
        let enc = cfg.encryption.unwrap();
        assert_eq!(enc.recipient, recipient);
        assert!(enc.pq_recipient.is_none());

        // Verify no KEK store
        let blu_dir = tmp.path().join(".blu");
        let store = KekStore::new(&blu_dir);
        assert!(!store.exists());
    }

    #[test]
    fn init_vault_creates_empty_index() {
        let tmp = tempdir().unwrap();
        let (identity, recipient) = test_x25519_only();

        init_vault(
            tmp.path(),
            InitVaultParams {
                identity,
                recipient,
                pq_recipient: None,
            },
        )
        .unwrap();

        let index_path = tmp.path().join(".blu/indexes/index.dat");
        assert!(index_path.exists());
        // File should be non-empty (encrypted empty index)
        assert!(fs::metadata(&index_path).unwrap().len() > 0);
    }

    #[test]
    fn init_vault_twice_at_same_dir_succeeds() {
        // Second init_vault at the same dir should still work
        // (KEK store will fail because it exists, but that is caught
        // at the CLI layer by the read_config check. init_vault
        // itself assumes a fresh directory.)
        let tmp = tempdir().unwrap();
        let (identity, recipient) = test_x25519_only();

        init_vault(
            tmp.path(),
            InitVaultParams {
                identity,
                recipient: recipient.clone(),
                pq_recipient: None,
            },
        )
        .unwrap();

        // A second init without PQ (no KEK store) should succeed
        // because it overwrites config and index.
        let (identity2, _) = test_x25519_only();
        let result = init_vault(
            tmp.path(),
            InitVaultParams {
                identity: identity2,
                recipient,
                pq_recipient: None,
            },
        );
        assert!(result.is_ok());
    }

    #[test]
    fn config_backward_compat_no_pq_field() {
        // Old config.toml without pq_recipient (and with the legacy
        // identity_file field) should deserialize fine; unknown fields
        // are silently ignored by serde.
        let toml_str = r#"
blu_version = "0.5.0"

[encryption]
recipient = "age1abc123"
identity_file = "identity.age"
"#;
        let cfg: config::Config = toml::from_str(toml_str).unwrap();
        let enc = cfg.encryption.unwrap();
        assert_eq!(enc.recipient, "age1abc123");
        assert!(enc.pq_recipient.is_none());
    }

    #[test]
    fn config_round_trip_with_pq() {
        let mut cfg = config::Config::default();
        cfg.set_encryption(EncryptionConfig {
            recipient: "age1test".to_string(),
            pq_recipient: Some("age1pqtest".to_string()),
        });

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: config::Config = toml::from_str(&toml_str).unwrap();
        let enc = parsed.encryption.unwrap();
        assert_eq!(enc.recipient, "age1test");
        assert_eq!(enc.pq_recipient.unwrap(), "age1pqtest");
    }

    #[test]
    fn config_round_trip_without_pq_omits_field() {
        let mut cfg = config::Config::default();
        cfg.set_encryption(EncryptionConfig {
            recipient: "age1test".to_string(),
            pq_recipient: None,
        });

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        // pq_recipient should not appear in serialized output
        assert!(!toml_str.contains("pq_recipient"));

        let parsed: config::Config = toml::from_str(&toml_str).unwrap();
        let enc = parsed.encryption.unwrap();
        assert!(enc.pq_recipient.is_none());
    }

    #[test]
    fn init_vault_pq_full_round_trip() {
        // Full round-trip: init with PQ -> unwrap KEK with PQ
        // -> wrap DEK -> unwrap DEK -> encrypt data -> decrypt data
        let tmp = tempdir().unwrap();
        let (identity, recipient, pq_recipient_str) = test_identity_and_recipients();

        init_vault(
            tmp.path(),
            InitVaultParams {
                identity,
                recipient,
                pq_recipient: Some(pq_recipient_str),
            },
        )
        .unwrap();

        let blu_dir = tmp.path().join(".blu");
        let store = KekStore::new(&blu_dir);

        // Derive PQ identity from the same mnemonic
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let pq_identity = mnemonic::derive_pq_identity(&seed).unwrap();

        // Unwrap KEK with PQ
        let (kek, _) = store
            .unwrap_current_kek_with(&[&pq_identity as &dyn Identity])
            .unwrap();

        // DEK round-trip
        let (dek, wrapped_dek) = crate::keys::dek::generate_and_wrap(&kek).unwrap();
        let recovered_dek = crate::keys::dek::Dek::unwrap(&kek, &wrapped_dek).unwrap();
        assert_eq!(recovered_dek.as_bytes(), dek.as_bytes());

        // Data round-trip
        let plaintext = b"most triumphant PQ vault data";
        let ciphertext = dek.encrypt_data(plaintext).unwrap();
        let decrypted = recovered_dek.decrypt_data(&ciphertext).unwrap();
        assert_eq!(&decrypted, plaintext);
    }
}
