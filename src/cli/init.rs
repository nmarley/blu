use std::fs;
use std::io::Write;
use std::path::Path;

use crate::block::PlainIndex;
use crate::cli::clapargs::InitArgs;
use crate::cli::{check_outfile_writable, write_index_file};
use crate::config::{self, EncryptionConfig};
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

    // Generate a new keypair
    println!("Generating new encryption key...");
    let (identity, recipient) = keys::generate_keypair();
    let recipient_str = recipient.to_string();
    println!("Public key: {}", recipient_str);

    // Prompt for passphrase to protect the private key
    let passphrase = keys::prompt_passphrase(
        "Enter passphrase to protect key (empty for no passphrase): ",
        false,
    )?;
    let passphrase = if passphrase.is_empty() {
        println!("Warning: storing key without passphrase protection");
        None
    } else {
        // Confirm passphrase
        let confirm = keys::prompt_passphrase("Confirm passphrase: ", false)?;
        if passphrase != confirm {
            return Err("passphrases do not match".into());
        }
        Some(passphrase)
    };

    // Save the identity (private key)
    let identity_path = bludir.join(IDENTITY_FILENAME);
    println!("Saving private key to {}", identity_path.display());
    keys::save_identity(&identity, &identity_path, passphrase.as_deref())?;

    // Create config with encryption settings
    let mut cfg = config::Config::default();
    cfg.set_encryption(EncryptionConfig {
        recipient: recipient_str,
        identity_file: IDENTITY_FILENAME.into(),
    });

    // Write config file
    let config_path = bludir.join("config.toml");
    let mut file = fs::File::create(&config_path)?;
    let cfg_bytes = toml::to_string_pretty(&cfg)?.into_bytes();
    file.write_all(&cfg_bytes)?;
    println!("Wrote config to {}", config_path.display());

    // Create indexes directory
    let indexes_dir = bludir.join("indexes");
    fs::create_dir_all(&indexes_dir)?;

    // Write an empty index file
    let index_path = indexes_dir.join("index.dat");
    check_outfile_writable(&index_path)?;

    // Load the BlackBox from the saved identity
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
    println!("\nIMPORTANT: Back up your private key!");
    println!("  Key file: {}", identity_path.display());
    if passphrase.is_none() {
        println!("  WARNING: Key is NOT passphrase protected!");
    }

    Ok(())
}
