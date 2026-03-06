use std::ffi::{CStr, CString};
use std::fs::File;
use std::io::Write;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};

use crate::config::Config;
use crate::platform::linux::{ScxScheduler, Strategy};
use crate::rules::{IoClass, ProcessContext, ResolvedRule};

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

        if !ctx.matches_current_pid()? {
            // The process exited (or re-exec'd under a new identity) between the exec event
            // and now.  This is the normal TOCTOU race inherent in netlink-based monitoring;
            // it is not an error worth warning about.
            tracing::debug!(
                pid  = ctx.pid,
                rule = %rule.name,
                "process exited before rule could be applied (expected TOCTOU race)"
            );
            return Ok(result);
        }

        let strategy = scheduler.strategy();

        // ── nice ─────────────────────────────────────────────────────────────
        if self.config.apply_nice
            && let Some(mut nice) = rule.nice
        {
            nice = nice.clamp(-20, 19);
            match self.set_nice(ctx.pid, nice) {
                Ok(()) => result.nice_applied = Some(nice),
                Err(e) => tracing::warn!(pid = ctx.pid, "nice={nice} failed: {e}"),
            }
        }

        // ── ionice ───────────────────────────────────────────────────────────
        if self.config.apply_ionice
            && let Some(ioclass) = rule.ioclass
        {
            let level = rule.ionice.unwrap_or(4).clamp(0, 7);
            match self.set_ionice(ctx.pid, ioclass, level) {
                Ok(()) => result.ionice_applied = true,
                Err(e) => tracing::warn!(pid = ctx.pid, "ionice failed: {e}"),
            }
        }

        // ── oom_score_adj ────────────────────────────────────────────────────
        if self.config.apply_oom
            && let Some(mut oom) = rule.oom_score_adj
        {
            oom = oom.clamp(-1000, 1000);
            match self.set_oom_score_adj(ctx.pid, oom) {
                Ok(()) => result.oom_applied = Some(oom),
                Err(e) => tracing::warn!(pid = ctx.pid, "oom_score_adj={oom} failed: {e}"),
            }
        }

        // ── cgroup placement ─────────────────────────────────────────────────
        if self.config.apply_cgroup
            && let Some(ref cgroup) = rule.cgroup
        {
            let weight = Self::effective_cgroup_weight(strategy, rule.cgroup_weight);
            match self.move_to_cgroup(ctx.pid, cgroup, weight) {
                Ok(()) => result.cgroup_applied = Some(cgroup.clone()),
                Err(e) => tracing::warn!(pid = ctx.pid, cgroup, "cgroup move failed: {e}"),
            }
        }

        // ── scx_layered: export layer JSON ───────────────────────────────────
        if strategy == Strategy::LayeredJson
            && let Some(ref path) = self.config.layered_export_path
        {
            // Deferred: layered export collects all rules and writes once.
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
        use std::io::{Cursor, Write as _};
        // Stack-allocate the value string ("-1000\n" = 6 bytes max) to avoid
        // a heap allocation per process event.
        let path = CString::new(format!("/proc/{pid}/oom_score_adj"))
            .expect("proc path is always valid ASCII");
        let mut val_buf = [0u8; 8];
        let val_len = {
            let mut c = Cursor::new(&mut val_buf[..]);
            write!(c, "{score}\n").expect("val_buf too small");
            c.position() as usize
        };
        let flags = libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let fd = unsafe { libc::open(path.as_ptr(), flags) };
        if fd < 0 {
            bail!(
                "open /proc/{pid}/oom_score_adj: {}",
                std::io::Error::last_os_error()
            );
        }
        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(&val_buf[..val_len])
            .with_context(|| format!("write /proc/{pid}/oom_score_adj"))?;
        Ok(())
    }

    /// Move `pid` into the cgroup at `<cgroup_root>/<cgroup>` and optionally
    /// set `cpu.weight`. Creates the cgroup directory if missing.
    fn move_to_cgroup(&self, pid: u32, cgroup: &str, weight: Option<u64>) -> Result<()> {
        use std::io::{Cursor, Write as _};
        let components = Self::validated_cgroup_components(cgroup)?;
        let cg_dir = self
            .open_cgroup_dir(&components)
            .with_context(|| format!("open cgroup dir for '{cgroup}'"))?;

        // Stack-allocate pid and weight strings to avoid heap allocation per event.
        let mut pid_buf = [0u8; 12]; // u32 max (4294967295) + '\n' = 11 bytes
        let pid_len = {
            let mut c = Cursor::new(&mut pid_buf[..]);
            write!(c, "{pid}\n").expect("pid_buf too small");
            c.position() as usize
        };

        self.write_control_file(&cg_dir, c"cgroup.procs", &pid_buf[..pid_len])
            .with_context(|| format!("write cgroup.procs for '{cgroup}'"))?;

        if let Some(w) = weight {
            let w = w.clamp(1, 10_000);
            let mut wgt_buf = [0u8; 8]; // "10000\n" = 6 bytes
            let wgt_len = {
                let mut c = Cursor::new(&mut wgt_buf[..]);
                write!(c, "{w}\n").expect("wgt_buf too small");
                c.position() as usize
            };
            match self.write_control_file(&cg_dir, c"cpu.weight", &wgt_buf[..wgt_len]) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                    tracing::debug!(cgroup, "cpu.weight missing, skipping weight write");
                }
                Err(err) => {
                    return Err(err).with_context(|| format!("write cpu.weight for '{cgroup}'"));
                }
            }
        }

        Ok(())
    }

    fn effective_cgroup_weight(strategy: Strategy, configured_weight: Option<u64>) -> Option<u64> {
        if matches!(strategy, Strategy::NiceAndWeight) {
            configured_weight
        } else {
            None
        }
    }

    fn validated_cgroup_components(cgroup: &str) -> Result<Vec<CString>> {
        if cgroup.is_empty() {
            bail!("Refusing empty cgroup path");
        }
        if cgroup.starts_with('/') {
            bail!("Refusing absolute cgroup path: '{cgroup}'");
        }

        let mut components = Vec::new();
        for component in Path::new(cgroup).components() {
            match component {
                Component::Normal(part) => {
                    let bytes = part.as_bytes();
                    if bytes.is_empty() {
                        bail!("Refusing empty cgroup path component in '{cgroup}'");
                    }
                    components.push(CString::new(bytes).context("cgroup component contains NUL")?);
                }
                Component::CurDir
                | Component::ParentDir
                | Component::RootDir
                | Component::Prefix(_) => {
                    bail!("Refusing unsafe cgroup path: '{cgroup}'");
                }
            }
        }

        if components.is_empty() {
            bail!("Refusing empty cgroup path");
        }

        Ok(components)
    }

    fn open_cgroup_dir(&self, components: &[CString]) -> Result<OwnedFd> {
        let root = CString::new("/sys/fs/cgroup").expect("static path without NUL");
        let root_fd = Self::open_dir_at(libc::AT_FDCWD, &root).context("open /sys/fs/cgroup")?;

        let mut current = root_fd;
        for component in components {
            let mkdir_ret =
                unsafe { libc::mkdirat(current.as_raw_fd(), component.as_ptr(), 0o755) };
            if mkdir_ret != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::EEXIST) {
                    return Err(err).with_context(|| {
                        format!("mkdirat component '{}'", component.to_string_lossy())
                    });
                }
            }

            current = Self::open_dir_at(current.as_raw_fd(), component)
                .with_context(|| format!("openat component '{}'", component.to_string_lossy()))?;
        }

        Ok(current)
    }

    fn open_dir_at(base_fd: libc::c_int, path: &CStr) -> Result<OwnedFd> {
        let flags = libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let fd = unsafe { libc::openat(base_fd, path.as_ptr(), flags) };
        if fd < 0 {
            bail!("openat: {}", std::io::Error::last_os_error());
        }

        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        Ok(owned)
    }

    fn write_control_file(
        &self,
        dir_fd: &OwnedFd,
        name: &CStr,
        contents: &[u8],
    ) -> Result<(), std::io::Error> {
        let flags = libc::O_WRONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW;
        let fd = unsafe { libc::openat(dir_fd.as_raw_fd(), name.as_ptr(), flags) };
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }

        let mut file = unsafe { File::from_raw_fd(fd) };
        file.write_all(contents)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Applier;
    use crate::platform::linux::Strategy;

    #[test]
    fn rejects_unsafe_cgroup_paths() {
        for path in [
            "",
            "/system.slice",
            "../escape",
            "games/../escape",
            "./games",
        ] {
            assert!(
                Applier::validated_cgroup_components(path).is_err(),
                "path {path} should fail"
            );
        }
    }

    #[test]
    fn accepts_nested_relative_cgroup_paths() {
        let parts = Applier::validated_cgroup_components("apps/games.slice").unwrap();
        let text: Vec<String> = parts
            .iter()
            .map(|part| part.to_string_lossy().into_owned())
            .collect();
        assert_eq!(text, vec!["apps".to_string(), "games.slice".to_string()]);
    }

    #[test]
    fn only_nice_and_weight_strategy_writes_weight() {
        assert_eq!(
            Applier::effective_cgroup_weight(Strategy::NiceAndWeight, Some(900)),
            Some(900)
        );
        assert_eq!(
            Applier::effective_cgroup_weight(Strategy::BasicHints, Some(900)),
            None
        );
        assert_eq!(
            Applier::effective_cgroup_weight(Strategy::LayeredJson, Some(900)),
            None
        );
    }
}
