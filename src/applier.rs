mod cgroup;
mod io_weight;
mod process;

use anyhow::Result;

use crate::config::Config;
use crate::platform::linux::{ScxScheduler, Strategy};
use crate::rules::ProcessContext;
use crate::rules::ResolvedRule;

/// Summary of what was actually done for one process event.
#[derive(Debug, Default)]
pub struct ApplyResult {
    pub dry_run: bool,
    pub nice_applied: Option<i32>,
    pub io_weight_applied: Option<u16>,
    pub oom_applied: Option<i32>,
    pub cgroup_applied: Option<String>,
}

pub struct Applier {
    config: Config,
}

impl Applier {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub fn apply(
        &self,
        ctx: &ProcessContext,
        rule: &ResolvedRule,
        scheduler: &ScxScheduler,
    ) -> Result<ApplyResult> {
        let mut result = ApplyResult::default();

        if self.config.dry_run {
            tracing::info!(
                pid       = ctx.pid,
                comm      = %ctx.comm,
                rule      = %rule.name,
                scheduler = %scheduler,
                "[dry-run] would apply"
            );
            result.dry_run = true;
            return Ok(result);
        }

        if !ctx.matches_current_pid()? {
            tracing::debug!(
                pid  = ctx.pid,
                rule = %rule.name,
                "process exited before rule could be applied (expected TOCTOU race)"
            );
            return Ok(result);
        }

        let strategy = scheduler.strategy();

        if self.config.apply_nice
            && let Some(mut nice) = rule.nice
        {
            nice = nice.clamp(-20, 19);
            match process::set_nice(ctx.pid, nice) {
                Ok(()) => result.nice_applied = Some(nice),
                Err(e) => tracing::warn!(pid = ctx.pid, "nice={nice} failed: {e}"),
            }
        }

        let io_weight = if self.config.apply_ionice {
            io_weight::ionice_to_io_weight(rule.ioclass, rule.ionice)
        } else {
            None
        };

        if self.config.apply_oom
            && let Some(mut oom) = rule.oom_score_adj
        {
            oom = oom.clamp(-1000, 1000);
            match process::set_oom_score_adj(ctx.pid, oom) {
                Ok(()) => result.oom_applied = Some(oom),
                Err(e) => tracing::warn!(pid = ctx.pid, "oom_score_adj={oom} failed: {e}"),
            }
        }

        if self.config.apply_cgroup
            && let Some(ref cgroup) = rule.cgroup
        {
            let weight = io_weight::effective_cgroup_weight(strategy, rule.cgroup_weight);
            match cgroup::move_to_cgroup(ctx.pid, cgroup, weight, io_weight) {
                Ok(applied_io_weight) => {
                    result.cgroup_applied = Some(cgroup.clone());
                    result.io_weight_applied = applied_io_weight;
                }
                Err(e) => tracing::warn!(pid = ctx.pid, cgroup, "cgroup move failed: {e}"),
            }
        } else if io_weight.is_some() {
            tracing::debug!(
                pid = ctx.pid,
                rule = %rule.name,
                "ionice configured but no cgroup target; skipping io.weight"
            );
        }

        if strategy == Strategy::LayeredJson
            && let Some(ref path) = self.config.layered_export_path
        {
            tracing::debug!("layered export target: {}", path.display());
        }

        tracing::info!(
            pid       = ctx.pid,
            comm      = %ctx.comm,
            rule      = %rule.name,
            nice      = ?result.nice_applied,
            cgroup    = ?result.cgroup_applied,
            scheduler = %scheduler,
            "Applied"
        );

        Ok(result)
    }
}
