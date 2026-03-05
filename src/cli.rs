use clap::{Parser, Subcommand};
use std::path::PathBuf;

// ── Top-level CLI ─────────────────────────────────────────────────────────────

#[derive(Parser, Debug)]
#[command(
    name = "arbiter",
    version,
    about = "scx-aware userspace process classifier daemon"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,

    /// Path to config file (default: /etc/arbiter/config.toml or
    /// $XDG_CONFIG_HOME/arbiter/config.toml).
    #[arg(long, global = true)]
    pub config: Option<PathBuf>,

    /// Override log level (trace / debug / info / warn / error).
    #[arg(long, global = true)]
    pub log_level: Option<String>,
}

// ── Sub-commands ──────────────────────────────────────────────────────────────

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Start the daemon (listens for exec events and applies rules).
    Daemon {
        /// Log actions only; do not write to /proc or cgroups.
        #[arg(long)]
        dry_run: bool,
    },

    /// Manage active profile.
    Profile {
        #[command(subcommand)]
        action: ProfileAction,
    },

    /// Show why a process would (or wouldn't) be matched.
    Explain {
        /// PID number or process name (`comm`).
        target: String,
    },

    /// Print current daemon status (scheduler, profile, rule count).
    Status,

    /// Validate rule and type files; exit non-zero on error.
    Check {
        /// Directory to check (default: dirs from config).
        path: Option<PathBuf>,
    },
}

#[derive(Subcommand, Debug)]
pub enum ProfileAction {
    /// List available profiles.
    List,
    /// Print the currently configured profile.
    Get,
    /// Update the active profile (requires daemon restart; SIGHUP only reloads rules).
    Set {
        /// Profile name: default | gaming | lowpower | server
        name: String,
    },
}
