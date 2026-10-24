//! Lifecycle owner for one Linux Docker sandbox.

mod config;
mod input;
mod cleanup;
mod snapshot;

pub use input::{CommandError, StagingError};
pub use cleanup::CleanupReceipt;
use cleanup::{cleanup_until_done, failed_creation};

pub use config::{
    ConfigError, DockerEndpoint, IoLimits, IsolationConfig, OWNER_LABEL_KEY, RuntimeName,
    SandboxConfig,
};

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::infrastructure::{
    CreateSpec, Engine, EngineError, EngineErrorKind, Recovered, SocketKind, StagedBytes,
};
use crate::workspace::{
    CodeSnapshot, SnapshotBudget, SnapshotError, StagedNode, Workspace, WorkspaceError,
};

const QUEUE: usize = 8;

/// Lifecycle phase. Only the worker mutates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPhase {
    Created,
    Running,
    Paused,
    Destroyed,
    CleanupRequired,
}

/// Where the owned container is after a cleanup attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    ConfirmedAbsent,
    StillPresent,
    Unknown,
}

/// Cleanup outcome. Success is never implied when ownership is not confirmed absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    pub container_id: Option<String>,
    pub ownership: Ownership,
    pub detail: String,
}

/// Engine-reported enforcement, not the request that was sent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStatus {
    pub phase: SandboxPhase,
    pub container_id: Option<String>,
    pub runtime: String,
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub pids_limit: i64,
    pub readonly_root: bool,
    pub network_mode: String,
    pub cap_drop: Vec<String>,
    pub security_opt: Vec<String>,
    pub bind_count: usize,
    pub tmpfs_workspace: Option<String>,
}

/// One exec. `argv` is passed as separate arguments, not a shell string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRequest {
    /// Separate arguments; must include at least the executable.
    pub argv: Vec<String>,
    /// Non-zero command deadline.
    pub timeout: Duration,
    /// Non-zero captured stdout/stderr byte bound.
    pub output_limit_bytes: usize,
}

/// Finished exec. A non-zero exit code is still `Ok`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutput {
    pub exit_code: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// Matchable failure. Backend SDK types are not part of this enum.
