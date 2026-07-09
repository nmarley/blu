//! Helper functions for CLI commands.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::agent::AgentClient;
use crate::config::{self, Config};
use crate::dek_provider::DekProvider;
use crate::error::{BluError, Result};
use crate::keys;

/// Global flag for --no-passphrase option.
/// Set by the main binary before calling CLI commands.
static NO_PASSPHRASE: AtomicBool = AtomicBool::new(false);

/// Set the global no-passphrase flag.
pub fn set_no_passphrase(value: bool) {
    NO_PASSPHRASE.store(value, Ordering::SeqCst);
}

/// Get the global no-passphrase flag.
pub fn get_no_passphrase() -> bool {
    NO_PASSPHRASE.load(Ordering::SeqCst)
}

/// Options for loading the encryption context.
pub struct LoadOptions<'a> {
    /// Passphrase to decrypt the identity file (if encrypted).
    /// If None and the file is encrypted, will prompt interactively.
    pub passphrase: Option<&'a str>,
    /// If true, skip passphrase prompt and fail if key is encrypted.
    pub no_passphrase: bool,
}

impl Default for LoadOptions<'_> {
    fn default() -> Self {
        Self {
            passphrase: None,
            no_passphrase: get_no_passphrase(),
        }
    }
}

/// Load the config and DekProvider for CLI operations.
///
/// This is the main entry point for CLI commands that need encryption.
/// It will try to use the agent daemon for session-cached keys. If the
/// agent is not available (or --no-passphrase is set), it falls back
/// to loading the key directly in-process.
pub fn load_config_and_keys(opts: &LoadOptions<'_>) -> Result<(Config, DekProvider)> {
    let dir = Path::new(".");

    let cfg = config::read_config(dir).inspect_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
    })?;

    let keys = load_keys_from_config(&cfg, opts)?;

    Ok((cfg, keys))
}

/// Load the DekProvider from a config, handling agent and passphrase prompting.
///
/// Strategy:
/// 1. Always use the agent. PQ-only vaults require the agent-held PQ
///    seed to unwrap KEKs.
/// 2. If --no-passphrase is set, try unlocking with an empty
///    passphrase only and never prompt.
/// 3. Otherwise, connect to the agent (auto-starting if needed),
///    check if already unlocked, prompt and unlock if locked.
pub fn load_keys_from_config(cfg: &Config, opts: &LoadOptions<'_>) -> Result<DekProvider> {
    if !cfg.has_encryption() {
        return Err(BluError::NoKeyConfigured);
    }

    // --no-passphrase: do not prompt, but still use the agent.
    if opts.no_passphrase {
        return load_keys_via_agent(cfg, "");
    }

    // If an explicit passphrase was provided, use the agent with it
    if let Some(pass) = opts.passphrase {
        return load_keys_via_agent(cfg, pass);
    }

    try_agent_keys(cfg)
}

/// Try to get a DekProvider through the agent daemon.
///
/// Connects to the agent (auto-starting if needed), checks status,
/// prompts for passphrase if locked, and returns an agent-backed DekProvider.
fn try_agent_keys(cfg: &Config) -> Result<DekProvider> {
    let client = AgentClient::new()?;
    client.ensure_running()?;
    let kek_dir = Some(cfg.bludir().to_string_lossy().into_owned());

    let resp = client.status()?;
    let unlocked = resp["result"]["unlocked"].as_bool().unwrap_or(false);

    if unlocked {
        return Ok(DekProvider::Agent { client, kek_dir });
    }

    // Agent is running but locked; try without passphrase first
    match client.unlock("") {
        Ok(_) => return Ok(DekProvider::Agent { client, kek_dir }),
        Err(BluError::WrongPassphrase) | Err(BluError::Internal(_)) => {
            // Key is passphrase-protected, need to prompt
        }
        Err(e) => return Err(e),
    }

    let pass = keys::prompt_passphrase("Enter passphrase: ", false)?;
    client.unlock(&pass)?;
    Ok(DekProvider::Agent { client, kek_dir })
}

/// Load a DekProvider via the agent using an explicit passphrase.
fn load_keys_via_agent(cfg: &Config, passphrase: &str) -> Result<DekProvider> {
    let client = AgentClient::new()?;
    client.ensure_running()?;

    client.unlock(passphrase)?;
    let kek_dir = Some(cfg.bludir().to_string_lossy().into_owned());
    Ok(DekProvider::Agent { client, kek_dir })
}

