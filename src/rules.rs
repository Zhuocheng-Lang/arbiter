use anyhow::{Context, Result};
use glob::glob;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

// ── IoClass ───────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IoClass {
    None,
    RealTime,
    BestEffort,
    Idle,
}

impl IoClass {
    /// Maps to Linux IOPRIO_CLASS_* values (0-3).
    pub fn as_linux_class(self) -> u32 {
        match self {
            Self::None => 0,
            Self::RealTime => 1,
            Self::BestEffort => 2,
            Self::Idle => 3,
        }
    }
}

// ── TypeDef (ananicy "type" file entry) ───────────────────────────────────────

/// Named preset (mirrors ananicy-cpp type format).
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TypeDef {
    #[serde(rename = "type")]
    pub name: String,
    pub nice: Option<i32>,
    pub ioclass: Option<IoClass>,
    pub ionice: Option<u8>,
    pub oom_score_adj: Option<i32>,
    pub cgroup: Option<String>,
    pub cgroup_weight: Option<u64>,
    /// Reserved — not applied but round-trips for compat.
    pub sched: Option<String>,
}

// ── Rule (ananicy "rules" file entry) ─────────────────────────────────────────

/// Single process matching rule. Fields override the inherited TypeDef.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Process name — matched against `comm` (15-char kernel truncated) or
    /// the basename of the executable path.
    pub name: String,

    /// Reference a TypeDef for default values.
    #[serde(rename = "type")]
    pub type_name: Option<String>,

    pub nice: Option<i32>,
    pub ioclass: Option<IoClass>,
    pub ionice: Option<u8>,
    pub oom_score_adj: Option<i32>,
    pub cgroup: Option<String>,
    pub cgroup_weight: Option<u64>,

    /// Optional regex matched against the full exe path.
    pub exe_pattern: Option<String>,

    /// Optional substring matched against the joined cmdline.
    pub cmdline_contains: Option<String>,
}

// ── ResolvedRule (type merged into rule) ──────────────────────────────────────

/// A rule with all TypeDef defaults merged in; ready for the matcher.
#[derive(Debug, Clone)]
pub struct ResolvedRule {
    pub name: String,
    pub nice: Option<i32>,
    pub ioclass: Option<IoClass>,
    pub ionice: Option<u8>,
    pub oom_score_adj: Option<i32>,
    pub cgroup: Option<String>,
    pub cgroup_weight: Option<u64>,
    pub exe_pattern: Option<Regex>,
    pub cmdline_contains: Option<String>,
}

impl ResolvedRule {
    /// True if this rule would actually change something for a process.
    pub fn has_effects(&self) -> bool {
        self.nice.is_some()
            || self.ioclass.is_some()
            || self.oom_score_adj.is_some()
            || self.cgroup.is_some()
    }
}

// ── RuleSet ───────────────────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct RuleSet {
    pub types: HashMap<String, TypeDef>,
    pub rules: Vec<Rule>,
}

impl RuleSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load all *.types then *.rules from each directory in `dirs`.
    pub fn load_from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut rs = Self::new();
        for dir in dirs {
            if dir.exists() {
                rs.load_dir(dir)
                    .with_context(|| format!("Failed to load rules from {}", dir.display()))?;
            }
        }
        tracing::info!(
            types = rs.types.len(),
            rules = rs.rules.len(),
            "Rule set loaded"
        );
        Ok(rs)
    }

    fn load_dir(&mut self, dir: &Path) -> Result<()> {
        let types_glob = dir.join("*.types").to_string_lossy().into_owned();
        let rules_glob = dir.join("*.rules").to_string_lossy().into_owned();

        for entry in glob(&types_glob).context("glob *.types")?.flatten() {
            self.load_types_file(&entry)
                .with_context(|| format!("types file: {}", entry.display()))?;
        }
        for entry in glob(&rules_glob).context("glob *.rules")?.flatten() {
            self.load_rules_file(&entry)
                .with_context(|| format!("rules file: {}", entry.display()))?;
        }
        Ok(())
    }

    fn load_types_file(&mut self, path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        for (lineno, raw) in content.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match serde_json::from_str::<TypeDef>(line) {
                Ok(t) => {
                    self.types.insert(t.name.clone(), t);
                }
                Err(e) => tracing::warn!(
                    file = %path.display(), line = lineno + 1,
                    "Skipping malformed type: {}", e
                ),
            }
        }
        Ok(())
    }

    fn load_rules_file(&mut self, path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        for (lineno, raw) in content.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            match serde_json::from_str::<Rule>(line) {
                Ok(r) => self.rules.push(r),
                Err(e) => tracing::warn!(
                    file = %path.display(), line = lineno + 1,
                    "Skipping malformed rule: {}", e
                ),
            }
        }
        Ok(())
    }

    /// Merge type defaults into a rule and compile regex patterns.
    pub fn resolve(&self, rule: &Rule) -> Result<ResolvedRule> {
        let type_def = rule.type_name.as_deref().and_then(|t| {
            let td = self.types.get(t);
            if td.is_none() {
                tracing::warn!(rule = %rule.name, r#type = t, "Referenced type not found");
            }
            td
        });

        // Rule fields override type defaults.
        macro_rules! merge {
            ($field:ident) => {
                rule.$field.or_else(|| type_def.and_then(|t| t.$field))
            };
        }

        let exe_pattern = match &rule.exe_pattern {
            Some(p) => Some(Regex::new(p).with_context(|| {
                format!("Invalid exe_pattern regex in rule '{}': {p}", rule.name)
            })?),
            None => None,
        };

        Ok(ResolvedRule {
            name: rule.name.clone(),
            nice: merge!(nice),
            ioclass: merge!(ioclass),
            ionice: merge!(ionice),
            oom_score_adj: merge!(oom_score_adj),
            cgroup: rule
                .cgroup
                .clone()
                .or_else(|| type_def.and_then(|t| t.cgroup.clone())),
            cgroup_weight: merge!(cgroup_weight),
            exe_pattern,
            cmdline_contains: rule.cmdline_contains.clone(),
        })
    }

    /// Resolve all rules, logging and skipping any that fail.
    pub fn resolved_rules(&self) -> Vec<ResolvedRule> {
        self.rules
            .iter()
            .filter_map(|r| {
                self.resolve(r)
                    .map_err(|e| tracing::warn!("Cannot resolve rule '{}': {}", r.name, e))
                    .ok()
            })
            .collect()
    }

    /// Resolve all rules strictly — returns `Err` listing every failure.
    /// Used by the `check` command to surface misconfiguration.
    pub fn validate(&self) -> Result<Vec<ResolvedRule>> {
        let mut resolved = Vec::with_capacity(self.rules.len());
        let mut errors: Vec<String> = Vec::new();
        for rule in &self.rules {
            match self.resolve(rule) {
                Ok(r) => resolved.push(r),
                Err(e) => errors.push(format!("  rule '{}': {e}", rule.name)),
            }
        }
        if errors.is_empty() {
            Ok(resolved)
        } else {
            anyhow::bail!(
                "{} rule(s) failed to resolve:\n{}",
                errors.len(),
                errors.join("\n")
            )
        }
    }
}