#[derive(Debug, thiserror::Error)]
pub enum SandboxError {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    /// Workspace snapshot failure, including retained temporary cleanup ownership.
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    /// An interrupted snapshot also failed explicit temporary workspace cleanup.
    #[error("{interruption}; snapshot cleanup: {snapshot}")]
    SnapshotInterrupted {
        interruption: Box<SandboxError>,
        #[source]
        snapshot: Box<SnapshotError>,
    },
    /// Invalid command input. No command is queued and ownership is unchanged.
    #[error("invalid command request: {0}")]
    Command(#[from] CommandError),
    /// Staged content exceeds its byte bound or cannot be counted.
    #[error("workspace staging failed: {0}")]
    Staging(#[from] StagingError),
    #[error("phase {current:?} cannot {operation}")]
    InvalidPhase {
        current: SandboxPhase,
        operation: &'static str,
    },
    #[error("local image was not found; this crate does not pull: {image}")]
    ImageNotFound { image: String },
    #[error("{message}")]
    Engine { message: String },
    #[error("command timed out; container ownership is {ownership:?}: {detail}")]
    TimedOut {
        container_id: Option<String>,
        ownership: Ownership,
        detail: String,
    },
    #[error("command cancelled; container ownership is {ownership:?}: {detail}")]
    Cancelled {
        container_id: Option<String>,
        ownership: Ownership,
        detail: String,
    },
    #[error("output exceeded {limit} bytes; container ownership is {ownership:?}")]
    OutputLimit {
        limit: usize,
        ownership: Ownership,
        detail: String,
    },
    /// Creation failed without returning a sandbox handle. The worker retains
    /// container and staging ownership; await `cleanup.wait()` before reusing
    /// capacity. Dropping the receipt does not stop cleanup.
    #[error("creation failed; cleanup for container {container_id} continues: {source}")]
    CreationCleanupRequired {
        container_id: String,
        #[source]
        source: Box<SandboxError>,
        cleanup: CleanupReceipt,
    },
    #[error("cleanup failed; container {container_id} is still owned: {detail}")]
    CleanupRequired {
        container_id: String,
        detail: String,
    },
    #[error("sandbox worker is gone: {detail}")]
    WorkerGone { detail: String },
    #[error("relative path rejected: {reason}")]
    RelativePath { reason: &'static str },
}

struct CommandGuard {
    token: CancellationToken,
    armed: bool,
}

impl Drop for CommandGuard {
    fn drop(&mut self) {
        if self.armed {
            self.token.cancel();
        }
    }
}

enum Op {
    Start(oneshot::Sender<Result<(), SandboxError>>),
    Pause(oneshot::Sender<Result<(), SandboxError>>),
    Resume(oneshot::Sender<Result<(), SandboxError>>),
    Destroy(oneshot::Sender<Result<(), SandboxError>>),
    Write {
        path: String,
        bytes: Vec<u8>,
        reply: oneshot::Sender<Result<(), SandboxError>>,
    },
    Read {
        path: String,
        reply: oneshot::Sender<Result<Vec<u8>, SandboxError>>,
    },
    Mkdir {
        path: String,
        reply: oneshot::Sender<Result<(), SandboxError>>,
    },
    Unlink {
        path: String,
        reply: oneshot::Sender<Result<(), SandboxError>>,
    },
    Exec {
        request: CommandRequest,
        reply: oneshot::Sender<Result<CommandOutput, SandboxError>>,
    },
    Status(oneshot::Sender<Result<SandboxStatus, SandboxError>>),
    CaptureSnapshot {
        budget: SnapshotBudget,
        max_archive_bytes: u64,
        reply: oneshot::Sender<Result<Arc<CodeSnapshot>, SandboxError>>,
    },
    RestoreSnapshot {
        snapshot: Arc<CodeSnapshot>,
        reply: oneshot::Sender<Result<(), SandboxError>>,
    },
}

struct Message {
    op: Op,
    cancel: CancellationToken,
}

/// One owned sandbox. Drop requests cleanup without claiming cleanup succeeded.
pub struct DockerSandbox {
    tx: mpsc::Sender<Message>,
    shutdown: CancellationToken,
    cleanup: CleanupReceipt,
    max_file_bytes: usize,
}

impl DockerSandbox {
    /// Interrupts queued or running operations and observes full owned cleanup.
    pub(crate) fn revoke(&self) -> CleanupReceipt {
        self.shutdown.cancel();
        self.cleanup.clone()
    }

    /// Connects, checks the local linux image, and creates one labelled container.
    ///
    /// The owner label and container name are fixed before the create request.
    /// Dropping this future, including while the create response is outstanding,
    /// makes the worker delete only that name after confirming the label. A
    /// single not-found during the request window is not treated as finished.
    ///
    /// # Errors
    ///
    /// Connection, image, runtime, and create failures are returned. If creation
    /// leaves cleanup outstanding, [`SandboxError::CreationCleanupRequired`]
    /// carries the original error and a receipt for container and staging cleanup.
    /// Its worker retries independently of the receipt or this future's lifetime.
    pub async fn create(config: SandboxConfig) -> Result<Self, SandboxError> {
        let nonce = uuid::Uuid::new_v4().to_string();
        let name = format!("circus-sandbox-{nonce}");
        let kind = match config.endpoint() {
            DockerEndpoint::UnixSocket { .. } => SocketKind::Unix,
            DockerEndpoint::NamedPipe { .. } => SocketKind::Pipe,
        };
        let engine = Engine::connect(kind, config.endpoint().bollard_path())
            .await
            .map_err(engine_err)?;
        if let Err(err) = engine
            .prepare(config.image(), config.runtime().as_str())
            .await
        {
            if err.not_found {
                return Err(SandboxError::ImageNotFound {
                    image: config.image().to_string(),
                });
            }
            return Err(engine_err(err));
        }
        let max_file = config.limits().max_file_bytes();
        let workspace = spawn_block(move || Workspace::new(max_file)).await??;
        let (tx, rx) = mpsc::channel(QUEUE);
        let shutdown = CancellationToken::new();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (cleanup_tx, cleanup) = cleanup::channel(name.clone());
        let state = State {
            engine,
            workspace: Arc::new(Mutex::new(workspace)),
            user: format!(
                "{}:{}",
                config.isolation().guest_uid(),
                config.isolation().guest_gid()
            ),
            config,
            name,
            nonce,
            id: None,
            phase: SandboxPhase::Created,
            shutdown: shutdown.clone(),
            create_unsettled: true,
            cleanup: cleanup_tx,
        };
        tokio::spawn(worker(state, rx, ready_tx));
        let mut guard = CommandGuard {
            token: shutdown.clone(),
            armed: true,
        };
        let ready = ready_rx.await.map_err(|_| SandboxError::WorkerGone {
            detail: "create worker ended before reporting ownership".to_string(),
        })?;
        guard.armed = false;
        ready?;
        Ok(Self {
            tx,
            shutdown,
            cleanup,
            max_file_bytes: max_file,
        })
    }

    /// Starts the container after validating all staged files.
    ///
    /// [`SandboxError::Staging`] leaves the container in `Created`, with its
    /// files and ownership retained. Remove or shrink files and retry, or
    /// explicitly [`destroy`](Self::destroy) it.
    pub async fn start(&self) -> Result<(), SandboxError> {
        self.call(Op::Start).await?
    }

    pub async fn pause(&self) -> Result<(), SandboxError> {
        self.call(Op::Pause).await?
    }

    pub async fn resume(&self) -> Result<(), SandboxError> {
        self.call(Op::Resume).await?
    }

    /// Force-removes the owned container. Repeat calls after confirmed absence
    /// return `Ok` and do not call the engine again. Failure keeps the id.
    pub async fn destroy(&self) -> Result<(), SandboxError> {
        self.call(Op::Destroy).await?
    }

    pub async fn write_file(
        &self,
        path: impl AsRef<Path>,
        contents: &[u8],
    ) -> Result<(), SandboxError> {
        let path = path_text(path.as_ref())?;
        if contents.len() > self.max_file_bytes {
            return Err(WorkspaceError::LimitExceeded {
                limit: self.max_file_bytes,
                actual: contents.len(),
            }
            .into());
        }
        self.call(|reply| Op::Write {
            path,
            bytes: contents.to_vec(),
            reply,
        })
        .await?
    }

    pub async fn read_file(&self, path: impl AsRef<Path>) -> Result<Vec<u8>, SandboxError> {
        let path = path_text(path.as_ref())?;
        self.call(|reply| Op::Read { path, reply }).await?
    }

    pub async fn create_dir_all(&self, path: impl AsRef<Path>) -> Result<(), SandboxError> {
        let path = path_text(path.as_ref())?;
        self.call(|reply| Op::Mkdir { path, reply }).await?
    }

    /// Unlinks a file using the confined guest transport when running.
    /// Cancelling an in-flight operation destroys the owned container.
    pub async fn remove_file(&self, path: impl AsRef<Path>) -> Result<(), SandboxError> {
        let path = path_text(path.as_ref())?;
        self.call(|reply| Op::Unlink { path, reply }).await?
    }

    /// Runs `request` inside the running container.
    ///
    /// Dropping this future, including while `create_exec` has not returned,
    /// destroys the owned container. A pending response is not treated as
    /// "not accepted". Timeout and observed output overflow do the same.
    ///
    /// # Errors
    ///
    /// [`SandboxError::Command`] rejects empty argv, zero timeout, and zero
    /// output bound before queueing, without changing phase or ownership.
    /// A request above the configured output bound returns
    /// [`SandboxError::OutputLimit`] with `StillPresent` without executing.
    pub async fn exec(&self, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
        if request.argv.is_empty() {
            return Err(CommandError::EmptyArgv.into());
        }
        if request.timeout.is_zero() {
            return Err(CommandError::InvalidTimeout {
                timeout: request.timeout,
            }
            .into());
        }
        if request.output_limit_bytes == 0 {
            return Err(CommandError::InvalidOutputLimit {
                output_limit_bytes: request.output_limit_bytes,
            }
            .into());
        }
        self.call(|reply| Op::Exec { request, reply }).await?
    }

    pub async fn status(&self) -> Result<SandboxStatus, SandboxError> {
        self.call(Op::Status).await?
    }

    async fn call<T>(&self, op: impl FnOnce(oneshot::Sender<T>) -> Op) -> Result<T, SandboxError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        let cancel = CancellationToken::new();
        let mut guard = CommandGuard {
            token: cancel.clone(),
            armed: true,
        };
        self.tx
            .send(Message {
                op: op(reply_tx),
                cancel,
            })
            .await
            .map_err(|_| SandboxError::WorkerGone {
                detail: "worker stopped accepting operations".to_string(),
            })?;
        let result = reply_rx.await.map_err(|_| SandboxError::WorkerGone {
            detail: "worker dropped the operation reply".to_string(),
        });
        guard.armed = false;
        result
    }
}

impl Drop for DockerSandbox {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

struct State {
    engine: Engine,
    workspace: Arc<Mutex<Workspace>>,
    config: SandboxConfig,
    name: String,
    nonce: String,
    user: String,
    id: Option<String>,
    phase: SandboxPhase,
    shutdown: CancellationToken,
    create_unsettled: bool,
    cleanup: tokio::sync::watch::Sender<cleanup::Progress>,
}

async fn worker(
    mut state: State,
    mut rx: mpsc::Receiver<Message>,
    ready: oneshot::Sender<Result<(), SandboxError>>,
) {
    let spec = create_spec(&state.config, &state.nonce);
    let created = state.engine.create_owned(&state.name, &spec).await;
    match created {
        Ok(id) => {
            state.id = Some(id);
            state.create_unsettled = false;
            if ready.send(Ok(())).is_err() {
                cleanup_until_done(&mut state).await;
                return;
            }
        }
        Err(err) => {
            if let Some(id) = err.create_id.clone() {
                state.id = Some(id);
                state.create_unsettled = false;
            }
            if err.create_pending {
                failed_creation(&mut state, ready, engine_err(err), None).await;
                return;
            }
            state.create_unsettled = false;
            let report = destroy_report(&mut state).await;
            if state.phase == SandboxPhase::Destroyed {
                let _ = ready.send(Err(engine_err(err)));
            } else {
                failed_creation(&mut state, ready, engine_err(err), Some(report)).await;
            }
            return;
        }
    }
    loop {
        let message = tokio::select! {
            biased;
            () = state.shutdown.cancelled() => None,
            message = rx.recv() => message,
        };
        let Some(message) = message else {
            cleanup_until_done(&mut state).await;
            return;
        };
        // A cancelled queued request must never reach an engine or workspace.
        if message.cancel.is_cancelled() {
            continue;
        }
        dispatch(&mut state, message).await;
    }
}

async fn dispatch(state: &mut State, message: Message) {
    let Message { op, cancel } = message;
    let shutdown = state.shutdown.clone();
    let file_timeout = state.config.file_timeout();
    // Drop an outstanding engine future before attempting container removal.
    macro_rules! run {
        ($future:expr, $timeout:expr) => {{
            let outcome = {
                let call = $future;
                tokio::pin!(call);
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => Err(true),
                    () = shutdown.cancelled() => Err(true),
                    () = tokio::time::sleep($timeout) => Err(false),
                    result = &mut call => Ok(result),
                }
            };
            match outcome {
                Ok(result) => result,
                Err(true) => {
                    destroy_for_cancel(state, "operation cancelled while acceptance was uncertain")
                        .await
                }
                Err(false) => {
                    let report = destroy_report(state).await;
                    Err(SandboxError::TimedOut {
                        container_id: report.container_id,
                        ownership: report.ownership,
                        detail: report.detail,
                    })
                }
            }
        }};
    }
    match op {
        Op::Start(reply) => {
            let result = run!(start(state), file_timeout);
            let _ = reply.send(result);
        }
        Op::Pause(reply) => {
            let result = run!(pause(state), file_timeout);
            let _ = reply.send(result);
        }
        Op::Resume(reply) => {
            let result = run!(resume(state), file_timeout);
            let _ = reply.send(result);
        }
        // Never cancel cleanup itself: a lost caller cannot relinquish ownership.
        Op::Destroy(reply) => {
            let report = destroy_report(state).await;
            let result = if state.phase == SandboxPhase::Destroyed {
                Ok(())
            } else {
                Err(SandboxError::CleanupRequired {
                    container_id: state.id.clone().unwrap_or_else(|| state.name.clone()),
                    detail: report.detail,
                })
            };
            let _ = reply.send(result);
        }
        Op::Write { path, bytes, reply } => {
            let result = run!(write_file(state, &path, bytes), file_timeout);
            let _ = reply.send(result);
        }
        Op::Read { path, reply } => {
            let result = run!(read_file(state, &path), file_timeout);
            let _ = reply.send(result);
        }
        Op::Mkdir { path, reply } => {
            let result = run!(mkdir(state, &path), file_timeout);
            let _ = reply.send(result);
        }
        Op::Unlink { path, reply } => {
            let result = run!(unlink(state, &path), file_timeout);
            let _ = reply.send(result);
        }
        Op::Exec { request, reply } => {
            let timeout = request.timeout;
            let result = run!(exec(state, request), timeout);
            let _ = reply.send(result);
        }
        Op::Status(reply) => {
            let result = run!(status(state), file_timeout);
            let _ = reply.send(result);
        }
        Op::CaptureSnapshot {
            budget,
            max_archive_bytes,
            reply,
        } => {
            let result = snapshot::capture(state, budget, max_archive_bytes, &cancel).await;
            let _ = reply.send(result);
        }
        Op::RestoreSnapshot { snapshot, reply } => {
            let result = snapshot::restore(state, snapshot, &cancel).await;
            let _ = reply.send(result);
        }
    }
}

async fn start(state: &mut State) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Created, "start")?;
    // Validate and read every staged node before a container can run.
    let entries = staged(state).await?;
    let id = required_id(state)?;
    if let Err(err) = state.engine.start(&id).await {
        return cleanup_engine_error(state, err).await;
    }
    if let Err(err) = state
        .engine
        .upload(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &entries,
            state.config.limits().workspace_bytes(),
        )
        .await
    {
        return cleanup_engine_error(state, err).await;
    }
    state.phase = SandboxPhase::Running;
    Ok(())
}

