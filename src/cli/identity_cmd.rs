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
/// This file is safe to share; it contains only public keys
/// and creation date.
#[derive(Debug, Serialize, Deserialize)]
pub struct IdentityMeta {
    /// The age X25519 public key (age1...).
    pub public_key: String,
    /// The post-quantum hybrid public key (age1pq...).
    #[serde(default)]
    pub pq_public_key: Option<String>,
    /// ISO 8601 timestamp of when the identity was created.
    pub created: String,
    /// Whether biometric unlock was set up.
    #[serde(default)]
    pub biometric: bool,
}

/// Load the global identity metadata from `~/.blu/identity.toml`.
///
/// Returns `None` if no global identity exists.
pub fn load_global_identity() -> Result<Option<IdentityMeta>, Box<dyn std::error::Error>> {
    let toml_path = identity_toml_path()?;
    if !toml_path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(&toml_path)?;
    let meta: IdentityMeta = toml::from_str(&content)?;
    Ok(Some(meta))
}

/// Path to the global identity age file (`~/.blu/identity.age`).
pub fn global_identity_age_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    identity_age_path()
}

/// Dispatch identity subcommands.
pub fn identity(args: IdentityArgs) -> Result<(), Box<dyn std::error::Error>> {
    match args.command {
        IdentityCommand::Init(a) => identity_init(a),
        IdentityCommand::Show => identity_show(),
        IdentityCommand::Recover(a) => identity_recover(a),
    }
}

/// Resolve the `~/.blu/` directory, creating it if needed.
fn global_blu_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    let home = dirs::home_dir().ok_or("could not determine home directory")?;
    let dir = home.join(".blu");
    if !dir.exists() {
        fs::create_dir_all(&dir)?;
    }
    Ok(dir)
}

fn identity_toml_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(global_blu_dir()?.join("identity.toml"))
}

fn identity_age_path() -> Result<PathBuf, Box<dyn std::error::Error>> {
    Ok(global_blu_dir()?.join("identity.age"))
}

fn identity_init(args: IdentityInitArgs) -> Result<(), Box<dyn std::error::Error>> {
    let toml_path = identity_toml_path()?;
    let age_path = identity_age_path()?;

    if toml_path.exists() && !args.force {
        return Err("identity already exists (use --force to overwrite)".into());
    }

    // Generate mnemonic
    let m = mnemonic::generate_mnemonic()?;

    // Ask for optional mnemonic passphrase ("25th word")
    let mnemonic_pass = prompt_optional_passphrase(
        "Enter mnemonic passphrase (optional \"25th word\", press Enter to skip): ",
    )?;

    // Derive identity (X25519 + PQ)
    let seed = mnemonic::mnemonic_to_seed(&m, &mnemonic_pass);
    let identity = mnemonic::derive_x25519_identity(&seed)?;
    let public_key = mnemonic::public_key_from_identity(&identity);
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
        return Err("aborted (mnemonic not confirmed)".into());
    }

    // Save identity file
    save_identity_age_file(&identity, &age_path, args.no_passphrase)?;

    // Set up biometric unlock if available
    let biometric_ok = setup_biometric_if_available(&seed);

    // Save metadata
    save_identity_meta(&public_key, Some(&pq_public_key), biometric_ok, &toml_path)?;

    println!();
    println!("Identity created.");
    println!("Public key:    {}", public_key);
    println!("PQ public key: {}...", &pq_public_key[..40]);
    if biometric_ok {
        println!("Touch ID:      enabled");
    }

    Ok(())
}

fn identity_recover(args: IdentityRecoverArgs) -> Result<(), Box<dyn std::error::Error>> {
    let toml_path = identity_toml_path()?;
    let age_path = identity_age_path()?;

    if toml_path.exists() && !args.force {
        return Err("identity already exists (use --force to overwrite)".into());
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

    // Derive identity (X25519 + PQ)
    let seed = mnemonic::mnemonic_to_seed(&m, &mnemonic_pass);
    let identity = mnemonic::derive_x25519_identity(&seed)?;
    let public_key = mnemonic::public_key_from_identity(&identity);
    let pq_recipient = mnemonic::derive_pq_recipient(&seed)?;
    let pq_public_key = pq_recipient.to_string();

    // Save identity file
    save_identity_age_file(&identity, &age_path, args.no_passphrase)?;

    // Set up biometric unlock if available
    let biometric_ok = setup_biometric_if_available(&seed);

    // Save metadata
    save_identity_meta(&public_key, Some(&pq_public_key), biometric_ok, &toml_path)?;

    println!();
    println!("Identity recovered.");
    println!("Public key:    {}", public_key);
    println!("PQ public key: {}...", &pq_public_key[..40]);
    if biometric_ok {
        println!("Touch ID:      enabled");
    }

    Ok(())
}

fn identity_show() -> Result<(), Box<dyn std::error::Error>> {
    let toml_path = identity_toml_path()?;

    if !toml_path.exists() {
        return Err("no identity found (run `blu identity init` to create one)".into());
    }

    let content = fs::read_to_string(&toml_path)?;
    let meta: IdentityMeta = toml::from_str(&content)?;

    println!("Public key:    {}", meta.public_key);
    if let Some(ref pq_pk) = meta.pq_public_key {
        println!("PQ public key: {}...", &pq_pk[..40.min(pq_pk.len())]);
    } else {
        println!("PQ public key: not configured (re-run identity init or recover)");
    }
    println!("Created:       {}", meta.created);
    if meta.biometric {
        let status = if biometric::has_biometric_identity() {
            "enabled"
        } else {
            "configured but identity.enc missing"
        };
        println!("Touch ID:   {}", status);
    } else {
        println!("Touch ID:   not configured");
    }

    Ok(())
}

/// Save the age identity file (optionally passphrase-encrypted).
fn save_identity_age_file(
    identity: &age::x25519::Identity,
    age_path: &PathBuf,
    no_passphrase: bool,
) -> Result<(), Box<dyn std::error::Error>> {
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

    keys::save_identity(identity, age_path, passphrase.as_deref())?;
    Ok(())
}

/// Save identity metadata to `identity.toml`.
fn save_identity_meta(
    public_key: &str,
    pq_public_key: Option<&str>,
    biometric_enabled: bool,
    toml_path: &PathBuf,
) -> Result<(), Box<dyn std::error::Error>> {
    let meta = IdentityMeta {
        public_key: public_key.to_string(),
        pq_public_key: pq_public_key.map(|s| s.to_string()),
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
fn prompt_optional_passphrase(prompt: &str) -> Result<String, Box<dyn std::error::Error>> {
    let pass = rpassword::prompt_password(prompt)
        .map_err(|e| BluError::Internal(format!("failed to read passphrase: {}", e)))?;
    Ok(pass)
}
