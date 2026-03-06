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
use std::collections::HashMap;
use std::fmt;
use std::path::{Component, Path, PathBuf};

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
    /// Reserved - not applied but round-trips for compat.
    pub sched: Option<String>,
}

/// Single process matching rule. Fields override the inherited TypeDef.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Rule {
    /// Process name - matched against `comm` (15-char kernel truncated) or
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DiagnosticSeverity {
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceLocation {
    path: PathBuf,
    line: usize,
}

impl SourceLocation {
    fn new(path: &Path, line: usize) -> Self {
        Self {
            path: path.to_path_buf(),
            line,
        }
    }
}

#[derive(Debug, Clone)]
struct RuleDiagnostic {
    severity: DiagnosticSeverity,
    source: SourceLocation,
    entry_name: Option<String>,
    message: String,
}

impl RuleDiagnostic {
    fn warning(
        source: &SourceLocation,
        entry_name: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Warning,
            source: source.clone(),
            entry_name,
            message: message.into(),
        }
    }

    fn error(
        source: &SourceLocation,
        entry_name: Option<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            severity: DiagnosticSeverity::Error,
            source: source.clone(),
            entry_name,
            message: message.into(),
        }
    }

    fn is_error(&self) -> bool {
        self.severity == DiagnosticSeverity::Error
    }

    fn is_warning(&self) -> bool {
        self.severity == DiagnosticSeverity::Warning
    }
}

impl fmt::Display for RuleDiagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let severity = match self.severity {
            DiagnosticSeverity::Warning => "warning",
            DiagnosticSeverity::Error => "error",
        };

        write!(f, "{severity}: {}", self.source.path.display())?;
        if self.source.line > 0 {
            write!(f, ":{}", self.source.line)?;
        }
        if let Some(entry_name) = &self.entry_name {
            write!(f, ": entry '{}'", entry_name)?;
        }
        write!(f, ": {}", self.message)
    }
}

#[derive(Debug)]
struct RuleLoadReport {
    ruleset: RuleSet,
    diagnostics: Vec<RuleDiagnostic>,
}

impl RuleLoadReport {
    fn emit_warnings(&self) {
        emit_warning_logs(&self.diagnostics);
    }

    fn into_result(self) -> Result<RuleSet> {
        into_result("loading", self.diagnostics, self.ruleset)
    }
}

#[derive(Debug)]
struct RuleValidationReport {
    resolved: Vec<ResolvedRule>,
    diagnostics: Vec<RuleDiagnostic>,
}

impl RuleValidationReport {
    fn emit_warnings(&self) {
        emit_warning_logs(&self.diagnostics);
    }

    fn into_result(self) -> Result<Vec<ResolvedRule>> {
        into_result("validation", self.diagnostics, self.resolved)
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
    type_sources: HashMap<String, SourceLocation>,
    rule_sources: Vec<SourceLocation>,
}

impl RuleSet {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load all *.types then *.rules from each directory in `dirs`.
    pub fn load_from_dirs(dirs: &[PathBuf]) -> Result<Self> {
        let report = Self::load_report_from_dirs(dirs)?;
        report.emit_warnings();
        let ruleset = report.into_result()?;
        tracing::info!(
            types = ruleset.types.len(),
            rules = ruleset.rules.len(),
            "Rule set loaded"
        );
        Ok(ruleset)
    }

    fn load_report_from_dirs(dirs: &[PathBuf]) -> Result<RuleLoadReport> {
        let mut ruleset = Self::new();
        let mut diagnostics = Vec::new();
        for dir in dirs {
            if dir.exists() {
                ruleset
                    .load_dir_with_diagnostics(dir, &mut diagnostics)
                    .with_context(|| format!("Failed to load rules from {}", dir.display()))?;
            }
        }
        Ok(RuleLoadReport {
            ruleset,
            diagnostics,
        })
    }

