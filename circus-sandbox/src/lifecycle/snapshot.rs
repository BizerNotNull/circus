//! Serialized workspace-only capture and staging replacement.

use super::*;
use crate::workspace::{CodeSnapshot, SnapshotBudget, SnapshotError};

impl DockerSandbox {
    /// Captures regular workspace files and directories, not processes or memory.
    ///
    /// In `Created`, captures host staging. In `Running`, the lifecycle worker
    /// pauses the guest, downloads only the configured tmpfs workspace through
    /// Docker's archive API, validates it, and resumes before returning (also on
    /// archive validation failure). An already `Paused` guest stays paused.
    /// Other phases return [`SandboxError::InvalidPhase`].
    ///
    /// `budget` bounds retained file payload and entries. `max_archive_bytes`
    /// must be positive and separately bounds archive transport, including tar
    /// overhead. A zero archive bound fails before queueing. All operations are
    /// serialized with other sandbox mutations. The configured file timeout
    /// covers pause, download, parsing and resume together. Cancellation or
    /// timeout uses whole-container cleanup; failed cleanup retains ownership.
    /// Engine pause, download or resume failures also trigger owned cleanup.
    /// Archive/domain failures are matchable as [`SandboxError::Snapshot`],
    /// including any temporary workspace whose explicit cleanup failed.
    /// A deadline initiates cancellation, not a bound on cleanup completion:
    /// already-started blocking filesystem work is drained before cleanup.
    /// [`SandboxError::SnapshotInterrupted`] preserves both an interruption
    /// and any snapshot failure, including owned temporary cleanup failures.
    pub async fn capture_workspace_snapshot(
        &self,
        budget: SnapshotBudget,
        max_archive_bytes: u64,
    ) -> Result<Arc<CodeSnapshot>, SandboxError> {
        if max_archive_bytes == 0 {
            return Err(SnapshotError::InvalidArchiveLimit.into());
        }
        self.call(|reply| Op::CaptureSnapshot {
            budget,
            max_archive_bytes,
            reply,
        })
        .await?
    }

    /// Replaces `Created` staging with this snapshot for the next `start`.
    ///
    /// Running, paused, destroyed and cleanup-required sandboxes reject this
    /// operation without modification. The target's configured per-file and
    /// workspace payload bounds apply, not the source snapshot's limits. The
    /// queued payload shares the supplied `Arc`; it is not deep-cloned.
    ///
    /// Validation and creation happen before old staging is removed. If old
    /// staging cleanup fails, the old workspace remains installed (its cleanup
    /// may have partially removed files), the replacement is explicitly cleaned,
    /// and [`SandboxError::Snapshot`] reports the failure. If replacement
    /// cleanup also fails, the nested [`SnapshotError::RestoreCleanup`] owns it
    /// for retry. Successful replacement commits only after old cleanup succeeds.
    /// Uses the configured file timeout and serialized worker; cancellation or
    /// timeout cleans the whole owned container and staging, retaining ownership
    /// when cleanup cannot be confirmed. Already-started filesystem work is
    /// drained before cleanup; completion may exceed the deadline. Staging
    /// installation is worker-owned and never occurs after interruption.
    pub async fn restore_workspace_snapshot(
        &self,
        snapshot: Arc<CodeSnapshot>,
    ) -> Result<(), SandboxError> {
        self.call(|reply| Op::RestoreSnapshot { snapshot, reply })
            .await?
    }
}

struct Deadline {
    cancel: CancellationToken,
    shutdown: CancellationToken,
    at: tokio::time::Instant,
}

impl Deadline {
    fn new(state: &State, cancel: &CancellationToken) -> Self {
        Self {
            cancel: cancel.clone(),
            shutdown: state.shutdown.clone(),
            at: tokio::time::Instant::now() + state.config.file_timeout(),
        }
    }

    async fn network<T>(&self, future: impl Future<Output = T>) -> Result<T, bool> {
        tokio::select! {
            biased;
            () = self.cancel.cancelled() => Err(true),
            () = self.shutdown.cancelled() => Err(true),
            () = tokio::time::sleep_until(self.at) => Err(false),
            value = future => Ok(value),
        }
    }

    async fn blocking<T: Send + 'static>(
        &self,
        work: impl FnOnce() -> T + Send + 'static,
    ) -> (Result<T, SandboxError>, Option<bool>) {
        let mut task = tokio::task::spawn_blocking(work);
        let (result, interrupted) = match self.network(&mut task).await {
            Ok(result) => (result, None),
            Err(cancelled) => (task.await, Some(cancelled)),
        };
        (
            result.map_err(|err| SandboxError::WorkerGone {
                detail: err.to_string(),
            }),
            interrupted,
        )
    }
}

async fn interrupted<T>(
    state: &mut State,
    cancelled: bool,
    snapshot: Option<SnapshotError>,
) -> Result<T, SandboxError> {
    let report = destroy_report(state).await;
    let interruption = if cancelled {
        SandboxError::Cancelled {
            container_id: report.container_id,
            ownership: report.ownership,
            detail: report.detail,
        }
    } else {
        SandboxError::TimedOut {
            container_id: report.container_id,
            ownership: report.ownership,
            detail: report.detail,
        }
    };
    Err(match snapshot {
        Some(snapshot) => SandboxError::SnapshotInterrupted {
            interruption: Box::new(interruption),
            snapshot: Box::new(snapshot),
        },
        None => interruption,
    })
}

