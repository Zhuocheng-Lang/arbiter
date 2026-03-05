use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::str::FromStr;

// ── Profile ──────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum ProfileKind {
    #[default]
    Default,
    Gaming,
    LowPower,
    Server,
}

impl std::fmt::Display for ProfileKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Default => write!(f, "default"),
            Self::Gaming => write!(f, "gaming"),
            Self::LowPower => write!(f, "lowpower"),
            Self::Server => write!(f, "server"),
        }
    }
}

impl FromStr for ProfileKind {
    type Err = anyhow::Error;
    fn from_str(s: &str) -> Result<Self> {
        match s {
            "default" => Ok(Self::Default),
            "gaming" => Ok(Self::Gaming),
            "lowpower" | "low-power" => Ok(Self::LowPower),
            "server" => Ok(Self::Server),
            _ => Err(anyhow::anyhow!(
                "Unknown profile '{}'. Valid: default, gaming, lowpower, server",
                s
            )),
        }
    }
}

// ── Main config ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Active profile.
    #[serde(default)]
    pub profile: ProfileKind,

    /// Directories scanned for *.types and *.rules files.
    #[serde(default = "default_rules_dirs")]
    pub rules_dirs: Vec<PathBuf>,

    /// Minimum log level (trace / debug / info / warn / error).
    #[serde(default = "default_log_level")]
    pub log_level: String,

    /// Log actions but do not apply them.
    #[serde(default)]
    pub dry_run: bool,

    /// Apply nice values.
    #[serde(default = "yes")]
    pub apply_nice: bool,

    /// Apply ionice class/level.
    #[serde(default = "yes")]
    pub apply_ionice: bool,

    /// Write /proc/PID/oom_score_adj.
    #[serde(default = "yes")]
    pub apply_oom: bool,

    /// Move processes to cgroup slices.
    #[serde(default = "yes")]
    pub apply_cgroup: bool,

    /// If set, arbiter writes a scx_layered-compatible layer JSON here.
    pub layered_export_path: Option<PathBuf>,

    /// Milliseconds to wait after an exec event before reading /proc.
    /// Allows the process to finish its execve before we inspect it.
    #[serde(default = "default_exec_delay_ms")]
    pub exec_delay_ms: u64,
}

fn xdg_config_home() -> PathBuf {
    std::env::var("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/root".to_string());
            PathBuf::from(home).join(".config")
        })
}

fn default_rules_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/etc/arbiter/rules.d"),
        xdg_config_home().join("arbiter/rules.d"),
    ]
}

fn default_log_level() -> String {
    "info".to_string()
}
fn default_exec_delay_ms() -> u64 {
    50
}
fn yes() -> bool {
    true
}

impl Default for Config {
    fn default() -> Self {
        Config {
            profile: ProfileKind::Default,
            rules_dirs: default_rules_dirs(),
            log_level: default_log_level(),
            dry_run: false,
            apply_nice: true,
            apply_ionice: true,
            apply_oom: true,
            apply_cgroup: true,
            layered_export_path: None,
            exec_delay_ms: default_exec_delay_ms(),
        }
    }
}

impl Config {
    /// Load from the first existing config file; fall back to defaults.
    pub fn load() -> Result<Self> {
        let candidates = [
            PathBuf::from("/etc/arbiter/config.toml"),
            xdg_config_home().join("arbiter/config.toml"),
        ];
        for path in &candidates {
            if path.exists() {
                tracing::info!("Loading config from {}", path.display());
                return Self::load_from(path);
            }
        }
        tracing::info!("No config file found, using defaults");
        Ok(Self::default())
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config: {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse config: {}", path.display()))
    }
}
