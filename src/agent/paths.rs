//! Resolve paths for the agent's socket and PID file.
//!
//! All agent state lives under `~/.blu/`. This module provides a
//! single struct that resolves and exposes those paths.

use std::fs;
use std::path::{Path, PathBuf};

use crate::error::{BluError, Result};

const BLU_USER_DIR: &str = ".blu";
const SOCKET_FILENAME: &str = "agent.sock";
const PID_FILENAME: &str = "agent.pid";

/// Resolved paths for agent socket and PID file.
#[derive(Debug, Clone)]
pub struct AgentPaths {
    /// Directory containing agent files (`~/.blu/`).
    pub dir: PathBuf,
    /// Unix socket path (`~/.blu/agent.sock`).
    pub socket: PathBuf,
    /// PID file path (`~/.blu/agent.pid`).
    pub pid_file: PathBuf,
}

impl AgentPaths {
    /// Resolve agent paths under the user's home directory.
    ///
    /// Creates `~/.blu/` if it does not exist.
    pub fn resolve() -> Result<Self> {
        let home = dirs::home_dir()
            .ok_or_else(|| BluError::Internal("could not determine home directory".into()))?;
        Self::from_base(&home)
    }

    /// Resolve agent paths under a specified base directory.
    ///
    /// Useful for testing. Creates the directory if it does not exist.
    pub fn from_base(base: &Path) -> Result<Self> {
        let dir = base.join(BLU_USER_DIR);
        fs::create_dir_all(&dir)?;
        Ok(Self {
            socket: dir.join(SOCKET_FILENAME),
            pid_file: dir.join(PID_FILENAME),
            dir,
        })
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

    /// Write a PID to the PID file.
    pub fn write_pid(&self, pid: u32) -> Result<()> {
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
    use tempfile::tempdir;

    #[test]
    fn resolve_creates_dir() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        assert!(paths.dir.exists());
        assert_eq!(paths.socket, tmp.path().join(".blu/agent.sock"));
        assert_eq!(paths.pid_file, tmp.path().join(".blu/agent.pid"));
    }

    #[test]
    fn pid_round_trip() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        paths.write_pid(12345).unwrap();
        assert_eq!(paths.read_pid(), Some(12345));
    }

    #[test]
    fn read_pid_missing_file() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        assert_eq!(paths.read_pid(), None);
    }

    #[test]
    fn cleanup_removes_files() {
        let tmp = tempdir().unwrap();
        let paths = AgentPaths::from_base(tmp.path()).unwrap();
        paths.write_pid(1).unwrap();
        fs::write(&paths.socket, "placeholder").unwrap();
        paths.cleanup();
        assert!(!paths.socket.exists());
        assert!(!paths.pid_file.exists());
    }
}
