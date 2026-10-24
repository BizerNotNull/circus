//! Cleanup receipts observe the lifecycle worker, never own its task.

use std::time::Duration;

use tokio::sync::{oneshot, watch};

use super::{CleanupReport, Ownership, SandboxError, SandboxPhase, State, destroy_report};

#[derive(Debug, Clone)]
pub(super) struct Progress {
    pub(super) report: CleanupReport,
    pub(super) complete: bool,
    pub(super) attempted: bool,
}

/// Observation of container and staging cleanup by the lifecycle worker.
///
/// The lifecycle worker retains ownership of the container and local staging and
/// retries cleanup independently of all receipts. Cloning or dropping a receipt,
/// or cancelling [`Self::wait`], never cancels that worker. This is an in-process
/// receipt, not a durable cleanup guarantee across process or runtime shutdown.
#[derive(Debug, Clone)]
pub struct CleanupReceipt {
    progress: watch::Receiver<Progress>,
}

impl CleanupReceipt {
    ///
    /// The initial `Unknown` report means cleanup has not been attempted; it is
    /// not itself evidence of a failed attempt. Use [`Self::wait`] for proof
    /// that both container absence and staging removal completed.
    pub fn status(&self) -> CleanupReport {
        self.progress.borrow().report.clone()
    }

    /// Returns whether the worker has published at least one cleanup attempt.
    pub fn attempted(&self) -> bool {
        self.progress.borrow().attempted
    }

    /// Returns whether the most recently published attempt failed.
    ///
    /// Before the first attempt this is `false`; an incomplete report with
    /// ownership confirmed absent is also not classified as a failed attempt.
    pub fn last_attempt_failed(&self) -> bool {
        let progress = self.progress.borrow();
        progress.attempted
            && !progress.complete
            && progress.report.ownership != Ownership::ConfirmedAbsent
    }

    /// Waits until the worker confirms container absence and staging removal.
    ///
    /// Each call observes independently, so a cancelled wait can be retried and
    /// cloned receipts can wait concurrently. There is no implicit deadline.
    /// Returns [`SandboxError::WorkerGone`] if the worker ends without confirming
    /// full cleanup. A final confirmed result remains available after it exits.
    pub async fn wait(&self) -> Result<CleanupReport, SandboxError> {
        let mut progress = self.progress.clone();
        loop {
            {
                let current = progress.borrow_and_update();
                if current.complete {
                    return Ok(current.report.clone());
                }
            }
            progress.changed().await.map_err(|_| SandboxError::WorkerGone {
                detail: "cleanup worker ended before confirming container and staging cleanup"
                    .to_string(),
            })?;
        }
    }
}

pub(super) fn channel(name: String) -> (watch::Sender<Progress>, CleanupReceipt) {
    let (sender, progress) = watch::channel(Progress {
        report: CleanupReport {
            container_id: Some(name),
            ownership: Ownership::Unknown,
            detail: "creation pending; cleanup has not been attempted".to_string(),
        },
        complete: false,
        attempted: false,
    });
    (sender, CleanupReceipt { progress })
}

pub(super) async fn failed_creation(
    state: &mut State,
    ready: oneshot::Sender<Result<(), SandboxError>>,
    source: SandboxError,
    initial_report: Option<CleanupReport>,
) {
    // Preserve the identity even if this first attempt completes cleanup.
    let container_id = state.id.clone().unwrap_or_else(|| state.name.clone());
    if initial_report.is_none() {
        destroy_report(state).await;
    }
    let _ = ready.send(Err(SandboxError::CreationCleanupRequired {
        container_id,
        source: Box::new(source),
        cleanup: CleanupReceipt { progress: state.cleanup.subscribe() },
    }));
    // Neither a dropped ready receiver nor the last receipt stops this loop.
    retry_cleanup(state, true).await;
}

pub(super) async fn cleanup_until_done(state: &mut State) {
    retry_cleanup(state, false).await;
}

async fn retry_cleanup(state: &mut State, mut attempted: bool) {
    let mut delay = Duration::from_millis(100);
    while state.phase != SandboxPhase::Destroyed {
        if attempted {
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(30));
        }
        destroy_report(state).await;
        attempted = true;
    }
}
