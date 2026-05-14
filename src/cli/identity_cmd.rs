//! CLI handlers for `blu identity init`, `blu identity show`, and
//! `blu identity recover`.
//!
//! Identity is global (per user, lives in `~/.blu/`), not per-vault.

use std::fs;
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

use chrono::Utc;
use serde::{Deserialize, Serialize};

use crate::agent::biometric;
use crate::cli::clapargs::{IdentityArgs, IdentityCommand, IdentityInitArgs, IdentityRecoverArgs};
use crate::error::BluError;
use crate::keys;
use crate::keys::mnemonic;

/// Identity metadata, stored in `~/.blu/identity.toml`.
///
/// This file is safe to share; it contains only the public key
/// and creation date.
#[derive(Debug, Serialize, Deserialize)]
pub struct IdentityMeta {
    /// The post-quantum hybrid public key (age1pq...).
    pub pq_public_key: String,
    /// ISO 8601 timestamp of when the identity was created.
    pub created: String,
    /// Whether biometric unlock was set up.
    #[serde(default)]
    pub biometric: bool,
}

/// Load the global identity metadata from `~/.blu/identity.toml`.
///
/// Returns `None` if no global identity exists.
pub fn load_global_identity() -> Result<Option<IdentityMeta>, BluError> {
    let toml_path = identity_toml_path()?;
    if !toml_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&toml_path)?;
    let meta: IdentityMeta = toml::from_str(&content)?;
    Ok(Some(meta))
}

/// Path to the global identity age file (`~/.blu/identity.age`).
pub fn global_identity_age_path() -> Result<PathBuf, BluError> {
    identity_age_path()
}

/// Dispatch identity subcommands.
pub fn identity(args: IdentityArgs) -> Result<(), BluError> {
    match args.command {
        IdentityCommand::Init(a) => identity_init(a),
        IdentityCommand::Show => identity_show(),
        IdentityCommand::Recover(a) => identity_recover(a),
    }
}

