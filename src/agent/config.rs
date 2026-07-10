//! Agent configuration: timeout profiles and user preferences.
//!
//! The agent reads its configuration from
//! `$XDG_CONFIG_HOME/blu/config.toml`. If the file does not exist,
//! sensible defaults are used (the "balanced" profile).
//!
//! ```toml
//! [agent]
//! profile = "balanced"        # "paranoid", "balanced", "relaxed", "custom"
//! timeout_idle = "1h"         # used when profile = "custom"
//! timeout_max = "8h"          # used when profile = "custom"
//! auto_start = true           # auto-start agent when a command needs keys
//! ```

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::{BluError, Result};
use crate::user_paths::UserPaths;

/// Top-level structure for `$XDG_CONFIG_HOME/blu/config.toml`.
#[derive(Debug, Deserialize, Serialize)]
struct ConfigFile {
    #[serde(default)]
    agent: AgentSection,
}

/// The `[agent]` section in the config file.
#[derive(Debug, Deserialize, Serialize)]
struct AgentSection {
    #[serde(default = "default_profile_name")]
    profile: String,
    #[serde(default)]
    timeout_idle: Option<String>,
    #[serde(default)]
    timeout_max: Option<String>,
    #[serde(default = "default_auto_start")]
    auto_start: bool,
}

fn default_profile_name() -> String {
    "balanced".to_string()
}

fn default_auto_start() -> bool {
    true
}

impl Default for AgentSection {
    fn default() -> Self {
        Self {
            profile: default_profile_name(),
            timeout_idle: None,
            timeout_max: None,
            auto_start: default_auto_start(),
        }
    }
}

/// A named timeout profile.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Profile {
    Paranoid,
    Balanced,
    Relaxed,
    Custom,
}

impl Profile {
    fn from_str(s: &str) -> Option<Profile> {
        match s {
            "paranoid" => Some(Profile::Paranoid),
            "balanced" => Some(Profile::Balanced),
            "relaxed" => Some(Profile::Relaxed),
            "custom" => Some(Profile::Custom),
            _ => None,
        }
    }
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Profile::Paranoid => write!(f, "paranoid"),
            Profile::Balanced => write!(f, "balanced"),
            Profile::Relaxed => write!(f, "relaxed"),
            Profile::Custom => write!(f, "custom"),
        }
    }
}

/// Resolved agent configuration with concrete durations.
#[derive(Debug, Clone)]
pub struct AgentConfig {
    /// The selected profile name.
    pub profile: Profile,
    /// Lock after this much idle time (no RPC activity).
    pub timeout_idle: Duration,
    /// Lock unconditionally after this much time since unlock.
    pub timeout_max: Duration,
    /// Whether to auto-start the agent when a command needs keys.
    pub auto_start: bool,
}

impl Default for AgentConfig {
    fn default() -> Self {
        Self::for_profile(Profile::Balanced)
    }
}

impl AgentConfig {
    /// Build a config for a named profile with its default durations.
    pub fn for_profile(profile: Profile) -> Self {
        let (idle, max) = match profile {
            Profile::Paranoid => (Duration::from_secs(5 * 60), Duration::from_secs(60 * 60)),
            Profile::Balanced => (
                Duration::from_secs(60 * 60),
                Duration::from_secs(8 * 60 * 60),
            ),
            Profile::Relaxed => (
                Duration::from_secs(4 * 60 * 60),
                Duration::from_secs(12 * 60 * 60),
            ),
            Profile::Custom => {
                // Custom with no overrides falls back to balanced values
                (
                    Duration::from_secs(60 * 60),
                    Duration::from_secs(8 * 60 * 60),
                )
            }
        };
        Self {
            profile,
            timeout_idle: idle,
            timeout_max: max,
            auto_start: true,
        }
    }

    /// Load the agent config from `$XDG_CONFIG_HOME/blu/config.toml`.
    ///
    /// If the file does not exist, returns the default (balanced) config.
    /// If the file exists but is malformed, returns an error.
    pub fn load() -> Result<Self> {
        let path = config_path()?;
        Self::load_from(&path)
    }

    /// Load from a specific path (for testing).
    pub fn load_from(path: &Path) -> Result<Self> {
        let content = match fs::read_to_string(path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return Ok(Self::default());
            }
            Err(e) => return Err(BluError::from(e)),
        };

        let file: ConfigFile = toml::from_str(&content)
            .map_err(|e| BluError::InvalidConfig(format!("{}: {}", path.display(), e)))?;

        Self::from_section(&file.agent)
    }

    fn from_section(section: &AgentSection) -> Result<Self> {
        let profile = Profile::from_str(&section.profile).ok_or_else(|| {
            BluError::InvalidConfig(format!(
                "unknown agent profile '{}' (expected paranoid, balanced, relaxed, or custom)",
                section.profile
            ))
        })?;

        let mut cfg = Self::for_profile(profile);
        cfg.auto_start = section.auto_start;

        // Custom overrides (only apply when profile is "custom", or
        // when explicit values are provided for any profile)
        if let Some(ref idle_str) = section.timeout_idle {
            cfg.timeout_idle = parse_duration(idle_str).ok_or_else(|| {
                BluError::InvalidConfig(format!("invalid timeout_idle: '{}'", idle_str))
            })?;
        }
        if let Some(ref max_str) = section.timeout_max {
            cfg.timeout_max = parse_duration(max_str).ok_or_else(|| {
                BluError::InvalidConfig(format!("invalid timeout_max: '{}'", max_str))
            })?;
        }

        Ok(cfg)
    }
}

