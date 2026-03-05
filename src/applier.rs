use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::matcher::ProcessContext;
use crate::rules::{IoClass, ResolvedRule};
use crate::scx::{ScxScheduler, Strategy};

// ── ApplyResult ───────────────────────────────────────────────────────────────

/// Summary of what was actually done for one process event.
#[derive(Debug, Default)]
pub struct ApplyResult {
    pub dry_run: bool,
    pub nice_applied: Option<i32>,
    pub ionice_applied: bool,
    pub oom_applied: Option<i32>,
    pub cgroup_applied: Option<String>,
}

// ── Applier ───────────────────────────────────────────────────────────────────

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

        let strategy = scheduler.strategy();

        // ── nice ─────────────────────────────────────────────────────────────
        if self.config.apply_nice {
            if let Some(mut nice) = rule.nice {
                nice = nice.clamp(-20, 19);
                match self.set_nice(ctx.pid, nice) {
                    Ok(()) => result.nice_applied = Some(nice),
                    Err(e) => tracing::warn!(pid = ctx.pid, "nice={nice} failed: {e}"),
                }
            }
        }

        // ── ionice ───────────────────────────────────────────────────────────
        if self.config.apply_ionice {
            if let Some(ioclass) = rule.ioclass {
                let level = rule.ionice.unwrap_or(4).clamp(0, 7);
                match self.set_ionice(ctx.pid, ioclass, level) {
                    Ok(()) => result.ionice_applied = true,
                    Err(e) => tracing::warn!(pid = ctx.pid, "ionice failed: {e}"),
                }
            }
        }

        // ── oom_score_adj ────────────────────────────────────────────────────
        if self.config.apply_oom {
            if let Some(mut oom) = rule.oom_score_adj {
                oom = oom.clamp(-1000, 1000);
                match self.set_oom_score_adj(ctx.pid, oom) {
                    Ok(()) => result.oom_applied = Some(oom),
                    Err(e) => tracing::warn!(pid = ctx.pid, "oom_score_adj={oom} failed: {e}"),
                }
            }
        }

        // ── cgroup placement ─────────────────────────────────────────────────
        if self.config.apply_cgroup {
            if let Some(ref cgroup) = rule.cgroup {
                // For scx_lavd/rusty/bpfland we also set cpu.weight via cgroup.
                let weight = if matches!(strategy, Strategy::NiceAndWeight) {
                    rule.cgroup_weight
                } else {
                    rule.cgroup_weight
                };
                match self.move_to_cgroup(ctx.pid, cgroup, weight) {
                    Ok(()) => result.cgroup_applied = Some(cgroup.clone()),
                    Err(e) => tracing::warn!(pid = ctx.pid, cgroup, "cgroup move failed: {e}"),
                }
            }
        }

        // ── scx_layered: export layer JSON ───────────────────────────────────
        if strategy == Strategy::LayeredJson {
            if let Some(ref path) = self.config.layered_export_path {
                // Deferred: layered export collects all rules and writes once.
                tracing::debug!("layered export target: {}", path.display());
            }
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

    // ── private helpers ───────────────────────────────────────────────────────

    fn set_nice(&self, pid: u32, nice: i32) -> Result<()> {
        let ret = unsafe { libc::setpriority(libc::PRIO_PROCESS, pid, nice) };
        if ret != 0 {
            bail!("setpriority: {}", std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn set_ionice(&self, pid: u32, ioclass: IoClass, level: u8) -> Result<()> {
        // ioprio value: (class << 13) | (level & 0x7)
        let ioprio: u32 = (ioclass.as_linux_class() << 13) | (level as u32 & 0x7);
        let ret = unsafe { libc::syscall(libc::SYS_ioprio_set, 1i64, pid as i64, ioprio as i64) };
        if ret != 0 {
            bail!("ioprio_set: {}", std::io::Error::last_os_error());
        }
        Ok(())
    }

    fn set_oom_score_adj(&self, pid: u32, score: i32) -> Result<()> {
        let path = format!("/proc/{pid}/oom_score_adj");
        std::fs::write(&path, format!("{score}\n")).with_context(|| format!("write {path}"))?;
        Ok(())
    }

    /// Move `pid` into the cgroup at `<cgroup_root>/<cgroup>` and optionally
    /// set `cpu.weight`. Creates the cgroup directory if missing.
    fn move_to_cgroup(&self, pid: u32, cgroup: &str, weight: Option<u64>) -> Result<()> {
        let root = Path::new("/sys/fs/cgroup");

        // Reject empty, absolute, or path-traversal inputs.
        // Empty string would resolve to the cgroup root itself.
        if cgroup.is_empty() {
            bail!("Refusing empty cgroup path");
        }
        // Explicit guard: Path::join silently replaces the prefix for absolute inputs.
        if cgroup.starts_with('/') {
            bail!("Refusing absolute cgroup path: '{cgroup}'");
        }
        for component in Path::new(cgroup).components() {
            use std::path::Component;
            if matches!(component, Component::ParentDir | Component::RootDir) {
                bail!("Refusing unsafe cgroup path: '{cgroup}'");
            }
        }

        let cg_path = root.join(cgroup);
        if !cg_path.exists() {
            std::fs::create_dir_all(&cg_path)
                .with_context(|| format!("create cgroup dir: {}", cg_path.display()))?;
        }

        let procs_file = cg_path.join("cgroup.procs");
        std::fs::write(&procs_file, format!("{pid}\n"))
            .with_context(|| format!("write {}", procs_file.display()))?;

        if let Some(w) = weight {
            let w = w.clamp(1, 10_000);
            let weight_file = cg_path.join("cpu.weight");
            if weight_file.exists() {
                std::fs::write(&weight_file, format!("{w}\n"))
                    .with_context(|| format!("write {}", weight_file.display()))?;
            }
        }

        Ok(())
    }
}
