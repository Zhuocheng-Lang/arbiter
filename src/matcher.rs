use crate::rules::ResolvedRule;
use anyhow::Result;

/// Maximum length of a process `comm` string; determined by the kernel's
/// TASK_COMM_LEN constant (16), which includes the NUL terminator,
/// so the usable maximum is 15 characters.
const MAX_COMM_LEN: usize = 15;

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
                // comm is truncated by the kernel at MAX_COMM_LEN chars (TASK_COMM_LEN - 1).
                // When the comm is at the boundary the executable's real name was likely longer;
                // check that the rule name *starts with* the truncated comm, not the other way round.
                || (ctx.comm.len() >= MAX_COMM_LEN && name_lc.starts_with(&comm_lc));

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

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::ResolvedRule;

    fn ctx(comm: &str, exe: Option<&str>) -> ProcessContext {
        ProcessContext {
            pid: 1,
            ppid: 0,
            start_time_ticks: 0,
            comm: comm.to_string(),
            exe: exe.map(|s| s.to_string()),
            cmdline: None,
        }
    }

    fn rule(name: &str) -> ResolvedRule {
        ResolvedRule {
            name: name.to_string(),
            nice: Some(0),
            ioclass: None,
            ionice: None,
            oom_score_adj: None,
            cgroup: None,
            cgroup_weight: None,
            exe_pattern: None,
            cmdline_contains: None,
        }
    }

    fn matcher(rules: Vec<ResolvedRule>) -> Matcher {
        Matcher::new(rules)
    }

    /// Exact comm match (no truncation involved).
    #[test]
    fn exact_comm_match() {
        let m = matcher(vec![rule("bash")]);
        assert!(m.find_match(&ctx("bash", None)).is_some());
        assert!(m.find_match(&ctx("dash", None)).is_none());
    }

    /// Rule name is longer than MAX_COMM_LEN (15 chars).
    /// The kernel truncates comm, so the rule name should *start with* the stored comm.
    #[test]
    fn truncated_comm_prefix_match() {
        // "firefox-esr-binary" (18 chars) → kernel truncates to "firefox-esr-binar" (15 chars)
        let long_name = "firefox-esr-binary";
        let truncated_comm = &long_name[..MAX_COMM_LEN]; // "firefox-esr-bina" — 15 chars
        assert_eq!(truncated_comm.len(), MAX_COMM_LEN);

        let m = matcher(vec![rule(long_name)]);
        assert!(
            m.find_match(&ctx(truncated_comm, None)).is_some(),
            "rule with long name should match truncated comm via prefix check"
        );
    }

    /// A short comm that coincidentally starts with the rule name should NOT
    /// match via the prefix path (prefix matching only activates at boundary).
    #[test]
    fn short_comm_no_false_prefix_match() {
        let m = matcher(vec![rule("firefox-esr-binary")]);
        // comm "fire" is 4 chars (below MAX_COMM_LEN): should NOT match
        assert!(
            m.find_match(&ctx("fire", None)).is_none(),
            "prefix match must not activate for comm shorter than MAX_COMM_LEN"
        );
    }

    /// Matching is case-insensitive for both comm and exe basename.
    #[test]
    fn case_insensitive_match() {
        let m = matcher(vec![rule("Firefox")]);
        assert!(m.find_match(&ctx("firefox", None)).is_some());
        assert!(m.find_match(&ctx("FIREFOX", None)).is_some());
    }

    /// exe basename match when comm does not match.
    #[test]
    fn exe_basename_match() {
        let m = matcher(vec![rule("steam")]);
        assert!(m.find_match(&ctx("steam-runtime", Some("/usr/bin/steam"))).is_some());
        assert!(m.find_match(&ctx("unrelated", Some("/usr/bin/steam"))).is_some());
        assert!(m.find_match(&ctx("unrelated", Some("/usr/bin/other"))).is_none());
    }

    /// The old (wrong) direction was `comm_lc.starts_with(name_lc)` — verify it is gone.
    /// Rule "ab" should NOT match a process whose comm is "abcdefghijklmno" (15 chars)
    /// because "ab" does not start with "abcdefghijklmno".
    #[test]
    fn short_rule_name_does_not_match_long_comm() {
        let long_comm = "abcdefghijklmno"; // exactly 15 chars
        assert_eq!(long_comm.len(), MAX_COMM_LEN);
        let m = matcher(vec![rule("ab")]);
        assert!(
            m.find_match(&ctx(long_comm, None)).is_none(),
            "short rule 'ab' must not match long comm via reversed prefix"
        );
    }
}