async fn pause(state: &mut State) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Running, "pause")?;
    let id = required_id(state)?;
    if let Err(err) = state.engine.pause(&id).await {
        return cleanup_engine_error(state, err).await;
    }
    state.phase = SandboxPhase::Paused;
    Ok(())
}

async fn resume(state: &mut State) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Paused, "resume")?;
    let id = required_id(state)?;
    if let Err(err) = state.engine.resume(&id).await {
        return cleanup_engine_error(state, err).await;
    }
    state.phase = SandboxPhase::Running;
    Ok(())
}

async fn cleanup_engine_error<T>(state: &mut State, err: EngineError) -> Result<T, SandboxError> {
    let report = destroy_report(state).await;
    if state.phase == SandboxPhase::CleanupRequired {
        return Err(SandboxError::CleanupRequired {
            container_id: state.id.clone().unwrap_or_else(|| state.name.clone()),
            detail: format!("{}; {}", err.message, report.detail),
        });
    }
    Err(engine_err(err))
}

async fn destroy_state(state: &mut State) -> Result<(), SandboxError> {
    if state.phase == SandboxPhase::Destroyed {
        return Ok(());
    }
    state.phase = SandboxPhase::CleanupRequired;
    let recovery = state.engine.recover_owned(&state.name, &state.nonce).await;
    match recovery {
        Ok(Recovered::Removed) => {}
        Ok(Recovered::Absent) if state.create_unsettled => {
            return Err(SandboxError::CleanupRequired {
                container_id: state.name.clone(),
                detail: "create response is unresolved; owned container may materialize later"
                    .to_string(),
            });
        }
        Ok(Recovered::Absent) => {}
        Err(err) => {
            return Err(SandboxError::CleanupRequired {
                container_id: state.id.clone().unwrap_or_else(|| state.name.clone()),
                detail: err.message,
            });
        }
    }
    state.create_unsettled = false;
    let workspace = Arc::clone(&state.workspace);
    // Do not clear the owned identity or discard staging on a failed cleanup.
    spawn_block(move || {
        workspace
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .cleanup()
    })
    .await??;
    state.id = None;
    state.phase = SandboxPhase::Destroyed;
    Ok(())
}

