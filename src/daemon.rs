use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::mpsc::error::TrySendError;
use tokio::sync::{RwLock, mpsc};

use crate::applier::Applier;
use crate::config::Config;
use crate::matcher::{Matcher, ProcessContext};
use crate::proc_events::{ProcEvent, start_event_stream};
use crate::rules::RuleSet;
use crate::scx;

const EXEC_QUEUE_CAPACITY: usize = 2048;
const EXEC_WORKERS: usize = 32;

// ── Daemon ────────────────────────────────────────────────────────────────────

pub struct Daemon {
    config: Config,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<()> {
        // ── load rules ────────────────────────────────────────────────────────
        let ruleset = RuleSet::load_from_dirs(&self.config.rules_dirs)?;
        let resolved = ruleset.validate()?;
        tracing::info!(count = resolved.len(), "Rules loaded");

        // ── build shared components ───────────────────────────────────────────
        let matcher = Arc::new(RwLock::new(Matcher::new(resolved)));
        let applier = Arc::new(Applier::new(self.config.clone()));
        let scheduler = Arc::new(scx::detect());
        let delay_ms = self.config.exec_delay_ms;
        let rules_dirs = self.config.rules_dirs.clone();

        tracing::info!(scheduler = %scheduler, profile = %self.config.profile, "Arbiter starting");

        // ── open proc-connector channel ───────────────────────────────────────
        let (tx, mut rx) = mpsc::channel::<ProcEvent>(2048);
        start_event_stream(tx).await?;

        // ── exec workers ──────────────────────────────────────────────────────
        let (exec_tx, exec_rx) = mpsc::channel::<u32>(EXEC_QUEUE_CAPACITY);
        let exec_rx = Arc::new(tokio::sync::Mutex::new(exec_rx));

        for _ in 0..EXEC_WORKERS {
            let m = Arc::clone(&matcher);
            let a = Arc::clone(&applier);
            let s = Arc::clone(&scheduler);
            let exec_rx = Arc::clone(&exec_rx);

            tokio::spawn(async move {
                loop {
                    let pid = {
                        let mut guard = exec_rx.lock().await;
                        match guard.recv().await {
                            Some(pid) => pid,
                            None => break,
                        }
                    };

                    // Give the process a moment to finish execve and populate
                    // /proc/<pid>/comm, exe, cmdline.
                    if delay_ms > 0 {
                        tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                    }

                    match ProcessContext::from_pid(pid) {
                        Ok(ctx) => {
                            let rule = {
                                let guard = m.read().await;
                                guard.find_match(&ctx).cloned()
                            };
                            if let Some(rule) = rule {
                                if let Err(e) = a.apply(&ctx, &rule, &s) {
                                    tracing::warn!(pid, "apply failed: {e}");
                                }
                            } else {
                                tracing::debug!(
                                    pid,
                                    comm = %ctx.comm,
                                    "no rule matched"
                                );
                            }
                        }
                        Err(e) => {
                            tracing::trace!(pid, "proc read failed: {e}");
                        }
                    }
                }
            });
        }

        // ── signal handlers ───────────────────────────────────────────────────
        let mut sig_term = signal(SignalKind::terminate())?;
        let mut sig_int = signal(SignalKind::interrupt())?;
        let mut sig_hup = signal(SignalKind::hangup())?;

        tracing::info!("Daemon running — waiting for process events");

        // ── main loop ─────────────────────────────────────────────────────────
        loop {
            tokio::select! {
                Some(event) = rx.recv() => {
                    match event {
                        ProcEvent::Exec { pid, .. } => {
                            match exec_tx.try_send(pid) {
                                Ok(()) => {}
                                Err(TrySendError::Full(_)) => {
                                    tracing::warn!(
                                        pid,
                                        capacity = EXEC_QUEUE_CAPACITY,
                                        "exec worker queue full; dropping process event"
                                    );
                                }
                                Err(TrySendError::Closed(_)) => {
                                    tracing::error!("exec worker queue closed unexpectedly");
                                    break;
                                }
                            }
                        }

                        ProcEvent::Fork { child_pid, .. } => {
                            tracing::trace!(pid = child_pid, "fork");
                        }

                        ProcEvent::Exit { pid, exit_code } => {
                            tracing::trace!(pid, exit_code, "exit");
                        }
                    }
                }

                _ = sig_hup.recv() => {
                    tracing::info!("SIGHUP received — reloading rules");
                    match RuleSet::load_from_dirs(&rules_dirs) {
                        Ok(rs) => {
                            let resolved = match rs.validate() {
                                Ok(resolved) => resolved,
                                Err(e) => {
                                    tracing::error!("Rule reload validation failed, keeping existing rules: {e}");
                                    continue;
                                }
                            };
                            let count = resolved.len();
                            *matcher.write().await = Matcher::new(resolved);
                            tracing::info!(count, "Rules reloaded");
                        }
                        Err(e) => tracing::error!("Rule reload failed, keeping existing rules: {e}"),
                    }
                }

                _ = sig_term.recv() => {
                    tracing::info!("SIGTERM received — shutting down");
                    break;
                }

                _ = sig_int.recv() => {
                    tracing::info!("SIGINT received — shutting down");
                    break;
                }
            }
        }

        drop(exec_tx);

        Ok(())
    }
}
