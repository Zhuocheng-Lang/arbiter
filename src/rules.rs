//! Rule loading, validation, and first-match resolution.
//!
//! Files are loaded in glob order from configured directories.
//! Types are loaded before rules, `.cgroups` files are warned about and ignored,
//! and the supported format remains the JSON-per-line style compatible with
//! ananicy-cpp rule files.
mod loader;
mod matcher;
mod types;
mod validator;

pub use matcher::{ExplainResult, Matcher, ProcessContext};
pub use types::{IoClass, ResolvedRule, Rule, RuleSet, TypeDef};

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
