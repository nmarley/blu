//! Environment-variable passphrase resolution for headless automation.
//!
//! Interactive TTY prompts remain the default everywhere. These resolvers give
//! scripts and CI a documented machine-supplied path, following the pattern of
//! restic (`RESTIC_PASSWORD`) and borg (`BORG_PASSPHRASE`).
//!
//! Semantics for both resolvers:
//!
//! - Unset: `None`, so the caller falls through to the next method in the
//!   precedence chain (ultimately the interactive prompt).
//! - Set but empty: `Some("")`, a valid empty passphrase equivalent to
//!   `--no-passphrase`.
//! - Non-UTF-8 value: treated as unset. Passphrases are always valid UTF-8 in
//!   practice, and `std::env::var` cannot represent raw bytes.

use zeroize::Zeroizing;

/// Environment variable holding the passphrase for the encrypted global identity file.
pub const PASSPHRASE_ENV_VAR: &str = "BLU_PASSPHRASE";

/// Environment variable holding the optional BIP39 "25th word" for identity init/recover.
pub const MNEMONIC_PASSPHRASE_ENV_VAR: &str = "BLU_MNEMONIC_PASSPHRASE";

/// Read the identity-file passphrase from `BLU_PASSPHRASE`.
///
/// Returns `None` when the variable is unset (or not valid UTF-8) so callers
/// fall through to the next unlock method. An explicitly empty value is
/// returned as `Some("")`, a valid empty passphrase.
pub fn passphrase_from_env() -> Option<Zeroizing<String>> {
    from_env(PASSPHRASE_ENV_VAR)
}

/// Read the BIP39 mnemonic passphrase ("25th word") from `BLU_MNEMONIC_PASSPHRASE`.
///
/// Same semantics as [`passphrase_from_env`].
pub fn mnemonic_passphrase_from_env() -> Option<Zeroizing<String>> {
    from_env(MNEMONIC_PASSPHRASE_ENV_VAR)
}

fn from_env(key: &str) -> Option<Zeroizing<String>> {
    std::env::var(key).ok().map(Zeroizing::new)
}

#[cfg(test)]
mod test {
    use super::*;
    use std::sync::Mutex;

    // Env vars are process-global; serialize access so these tests cannot race
    // each other on parallel test threads.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// Run `f` with `value` installed for `key`, restoring the original value
    /// afterwards so a developer's own environment cannot leak between tests.
    fn with_env(key: &str, value: Option<&str>, f: impl FnOnce()) {
        // Ignore poisoning so one failing assertion does not cascade.
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let original = std::env::var(key).ok();
        match value {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
        f();
        match original {
            Some(v) => std::env::set_var(key, v),
            None => std::env::remove_var(key),
        }
    }

    #[test]
    fn passphrase_unset_returns_none() {
        with_env(PASSPHRASE_ENV_VAR, None, || {
            assert!(passphrase_from_env().is_none());
        });
    }

    #[test]
    fn passphrase_set_returns_value() {
        with_env(PASSPHRASE_ENV_VAR, Some("open-sesame"), || {
            let pass = passphrase_from_env().expect("env var is set");
            assert_eq!(pass.as_str(), "open-sesame");
        });
    }

    #[test]
    fn passphrase_empty_returns_empty() {
        with_env(PASSPHRASE_ENV_VAR, Some(""), || {
            let pass = passphrase_from_env().expect("env var is set");
            assert_eq!(pass.as_str(), "");
        });
    }

    #[test]
    fn mnemonic_passphrase_unset_returns_none() {
        with_env(MNEMONIC_PASSPHRASE_ENV_VAR, None, || {
            assert!(mnemonic_passphrase_from_env().is_none());
        });
    }

    #[test]
    fn mnemonic_passphrase_set_returns_value() {
        with_env(MNEMONIC_PASSPHRASE_ENV_VAR, Some("entropy-25"), || {
            let pass = mnemonic_passphrase_from_env().expect("env var is set");
            assert_eq!(pass.as_str(), "entropy-25");
        });
    }

    #[test]
    fn mnemonic_passphrase_empty_returns_empty() {
        with_env(MNEMONIC_PASSPHRASE_ENV_VAR, Some(""), || {
            let pass = mnemonic_passphrase_from_env().expect("env var is set");
            assert_eq!(pass.as_str(), "");
        });
    }
}