    fn load_dir_with_diagnostics(
        &mut self,
        dir: &Path,
        diagnostics: &mut Vec<RuleDiagnostic>,
    ) -> Result<()> {
        let types_glob = dir.join("*.types").to_string_lossy().into_owned();
        let rules_glob = dir.join("*.rules").to_string_lossy().into_owned();
        let cgroups_glob = dir.join("*.cgroups").to_string_lossy().into_owned();

        for entry in sorted_glob(&types_glob).context("glob *.types")? {
            self.load_types_file_with_diagnostics(&entry, diagnostics)
                .with_context(|| format!("types file: {}", entry.display()))?;
        }
        for entry in sorted_glob(&rules_glob).context("glob *.rules")? {
            self.load_rules_file_with_diagnostics(&entry, diagnostics)
                .with_context(|| format!("rules file: {}", entry.display()))?;
        }
        for entry in sorted_glob(&cgroups_glob).context("glob *.cgroups")? {
            diagnostics.push(RuleDiagnostic::warning(
                &SourceLocation::new(&entry, 0),
                None,
                "'.cgroups' files are currently ignored; convert their settings into '.types' or '.rules' entries",
            ));
        }
        Ok(())
    }

    fn load_types_file_with_diagnostics(
        &mut self,
        path: &Path,
        diagnostics: &mut Vec<RuleDiagnostic>,
    ) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        for (lineno, raw) in content.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let source = SourceLocation::new(path, lineno + 1);
            let Some(value) = parse_json_object(line, &source, diagnostics) else {
                continue;
            };

            push_unknown_field_warning(&source, None, &value, TYPEDEF_FIELDS, diagnostics);

            let type_def: TypeDef = match serde_json::from_value(value) {
                Ok(type_def) => type_def,
                Err(err) => {
                    diagnostics.push(RuleDiagnostic::warning(
                        &source,
                        None,
                        format!("skipping invalid type entry: {err}"),
                    ));
                    continue;
                }
            };

            if type_def.name.trim().is_empty() {
                diagnostics.push(RuleDiagnostic::warning(
                    &source,
                    None,
                    "skipping type entry with missing or empty 'type' name",
                ));
                continue;
            }

            if type_def.sched.is_some() {
                diagnostics.push(RuleDiagnostic::warning(
                    &source,
                    Some(type_def.name.clone()),
                    "field 'sched' is reserved for compatibility and is ignored",
                ));
            }

            if let Some(previous) = self.type_sources.get(&type_def.name) {
                diagnostics.push(RuleDiagnostic::warning(
                    &source,
                    Some(type_def.name.clone()),
                    format!(
                        "duplicate type overrides earlier definition at {}:{}",
                        previous.path.display(),
                        previous.line
                    ),
                ));
            }

