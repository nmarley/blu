//! XDG Base Directory paths for user-global blu state.
//!
//! Vault-local state still lives under a project `.blu/` directory.
//! This module resolves only per-user (non-vault) locations.
//!
//! Layout (XDG on all Unix platforms, including macOS):
//!
//! | Base | Default | Contents |
//! |------|---------|----------|
//! | `$XDG_CONFIG_HOME/blu` | `~/.config/blu` | `config.toml` |
//! | `$XDG_DATA_HOME/blu` | `~/.local/share/blu` | identity files |
//! | `$XDG_STATE_HOME/blu` | `~/.local/state/blu` | `agent.pid` |
//! | `$XDG_RUNTIME_DIR/blu` | (see below) | `agent.sock` |
//!
//! When `$XDG_RUNTIME_DIR` is unset or not absolute, the agent socket
//! falls back to the state directory.
//!
//! Path resolution is pure math: it does not create directories.
//! Callers that write files must ensure parents exist via
//! [`ensure_private_dir`] or [`ensure_parent`] (mode `0o700`).

use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{BluError, Result};

const APP_DIR: &str = "blu";

const IDENTITY_AGE: &str = "identity.age";
const IDENTITY_ENC: &str = "identity.enc";
const IDENTITY_TOML: &str = "identity.toml";
const CONFIG_TOML: &str = "config.toml";
const SOCKET_FILENAME: &str = "agent.sock";
const PID_FILENAME: &str = "agent.pid";

/// Resolved XDG paths for user-global blu state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserPaths {
    /// `$XDG_CONFIG_HOME/blu`
    pub config_dir: PathBuf,
    /// `$XDG_DATA_HOME/blu`
    pub data_dir: PathBuf,
    /// `$XDG_STATE_HOME/blu`
    pub state_dir: PathBuf,
    /// `$XDG_RUNTIME_DIR/blu`, or state dir when runtime is unavailable
    pub runtime_dir: PathBuf,
    /// Whether `runtime_dir` came from `$XDG_RUNTIME_DIR` (vs fallback)
    pub runtime_is_xdg: bool,

    /// Agent config: `config_dir/config.toml`
    pub config_toml: PathBuf,
    /// PQ identity (secret): `data_dir/identity.age`
    pub identity_age: PathBuf,
    /// Biometric-wrapped seed: `data_dir/identity.enc`
    pub identity_enc: PathBuf,
    /// Public identity metadata: `data_dir/identity.toml`
    pub identity_toml: PathBuf,
    /// Agent Unix socket: `runtime_dir/agent.sock`
    pub agent_socket: PathBuf,
    /// Agent PID file: `state_dir/agent.pid`
    pub agent_pid: PathBuf,
}

impl UserPaths {
    /// Resolve paths from the process environment.
    ///
    /// Reads `$HOME` (via [`dirs::home_dir`]) and the standard `XDG_*`
    /// variables. Does not create directories; writers must call
    /// [`ensure_private_dir`] or [`ensure_parent`].
    pub fn resolve() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| BluError::Internal("could not determine home directory".into()))?;
        Ok(Self::resolve_with(&home, |key| env::var_os(key)))
    }

    /// Resolve paths under a fixed home directory and env lookup.
    ///
    /// Pure path math: does not create directories. Intended for tests:
    /// pass a temp home and a closure that supplies XDG variable values
    /// without mutating the process environment.
    pub fn resolve_with<F>(home: &Path, mut env_get: F) -> Self
    where
        F: FnMut(&str) -> Option<std::ffi::OsString>,
    {
        let config_home = xdg_base(&mut env_get, "XDG_CONFIG_HOME", home, ".config");
        let data_home = xdg_base(&mut env_get, "XDG_DATA_HOME", home, ".local/share");
        let state_home = xdg_base(&mut env_get, "XDG_STATE_HOME", home, ".local/state");

        let (runtime_home, runtime_is_xdg) = match absolute_env(&mut env_get, "XDG_RUNTIME_DIR") {
            Some(dir) => (dir, true),
            None => (state_home.clone(), false),
        };

        Self::from_bases(
            &config_home.join(APP_DIR),
            &data_home.join(APP_DIR),
            &state_home.join(APP_DIR),
            &runtime_home.join(APP_DIR),
            runtime_is_xdg,
        )
    }

    /// Build paths from explicit base directories.
    ///
    /// Pure path math: does not create directories.
    pub fn from_bases(
        config_dir: &Path,
        data_dir: &Path,
        state_dir: &Path,
        runtime_dir: &Path,
        runtime_is_xdg: bool,
    ) -> Self {
        Self {
            config_toml: config_dir.join(CONFIG_TOML),
            identity_age: data_dir.join(IDENTITY_AGE),
            identity_enc: data_dir.join(IDENTITY_ENC),
            identity_toml: data_dir.join(IDENTITY_TOML),
            agent_socket: runtime_dir.join(SOCKET_FILENAME),
            agent_pid: state_dir.join(PID_FILENAME),
            config_dir: config_dir.to_path_buf(),
            data_dir: data_dir.to_path_buf(),
            state_dir: state_dir.to_path_buf(),
            runtime_dir: runtime_dir.to_path_buf(),
            runtime_is_xdg,
        }
    }
}

