use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};
use tokio::sync::{RwLock, Semaphore, mpsc};

use crate::applier::Applier;
use crate::config::Config;
use crate::platform::linux::{self, ProcEvent, start_event_stream};
use crate::rules::{Matcher, ProcessContext, RuleSet};

/// Maximum number of pending exec events buffered before new events are
/// dropped under sustained burst load.
const EXEC_QUEUE_CAPACITY: usize = 2048;
/// Maximum number of exec events processed concurrently after dequeue.
const EXEC_CONCURRENCY: usize = 32;

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
        let scheduler = Arc::new(linux::detect());
        let delay_ms = self.config.exec_delay_ms;
        let rules_dirs = self.config.rules_dirs.clone();

        tracing::info!(scheduler = %scheduler, profile = %self.config.profile, "Arbiter starting");

        // ── open proc-connector channel ───────────────────────────────────────
        let (tx, mut rx) = mpsc::channel::<ProcEvent>(EXEC_QUEUE_CAPACITY);
        start_event_stream(tx).await?;

        // ── exec queue + concurrency limiter ─────────────────────────────────
        // Keep the old bounded-queue semantics so short bursts can be absorbed
        // without dropping immediately, but replace the contended
        // Arc<Mutex<Receiver>> worker pattern with a single dequeue task and a
        // semaphore-limited spawn model.
        let (exec_tx, mut exec_rx) = mpsc::channel::<u32>(EXEC_QUEUE_CAPACITY);
        let sem = Arc::new(Semaphore::new(EXEC_CONCURRENCY));
        {
            let m = Arc::clone(&matcher);
            let a = Arc::clone(&applier);
            let s = Arc::clone(&scheduler);
            let sem = Arc::clone(&sem);

            tokio::spawn(async move {
                while let Some(pid) = exec_rx.recv().await {
                    let permit = match Arc::clone(&sem).acquire_owned().await {
                        Ok(permit) => permit,
                        Err(_) => break,
                    };

                    let m = Arc::clone(&m);
                    let a = Arc::clone(&a);
                    let s = Arc::clone(&s);
                    tokio::spawn(async move {
                        let _permit = permit;

                        match load_process_context(pid, delay_ms).await {
                            Ok(ctx) => {
                                let rule = {
                                    let guard = m.read().await;
                                    guard.find_match(&ctx)
                                };
                                if let Some(rule) = rule {
                                    if let Err(e) = a.apply(&ctx, rule.as_ref(), &s) {
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
                    });
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
                            if exec_tx.try_send(pid).is_err() {
                                    tracing::warn!(
                                        pid,
                                        capacity = EXEC_QUEUE_CAPACITY,
                                        "exec queue full; dropping process event"
                                    );
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

async fn load_process_context(pid: u32, max_wait_ms: u64) -> Result<ProcessContext> {
    let mut waited_ms = 0u64;
    let mut backoff_ms = 1u64;
    let mut last_partial = None;

    loop {
        match ProcessContext::from_pid(pid) {
            Ok(ctx) => {
                if waited_ms >= max_wait_ms || (ctx.exe.is_some() && ctx.cmdline.is_some()) {
                    return Ok(ctx);
                }
                last_partial = Some(ctx);
            }
            Err(err) => {
                if waited_ms >= max_wait_ms {
                    return last_partial.ok_or(err);
                }
            }
        }

        let remaining_ms = max_wait_ms.saturating_sub(waited_ms);
        if remaining_ms == 0 {
            return last_partial.ok_or_else(|| {
                anyhow::anyhow!("process context for pid {pid} was unavailable within retry budget")
            });
        }

        let sleep_ms = backoff_ms.min(remaining_ms);
        tokio::time::sleep(Duration::from_millis(sleep_ms)).await;
        waited_ms += sleep_ms;
        backoff_ms = (backoff_ms.saturating_mul(2)).min(8);
    }
}
