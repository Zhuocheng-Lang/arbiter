use anyhow::{Result, bail};
use regex::Regex;
use std::collections::HashSet;

use super::types::RuleSelectorKey;
use super::{ResolvedRule, RuleSet};

impl RuleSet {
    /// Resolve rule inheritance, compile regexes, and reject invalid entries.
    ///
    /// Validation is deliberately two-stage: it first detects structural issues
    /// such as missing types or invalid regex syntax, then checks semantic
    /// bounds on the resolved rule values. Non-fatal configuration mistakes are
    /// warned about, while hard inconsistencies increment the error count.
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

/// Check the resolved rule for value ranges and structural consistency.
///
/// Returns `false` only for hard failures such as an invalid cgroup path. Range
/// problems are kept as warnings so users can decide whether a borderline rule
/// is intentional before the validator aborts on the accumulated error count.
fn check_rule_semantics(r: &ResolvedRule) -> bool {
    let mut ok = true;

    if let Some(nice) = r.nice
        && !(-20..=19).contains(&nice)
    {
        tracing::warn!("Rule '{}': nice {} out of bounds", r.name, nice);
    }

    if let Some(ionice) = r.ionice {
        if r.ioclass.is_none() {
            tracing::warn!("Rule '{}': ionice set but ioclass missing", r.name);
        }
        if ionice > 7 {
            tracing::warn!("Rule '{}': ionice {} out of bounds", r.name, ionice);
        }
    }

    if let Some(oom) = r.oom_score_adj
        && !(-1000..=1000).contains(&oom)
    {
        tracing::warn!("Rule '{}': oom_score_adj {} out of bounds", r.name, oom);
    }

    if let Some(cw) = r.cgroup_weight {
        if r.cgroup.is_none() {
            tracing::warn!("Rule '{}': cgroup_weight set without cgroup", r.name);
        }
        // cgroup v2 weights are defined as a relative share, so the loader and
        // applier clamp them into the kernel's documented 1..=10_000 range.
        if !(1..=10_000).contains(&cw) {
            tracing::warn!("Rule '{}': cgroup_weight {} out of bounds", r.name, cw);
        }
    }

    if let Some(cg) = &r.cgroup
        && (cg.is_empty() || cg.starts_with('/') || cg.contains(".."))
    {
        tracing::error!("Rule '{}': invalid cgroup path '{}'", r.name, cg);
        ok = false;
    }

    if !r.has_effects() {
        tracing::warn!(
            "Rule '{}': matches processes but does not change any setting",
            r.name
        );
    }

    ok
}