/// Push the local index files to a backend, resolved by optional name.
///
/// Resolves the backend the same way every command does: the named
/// backend if `backend_name` is `Some`, otherwise the config's default
/// backend. Reuses an already-initialized backend when the caller
/// passes one via `backend`, avoiding a redundant client construction.
///
/// This is the shared seam that keeps index-push behavior uniform
/// across every index-modifying CLI command. Pushing is not optional:
/// the backend is the source of truth, so a push failure is a hard
/// error with a message that makes clear the local indexes are already
/// written and only the remote copy is behind.
pub async fn push_indexes_or_fail(
    cfg: &Config,
    backend_name: Option<&str>,
    backend: Option<&crate::storage::BackendKind>,
) -> Result<()> {
    let resolved_name = backend_name.unwrap_or(&cfg.default_backend);

    let owned;
    let backend = match backend {
        Some(b) => b,
        None => {
            owned = cfg.init_named_backend(resolved_name).await?;
            &owned
        }
    };

    cfg.push_indexes(backend).await.map_err(|e| {
        BluError::Internal(format!(
            "Local indexes updated, but push to backend `{}` failed: {}. \
             Re-run when the backend is reachable.",
            resolved_name, e
        ))
    })
}

/// Load just the config (for commands that don't need encryption).
pub fn load_config() -> Result<Config> {
    let dir = Path::new(".");

    config::read_config(dir).inspect_err(|e| {
        eprintln!("Unable to read config file. Please create configuration via `init` subcommand");
        eprintln!("More info: {}", e);
    })
}

#[cfg(test)]
mod test {
    use super::*;

    /// Build a `Config` with a single local backend named `local`
    /// pointing at `datadir`, with `basedir` set to `basedir`. The
    /// backend path is absolute so it resolves independently of the
    /// process working directory.
    fn local_config(datadir: &Path, basedir: &Path) -> Config {
        let toml_str = format!(
            r#"
            blu_version = "0.5.0"
            default_backend = "local"

            [backends.local]
            type = "local"
            path = "{}"
            "#,
            datadir.display()
        );
        let mut cfg: Config = toml::from_str(&toml_str).unwrap();
        cfg.set_basedir(basedir.to_path_buf());
        cfg
    }

    #[tokio::test]
    async fn push_indexes_or_fail_uploads_local_indexes() {
        let tmp = tempfile::tempdir().unwrap();
        let datadir = tmp.path().join("data");
        let basedir = tmp.path().join("vault");
        let cfg = local_config(&datadir, &basedir);

        // Write local index files into the vault's idxdir.
        let idxdir = cfg.idxdir();
        std::fs::create_dir_all(&idxdir).unwrap();
        std::fs::write(idxdir.join("index.dat"), b"plain-bytes").unwrap();
        std::fs::write(idxdir.join("blob_index.dat"), b"blob-bytes").unwrap();
        std::fs::write(idxdir.join("tags.dat"), b"tag-bytes").unwrap();

        // Push via the shared helper (resolves the default backend).
        push_indexes_or_fail(&cfg, None, None).await.unwrap();

        // Each index must now exist under the backend datadir at the
        // `indexes/` prefix, byte-for-byte.
        assert_eq!(
            std::fs::read(datadir.join("indexes/index.dat")).unwrap(),
            b"plain-bytes"
        );
        assert_eq!(
            std::fs::read(datadir.join("indexes/blob_index.dat")).unwrap(),
            b"blob-bytes"
        );
        assert_eq!(
            std::fs::read(datadir.join("indexes/tags.dat")).unwrap(),
            b"tag-bytes"
        );
    }

    #[tokio::test]
    async fn push_indexes_or_fail_reports_hard_fail_message() {
        let tmp = tempfile::tempdir().unwrap();

        // Place a regular file where the backend expects a directory
        // parent, so the local backend's create_dir_all fails and the
        // push errors out.
        let blocker = tmp.path().join("blocker");
        std::fs::write(&blocker, b"i am a file, not a directory").unwrap();
        let datadir = blocker.join("data");
        let basedir = tmp.path().join("vault");
        let cfg = local_config(&datadir, &basedir);

        let idxdir = cfg.idxdir();
        std::fs::create_dir_all(&idxdir).unwrap();
        std::fs::write(idxdir.join("index.dat"), b"plain-bytes").unwrap();

        let err = push_indexes_or_fail(&cfg, None, None)
            .await
            .expect_err("push must fail when the backend is unwritable");
        let msg = err.to_string();
        assert!(
            msg.contains("Local indexes updated, but push to backend `local` failed"),
            "unexpected error message: {msg}"
        );
        assert!(
            msg.contains("Re-run when the backend is reachable"),
            "unexpected error message: {msg}"
        );
    }
}