/// Resolve the `~/.blu/` directory, creating it if needed.
fn global_blu_dir() -> Result<PathBuf, BluError> {
    let home = dirs::home_dir()
        .ok_or_else(|| BluError::Internal("could not determine home directory".into()))?;
    let dir = home.join(".blu");
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

fn identity_toml_path() -> Result<PathBuf, BluError> {
    Ok(global_blu_dir()?.join("identity.toml"))
}

fn identity_age_path() -> Result<PathBuf, BluError> {
    Ok(global_blu_dir()?.join("identity.age"))
}

fn identity_init(args: IdentityInitArgs) -> Result<(), BluError> {
    let toml_path = identity_toml_path()?;
    let age_path = identity_age_path()?;

    if toml_path.exists() && !args.force {
        return Err(BluError::Internal(
            "identity already exists (use --force to overwrite)".into(),
        ));
    }

    // Generate mnemonic
    let m = mnemonic::generate_mnemonic()?;

    // Ask for optional mnemonic passphrase ("25th word")
    let mnemonic_pass = prompt_optional_passphrase(
        "Enter mnemonic passphrase (optional \"25th word\", press Enter to skip): ",
    )?;

    // Derive keys from BIP39 seed
    let seed = mnemonic::mnemonic_to_seed(&m, &mnemonic_pass);
    let pq_seed = mnemonic::derive_pq_seed(&seed)?;
    let pq_recipient = mnemonic::derive_pq_recipient(&seed)?;
    let pq_public_key = pq_recipient.to_string();

    // Display mnemonic to user
    println!();
    println!("Your 24-word recovery mnemonic:");
    println!();
    let words_str = m.to_string();
    let words: Vec<&str> = words_str.split_whitespace().collect();
    for (i, chunk) in words.chunks(6).enumerate() {
        let start = i * 6 + 1;
        let line: Vec<String> = chunk
            .iter()
            .enumerate()
            .map(|(j, w)| format!("{:>2}. {:<12}", start + j, w))
            .collect();
        println!("  {}", line.join(""));
    }
    println!();
    println!("WRITE THESE WORDS DOWN. They are your only way to recover");
    println!("this identity if you lose access to this device.");
    if !mnemonic_pass.is_empty() {
        println!();
        println!("You also set a mnemonic passphrase. You will need both");
        println!("the 24 words AND the passphrase to recover.");
    }
    println!();

    // Confirm user has written them down
    eprint!("Have you written down your mnemonic? Type 'yes' to continue: ");
    io::stderr().flush()?;
    let mut confirm = String::new();
    io::stdin().lock().read_line(&mut confirm)?;
    if confirm.trim().to_lowercase() != "yes" {
        return Err(BluError::Internal(
            "aborted (mnemonic not confirmed)".into(),
        ));
    }

    // Save PQ seed to identity file
    save_pq_seed_file(&pq_seed, &age_path, args.no_passphrase)?;

    // Set up biometric unlock if available
    let biometric_ok = setup_biometric_if_available(&seed);

    // Save metadata
    save_identity_meta(&pq_public_key, biometric_ok, &toml_path)?;

    println!();
    println!("Identity created.");
    println!("PQ public key: {}...", &pq_public_key[..40]);
    if biometric_ok {
        println!("Touch ID:      enabled");
    }

    Ok(())
}

fn identity_recover(args: IdentityRecoverArgs) -> Result<(), BluError> {
    let toml_path = identity_toml_path()?;
    let age_path = identity_age_path()?;

    if toml_path.exists() && !args.force {
        return Err(BluError::Internal(
            "identity already exists (use --force to overwrite)".into(),
        ));
    }

    // Prompt for mnemonic
    println!("Enter your 24-word recovery mnemonic:");
    eprint!("> ");
    io::stderr().flush()?;
    let mut words = String::new();
    io::stdin().lock().read_line(&mut words)?;
    let words = words.trim();

    let m = mnemonic::parse_mnemonic(words)?;

    // Ask for optional mnemonic passphrase
    let mnemonic_pass =
        prompt_optional_passphrase("Enter mnemonic passphrase (press Enter if none): ")?;

    // Derive keys from BIP39 seed
    let seed = mnemonic::mnemonic_to_seed(&m, &mnemonic_pass);
    let pq_seed = mnemonic::derive_pq_seed(&seed)?;
    let pq_recipient = mnemonic::derive_pq_recipient(&seed)?;
    let pq_public_key = pq_recipient.to_string();

    // Save PQ seed to identity file
    save_pq_seed_file(&pq_seed, &age_path, args.no_passphrase)?;

    // Set up biometric unlock if available
    let biometric_ok = setup_biometric_if_available(&seed);

    // Save metadata
    save_identity_meta(&pq_public_key, biometric_ok, &toml_path)?;

    println!();
    println!("Identity recovered.");
    println!("PQ public key: {}...", &pq_public_key[..40]);
    if biometric_ok {
        println!("Touch ID:      enabled");
    }

    Ok(())
}

fn identity_show() -> Result<(), BluError> {
    let toml_path = identity_toml_path()?;

    if !toml_path.exists() {
        return Err(BluError::Internal(
            "no identity found (run `blu identity init` to create one)".into(),
        ));
    }

    let content = fs::read_to_string(&toml_path)?;
    let meta: IdentityMeta = toml::from_str(&content)?;

    println!(
        "PQ public key: {}...",
        &meta.pq_public_key[..40.min(meta.pq_public_key.len())]
    );
    println!("Created:       {}", meta.created);
    if meta.biometric {
        let status = if biometric::has_biometric_identity() {
            "enabled"
        } else {
            "configured but identity.enc missing"
        };
        println!("Touch ID:      {}", status);
    } else {
        println!("Touch ID:      not configured");
    }

    Ok(())
}

/// Save the PQ seed to the identity file (optionally passphrase-encrypted).
fn save_pq_seed_file(
    seed: &keys::hybrid_kem::HybridSeed,
    age_path: &PathBuf,
    no_passphrase: bool,
) -> Result<(), BluError> {
    let passphrase = if no_passphrase {
        None
    } else {
        let p = keys::prompt_passphrase("Enter passphrase to encrypt identity file: ", true)?;
        if p.is_empty() {
            None
        } else {
            Some(p)
        }
    };

    keys::save_pq_seed(seed, age_path, passphrase.as_deref())?;
    Ok(())
}

/// Save identity metadata to `identity.toml`.
fn save_identity_meta(
    pq_public_key: &str,
    biometric_enabled: bool,
    toml_path: &PathBuf,
) -> Result<(), BluError> {
    let meta = IdentityMeta {
        pq_public_key: pq_public_key.to_string(),
        created: Utc::now().to_rfc3339(),
        biometric: biometric_enabled,
    };
    let toml_str =
        toml::to_string_pretty(&meta).map_err(|e| BluError::SerializationError(e.to_string()))?;
    fs::write(toml_path, toml_str)?;
    Ok(())
}

/// Attempt to set up biometric unlock. Returns true if successful.
/// On failure or unsupported platforms, prints a message and returns false.
fn setup_biometric_if_available(seed: &mnemonic::Seed) -> bool {
    if !biometric::is_available() {
        return false;
    }

    match biometric::setup(seed) {
        Ok(()) => true,
        Err(e) => {
            eprintln!("Warning: could not set up Touch ID: {}", e);
            eprintln!("You can still unlock with your mnemonic or passphrase.");
            false
        }
    }
}

/// Prompt for an optional passphrase (hidden input). Returns empty
/// string if the user presses Enter without typing.
fn prompt_optional_passphrase(prompt: &str) -> Result<String, BluError> {
    let pass = rpassword::prompt_password(prompt)
        .map_err(|e| BluError::Internal(format!("failed to read passphrase: {}", e)))?;
    Ok(pass)
}