            self.type_sources.insert(type_def.name.clone(), source);
            self.types.insert(type_def.name.clone(), type_def);
        }
        Ok(())
    }

    fn load_rules_file_with_diagnostics(
        &mut self,
        path: &Path,
        diagnostics: &mut Vec<RuleDiagnostic>,
    ) -> Result<()> {
        let content = std::fs::read_to_string(path)?;
        for (lineno, raw) in content.lines().enumerate() {
            let line = raw.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }

            let source = SourceLocation::new(path, lineno + 1);
            let Some(value) = parse_json_object(line, &source, diagnostics) else {
                continue;
            };

            push_unknown_field_warning(&source, None, &value, RULE_FIELDS, diagnostics);

            let rule: Rule = match serde_json::from_value(value) {
                Ok(rule) => rule,
                Err(err) => {
                    diagnostics.push(RuleDiagnostic::warning(
                        &source,
                        None,
                        format!("skipping invalid rule entry: {err}"),
                    ));
                    continue;
                }
            };

            if rule.name.trim().is_empty() {
                diagnostics.push(RuleDiagnostic::warning(
                    &source,
                    None,
                    "skipping rule entry with missing or empty 'name'",
                ));
                continue;
            }

            self.rule_sources.push(source);
            self.rules.push(rule);
        }
        Ok(())
    }

    /// Resolve all rules, logging and skipping any that fail.
    pub fn resolved_rules(&self) -> Vec<ResolvedRule> {
        let report = self.validation_report();
        report.emit_warnings();
        for diagnostic in report
            .diagnostics
            .iter()
            .filter(|diagnostic| diagnostic.is_error())
        {
            tracing::error!("Skipping invalid rule: {diagnostic}");
        }
        report.resolved
    }

    /// Resolve all rules strictly - returns `Err` listing every failure.
    pub fn validate(&self) -> Result<Vec<ResolvedRule>> {
        let report = self.validation_report();
        report.emit_warnings();
        report.into_result()
    }

    fn validation_report(&self) -> RuleValidationReport {
        let mut resolved = Vec::with_capacity(self.rules.len());
        let mut diagnostics = Vec::new();
        let mut seen_selectors: HashMap<RuleSelectorKey, SourceLocation> = HashMap::new();

        for (index, rule) in self.rules.iter().enumerate() {
            let source = self
                .rule_sources
                .get(index)
                .cloned()
                .unwrap_or_else(|| SourceLocation::new(Path::new("<unknown>"), 0));

            let selector_key = RuleSelectorKey {
                name: rule.name.clone(),
                exe_pattern: rule.exe_pattern.clone(),
                cmdline_contains: rule.cmdline_contains.clone(),
            };
            if let Some(previous) = seen_selectors.get(&selector_key) {
                diagnostics.push(RuleDiagnostic::warning(
                    &source,
                    Some(rule.name.clone()),
                    format!(
                        "selector duplicates earlier rule at {}:{}; first-match-wins means this rule is shadowed",
                        previous.path.display(),
                        previous.line
                    ),
                ));
            } else {
                seen_selectors.insert(selector_key, source.clone());
            }

            let type_def = match rule.type_name.as_deref() {
                Some(type_name) => match self.types.get(type_name) {
                    Some(type_def) => Some(type_def),
                    None => {
                        diagnostics.push(RuleDiagnostic::error(
                            &source,
                            Some(rule.name.clone()),
                            format!("referenced type '{type_name}' was not loaded"),
                        ));
                        continue;
                    }
                },
                None => None,
            };

            macro_rules! merge {
                ($field:ident) => {
                    rule.$field
                        .or_else(|| type_def.and_then(|type_def| type_def.$field))
                };
            }

            let exe_pattern = match &rule.exe_pattern {
                Some(pattern) => match Regex::new(pattern) {
                    Ok(regex) => Some(regex),
                    Err(err) => {
                        diagnostics.push(RuleDiagnostic::error(
                            &source,
                            Some(rule.name.clone()),
                            format!("invalid exe_pattern regex '{pattern}': {err}"),
                        ));
                        continue;
                    }
                },
                None => None,
            };

            let resolved_rule = ResolvedRule {
                name: rule.name.clone(),
                nice: merge!(nice),
                ioclass: merge!(ioclass),
                ionice: merge!(ionice),
                oom_score_adj: merge!(oom_score_adj),
                cgroup: rule
                    .cgroup
                    .clone()
                    .or_else(|| type_def.and_then(|type_def| type_def.cgroup.clone())),
                cgroup_weight: merge!(cgroup_weight),
                exe_pattern,
                cmdline_contains: rule.cmdline_contains.clone(),
            };

            let prev_diag_len = diagnostics.len();
            validate_resolved_rule(&source, rule, &resolved_rule, &mut diagnostics);
            let has_new_errors = diagnostics[prev_diag_len..]
                .iter()
                .any(|diagnostic| diagnostic.is_error());
            if !has_new_errors {
                resolved.push(resolved_rule);
            }
        }

        RuleValidationReport {
            resolved,
            diagnostics,
        }
    }
}

fn sorted_glob(pattern: &str) -> Result<Vec<PathBuf>> {
    let mut entries = Vec::new();
    for entry in glob(pattern)? {
        entries.push(entry.with_context(|| format!("glob entry for pattern '{pattern}'"))?);
    }
    entries.sort();
    Ok(entries)
}

fn parse_json_object(
    line: &str,
    source: &SourceLocation,
    diagnostics: &mut Vec<RuleDiagnostic>,
) -> Option<Value> {
    let value: Value = match serde_json::from_str(line) {
        Ok(value) => value,
        Err(err) => {
            diagnostics.push(RuleDiagnostic::warning(
                source,
                None,
                format!("skipping malformed JSON entry: {err}"),
            ));
            return None;
        }
    };

    if !value.is_object() {
        diagnostics.push(RuleDiagnostic::warning(
            source,
            None,
            "skipping entry: expected a JSON object",
        ));
        return None;
    }

    Some(value)
}

