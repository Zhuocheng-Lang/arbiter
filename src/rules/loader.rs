use anyhow::{Context, Result};
use glob::glob;
use serde_json::Value;
use std::path::{Path, PathBuf};

use super::{Rule, RuleSet, TypeDef};

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

impl RuleSet {
    /// Load every supported rule file from the provided directories.
    ///
    /// Directories are processed in the order they are supplied, while each
    /// directory still loads types before rules so type inheritance is ready by
    /// the time rule entries are parsed.
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

    /// Load one rules directory in the same order the runtime will resolve it.
    ///
    /// The loader is intentionally line-oriented: each non-empty line is parsed
    /// as a standalone JSON object so the format stays compatible with
    /// ananicy-cpp-style rule files and remains easy to diff and comment.
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

        // Arbiter intentionally ignores `.cgroups` files because its current
        // model only supports rule-level cgroup placement plus optional weight.
        for entry in glob(&dir.join("*.cgroups").to_string_lossy())?.flatten() {
            tracing::warn!("{}: '.cgroups' files ignored", entry.display());
        }
        Ok(())
    }

    /// Load a `.types` file and merge each preset into the in-memory map.
    ///
    /// Later duplicates override earlier definitions, which mirrors the common
    /// override-by-order behavior used by layered configuration directories.
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

    /// Load a `.rules` file and append each valid rule to the rule list.
    ///
    /// Rules are kept in file order so validation and matching preserve the same
    /// first-match semantics users see in the source files.
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
}

/// Parse one trimmed line as a JSON object and reject anything else.
///
/// The loader keeps malformed or non-object lines as warnings instead of hard
/// failures so a single bad entry does not discard an entire rules file.
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

/// Warn about fields that the current rule schema does not understand.
///
/// Unknown fields are ignored for forward compatibility: upstream rule files
/// can carry extra metadata without breaking Arbiter's parser.
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
