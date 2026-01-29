use std::fs;
use std::io::Write;
use std::path::Path;
use std::str::FromStr;

use age::x25519::Identity;

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

    // Get or generate the identity (private key)
    let identity = if let Some(key_file) = &args.key_file {
        // Import existing key
        println!("Importing key from {}", key_file);
        let key_contents = fs::read_to_string(key_file)
            .map_err(|e| format!("failed to read key file '{}': {}", key_file, e))?;
        // Find the AGE-SECRET-KEY line (skip comments)
        let key_str = key_contents
            .lines()
            .find(|line| line.starts_with("AGE-SECRET-KEY-"))
            .ok_or_else(|| format!("no AGE-SECRET-KEY found in '{}'", key_file))?
            .trim();
        Identity::from_str(key_str)
            .map_err(|e| format!("invalid age key in '{}': {}", key_file, e))?
    } else {
        // Generate new keypair
        println!("Generating new encryption key...");
        let (identity, _) = keys::generate_keypair();
        identity
    };

    let recipient = identity.to_public();
    let recipient_str = recipient.to_string();
    println!("Public key: {}", recipient_str);

    // Determine passphrase handling
    let passphrase = if args.no_passphrase {
        println!("Storing key without passphrase protection (--no-passphrase)");
        None
    } else {
        // Prompt for passphrase
        let pass = keys::prompt_passphrase(
            "Enter passphrase to protect key (empty for no passphrase): ",
            false,
        )?;
        if pass.is_empty() {
            println!("Warning: storing key without passphrase protection");
            None
        } else {
            // Confirm passphrase
            let confirm = keys::prompt_passphrase("Confirm passphrase: ", false)?;
            if pass != confirm {
                return Err("passphrases do not match".into());
            }
            Some(pass)
        }
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
    println!("\nIMPORTANT: Back up your private key!");
    println!("  Key file: {}", identity_path.display());
    if passphrase.is_none() {
        println!("  WARNING: Key is NOT passphrase protected!");
    }

    Ok(())
}
