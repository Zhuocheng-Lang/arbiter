use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, RwLock, Semaphore};

use crate::applier::Applier;
use crate::platform::linux::{ProcEvent, ScxScheduler};
use crate::rules::{Matcher, RuleSet};

use super::load_process_context;

/// Maximum number of pending exec events buffered before new events are
/// dropped under sustained burst load.
pub(crate) const EXEC_QUEUE_CAPACITY: usize = 2048;
/// Maximum number of exec events processed concurrently after dequeue.
const EXEC_CONCURRENCY: usize = 32;

pub(crate) enum DaemonMessage {
    Proc(ProcEvent),
    ReloadRules,
    Shutdown,
}

pub(crate) struct DaemonActor {
    rules_dirs: Vec<PathBuf>,
    matcher: Arc<RwLock<Matcher>>,
    applier: Arc<Applier>,
    scheduler: Arc<ScxScheduler>,
    exec_delay_ms: u64,
}

impl DaemonActor {
    pub(crate) fn new(
        rules_dirs: Vec<PathBuf>,
        exec_delay_ms: u64,
        matcher: Arc<RwLock<Matcher>>,
        applier: Arc<Applier>,
        scheduler: Arc<ScxScheduler>,
    ) -> Self {
        Self {
            rules_dirs,
            matcher,
            applier,
            scheduler,
            exec_delay_ms,
        }
    }

    pub(crate) fn matcher(&self) -> Arc<RwLock<Matcher>> {
        Arc::clone(&self.matcher)
    }

    pub(crate) fn applier(&self) -> Arc<Applier> {
        Arc::clone(&self.applier)
    }

    pub(crate) fn scheduler_arc(&self) -> Arc<ScxScheduler> {
        Arc::clone(&self.scheduler)
    }

    pub(crate) fn scheduler(&self) -> &ScxScheduler {
        self.scheduler.as_ref()
    }

    pub(crate) fn exec_delay_ms(&self) -> u64 {
        self.exec_delay_ms
    }

    pub(crate) async fn handle_message(
        &self,
        message: DaemonMessage,
        exec_tx: &mpsc::Sender<u32>,
    ) -> bool {
        match message {
            DaemonMessage::Proc(event) => {
                self.handle_proc_event(event, exec_tx);
                true
            }
            DaemonMessage::ReloadRules => {
                self.reload_rules().await;
                true
            }
            DaemonMessage::Shutdown => false,
        }
    }

    fn handle_proc_event(&self, event: ProcEvent, exec_tx: &mpsc::Sender<u32>) {
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

    async fn reload_rules(&self) {
        tracing::info!("SIGHUP received — reloading rules");

        let ruleset = match RuleSet::load_from_dirs(&self.rules_dirs) {
            Ok(ruleset) => ruleset,
            Err(e) => {
                tracing::error!("Rule reload failed, keeping existing rules: {e}");
                return;
            }
        };

        let resolved = match ruleset.validate() {
            Ok(resolved) => resolved,
            Err(e) => {
                tracing::error!("Rule reload validation failed, keeping existing rules: {e}");
                return;
            }
        };

        let count = resolved.len();
        *self.matcher.write().await = Matcher::new(resolved);
        tracing::info!(count, "Rules reloaded");
    }
}

pub(crate) fn spawn_exec_dispatcher(
    mut exec_rx: mpsc::Receiver<u32>,
    matcher: Arc<RwLock<Matcher>>,
    applier: Arc<Applier>,
    scheduler: Arc<ScxScheduler>,
    delay_ms: u64,
) -> tokio::task::JoinHandle<()> {
    let sem = Arc::new(Semaphore::new(EXEC_CONCURRENCY));

    tokio::spawn(async move {
        while let Some(pid) = exec_rx.recv().await {
            let permit = match Arc::clone(&sem).acquire_owned().await {
                Ok(permit) => permit,
                Err(_) => break,
            };

            let matcher = Arc::clone(&matcher);
            let applier = Arc::clone(&applier);
            let scheduler = Arc::clone(&scheduler);

            tokio::spawn(async move {
                let _permit = permit;
                process_exec(pid, delay_ms, matcher, applier, scheduler).await;
            });
        }
    })
}

async fn process_exec(
    pid: u32,
    delay_ms: u64,
    matcher: Arc<RwLock<Matcher>>,
    applier: Arc<Applier>,
    scheduler: Arc<ScxScheduler>,
) {
    let ctx = match load_process_context(pid, delay_ms).await {
        Ok(ctx) => ctx,
        Err(e) => {
            tracing::trace!(pid, "proc read failed: {e}");
            return;
        }
    };

    let rule = {
        let guard = matcher.read().await;
        guard.find_match(&ctx)
    };

    let Some(rule) = rule else {
        tracing::debug!(pid, comm = %ctx.comm, "no rule matched");
        return;
    };

    if let Err(e) = applier.apply(&ctx, rule.as_ref(), &scheduler) {
        tracing::warn!(pid, "apply failed: {e}");
    }
}