fn push_unknown_field_warning(
    source: &SourceLocation,
    entry_name: Option<String>,
    value: &Value,
    allowed_fields: &[&str],
    diagnostics: &mut Vec<RuleDiagnostic>,
) {
    let mut unknown_fields = value
        .as_object()
        .into_iter()
        .flat_map(|object| object.keys())
        .filter(|key| !allowed_fields.contains(&key.as_str()))
        .cloned()
        .collect::<Vec<_>>();

    if unknown_fields.is_empty() {
        return;
    }

    unknown_fields.sort();
    diagnostics.push(RuleDiagnostic::warning(
        source,
        entry_name,
        format!("unknown fields ignored: {}", unknown_fields.join(", ")),
    ));
}

fn validate_resolved_rule(
    source: &SourceLocation,
    rule: &Rule,
    resolved_rule: &ResolvedRule,
    diagnostics: &mut Vec<RuleDiagnostic>,
) {
    if let Some(nice) = resolved_rule.nice
        && !(-20..=19).contains(&nice)
    {
        diagnostics.push(RuleDiagnostic::warning(
            source,
            Some(rule.name.clone()),
            format!("nice value {nice} is outside [-20, 19] and will be clamped during apply"),
        ));
    }

    if let Some(ionice) = resolved_rule.ionice {
        if resolved_rule.ioclass.is_none() {
            diagnostics.push(RuleDiagnostic::warning(
                source,
                Some(rule.name.clone()),
                "ionice is set but ioclass is missing; ionice will be ignored during apply",
            ));
        }
        if ionice > 7 {
            diagnostics.push(RuleDiagnostic::warning(
                source,
                Some(rule.name.clone()),
                format!("ionice level {ionice} is outside [0, 7] and will be clamped during apply"),
            ));
        }
    }

    if let Some(oom_score_adj) = resolved_rule.oom_score_adj
        && !(-1000..=1000).contains(&oom_score_adj)
    {
        diagnostics.push(RuleDiagnostic::warning(
            source,
            Some(rule.name.clone()),
            format!(
                "oom_score_adj value {oom_score_adj} is outside [-1000, 1000] and will be clamped during apply"
            ),
        ));
    }

    if let Some(cgroup_weight) = resolved_rule.cgroup_weight {
        if resolved_rule.cgroup.is_none() {
            diagnostics.push(RuleDiagnostic::warning(
                source,
                Some(rule.name.clone()),
                "cgroup_weight is set without cgroup; it has no effect",
            ));
        }
        if !(1..=10_000).contains(&cgroup_weight) {
            diagnostics.push(RuleDiagnostic::warning(
                source,
                Some(rule.name.clone()),
                format!(
                    "cgroup_weight value {cgroup_weight} is outside [1, 10000] and will be clamped during apply"
                ),
            ));
        }
    }

    if let Some(cgroup) = &resolved_rule.cgroup
        && let Err(err) = validate_cgroup_path(cgroup)
    {
        diagnostics.push(RuleDiagnostic::error(
            source,
            Some(rule.name.clone()),
            format!("invalid cgroup path '{cgroup}': {err}"),
        ));
    }

    if !resolved_rule.has_effects() {
        diagnostics.push(RuleDiagnostic::warning(
            source,
            Some(rule.name.clone()),
            "rule matches processes but does not change any supported setting",
        ));
    }
}

fn validate_cgroup_path(cgroup: &str) -> Result<()> {
    if cgroup.is_empty() {
        bail!("path is empty");
    }
    if cgroup.starts_with('/') {
        bail!("absolute paths are not allowed");
    }

    let mut saw_component = false;
    for component in Path::new(cgroup).components() {
        match component {
            Component::Normal(part) => {
                if part.is_empty() {
                    bail!("path contains an empty component");
                }
                saw_component = true;
            }
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => {
                bail!("path must not contain '.', '..', or a root prefix");
            }
        }
    }

    if !saw_component {
        bail!("path is empty");
    }

    Ok(())
}

fn emit_warning_logs(diagnostics: &[RuleDiagnostic]) {
    for diagnostic in diagnostics
        .iter()
        .filter(|diagnostic| diagnostic.is_warning())
    {
        tracing::warn!("{diagnostic}");
    }
}

fn into_result<T>(stage: &str, diagnostics: Vec<RuleDiagnostic>, value: T) -> Result<T> {
    let errors = diagnostics
        .into_iter()
        .filter(|diagnostic| diagnostic.is_error())
        .map(|diagnostic| format!("  {diagnostic}"))
        .collect::<Vec<_>>();

    if errors.is_empty() {
        Ok(value)
    } else {
        bail!(
            "{} problem(s) found during rule {stage}:\n{}",
            errors.len(),
            errors.join("\n")
        );
    }
}

