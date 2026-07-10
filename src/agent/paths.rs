//! Resolve paths for the agent's socket and PID file.
//!
//! Socket and PID locations come from [`crate::user_paths::UserPaths`]
//! (XDG runtime + state). This module keeps the agent-facing helpers
//! (read/write PID, cleanup) in one place.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::Result;
use crate::user_paths::{self, UserPaths};

/// Resolved paths for agent socket and PID file.
#[derive(Debug, Clone)]
pub struct AgentPaths {
    /// Unix socket path (`$XDG_RUNTIME_DIR/blu/agent.sock`, or state fallback).
    pub socket: PathBuf,
    /// PID file path (`$XDG_STATE_HOME/blu/agent.pid`).
    pub pid_file: PathBuf,
}

impl AgentPaths {
    /// Resolve agent paths from the process XDG environment.
    pub fn resolve() -> Result<Self> {
        Ok(Self::from_user_paths(&UserPaths::resolve()?))
    }

    /// Build agent paths from a fully resolved [`UserPaths`].
    pub fn from_user_paths(paths: &UserPaths) -> Self {
        Self {
            socket: paths.agent_socket.clone(),
            pid_file: paths.agent_pid.clone(),
        }
    }

    /// Resolve agent paths under a temporary base (tests).
    ///
    /// Builds XDG-style subdirs under `base` (`config`, `data`, `state`,
    /// `runtime`) so socket and PID land in separate dirs, matching
    /// production layout. Does not create directories.
    pub fn from_base(base: &Path) -> Self {
        let paths = UserPaths::from_bases(
            &base.join("config"),
            &base.join("data"),
            &base.join("state"),
            &base.join("runtime"),
            true,
        );
        Self::from_user_paths(&paths)
    }

    /// Check whether the agent socket file exists on disk.
    pub fn socket_exists(&self) -> bool {
        self.socket.exists()
    }

    /// Read the PID from the PID file, if it exists and contains a
    /// valid integer.
    pub fn read_pid(&self) -> Option<u32> {
        fs::read_to_string(&self.pid_file)
            .ok()
            .and_then(|s| s.trim().parse::<u32>().ok())
    }

    /// Write a PID to the PID file, creating the parent directory if needed.
    pub fn write_pid(&self, pid: u32) -> Result<()> {
        user_paths::ensure_parent(&self.pid_file)?;
        fs::write(&self.pid_file, pid.to_string())?;
        Ok(())
    }

    /// Remove the socket and PID file (cleanup on shutdown).
    pub fn cleanup(&self) {
        let _ = fs::remove_file(&self.socket);
        let _ = fs::remove_file(&self.pid_file);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::tempdir;

    #[test]
    fn resolve_layout() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path());
        assert_eq!(paths.socket, tmp.path().join("runtime/agent.sock"));
        assert_eq!(paths.pid_file, tmp.path().join("state/agent.pid"));
        assert!(!paths.socket.parent().unwrap().exists());
        assert!(!paths.pid_file.parent().unwrap().exists());
    }

    #[test]
    fn pid_round_trip() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path());
        paths.write_pid(12345).unwrap();
        assert_eq!(paths.read_pid(), Some(12345));
        let mode = fs::metadata(paths.pid_file.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o700);
    }

    #[test]
    fn read_pid_missing_file() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path());
        assert_eq!(paths.read_pid(), None);
    }

    #[test]
    fn cleanup_removes_files() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path());
        paths.write_pid(1).unwrap();
        user_paths::ensure_parent(&paths.socket).unwrap();
        fs::write(&paths.socket, "placeholder").unwrap();
        paths.cleanup();
        assert!(!paths.socket.exists());
        assert!(!paths.pid_file.exists());
    }

    #[test]
    fn from_user_paths_maps_socket_and_pid() {
        let tmp = tempdir().unwrap();
        let up = UserPaths::from_bases(
            &tmp.path().join("c"),
            &tmp.path().join("d"),
            &tmp.path().join("s"),
            &tmp.path().join("r"),
            true,
        );
        let paths = AgentPaths::from_user_paths(&up);
        assert_eq!(paths.socket, up.agent_socket);
        assert_eq!(paths.pid_file, up.agent_pid);
    }
}
