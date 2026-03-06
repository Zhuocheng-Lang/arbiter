use std::collections::HashMap;

use anyhow::Result;

use super::ResolvedRule;

/// Maximum length of a process `comm` string; determined by the kernel's
/// TASK_COMM_LEN constant (16), which includes the NUL terminator,
/// so the usable maximum is 15 characters.
const MAX_COMM_LEN: usize = 15;

fn truncate_to_char_boundary(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }

    let mut end = max_bytes;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Snapshot of a running process used for rule matching.
#[derive(Debug, Clone)]
pub struct ProcessContext {
    pub pid: u32,
    pub ppid: u32,
    /// Monotonic kernel start time from /proc/PID/stat, used to detect PID reuse.
    pub start_time_ticks: u64,
    /// `comm` from /proc/PID/stat` (kernel-truncated to 15 chars).
    pub comm: String,
    /// Pre-computed lowercase of `comm`; avoids per-rule allocation in hot path.
    pub comm_lowercase: String,
    /// Resolved exe path from /proc/PID/exe.
    pub exe: Option<String>,
    /// Pre-computed lowercase of the exe basename; avoids per-rule allocation in hot path.
    pub exe_name_lowercase: Option<String>,
    /// Space-joined argv from /proc/PID/cmdline.
    pub cmdline: Option<String>,
}

impl ProcessContext {
    /// Build a context by reading /proc/<pid>. Returns Err if the process
    /// has already exited or lacks permissions.
    pub fn from_pid(pid: u32) -> Result<Self> {
        let proc = procfs::process::Process::new(pid as i32)?;
        let stat = proc.stat()?;
        let exe = proc
            .exe()
            .ok()
            .map(|path| path.to_string_lossy().into_owned());
        let cmdline = proc
            .cmdline()
            .ok()
            .filter(|args| !args.is_empty())
            .map(|args| args.join(" "));

        let comm_lowercase = stat.comm.to_lowercase();
        let exe_name_lowercase = exe
            .as_deref()
            .and_then(|path| path.rsplit('/').next())
            .map(|s| s.to_lowercase());

        Ok(ProcessContext {
            pid,
            ppid: stat.ppid as u32,
            start_time_ticks: stat.starttime,
            comm: stat.comm,
            comm_lowercase,
            exe,
            exe_name_lowercase,
            cmdline,
        })
    }

    /// Best-effort PID reuse check before applying scheduler hints.
    pub fn matches_current_pid(&self) -> Result<bool> {
        let proc = match procfs::process::Process::new(self.pid as i32) {
            Ok(proc) => proc,
            Err(_) => return Ok(false),
        };
        let stat = match proc.stat() {
            Ok(stat) => stat,
            Err(_) => return Ok(false),
        };
        Ok(stat.starttime == self.start_time_ticks)
    }

    /// Basename of the exe path (useful when comm is truncated).
    pub fn exe_name(&self) -> Option<&str> {
        self.exe.as_deref()?.rsplit('/').next()
    }
}

pub struct Matcher {
    rules: Vec<ResolvedRule>,
    /// Exact name lookup: `name_lowercase` → sorted rule indices.
    /// Covers the vast majority of named rules.
    name_index: HashMap<String, Vec<u32>>,
    /// Prefix lookup for truncated `comm` (kernel truncates to 15 chars):
    /// `name_lowercase[..MAX_COMM_LEN]` → sorted rule indices.
    /// Only populated for rules whose `name_lowercase.len() > MAX_COMM_LEN`.
    prefix_index: HashMap<String, Vec<u32>>,
    /// True if any rule has an empty name (must always be checked).
    has_nameless: bool,
}

impl Matcher {
    pub fn new(rules: Vec<ResolvedRule>) -> Self {
        let mut name_index: HashMap<String, Vec<u32>> = HashMap::with_capacity(rules.len());
        let mut prefix_index: HashMap<String, Vec<u32>> = HashMap::new();
        let mut has_nameless = false;

        for (i, rule) in rules.iter().enumerate() {
            if rule.name.is_empty() {
                has_nameless = true;
            } else {
                name_index
                    .entry(rule.name_lowercase.clone())
                    .or_default()
                    .push(i as u32);
                // Build prefix index for long names so truncated `comm` can still match.
                if rule.name_lowercase.len() > MAX_COMM_LEN {
                    let prefix = truncate_to_char_boundary(&rule.name_lowercase, MAX_COMM_LEN)
                        .to_string();
                    prefix_index.entry(prefix).or_default().push(i as u32);
                }
            }
        }

        Self {
            rules,
            name_index,
            prefix_index,
            has_nameless,
        }
    }

