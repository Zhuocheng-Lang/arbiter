use std::path::Path;

// ── ScxScheduler ──────────────────────────────────────────────────────────────

/// The currently active sched-ext scheduler, or `None` if CFS is running.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScxScheduler {
    Lavd,
    Layered,
    Rusty,
    Bpfland,
    Unknown(String),
    /// No scx scheduler active (CFS / other).
    None,
}

impl std::fmt::Display for ScxScheduler {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Lavd => write!(f, "scx_lavd"),
            Self::Layered => write!(f, "scx_layered"),
            Self::Rusty => write!(f, "scx_rusty"),
            Self::Bpfland => write!(f, "scx_bpfland"),
            Self::Unknown(s) => write!(f, "{s}"),
            Self::None => write!(f, "none (CFS)"),
        }
    }
}

// ── Strategy ──────────────────────────────────────────────────────────────────

/// How arbiter translates rules into kernel hints for the active scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// nice + cgroup.weight — respected by lavd, rusty, bpfland, and most
    /// scx schedulers that honour UNIX priority.
    NiceAndWeight,

    /// Generate a scx_layered layer-spec JSON file so that layered can
    /// automatically assign processes to layers via regex/comm rules.
    LayeredJson,

    /// Fallback on a plain CFS system: nice / ionice / oom_score_adj only.
    BasicHints,
}

impl ScxScheduler {
    pub fn strategy(&self) -> Strategy {
        match self {
            Self::Lavd | Self::Rusty | Self::Bpfland | Self::Unknown(_) => Strategy::NiceAndWeight,
            Self::Layered => Strategy::LayeredJson,
            Self::None => Strategy::BasicHints,
        }
    }
}

// ── Detection ─────────────────────────────────────────────────────────────────

/// Probe sysfs to identify the running scx scheduler (if any).
///
/// Reads:
///   `/sys/kernel/sched_ext/state`        → "enabled" or "disabled"
///   `/sys/kernel/sched_ext/root/ops`     → scheduler name, e.g. "scx_lavd"
pub fn detect() -> ScxScheduler {
    let state_path = Path::new("/sys/kernel/sched_ext/state");
    if !state_path.exists() {
        return ScxScheduler::None;
    }

    let state = match std::fs::read_to_string(state_path) {
        Ok(s) => s.trim().to_string(),
        Err(_) => return ScxScheduler::None,
    };

    if state != "enabled" {
        return ScxScheduler::None;
    }

    let ops_path = Path::new("/sys/kernel/sched_ext/root/ops");
    let name = match std::fs::read_to_string(ops_path) {
        Ok(s) => s.trim().to_string(),
        Err(e) => {
            tracing::warn!("Could not read sched_ext ops: {e}");
            return ScxScheduler::Unknown("unknown".to_string());
        }
    };

    // The ops name reported by the kernel may include a version suffix,
    // e.g. "lavd_1.0.21_g7298f797_x86_64_unknown_linux_gnu".
    // Match by prefix rather than exact name.
    let name_lc = name.to_lowercase();
    if name_lc.starts_with("scx_lavd") || name_lc.starts_with("lavd") {
        ScxScheduler::Lavd
    } else if name_lc.starts_with("scx_layered") || name_lc.starts_with("layered") {
        ScxScheduler::Layered
    } else if name_lc.starts_with("scx_rusty") || name_lc.starts_with("rusty") {
        ScxScheduler::Rusty
    } else if name_lc.starts_with("scx_bpfland") || name_lc.starts_with("bpfland") {
        ScxScheduler::Bpfland
    } else {
        ScxScheduler::Unknown(name)
    }
}