async fn write_file(state: &mut State, path: &str, bytes: Vec<u8>) -> Result<(), SandboxError> {
    config::check_relative(path)?;
    if !matches!(state.phase, SandboxPhase::Created | SandboxPhase::Running) {
        return Err(phase_err(state, "write_file"));
    }
    let limit = state.config.limits().max_file_bytes();
    if bytes.len() > limit {
        return Err(WorkspaceError::LimitExceeded {
            limit,
            actual: bytes.len(),
        }
        .into());
    }
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .write_file(path, &bytes)
        })
        .await??);
    }
    let id = required_id(state)?;
    let result = state
        .engine
        .upload(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &[StagedBytes {
                path: path.to_string(),
                is_dir: false,
                bytes,
            }],
            state.config.limits().workspace_bytes(),
        )
        .await;
    file_result(state, path, result).await
}

async fn read_file(state: &mut State, path: &str) -> Result<Vec<u8>, SandboxError> {
    config::check_relative(path)?;
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .read_file(path)
        })
        .await??);
    }
    need(state, SandboxPhase::Running, "read_file")?;
    let id = required_id(state)?;
    let result = state
        .engine
        .download_file(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            path,
            state.config.limits().max_file_bytes(),
        )
        .await;
    file_result(state, path, result).await
}

