use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::{mpsc, RwLock};

use crate::applier::Applier;
use crate::config::Config;
use crate::platform::linux::{self, start_event_stream, ProcEvent};
use crate::rules::{Matcher, ProcessContext, RuleSet};

mod actor;

use actor::{spawn_exec_dispatcher, DaemonActor, DaemonMessage, EXEC_QUEUE_CAPACITY};

// ── Daemon ────────────────────────────────────────────────────────────────────

pub struct Daemon {
    config: Config,
}

impl Daemon {
    pub fn new(config: Config) -> Self {
        Self { config }
    }

    pub async fn run(self) -> Result<()> {
        let ruleset = RuleSet::load_from_dirs(&self.config.rules_dirs)?;
        let resolved = ruleset.validate()?;
        tracing::info!(count = resolved.len(), "Rules loaded");

        let matcher = Arc::new(RwLock::new(Matcher::new(resolved)));
        let applier = Arc::new(Applier::new(self.config.clone()));
        let scheduler = Arc::new(linux::detect());
        let actor = DaemonActor::new(
            self.config.rules_dirs.clone(),
            self.config.exec_delay_ms,
            matcher,
            applier,
            scheduler,
        );

        tracing::info!(scheduler = %actor.scheduler(), profile = %self.config.profile, "Arbiter starting");

        let (proc_tx, mut proc_rx) = mpsc::channel::<ProcEvent>(EXEC_QUEUE_CAPACITY);
        start_event_stream(proc_tx).await?;

        let (exec_tx, exec_rx) = mpsc::channel::<u32>(EXEC_QUEUE_CAPACITY);
        let exec_worker = spawn_exec_dispatcher(
            exec_rx,
            actor.matcher(),
            actor.applier(),
            actor.scheduler_arc(),
            actor.exec_delay_ms(),
        );

        let mut sig_term = signal(SignalKind::terminate())?;
        let mut sig_int = signal(SignalKind::interrupt())?;
        let mut sig_hup = signal(SignalKind::hangup())?;

        tracing::info!("Daemon running — waiting for process events");

        loop {
            tokio::select! {
                Some(event) = proc_rx.recv() => {
                    if !actor.handle_message(DaemonMessage::Proc(event), &exec_tx).await {
                        break;
                    }
                }

                _ = sig_hup.recv() => {
                    actor.handle_message(DaemonMessage::ReloadRules, &exec_tx).await;
                }

                _ = sig_term.recv() => {
                    tracing::info!("SIGTERM received — shutting down");
                    if !actor.handle_message(DaemonMessage::Shutdown, &exec_tx).await {
                        break;
                    }
                }

                _ = sig_int.recv() => {
                    tracing::info!("SIGINT received — shutting down");
                    if !actor.handle_message(DaemonMessage::Shutdown, &exec_tx).await {
                        break;
                    }
                }
            }
        }

        drop(exec_tx);
        let _ = exec_worker.await;

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