/// Resolve an XDG base: absolute env value if set, else `$home/$default_rel`.
fn xdg_base<F>(env_get: &mut F, var: &str, home: &Path, default_rel: &str) -> PathBuf
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    absolute_env(env_get, var).unwrap_or_else(|| home.join(default_rel))
}

/// Return the env value only when it is non-empty and absolute.
fn absolute_env<F>(env_get: &mut F, var: &str) -> Option<PathBuf>
where
    F: FnMut(&str) -> Option<std::ffi::OsString>,
{
    env_get(var).map(PathBuf::from).filter(|p| p.is_absolute())
}

/// Create `dir` (and parents) and set mode `0o700` on Unix.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Ensure the parent directory of `path` exists with mode `0o700`.
///
/// No-op when `path` has no parent (e.g. a bare filename).
pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            ensure_private_dir(parent)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod test {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    fn env_map(pairs: &[(&str, &Path)]) -> HashMap<String, std::ffi::OsString> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.as_os_str().to_os_string()))
            .collect()
    }

    fn resolve(home: &Path, map: &HashMap<String, std::ffi::OsString>) -> UserPaths {
        UserPaths::resolve_with(home, |key| map.get(key).cloned())
    }

    #[test]
    fn defaults_under_home_when_xdg_unset() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let paths = resolve(home, &HashMap::new());

        assert_eq!(paths.config_dir, home.join(".config/blu"));
        assert_eq!(paths.data_dir, home.join(".local/share/blu"));
        assert_eq!(paths.state_dir, home.join(".local/state/blu"));
        assert_eq!(paths.runtime_dir, home.join(".local/state/blu"));
        assert!(!paths.runtime_is_xdg);

        assert_eq!(paths.config_toml, home.join(".config/blu/config.toml"));
        assert_eq!(
            paths.identity_age,
            home.join(".local/share/blu/identity.age")
        );
        assert_eq!(
            paths.identity_enc,
            home.join(".local/share/blu/identity.enc")
        );
        assert_eq!(
            paths.identity_toml,
            home.join(".local/share/blu/identity.toml")
        );
        assert_eq!(paths.agent_socket, home.join(".local/state/blu/agent.sock"));
        assert_eq!(paths.agent_pid, home.join(".local/state/blu/agent.pid"));
    }

    #[test]
    fn respects_absolute_xdg_env_vars() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let config = tmp.path().join("cfg");
        let data = tmp.path().join("data");
        let state = tmp.path().join("state");
        let runtime = tmp.path().join("run");

        let map = env_map(&[
            ("XDG_CONFIG_HOME", config.as_path()),
            ("XDG_DATA_HOME", data.as_path()),
            ("XDG_STATE_HOME", state.as_path()),
            ("XDG_RUNTIME_DIR", runtime.as_path()),
        ]);
        let paths = resolve(&home, &map);

        assert_eq!(paths.config_dir, config.join("blu"));
        assert_eq!(paths.data_dir, data.join("blu"));
        assert_eq!(paths.state_dir, state.join("blu"));
        assert_eq!(paths.runtime_dir, runtime.join("blu"));
        assert!(paths.runtime_is_xdg);
        assert_eq!(paths.agent_socket, runtime.join("blu/agent.sock"));
        assert_eq!(paths.agent_pid, state.join("blu/agent.pid"));
        assert_eq!(paths.identity_age, data.join("blu/identity.age"));
        assert_eq!(paths.config_toml, config.join("blu/config.toml"));
    }

    #[test]
    fn relative_xdg_env_is_ignored() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let mut map = HashMap::new();
        map.insert(
            "XDG_CONFIG_HOME".into(),
            std::ffi::OsString::from("relative/config"),
        );
        map.insert(
            "XDG_RUNTIME_DIR".into(),
            std::ffi::OsString::from("relative/run"),
        );

        let paths = resolve(home, &map);
        assert_eq!(paths.config_dir, home.join(".config/blu"));
        assert_eq!(paths.runtime_dir, home.join(".local/state/blu"));
        assert!(!paths.runtime_is_xdg);
    }

    #[test]
    fn empty_xdg_env_is_ignored() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let mut map = HashMap::new();
        map.insert("XDG_DATA_HOME".into(), std::ffi::OsString::from(""));
        map.insert("XDG_RUNTIME_DIR".into(), std::ffi::OsString::new());

        let paths = resolve(home, &map);
        assert_eq!(paths.data_dir, home.join(".local/share/blu"));
        assert!(!paths.runtime_is_xdg);
    }

    #[test]
    fn resolve_does_not_create_directories() {
        let tmp = tempdir().unwrap();
        let home = tmp.path();
        let paths = resolve(home, &HashMap::new());

        for dir in [
            &paths.config_dir,
            &paths.data_dir,
            &paths.state_dir,
            &paths.runtime_dir,
        ] {
            assert!(!dir.exists(), "should not create {}", dir.display());
        }
    }

    #[test]
    fn ensure_private_dir_sets_mode() {
        let tmp = tempdir().unwrap();
        let dir = tmp.path().join("nested/private");
        ensure_private_dir(&dir).unwrap();

        assert!(dir.is_dir());
        let mode = fs::metadata(&dir).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn ensure_parent_creates_file_parent() {
        let tmp = tempdir().unwrap();
        let file = tmp.path().join("a/b/c.dat");
        ensure_parent(&file).unwrap();

        let parent = file.parent().unwrap();
        assert!(parent.is_dir());
        let mode = fs::metadata(parent).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn runtime_dir_layout_when_xdg_runtime_set() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let runtime = tmp.path().join("run");

        let map = env_map(&[("XDG_RUNTIME_DIR", runtime.as_path())]);
        let paths = resolve(&home, &map);

        assert!(paths.runtime_is_xdg);
        assert_ne!(paths.runtime_dir, paths.state_dir);
        assert_eq!(paths.runtime_dir, runtime.join("blu"));
        assert_eq!(paths.state_dir, home.join(".local/state/blu"));
        assert!(!paths.runtime_dir.exists());
        assert!(!paths.state_dir.exists());
    }

    #[test]
    fn from_bases_does_not_require_home() {
        let tmp = tempdir().unwrap();
        let config = tmp.path().join("c");
        let data = tmp.path().join("d");
        let state = tmp.path().join("s");
        let runtime = tmp.path().join("r");

        let paths = UserPaths::from_bases(&config, &data, &state, &runtime, true);
        assert_eq!(paths.identity_enc, data.join(IDENTITY_ENC));
        assert_eq!(paths.agent_socket, runtime.join(SOCKET_FILENAME));
        assert!(paths.runtime_is_xdg);
        assert!(!config.exists());
        assert!(!data.exists());
    }

    #[test]
    fn partial_xdg_overrides_mix_with_defaults() {
        let tmp = tempdir().unwrap();
        let home = tmp.path().join("home");
        let data = tmp.path().join("custom-data");

        let map = env_map(&[("XDG_DATA_HOME", data.as_path())]);
        let paths = resolve(&home, &map);

        assert_eq!(paths.data_dir, data.join("blu"));
        assert_eq!(paths.config_dir, home.join(".config/blu"));
        assert_eq!(paths.state_dir, home.join(".local/state/blu"));
        assert_eq!(paths.identity_age, data.join("blu/identity.age"));
        assert_eq!(paths.config_toml, home.join(".config/blu/config.toml"));
    }
}