    /// Return the first rule that matches `ctx`, or `None`.
    ///
    /// Fast path: if neither `comm_lowercase` nor `exe_name_lowercase` appears
    /// in any rule's name (and there are no nameless rules), we return `None`
    /// immediately without iterating over the rule list at all.  This is the
    /// common case — most short-lived processes (`ls`, `sh`, ...) have no
    /// matching rule and would otherwise trigger an O(n) scan.
    pub fn find_match<'a>(&'a self, ctx: &ProcessContext) -> Option<&'a ResolvedRule> {
        let comm_lc = &ctx.comm_lowercase;

        // Check whether any named rule *could* match before starting the scan.
        let any_keyed_candidate = self.name_index.contains_key(comm_lc.as_str())
            || (ctx.comm.len() >= MAX_COMM_LEN
                && self.prefix_index.contains_key(comm_lc.as_str()))
            || ctx
                .exe_name_lowercase
                .as_deref()
                .map(|n| self.name_index.contains_key(n))
                .unwrap_or(false);

        if !any_keyed_candidate && !self.has_nameless {
            // Fast-reject: no rule in the set can possibly match this process.
            return None;
        }

        // Linear scan is kept for correctness (first-match-wins, prefix matching).
        self.rules.iter().find(|rule| self.rule_matches(rule, ctx))
    }

    /// Describe the matching process for debugging.
    pub fn explain(&self, ctx: &ProcessContext) -> ExplainResult {
        let mut attempts = Vec::new();
        let mut matched = None;
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
        if !rule.name.is_empty() {
            let name_lc = &rule.name_lowercase;
            let comm_lc = &ctx.comm_lowercase;

            let comm_matches = comm_lc == name_lc
                || (ctx.comm.len() >= MAX_COMM_LEN && name_lc.starts_with(comm_lc.as_str()));

            let exe_matches = ctx
                .exe_name_lowercase
                .as_deref()
                .map(|exe_name_lc| exe_name_lc == name_lc.as_str())
                .unwrap_or(false);

            if !comm_matches && !exe_matches {
                return false;
            }
        }

        if let Some(pattern) = &rule.exe_pattern {
            match ctx.exe.as_deref() {
                Some(exe) if pattern.is_match(exe) => {}
                _ => return false,
            }
        }

        if let Some(needle) = &rule.cmdline_contains {
            match ctx.cmdline.as_deref() {
                Some(cmdline) if cmdline.contains(needle.as_str()) => {}
                _ => return false,
            }
        }

        true
    }
}

pub struct ExplainResult {
    /// The first matching rule, if any.
    pub matched: Option<ResolvedRule>,
    /// `(rule_name, did_match)` for every rule checked.
    pub attempts: Vec<(String, bool)>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx(comm: &str, exe: Option<&str>) -> ProcessContext {
        let comm_lowercase = comm.to_lowercase();
        let exe_name_lowercase = exe
            .and_then(|path| path.rsplit('/').next())
            .map(|s| s.to_lowercase());
        ProcessContext {
            pid: 1,
            ppid: 0,
            start_time_ticks: 0,
            comm: comm.to_string(),
            comm_lowercase,
            exe: exe.map(|value| value.to_string()),
            exe_name_lowercase,
            cmdline: None,
        }
    }

    fn rule(name: &str) -> ResolvedRule {
        ResolvedRule {
            name: name.to_string(),
            name_lowercase: name.to_lowercase(),
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

    #[test]
    fn exact_comm_match() {
        let matcher = matcher(vec![rule("bash")]);
        assert!(matcher.find_match(&ctx("bash", None)).is_some());
        assert!(matcher.find_match(&ctx("dash", None)).is_none());
    }

    #[test]
    fn truncated_comm_prefix_match() {
        let long_name = "firefox-esr-binary";
        let truncated_comm = &long_name[..MAX_COMM_LEN];
        assert_eq!(truncated_comm.len(), MAX_COMM_LEN);

        let matcher = matcher(vec![rule(long_name)]);
        assert!(
            matcher.find_match(&ctx(truncated_comm, None)).is_some(),
            "rule with long name should match truncated comm via prefix check"
        );
    }

    #[test]
    fn short_comm_no_false_prefix_match() {
        let matcher = matcher(vec![rule("firefox-esr-binary")]);
        assert!(
            matcher.find_match(&ctx("fire", None)).is_none(),
            "prefix match must not activate for comm shorter than MAX_COMM_LEN"
        );
    }

    #[test]
    fn unicode_names_do_not_panic_when_indexing_prefixes() {
        let matcher = matcher(vec![rule("火狐浏览器进程示例")]);
        assert!(matcher.find_match(&ctx("火狐浏览器", None)).is_some());
    }

    #[test]
    fn case_insensitive_match() {
        let matcher = matcher(vec![rule("Firefox")]);
        assert!(matcher.find_match(&ctx("firefox", None)).is_some());
        assert!(matcher.find_match(&ctx("FIREFOX", None)).is_some());
    }

    #[test]
    fn exe_basename_match() {
        let matcher = matcher(vec![rule("steam")]);
        assert!(
            matcher
                .find_match(&ctx("steam-runtime", Some("/usr/bin/steam")))
                .is_some()
        );
        assert!(
            matcher
                .find_match(&ctx("unrelated", Some("/usr/bin/steam")))
                .is_some()
        );
        assert!(
            matcher
                .find_match(&ctx("unrelated", Some("/usr/bin/other")))
                .is_none()
        );
    }

    #[test]
    fn short_rule_name_does_not_match_long_comm() {
        let long_comm = "abcdefghijklmno";
        assert_eq!(long_comm.len(), MAX_COMM_LEN);
        let matcher = matcher(vec![rule("ab")]);
        assert!(
            matcher.find_match(&ctx(long_comm, None)).is_none(),
            "short rule 'ab' must not match long comm via reversed prefix"
        );
    }
}
