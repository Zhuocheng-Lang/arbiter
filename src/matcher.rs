use crate::rules::ResolvedRule;
use anyhow::Result;

// ── ProcessContext ────────────────────────────────────────────────────────────

/// Snapshot of a running process used for rule matching.
#[derive(Debug, Clone)]
pub struct ProcessContext {
    pub pid: u32,
    pub ppid: u32,
    /// Monotonic kernel start time from /proc/PID/stat, used to detect PID reuse.
    pub start_time_ticks: u64,
    /// `comm` from /proc/PID/stat  (kernel-truncated to 15 chars).
    pub comm: String,
    /// Resolved exe path from /proc/PID/exe.
    pub exe: Option<String>,
    /// Space-joined argv from /proc/PID/cmdline.
    pub cmdline: Option<String>,
}

impl ProcessContext {
    /// Build a context by reading /proc/<pid>. Returns Err if the process
    /// has already exited or lacks permissions.
    pub fn from_pid(pid: u32) -> Result<Self> {
        let proc = procfs::process::Process::new(pid as i32)?;
        let stat = proc.stat()?;
        let exe = proc.exe().ok().map(|p| p.to_string_lossy().into_owned());
        let cmdline = proc
            .cmdline()
            .ok()
            .filter(|v| !v.is_empty())
            .map(|v| v.join(" "));

        Ok(ProcessContext {
            pid,
            ppid: stat.ppid as u32,
            start_time_ticks: stat.starttime,
            comm: stat.comm,
            exe,
            cmdline,
        })
    }

    /// Best-effort PID reuse check before applying scheduler hints.
    /// Returns `Ok(false)` when the process has already exited — that is the
    /// normal TOCTOU race and must not propagate as an error.
    pub fn matches_current_pid(&self) -> Result<bool> {
        let proc = match procfs::process::Process::new(self.pid as i32) {
            Ok(p) => p,
            Err(_) => return Ok(false), // process exited between exec event and apply
        };
        let stat = match proc.stat() {
            Ok(s) => s,
            Err(_) => return Ok(false),
        };
        Ok(stat.starttime == self.start_time_ticks)
    }

    /// Basename of the exe path (useful when comm is truncated).
    pub fn exe_name(&self) -> Option<&str> {
        self.exe.as_deref()?.rsplit('/').next()
    }
}

// ── Matcher ───────────────────────────────────────────────────────────────────

pub struct Matcher {
    rules: Vec<ResolvedRule>,
}

impl Matcher {
    pub fn new(rules: Vec<ResolvedRule>) -> Self {
        Self { rules }
    }

    /// Return the first rule that matches `ctx`, or `None`.
    pub fn find_match<'a>(&'a self, ctx: &ProcessContext) -> Option<&'a ResolvedRule> {
        self.rules.iter().find(|r| self.rule_matches(r, ctx))
    }

    /// Describe the matching process for debugging.
    pub fn explain(&self, ctx: &ProcessContext) -> ExplainResult {
        let mut attempts: Vec<(String, bool)> = Vec::new();
        let mut matched: Option<ResolvedRule> = None;
        for rule in &self.rules {
            let hit = self.rule_matches(rule, ctx);
            attempts.push((rule.name.clone(), hit));
            if hit && matched.is_none() {
                matched = Some(rule.clone());
            }
        }
        ExplainResult { matched, attempts }
    }

    fn rule_matches(&self, rule: &ResolvedRule, ctx: &ProcessContext) -> bool {
        // ── name check (comm OR exe basename, case-insensitive) ────────────
        if !rule.name.is_empty() {
            let name_lc = rule.name.to_lowercase();
            let comm_lc = ctx.comm.to_lowercase();

            let comm_matches = comm_lc == name_lc
                // comm is truncated by the kernel at 15 chars (TASK_COMM_LEN - 1).
                // Only allow prefix matching when the recorded comm is at that boundary,
                // which indicates the original name was likely longer.
                || (ctx.comm.len() >= 15 && comm_lc.starts_with(&name_lc));

            let exe_matches = ctx
                .exe_name()
                .map(|e| e.to_lowercase() == name_lc)
                .unwrap_or(false);

            if !comm_matches && !exe_matches {
                return false;
            }
        }

        // ── optional exe path regex ────────────────────────────────────────
        if let Some(pat) = &rule.exe_pattern {
            match ctx.exe.as_deref() {
                Some(exe) if pat.is_match(exe) => {}
                _ => return false,
            }
        }

        // ── optional cmdline substring ─────────────────────────────────────
        if let Some(needle) = &rule.cmdline_contains {
            match ctx.cmdline.as_deref() {
                Some(cl) if cl.contains(needle.as_str()) => {}
                _ => return false,
            }
        }

        true
    }
}

// ── ExplainResult ─────────────────────────────────────────────────────────────

pub struct ExplainResult {
    /// The first matching rule, if any.
    pub matched: Option<ResolvedRule>,
    /// `(rule_name, did_match)` for every rule checked.
    pub attempts: Vec<(String, bool)>,
}