#[cfg(test)]
mod tests {
    use super::{Rule, RuleSet, SourceLocation, TypeDef};
    use std::path::Path;

    fn source(line: usize) -> SourceLocation {
        SourceLocation::new(Path::new("rules/test.rules"), line)
    }

    #[test]
    fn missing_type_is_a_validation_error() {
        let mut ruleset = RuleSet::new();
        ruleset.rules.push(Rule {
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
        ruleset.rule_sources.push(source(3));

        let report = ruleset.validation_report();

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.is_error())
        );
        assert!(report.resolved.is_empty());
    }

    #[test]
    fn duplicate_selectors_are_warned_and_rule_still_resolves() {
        let mut ruleset = RuleSet::new();
        ruleset.types.insert(
            "Game".into(),
            TypeDef {
                name: "Game".into(),
                nice: Some(-5),
                ioclass: None,
                ionice: None,
                oom_score_adj: None,
                cgroup: None,
                cgroup_weight: None,
                sched: None,
            },
        );

        for line in [4, 5] {
            ruleset.rules.push(Rule {
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
            ruleset.rule_sources.push(source(line));
        }

        let report = ruleset.validation_report();

        assert_eq!(report.resolved.len(), 2);
        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.is_warning())
        );
    }

    #[test]
    fn unsafe_cgroup_path_is_rejected_during_validation() {
        let mut ruleset = RuleSet::new();
        ruleset.rules.push(Rule {
            name: "bad".into(),
            type_name: None,
            nice: Some(0),
            ioclass: None,
            ionice: None,
            oom_score_adj: None,
            cgroup: Some("../escape".into()),
            cgroup_weight: None,
            exe_pattern: None,
            cmdline_contains: None,
        });
        ruleset.rule_sources.push(source(7));

        let report = ruleset.validation_report();

        assert!(
            report
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.is_error())
        );
        assert!(report.resolved.is_empty());
    }

    #[test]
    fn no_effects_rule_produces_warning() {
        let mut ruleset = RuleSet::new();
        ruleset.rules.push(Rule {
            name: "mystery".into(),
            type_name: None,
            nice: None,
            ioclass: None,
            ionice: None,
            oom_score_adj: None,
            cgroup: None,
            cgroup_weight: None,
            exe_pattern: None,
            cmdline_contains: None,
        });
        ruleset.rule_sources.push(source(1));

        let report = ruleset.validation_report();

        assert_eq!(
            report.resolved.len(),
            1,
            "no-effects rule should still resolve"
        );
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.is_warning())
                .any(|diagnostic| diagnostic.message.contains("does not change")),
            "expected a no-effects warning"
        );
    }

    #[test]
    fn ionice_without_ioclass_produces_warning() {
        let mut ruleset = RuleSet::new();
        ruleset.rules.push(Rule {
            name: "proc".into(),
            type_name: None,
            nice: Some(0),
            ioclass: None,
            ionice: Some(4),
            oom_score_adj: None,
            cgroup: None,
            cgroup_weight: None,
            exe_pattern: None,
            cmdline_contains: None,
        });
        ruleset.rule_sources.push(source(1));

        let report = ruleset.validation_report();

        assert_eq!(report.resolved.len(), 1);
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.is_warning())
                .any(|diagnostic| diagnostic.message.contains("ioclass")),
            "expected an ioclass-missing warning"
        );
    }

    #[test]
    fn cgroup_weight_without_cgroup_produces_warning() {
        let mut ruleset = RuleSet::new();
        ruleset.rules.push(Rule {
            name: "proc".into(),
            type_name: None,
            nice: Some(5),
            ioclass: None,
            ionice: None,
            oom_score_adj: None,
            cgroup: None,
            cgroup_weight: Some(800),
            exe_pattern: None,
            cmdline_contains: None,
        });
        ruleset.rule_sources.push(source(1));

        let report = ruleset.validation_report();

        assert_eq!(report.resolved.len(), 1);
        assert!(
            report
                .diagnostics
                .iter()
                .filter(|diagnostic| diagnostic.is_warning())
                .any(|diagnostic| diagnostic.message.contains("cgroup_weight")),
            "expected a cgroup_weight-without-cgroup warning"
        );
    }
}