/// Resolve the path to `$XDG_CONFIG_HOME/blu/config.toml`.
fn config_path() -> Result<PathBuf> {
    Ok(UserPaths::resolve()?.config_toml)
}

/// Parse a human-friendly duration string.
///
/// Supported formats:
/// - `"30s"` / `"30sec"` / `"30 seconds"`
/// - `"5m"` / `"5min"` / `"5 minutes"`
/// - `"1h"` / `"1hr"` / `"1 hour"`
///
/// Returns None if the string cannot be parsed.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Find where digits end and the unit begins
    let num_end = s
        .find(|c: char| !c.is_ascii_digit() && c != '.')
        .unwrap_or(s.len());
    let num_str = s[..num_end].trim();
    let unit_str = s[num_end..].trim().to_lowercase();

    let value: f64 = num_str.parse().ok()?;
    if value < 0.0 {
        return None;
    }

    let seconds = match unit_str.as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => value,
        "m" | "min" | "mins" | "minute" | "minutes" => value * 60.0,
        "h" | "hr" | "hrs" | "hour" | "hours" => value * 3600.0,
        _ => return None,
    };

    Some(Duration::from_secs_f64(seconds))
}

#[cfg(test)]
mod test {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn parse_duration_basic() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("4h"), Some(Duration::from_secs(14400)));
        assert_eq!(parse_duration("8h"), Some(Duration::from_secs(28800)));
        assert_eq!(parse_duration("12h"), Some(Duration::from_secs(43200)));
    }

    #[test]
    fn parse_duration_verbose() {
        assert_eq!(parse_duration("5 minutes"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("1 hour"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("30 seconds"), Some(Duration::from_secs(30)));
    }

    #[test]
    fn parse_duration_invalid() {
        assert_eq!(parse_duration(""), None);
        assert_eq!(parse_duration("abc"), None);
        assert_eq!(parse_duration("5d"), None);
    }

    #[test]
    fn default_config_is_balanced() {
        let cfg = AgentConfig::default();
        assert_eq!(cfg.profile, Profile::Balanced);
        assert_eq!(cfg.timeout_idle, Duration::from_secs(3600));
        assert_eq!(cfg.timeout_max, Duration::from_secs(28800));
        assert!(cfg.auto_start);
    }

    #[test]
    fn load_missing_file_returns_default() {
        let path = Path::new("/tmp/nonexistent-blu-config-test.toml");
        let cfg = AgentConfig::load_from(path).unwrap();
        assert_eq!(cfg.profile, Profile::Balanced);
    }

    #[test]
    fn load_paranoid_profile() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[agent]").unwrap();
        writeln!(f, "profile = \"paranoid\"").unwrap();

        let cfg = AgentConfig::load_from(f.path()).unwrap();
        assert_eq!(cfg.profile, Profile::Paranoid);
        assert_eq!(cfg.timeout_idle, Duration::from_secs(300));
        assert_eq!(cfg.timeout_max, Duration::from_secs(3600));
    }

    #[test]
    fn load_custom_overrides() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[agent]").unwrap();
        writeln!(f, "profile = \"custom\"").unwrap();
        writeln!(f, "timeout_idle = \"10m\"").unwrap();
        writeln!(f, "timeout_max = \"2h\"").unwrap();

        let cfg = AgentConfig::load_from(f.path()).unwrap();
        assert_eq!(cfg.profile, Profile::Custom);
        assert_eq!(cfg.timeout_idle, Duration::from_secs(600));
        assert_eq!(cfg.timeout_max, Duration::from_secs(7200));
    }

    #[test]
    fn load_override_named_profile() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[agent]").unwrap();
        writeln!(f, "profile = \"balanced\"").unwrap();
        writeln!(f, "timeout_idle = \"30m\"").unwrap();

        let cfg = AgentConfig::load_from(f.path()).unwrap();
        assert_eq!(cfg.profile, Profile::Balanced);
        assert_eq!(cfg.timeout_idle, Duration::from_secs(1800));
        // max stays at balanced default
        assert_eq!(cfg.timeout_max, Duration::from_secs(28800));
    }

    #[test]
    fn load_auto_start_false() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[agent]").unwrap();
        writeln!(f, "auto_start = false").unwrap();

        let cfg = AgentConfig::load_from(f.path()).unwrap();
        assert!(!cfg.auto_start);
    }

    #[test]
    fn load_invalid_profile_errors() {
        let mut f = NamedTempFile::new().unwrap();
        writeln!(f, "[agent]").unwrap();
        writeln!(f, "profile = \"turbo\"").unwrap();

        let result = AgentConfig::load_from(f.path());
        assert!(result.is_err());
    }

    #[test]
    fn load_empty_file_returns_default() {
        let f = NamedTempFile::new().unwrap();
        let cfg = AgentConfig::load_from(f.path()).unwrap();
        assert_eq!(cfg.profile, Profile::Balanced);
    }

    #[test]
    fn profile_display() {
        assert_eq!(Profile::Paranoid.to_string(), "paranoid");
        assert_eq!(Profile::Balanced.to_string(), "balanced");
        assert_eq!(Profile::Relaxed.to_string(), "relaxed");
        assert_eq!(Profile::Custom.to_string(), "custom");
    }
}