async fn mkdir(state: &mut State, path: &str) -> Result<(), SandboxError> {
    config::check_relative(path)?;
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .create_dir_all(path)
        })
        .await??);
    }
    need(state, SandboxPhase::Running, "create_dir_all")?;
    let id = required_id(state)?;
    let result = state
        .engine
        .upload(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &[StagedBytes {
                path: path.to_string(),
                is_dir: true,
                bytes: Vec::new(),
            }],
            state.config.limits().workspace_bytes(),
        )
        .await;
    file_result(state, path, result).await
}

async fn unlink(state: &mut State, path: &str) -> Result<(), SandboxError> {
    config::check_relative(path)?;
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .remove_file(path)
        })
        .await??);
    }
    need(state, SandboxPhase::Running, "remove_file")?;
    let id = required_id(state)?;
    let result = state
        .engine
        .remove_file(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            path,
        )
        .await;
    file_result(state, path, result).await
}

async fn exec(state: &mut State, request: CommandRequest) -> Result<CommandOutput, SandboxError> {
    need(state, SandboxPhase::Running, "exec")?;
    if request.output_limit_bytes > state.config.output_limit_bytes() {
        return Err(SandboxError::OutputLimit {
            limit: state.config.output_limit_bytes(),
            ownership: Ownership::StillPresent,
            detail: "request exceeds configured output limit".to_string(),
        });
    }
    let id = required_id(state)?;
    let result = state
        .engine
        .exec(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &request.argv,
            request.output_limit_bytes,
        )
        .await;
    match result {
        Ok(done) => Ok(CommandOutput {
            exit_code: done.exit_code,
            stdout: done.stdout,
            stderr: done.stderr,
        }),
        Err(err) if err.kind == EngineErrorKind::Limit => {
            let report = destroy_report(state).await;
            Err(SandboxError::OutputLimit {
                limit: request.output_limit_bytes,
                ownership: report.ownership,
                detail: report.detail,
            })
        }
        Err(err) => cleanup_engine_error(state, err).await,
    }
}

