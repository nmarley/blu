use std::fs;
use std::io::Write;
use std::path::Path;

use crate::age::BlackBox;
use crate::block::PlainIndex;
use crate::cli::clapargs::InitArgs;
use crate::cli::{
    check_outfile_writable, global_identity_age_path, load_global_identity, write_index_file,
};
use crate::config::{self, EncryptionConfig};
use crate::keys;

/// Resolved inputs for vault initialization.
///
/// This struct captures everything needed to create a vault, with no
/// interactive prompts or global state reads. The CLI `init()` function
/// resolves user input into this struct, then passes it to
/// `init_vault()` for the actual work.
///
pub struct InitVaultParams {
    /// The PQ public key string (age1pq...).
    pub pq_recipient: String,
}

/// Initialize a vault at the given directory.
///
/// Pure logic: creates .blu/, config.toml, KEK store, and empty
/// indexes. No interactive prompts, no global state reads, no println
/// output.
///
/// Every vault requires a PQ recipient. The KEK is wrapped with the
/// PQ hybrid key (ML-KEM-768 + X25519), and all data is encrypted
/// via the v2 envelope format (KEK/DEK hierarchy).
pub fn init_vault(
    dir: &Path,
    params: InitVaultParams,
) -> Result<InitVaultResult, Box<dyn std::error::Error>> {
    let bludir = dir.join(".blu/");
    fs::create_dir_all(&bludir)?;

    // Create config with encryption settings
    let mut cfg = config::Config::default();
    cfg.set_encryption(EncryptionConfig {
        pq_recipient: params.pq_recipient.clone(),
    });

    // Write config file
    let config_path = bludir.join("config.toml");
    let mut file = fs::File::create(&config_path)?;
    let cfg_bytes = toml::to_string_pretty(&cfg)?.into_bytes();
    file.write_all(&cfg_bytes)?;

    // Initialize KEK store with the PQ recipient only.
    let pq_recipient = crate::keys::pq::parse_pq_recipient(&params.pq_recipient)?;

    let recipients: Vec<&dyn age::Recipient> = vec![&pq_recipient as &dyn age::Recipient];
    let user_strings = vec![params.pq_recipient.clone()];

    let store = crate::keys::kek::KekStore::new(&bludir);
    let kek = store.init_with(&recipients, &user_strings)?;

    // Create indexes directory and write empty index
    let indexes_dir = bludir.join("indexes");
    fs::create_dir_all(&indexes_dir)?;

    let index_path = indexes_dir.join("index.dat");
    check_outfile_writable(&index_path)?;

    let bbox = BlackBox::new().with_kek(kek, 0);
    let index = PlainIndex::new_empty();
    write_index_file(&index, &bbox, &index_path)?;

    Ok(InitVaultResult { config_path })
}

/// Result of a successful vault initialization.
pub struct InitVaultResult {
    /// Path to the config file.
    pub config_path: std::path::PathBuf,
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

    // Resolve the identity from global ~/.blu/identity.toml.
    let global_meta = load_global_identity()?;

    let meta = global_meta.ok_or(
        "no global identity found\n\
         Run `blu identity init` to create one",
    )?;

    let global_age_path = global_identity_age_path()?;
    let pq_seed = load_global_identity_pq_seed(&global_age_path, &args)?;
    let pq_recipient_str = meta.pq_public_key;

    // Verify the loaded PQ seed matches the metadata
    let derived_recipient = crate::keys::pq::PqIdentity::new(pq_seed)
        .to_public()
        .to_string();
    if derived_recipient != pq_recipient_str {
        return Err(format!(
            "identity mismatch: identity.age PQ key ({}) does not match identity.toml ({})",
            &derived_recipient[..20],
            &pq_recipient_str[..20],
        )
        .into());
    }

    println!("Using global identity from ~/.blu/identity.toml");
    println!(
        "PQ key: {}...",
        &pq_recipient_str[..40.min(pq_recipient_str.len())]
    );

    let params = InitVaultParams {
        pq_recipient: pq_recipient_str,
    };

    let result = init_vault(&abs_path, params)?;

    // Print summary
    println!("Wrote config to {}", result.config_path.display());
    println!("Created KEK store with PQ recipient");
    println!("\nInitialized empty blu repository.");
    println!("Vault is protected with post-quantum hybrid encryption.");

    Ok(())
}

/// Load the global PQ identity file, trying without passphrase
/// first, then prompting if needed.
fn load_global_identity_pq_seed(
    path: &std::path::Path,
    args: &InitArgs,
) -> Result<crate::keys::hybrid_kem::HybridSeed, Box<dyn std::error::Error>> {
    if args.no_passphrase {
        return keys::load_pq_seed(path, None).map_err(|e| e.into());
    }

    // Try without passphrase first (unencrypted key)
    match keys::load_pq_seed(path, None) {
        Ok(id) => return Ok(id),
        Err(crate::error::BluError::PassphraseRequired) => {}
        Err(e) => return Err(e.into()),
    }

    // Prompt for passphrase to decrypt global identity
    let pass = keys::prompt_passphrase("Enter passphrase for global identity: ", false)?;
    keys::load_pq_seed(path, Some(&pass)).map_err(|e| e.into())
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

    fn test_pq_recipient() -> String {
        let m = mnemonic::parse_mnemonic(TEST_MNEMONIC).unwrap();
        let seed = mnemonic::mnemonic_to_seed(&m, "");
        let pq_recipient = mnemonic::derive_pq_recipient(&seed).unwrap();
        pq_recipient.to_string()
    }

    #[test]
    fn init_vault_creates_kek_store() {
        let tmp = tempdir().unwrap();
        let pq_recipient = test_pq_recipient();

        let result = init_vault(
            tmp.path(),
            InitVaultParams {
                pq_recipient: pq_recipient.clone(),
            },
        )
        .unwrap();

        assert!(result.config_path.exists());

        // Verify config.toml has PQ recipient
        let cfg = config::read_config(tmp.path()).unwrap();
        let enc = cfg.encryption.unwrap();
        assert_eq!(enc.pq_recipient, pq_recipient);

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

        // Metadata records the PQ user key
        let meta = store.load_metadata().unwrap();
        assert_eq!(meta.versions[0].users.len(), 1);
    }

    #[test]
    fn init_vault_creates_empty_index() {
        let tmp = tempdir().unwrap();
        let pq_recipient = test_pq_recipient();

        init_vault(tmp.path(), InitVaultParams { pq_recipient }).unwrap();

        let index_path = tmp.path().join(".blu/indexes/index.dat");
        assert!(index_path.exists());
        // File should be non-empty (encrypted empty index)
        assert!(fs::metadata(&index_path).unwrap().len() > 0);
    }

    #[test]
    fn config_round_trip_with_pq() {
        let mut cfg = config::Config::default();
        cfg.set_encryption(EncryptionConfig {
            pq_recipient: "age1pqtest".to_string(),
        });

        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let parsed: config::Config = toml::from_str(&toml_str).unwrap();
        let enc = parsed.encryption.unwrap();
        assert_eq!(enc.pq_recipient, "age1pqtest");
    }

    #[test]
    fn init_vault_pq_full_round_trip() {
        // Full round-trip: init with PQ -> unwrap KEK with PQ
        // -> wrap DEK -> unwrap DEK -> encrypt data -> decrypt data
        let tmp = tempdir().unwrap();
        let pq_recipient_str = test_pq_recipient();

        init_vault(
            tmp.path(),
            InitVaultParams {
                pq_recipient: pq_recipient_str,
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
