use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
/// On-disk and in-memory IO priority class used by rule data.
///
/// The value is preserved as rule metadata and later interpreted by the
/// applier according to the currently supported runtime strategy.
pub enum IoClass {
    None,
    RealTime,
    BestEffort,
    Idle,
}

impl IoClass {
    /// Convert the rule-level class into the Linux numeric class identifier.
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
/// A reusable preset loaded from a `.types` file.
///
/// Type definitions provide shared defaults that rule entries can inherit.
/// They are intentionally shallow: the loader and validator merge field values
/// directly, rather than introducing another inheritance layer.
pub struct TypeDef {
    /// Human-readable preset name, serialized from the `type` field.
    #[serde(rename = "type")]
    pub name: String,
    /// Optional niceness to apply when a rule does not override it.
    pub nice: Option<i32>,
    /// Optional IO class carried through to validation and application.
    pub ioclass: Option<IoClass>,
    /// Optional numeric IO priority value used together with `ioclass`.
    pub ionice: Option<u8>,
    /// Optional `oom_score_adj` inherited by rules that select this type.
    pub oom_score_adj: Option<i32>,
    /// Optional cgroup path inherited by rules that select this type.
    pub cgroup: Option<String>,
    /// Optional cgroup-relative weight inherited by rules that select this type.
    pub cgroup_weight: Option<u64>,
    /// Compatibility-only field retained for upstream format parity.
    ///
    /// Arbiter does not currently interpret this field at runtime.
    pub sched: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// A single process rule loaded from a `.rules` file.
///
/// Rules may override type defaults field by field. Matching-related fields are
/// kept alongside application fields so the resolver can validate, merge, and
/// explain the final rule in one pass.
pub struct Rule {
    /// Rule name used for matching against process identity.
    pub name: String,
    /// Optional preset reference; when present, the validator resolves it from
    /// the loaded type map before applying the rule.
    #[serde(rename = "type")]
    pub type_name: Option<String>,
    /// Optional niceness override.
    pub nice: Option<i32>,
    /// Optional IO class override.
    pub ioclass: Option<IoClass>,
    /// Optional IO priority override.
    pub ionice: Option<u8>,
    /// Optional memory pressure hint override.
    pub oom_score_adj: Option<i32>,
    /// Optional cgroup placement override.
    pub cgroup: Option<String>,
    /// Optional cgroup-relative weight override.
    pub cgroup_weight: Option<u64>,
    /// Optional regex applied to the full executable path.
    pub exe_pattern: Option<String>,
    /// Optional substring match against the joined command line.
    pub cmdline_contains: Option<String>,
}

#[derive(Debug, Clone)]
/// A validated rule with all inheritance resolved and regexes compiled.
///
/// This is the form used by the matcher and applier. Storing derived helper
/// fields such as `name_lowercase` avoids repeated allocation in the hot path.
pub struct ResolvedRule {
    /// Original rule name, preserved for diagnostics.
    pub name: String,
    /// Lowercased name for case-insensitive matching.
    pub name_lowercase: String,
    /// Effective niceness after merging rule and type defaults.
    pub nice: Option<i32>,
    /// Effective IO class after merging rule and type defaults.
    pub ioclass: Option<IoClass>,
    /// Effective IO priority after merging rule and type defaults.
    pub ionice: Option<u8>,
    /// Effective memory pressure hint after merging rule and type defaults.
    pub oom_score_adj: Option<i32>,
    /// Effective cgroup path after merging rule and type defaults.
    pub cgroup: Option<String>,
    /// Effective cgroup-relative weight after merging rule and type defaults.
    pub cgroup_weight: Option<u64>,
    /// Compiled executable-path regex used by the matcher.
    pub exe_pattern: Option<Regex>,
    /// Lower-cost string predicate applied to the joined command line.
    pub cmdline_contains: Option<String>,
}

impl ResolvedRule {
    /// Report whether the rule changes any process attribute or placement.
    ///
    /// Matching-only rules are usually a configuration mistake, so the validator
    /// emits a warning when this returns `false`.
    pub fn has_effects(&self) -> bool {
        self.nice.is_some()
            || self.ioclass.is_some()
            || self.oom_score_adj.is_some()
            || self.cgroup.is_some()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Stable key for identifying duplicated selectors during validation.
///
/// Two rules with the same name and match predicates are effectively competing
/// for the same target, so the validator records that overlap and warns.
pub(crate) struct RuleSelectorKey {
    pub(crate) name: String,
    pub(crate) exe_pattern: Option<String>,
    pub(crate) cmdline_contains: Option<String>,
}

#[derive(Debug, Default)]
/// In-memory collection of all loaded rule inputs.
///
/// Types and rules are stored separately so the loader can accept the upstream
/// file layout and the validator can resolve inheritance in a second pass.
pub struct RuleSet {
    /// Loaded type presets keyed by preset name.
    pub types: HashMap<String, TypeDef>,
    /// Loaded rule entries in file order.
    pub rules: Vec<Rule>,
}

impl RuleSet {
    /// Create an empty rule set.
    pub fn new() -> Self {
        Self::default()
    }
}