async fn destroy_for_cancel<T>(state: &mut State, detail: &str) -> Result<T, SandboxError> {
    let report = destroy_report(state).await;
    Err(SandboxError::Cancelled {
        container_id: report.container_id,
        ownership: report.ownership,
        detail: format!("{detail}; {}", report.detail),
    })
}

async fn destroy_report(state: &mut State) -> CleanupReport {
    let id = state.id.clone();
    let report = match destroy_state(state).await {
        Ok(()) => CleanupReport {
            container_id: None,
            ownership: Ownership::ConfirmedAbsent,
            detail: "owned container confirmed absent".to_string(),
        },
        Err(err) => CleanupReport {
            container_id: id.or_else(|| Some(state.name.clone())),
            ownership: Ownership::Unknown,
            detail: err.to_string(),
        },
    };
    state.cleanup.send_replace(cleanup::Progress {
        report: report.clone(),
        complete: state.phase == SandboxPhase::Destroyed,
        attempted: true,
    });
    report
}

async fn status(state: &State) -> Result<SandboxStatus, SandboxError> {
    let Some(id) = state.id.as_deref() else {
        return Ok(SandboxStatus {
            phase: state.phase,
            container_id: None,
            runtime: state.config.runtime().as_str().to_string(),
            memory_bytes: 0,
            nano_cpus: 0,
            pids_limit: 0,
            readonly_root: false,
            network_mode: String::new(),
            cap_drop: Vec::new(),
            security_opt: Vec::new(),
            bind_count: 0,
            tmpfs_workspace: None,
        });
    };
    let report = state.engine.host_report(id).await.map_err(engine_err)?;
    Ok(SandboxStatus {
        phase: state.phase,
        container_id: Some(id.to_string()),
        runtime: report.runtime,
        memory_bytes: report.memory_bytes,
        nano_cpus: report.nano_cpus,
        pids_limit: report.pids_limit,
        readonly_root: report.readonly_root,
        network_mode: report.network_mode,
        cap_drop: report.cap_drop,
        security_opt: report.security_opt,
        bind_count: report.bind_count,
        tmpfs_workspace: report.tmpfs_workspace,
    })
}

