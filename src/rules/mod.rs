//! Rule loading, validation, and first-match resolution.
//!
//! Files are loaded in glob order from configured directories.
//! Types and rules use the JSON-per-line format compatible with ananicy-cpp.
mod matcher;

pub use matcher::{ExplainResult, Matcher, ProcessContext};

use anyhow::{Context, Result, bail};
use glob::glob;
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

const TYPEDEF_FIELDS: &[&str] = &[
    "type",
    "nice",
    "ioclass",
    "ionice",
    "oom_score_adj",
    "cgroup",
    "cgroup_weight",
    "sched",
];

const RULE_FIELDS: &[&str] = &[
    "name",
    "type",
    "nice",
    "ioclass",
    "ionice",
    "oom_score_adj",
    "cgroup",
    "cgroup_weight",
    "exe_pattern",
    "cmdline_contains",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum IoClass {
    None,
    RealTime,
    BestEffort,
    Idle,
}

impl IoClass {
    pub fn as_linux_class(self) -> u32 {
        match self {
            Self::None => 0,
            Self::RealTime => 1,
            Self::BestEffort => 2,
            Self::Idle => 3,
        }
    }
}

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
    pub sched: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    pub name: String,
    #[serde(rename = "type")]
    pub type_name: Option<String>,
    pub nice: Option<i32>,
    pub ioclass: Option<IoClass>,
    pub ionice: Option<u8>,
    pub oom_score_adj: Option<i32>,
    pub cgroup: Option<String>,
    pub cgroup_weight: Option<u64>,
    pub exe_pattern: Option<String>,
    pub cmdline_contains: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResolvedRule {
    pub name: String,
    pub name_lowercase: String,
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
    pub fn has_effects(&self) -> bool {
        self.nice.is_some()
            || self.ioclass.is_some()
            || self.oom_score_adj.is_some()
            || self.cgroup.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct RuleSelectorKey {
    name: String,
    exe_pattern: Option<String>,
    cmdline_contains: Option<String>,
}

#[derive(Debug, Default)]
pub struct RuleSet {
    pub types: HashMap<String, TypeDef>,
    pub rules: Vec<Rule>,
}

impl RuleSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn load_from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let mut ruleset = Self::new();
        for dir in dirs {
            if dir.exists() {
                ruleset
                    .load_dir(dir)
                    .with_context(|| format!("Failed to load rules from {}", dir.display()))?;
            }
        }
        tracing::info!(
            types = ruleset.types.len(),
            rules = ruleset.rules.len(),
            "Rule set loaded"
        );
        Ok(ruleset)
    }

    fn load_dir(&mut self, dir: &Path) -> Result<()> {
        let mut type_files: Vec<_> = glob(&dir.join("*.types").to_string_lossy())?
            .flatten()
            .collect();
        type_files.sort();
        for entry in type_files {
            self.load_types_file(&entry)?;
        }

        let mut rule_files: Vec<_> = glob(&dir.join("*.rules").to_string_lossy())?
            .flatten()
            .collect();
        rule_files.sort();
        for entry in rule_files {
            self.load_rules_file(&entry)?;
        }

        for entry in glob(&dir.join("*.cgroups").to_string_lossy())?.flatten() {
            tracing::warn!("{}: '.cgroups' files ignored", entry.display());
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

            let Some(value) = check_json_object(line, path, lineno) else {
                continue;
            };
            warn_unknown_fields(path, lineno, &value, TYPEDEF_FIELDS);

            match serde_json::from_value::<TypeDef>(value) {
                Ok(type_def) if !type_def.name.trim().is_empty() => {
                    if type_def.sched.is_some() {
                        tracing::warn!("{}:{}: 'sched' is ignored", path.display(), lineno + 1);
                    }
                    if self.types.insert(type_def.name.clone(), type_def).is_some() {
                        tracing::warn!(
                            "{}:{}: duplicate type overrides earlier definition",
                            path.display(),
                            lineno + 1
                        );
                    }
                }
                Ok(_) => tracing::warn!("{}:{}: missing 'type' name", path.display(), lineno + 1),
                Err(err) => tracing::warn!(
                    "{}:{}: invalid type entry: {}",
                    path.display(),
                    lineno + 1,
                    err
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

            let Some(value) = check_json_object(line, path, lineno) else {
                continue;
            };
            warn_unknown_fields(path, lineno, &value, RULE_FIELDS);

            match serde_json::from_value::<Rule>(value) {
                Ok(rule) if !rule.name.trim().is_empty() => self.rules.push(rule),
                Ok(_) => tracing::warn!("{}:{}: missing 'name'", path.display(), lineno + 1),
                Err(err) => tracing::warn!(
                    "{}:{}: invalid rule entry: {}",
                    path.display(),
                    lineno + 1,
                    err
                ),
            }
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<Vec<ResolvedRule>> {
        let mut resolved = Vec::with_capacity(self.rules.len());
        let mut seen_selectors = HashSet::new();
        let mut errors = 0;

        for rule in &self.rules {
            let key = RuleSelectorKey {
                name: rule.name.clone(),
                exe_pattern: rule.exe_pattern.clone(),
                cmdline_contains: rule.cmdline_contains.clone(),
            };
            if !seen_selectors.insert(key) {
                tracing::warn!(
                    "Rule '{}' duplicates earlier selector (shadowed)",
                    rule.name
                );
            }

            let type_def = match rule.type_name.as_deref() {
                Some(name) => match self.types.get(name) {
                    Some(td) => Some(td),
                    None => {
                        tracing::error!("Rule '{}': missing type '{}'", rule.name, name);
                        errors += 1;
                        continue;
                    }
                },
                None => None,
            };

            let exe_pattern = match &rule.exe_pattern {
                Some(p) => match Regex::new(p) {
                    Ok(r) => Some(r),
                    Err(e) => {
                        tracing::error!("Rule '{}': invalid regex '{}': {}", rule.name, p, e);
                        errors += 1;
                        continue;
                    }
                },
                None => None,
            };

            macro_rules! merge {
                ($field:ident) => {
                    rule.$field.or_else(|| type_def.and_then(|t| t.$field))
                };
            }

            let resolved_rule = ResolvedRule {
                name: rule.name.clone(),
                name_lowercase: rule.name.to_lowercase(),
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
            };

            if check_rule_semantics(&resolved_rule) {
                resolved.push(resolved_rule);
            } else {
                errors += 1;
            }
        }

        if errors == 0 {
            Ok(resolved)
        } else {
            bail!("{} problem(s) found during rule validation", errors);
        }
    }
}

fn check_json_object(line: &str, path: &Path, lineno: usize) -> Option<Value> {
    match serde_json::from_str(line) {
        Ok(Value::Object(v)) => Some(Value::Object(v)),
        Ok(_) => {
            tracing::warn!("{}:{}: expected JSON object", path.display(), lineno + 1);
            None
        }
        Err(e) => {
            tracing::warn!("{}:{}: malformed: {}", path.display(), lineno + 1, e);
            None
        }
    }
}

fn warn_unknown_fields(path: &Path, lineno: usize, value: &Value, allowed: &[&str]) {
    if let Value::Object(obj) = value {
        let mut unknown: Vec<_> = obj
            .keys()
            .filter(|k| !allowed.contains(&k.as_str()))
            .collect();
        if !unknown.is_empty() {
            unknown.sort();
            tracing::warn!(
                "{}:{}: unknown fields ignored: {:?}",
                path.display(),
                lineno + 1,
                unknown
            );
        }
    }
}

fn check_rule_semantics(r: &ResolvedRule) -> bool {
    let mut ok = true;

    if let Some(nice) = r.nice {
        if !(-20..=19).contains(&nice) {
            tracing::warn!("Rule '{}': nice {} out of bounds", r.name, nice);
        }
    }

    if let Some(ionice) = r.ionice {
        if r.ioclass.is_none() {
            tracing::warn!("Rule '{}': ionice set but ioclass missing", r.name);
        }
        if ionice > 7 {
            tracing::warn!("Rule '{}': ionice {} out of bounds", r.name, ionice);
        }
    }

    if let Some(oom) = r.oom_score_adj {
        if !(-1000..=1000).contains(&oom) {
            tracing::warn!("Rule '{}': oom_score_adj {} out of bounds", r.name, oom);
        }
    }

    if let Some(cw) = r.cgroup_weight {
        if r.cgroup.is_none() {
            tracing::warn!("Rule '{}': cgroup_weight set without cgroup", r.name);
        }
        if !(1..=10_000).contains(&cw) {
            tracing::warn!("Rule '{}': cgroup_weight {} out of bounds", r.name, cw);
        }
    }

    if let Some(cg) = &r.cgroup {
        if cg.is_empty() || cg.starts_with('/') || cg.contains("..") {
            tracing::error!("Rule '{}': invalid cgroup path '{}'", r.name, cg);
            ok = false;
        }
    }

    if !r.has_effects() {
        tracing::warn!(
            "Rule '{}': matches processes but does not change any setting",
            r.name
        );
    }

    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_valid_rules() {
        let mut rs = RuleSet::new();
        rs.types.insert(
            "Game".into(),
            TypeDef {
                name: "Game".into(),
                nice: Some(-5),
                ..Default::default()
            },
        );
        rs.rules.push(Rule {
            name: "steam".into(),
            type_name: Some("Game".into()),
            nice: None,
            ioclass: None,
            ionice: None,
            oom_score_adj: None,
            cgroup: None,
            cgroup_weight: None,
            exe_pattern: None,
            cmdline_contains: None,
        });

        let resolved = rs.validate().unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].nice, Some(-5));
    }
}
