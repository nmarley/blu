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
use crate::keys::kek::KekStore;
use crate::keys::pq::parse_pq_recipient;
use crate::keys::{self, IDENTITY_FILENAME};

/// initialize the .blu repository
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

    // create .blu dir
    let bludir = abs_path.join(".blu/");
    info!("Initializing new .blu dir in {:?}", bludir);
    fs::create_dir_all(&bludir)?;

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

    // Save the identity to the vault (keeps vault self-contained for
    // the passphrase unlock path)
    let identity_path = bludir.join(IDENTITY_FILENAME);
    println!("Saving private key to {}", identity_path.display());
    let passphrase = resolve_passphrase(&args)?;
    keys::save_identity(&identity, &identity_path, passphrase.as_deref())?;

    // Create config with encryption settings
    let mut cfg = config::Config::default();
    cfg.set_encryption(EncryptionConfig {
        recipient: recipient_str.clone(),
        pq_recipient: pq_recipient_str.clone(),
        identity_file: IDENTITY_FILENAME.into(),
    });

    // Write config file
    let config_path = bludir.join("config.toml");
    let mut file = fs::File::create(&config_path)?;
    let cfg_bytes = toml::to_string_pretty(&cfg)?.into_bytes();
    file.write_all(&cfg_bytes)?;
    println!("Wrote config to {}", config_path.display());

    // Initialize KEK store when PQ recipient is available
    if let Some(ref pq_str) = pq_recipient_str {
        let pq_recipient = parse_pq_recipient(pq_str)?;
        let x25519_recipient = keys::parse_recipient(&recipient_str)?;

        let recipients: Vec<&dyn age::Recipient> =
            vec![&pq_recipient as &dyn age::Recipient, &x25519_recipient];
        let user_strings = vec![pq_str.clone(), recipient_str.clone()];

        let store = KekStore::new(&bludir);
        store.init_with(&recipients, &user_strings)?;

        println!("Created KEK store with PQ + X25519 recipients");
    }

    // Create indexes directory
    let indexes_dir = bludir.join("indexes");
    fs::create_dir_all(&indexes_dir)?;

    // Write an empty index file
    let index_path = indexes_dir.join("index.dat");
    check_outfile_writable(&index_path)?;

    // Load the BlackBox from the identity
    let bbox = keys::blackbox_from_identity(identity);
    let index = PlainIndex::new_empty();
    match write_index_file(&index, &bbox, &index_path) {
        Ok(_num_bytes) => println!("Wrote empty index to {}", index_path.display()),
        Err(e) => {
            error!("Error writing index: {}", e);
            return Err(e);
        }
    }

    println!("\nInitialized empty blu repository.");
    if pq_recipient_str.is_some() {
        println!("Vault is protected with post-quantum hybrid encryption.");
    } else {
        println!("\nIMPORTANT: Back up your private key!");
        println!("  Key file: {}", identity_path.display());
        if passphrase.is_none() {
            println!("  WARNING: Key is NOT passphrase protected!");
        }
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

/// Resolve passphrase from args (prompt or skip).
fn resolve_passphrase(args: &InitArgs) -> Result<Option<String>, Box<dyn std::error::Error>> {
    if args.no_passphrase {
        println!("Storing key without passphrase protection (--no-passphrase)");
        return Ok(None);
    }

    let pass = keys::prompt_passphrase(
        "Enter passphrase to protect key (empty for no passphrase): ",
        false,
    )?;
    if pass.is_empty() {
        println!("Warning: storing key without passphrase protection");
        return Ok(None);
    }

    let confirm = keys::prompt_passphrase("Confirm passphrase: ", false)?;
    if pass != confirm {
        return Err("passphrases do not match".into());
    }
    Ok(Some(pass))
}