async fn staged(state: &State) -> Result<Vec<StagedBytes>, SandboxError> {
    let workspace = Arc::clone(&state.workspace);
    let nodes = spawn_block(move || {
        workspace
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .staged_nodes()
    })
    .await??;
    let mut entries = Vec::new();
    let mut total = 0u64;
    for StagedNode { relative, is_dir } in nodes {
        config::check_relative(&relative)?;
        if is_dir {
            entries.push(StagedBytes {
                path: relative,
                is_dir: true,
                bytes: Vec::new(),
            });
            continue;
        }
        let workspace = Arc::clone(&state.workspace);
        let path = relative.clone();
        let bytes = spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .read_file(path)
        })
        .await??;
        total = input::staged_total(
            total,
            bytes.len() as u64,
            state.config.limits().workspace_bytes(),
        )?;
        entries.push(StagedBytes {
            path: relative,
            is_dir: false,
            bytes,
        });
    }
    Ok(entries)
}

fn create_spec(config: &SandboxConfig, nonce: &str) -> CreateSpec {
    let limits = config.limits();
    CreateSpec {
        image: config.image().to_string(),
        runtime: config.runtime().as_str().to_string(),
        memory: i64::try_from(limits.memory_bytes()).unwrap_or(i64::MAX),
        nano_cpus: i64::from(limits.cpu_millis()) * 1_000_000,
        pids: i64::from(limits.pids()),
        workspace_path: config.isolation().workspace_path().to_string(),
        workspace_bytes: limits.workspace_bytes(),
        guest_uid: config.isolation().guest_uid(),
        guest_gid: config.isolation().guest_gid(),
        owner_label: nonce.to_string(),
    }
}

fn need(state: &State, phase: SandboxPhase, operation: &'static str) -> Result<(), SandboxError> {
    if state.phase == phase {
        Ok(())
    } else {
        Err(phase_err(state, operation))
    }
}

fn phase_err(state: &State, operation: &'static str) -> SandboxError {
    SandboxError::InvalidPhase {
        current: state.phase,
        operation,
    }
}

fn required_id(state: &State) -> Result<String, SandboxError> {
    state
        .id
        .clone()
        .ok_or_else(|| SandboxError::CleanupRequired {
            container_id: state.name.clone(),
            detail: "container id is not retained".to_string(),
        })
}