pub(super) async fn capture(
    state: &mut State,
    budget: SnapshotBudget,
    max_archive_bytes: u64,
    cancel: &CancellationToken,
) -> Result<Arc<CodeSnapshot>, SandboxError> {
    let deadline = Deadline::new(state, cancel);
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let (result, stopped) = deadline
            .blocking(move || {
                workspace
                    .lock()
                    .unwrap_or_else(|err| err.into_inner())
                    .capture_code_snapshot(budget)
            })
            .await;
        if let Some(cancelled) = stopped {
            return interrupted(state, cancelled, result.ok().and_then(Result::err)).await;
        }
        return Ok(Arc::new(result??));
    }
    if !matches!(state.phase, SandboxPhase::Running | SandboxPhase::Paused) {
        return Err(phase_err(state, "capture_workspace_snapshot"));
    }
    let resume_after = state.phase == SandboxPhase::Running;
    if resume_after {
        match deadline.network(pause(state)).await {
            Ok(result) => result?,
            Err(cancelled) => return interrupted(state, cancelled, None).await,
        }
    }
    let id = required_id(state)?;
    let path = state.config.isolation().workspace_path().to_owned();
    let archive = match deadline
        .network(
            state
                .engine
                .download_workspace_archive(&id, &path, max_archive_bytes),
        )
        .await
    {
        Ok(Ok(archive)) => archive,
        Ok(Err(err)) => return cleanup_engine_error(state, err).await,
        Err(cancelled) => return interrupted(state, cancelled, None).await,
    };
    let root = path.rsplit('/').next().unwrap_or_default().to_owned();
    let max_file = state.config.limits().max_file_bytes();
    let (result, stopped) = deadline
        .blocking(move || CodeSnapshot::from_workspace_archive(archive, &root, max_file, budget))
        .await;
    if let Some(cancelled) = stopped {
        return interrupted(state, cancelled, result.ok().and_then(Result::err)).await;
    }
    // Keep a domain error owned until resume has succeeded or failed.
    if resume_after {
        match deadline.network(resume(state)).await {
            Ok(Ok(())) => {}
            Ok(Err(err)) => {
                return match result {
                    Ok(Err(snapshot)) => Err(SandboxError::SnapshotInterrupted {
                        interruption: Box::new(err),
                        snapshot: Box::new(snapshot),
                    }),
                    _ => Err(err),
                };
            }
            Err(cancelled) => {
                return interrupted(state, cancelled, result.ok().and_then(Result::err)).await;
            }
        }
    }
    Ok(Arc::new(result??))
}

pub(super) async fn restore(
    state: &mut State,
    snapshot: Arc<CodeSnapshot>,
    cancel: &CancellationToken,
) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Created, "restore_workspace_snapshot")?;
    let deadline = Deadline::new(state, cancel);
    let max_file = state.config.limits().max_file_bytes();
    let max_total = usize::try_from(state.config.limits().workspace_bytes()).unwrap_or(usize::MAX);
    let (result, stopped) = deadline
        .blocking(move || snapshot.restore_with_limits(max_file, max_total))
        .await;
    if let Some(cancelled) = stopped {
        let error = match result {
            Ok(Err(err)) => Some(err),
            Ok(Ok(replacement)) => cleanup_replacement(replacement, None).await,
            Err(_) => None,
        };
        return interrupted(state, cancelled, error).await;
    }
    let replacement = result??;
    let workspace = Arc::clone(&state.workspace);
    let (cleaned, stopped) = deadline
        .blocking(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .cleanup()
        })
        .await;
    if let Some(cancelled) = stopped {
        let cause = cleaned
            .ok()
            .and_then(Result::err)
            .map(SnapshotError::Workspace);
        let error = cleanup_replacement(replacement, cause).await;
        return interrupted(state, cancelled, error).await;
    }
    match cleaned {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            let error = cleanup_replacement(replacement, Some(err.into())).await;
            return Err(error.expect("cleanup cause is retained").into());
        }
        Err(error) => {
            return match cleanup_replacement(replacement, None).await {
                Some(snapshot) => Err(SandboxError::SnapshotInterrupted {
                    interruption: Box::new(error),
                    snapshot: Box::new(snapshot),
                }),
                None => Err(error),
            };
        }
    }
    // Only the lifecycle worker installs staging, never a detached blocking task.
    *state
        .workspace
        .lock()
        .unwrap_or_else(|err| err.into_inner()) = replacement;
    Ok(())
}

async fn cleanup_replacement(
    mut replacement: Workspace,
    cause: Option<SnapshotError>,
) -> Option<SnapshotError> {
    match spawn_block(move || match replacement.cleanup() {
        Ok(()) => cause,
        Err(cleanup) => Some(SnapshotError::RestoreCleanup {
            restore: Box::new(cause.unwrap_or(SnapshotError::Interrupted)),
            destination: Box::new(replacement),
            cleanup,
        }),
    })
    .await
    {
        Ok(error) => error,
        // A panicking filesystem task has no recoverable workspace value.
        Err(err) => Some(SnapshotError::InvalidArchive {
            reason: err.to_string(),
        }),
    }
}
