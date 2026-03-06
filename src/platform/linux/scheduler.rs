use std::path::Path;

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
            Self::Unknown(name) => write!(f, "{name}"),
            Self::None => write!(f, "none (CFS)"),
        }
    }
}

/// How arbiter translates rules into kernel hints for the active scheduler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// nice + cgroup.weight - respected by lavd, rusty, bpfland, and most
    /// scx schedulers that honour UNIX priority.
    NiceAndWeight,
    /// Generate a scx_layered layer-spec JSON file so layered can
    /// automatically assign processes to layers via regex or comm rules.
    LayeredJson,
    /// Fallback on a plain CFS system: nice, ionice, and oom_score_adj only.
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

/// Probe sysfs to identify the running scx scheduler, if any.
pub fn detect() -> ScxScheduler {
    let state_path = Path::new("/sys/kernel/sched_ext/state");
    if !state_path.exists() {
        return ScxScheduler::None;
    }

    let state = match std::fs::read_to_string(state_path) {
        Ok(state) => state.trim().to_string(),
        Err(_) => return ScxScheduler::None,
    };

    if state != "enabled" {
        return ScxScheduler::None;
    }

    let ops_path = Path::new("/sys/kernel/sched_ext/root/ops");
    let name = match std::fs::read_to_string(ops_path) {
        Ok(name) => name.trim().to_string(),
        Err(err) => {
            tracing::warn!("Could not read sched_ext ops: {err}");
            return ScxScheduler::Unknown("unknown".to_string());
        }
    };

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