fn path_text(path: &Path) -> Result<String, SandboxError> {
    path.to_str()
        .map(str::to_string)
        .ok_or(SandboxError::RelativePath {
            reason: "path is not utf-8",
        })
}

async fn file_result<T>(
    state: &mut State,
    path: &str,
    result: Result<T, EngineError>,
) -> Result<T, SandboxError> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => match err.kind {
            EngineErrorKind::UnsafeFile => {
                Err(WorkspaceError::NotRegularFile { path: path.into() }.into())
            }
            EngineErrorKind::Limit => Err(WorkspaceError::LimitExceeded {
                limit: state.config.limits().max_file_bytes(),
                actual: state.config.limits().max_file_bytes().saturating_add(1),
            }
            .into()),
            EngineErrorKind::NotFound => Err(WorkspaceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                err.message,
            ))
            .into()),
            EngineErrorKind::PermissionDenied => Err(WorkspaceError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                err.message,
            ))
            .into()),
            EngineErrorKind::FileIo => {
                Err(WorkspaceError::Io(std::io::Error::other(err.message)).into())
            }
            EngineErrorKind::InvalidPath => Err(SandboxError::RelativePath {
                reason: "guest transport rejected the relative path",
            }),
            EngineErrorKind::Other | EngineErrorKind::Protocol => {
                cleanup_engine_error(state, err).await
            }
        },
    }
}

fn engine_err(err: EngineError) -> SandboxError {
    match err.kind {
        EngineErrorKind::InvalidPath => SandboxError::RelativePath {
            reason: "guest transport rejected the relative path",
        },
        EngineErrorKind::UnsafeFile
        | EngineErrorKind::Limit
        | EngineErrorKind::FileIo
        | EngineErrorKind::NotFound
        | EngineErrorKind::PermissionDenied
        | EngineErrorKind::Protocol
        | EngineErrorKind::Other => SandboxError::Engine {
            message: err.message,
        },
    }
}

async fn spawn_block<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SandboxError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| SandboxError::WorkerGone {
            detail: err.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn rejected_without_worker(request: CommandRequest, expected: CommandError) {
        let (tx, mut rx) = mpsc::channel(QUEUE);
        let sandbox = DockerSandbox {
            tx,
            shutdown: CancellationToken::new(),
            cleanup: cleanup::channel("unused-test-container".to_string()).1,
            max_file_bytes: 1024,
        };
        let err = tokio::time::timeout(Duration::from_secs(1), sandbox.exec(request))
            .await
            .expect("invalid command must not wait for a worker")
            .unwrap_err();
        assert!(
            !matches!(err, SandboxError::RelativePath { .. }),
            "invalid command was classified as a path failure: {err:?}"
        );
        assert!(matches!(&err, SandboxError::Command(actual) if actual == &expected));
        assert_eq!(
            std::error::Error::source(&err)
                .and_then(|source| source.downcast_ref::<CommandError>()),
            Some(&expected)
        );
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
        assert!(!sandbox.shutdown.is_cancelled());
    }

    fn request() -> CommandRequest {
        CommandRequest {
            argv: vec!["/bin/true".into()],
            timeout: Duration::from_secs(1),
            output_limit_bytes: 1024,
        }
    }

    #[tokio::test]
    async fn empty_command_is_not_a_path_error() {
        let mut request = request();
        request.argv.clear();
        rejected_without_worker(request, CommandError::EmptyArgv).await;
    }

    #[tokio::test]
    async fn zero_command_timeout_is_not_a_path_error() {
        let mut request = request();
        request.timeout = Duration::ZERO;
        rejected_without_worker(
            request,
            CommandError::InvalidTimeout {
                timeout: Duration::ZERO,
            },
        )
        .await;
    }

    #[tokio::test]
    async fn zero_command_output_limit_is_not_a_path_error() {
        let mut request = request();
        request.output_limit_bytes = 0;
        rejected_without_worker(
            request,
            CommandError::InvalidOutputLimit {
                output_limit_bytes: 0,
            },
        )
        .await;
    }
}
