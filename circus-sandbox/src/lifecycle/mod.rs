//! Lifecycle owner for one sandbox instance.

mod cleanup;
mod input;
mod model;
mod operations;
mod snapshot;

pub use cleanup::CleanupReceipt;
pub use input::{CommandError, StagingError};
pub use model::{
    CleanupReport, CommandOutput, CommandRequest, EnforcementReport, Ownership, SandboxError,
    SandboxPhase, SandboxStatus,
};

use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::infrastructure::{
    Backend, CommonSpec, DockerBackend, DockerConfig, DockerConfigError, InstanceId, PrepareError,
};
use crate::runtime::{BackendConfig, BackendConfigError, SandboxConfig};
use crate::workspace::Workspace;

use cleanup::{cleanup_until_done, failed_creation};
use operations::{
    common_spec, destroy_report, engine_err, path_text, require_capabilities, spawn_block,
};

const QUEUE: usize = 8;

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
        budget: crate::workspace::SnapshotBudget,
        max_archive_bytes: u64,
        reply: oneshot::Sender<Result<Arc<crate::workspace::CodeSnapshot>, SandboxError>>,
    },
    RestoreSnapshot {
        snapshot: Arc<crate::workspace::CodeSnapshot>,
        reply: oneshot::Sender<Result<(), SandboxError>>,
    },
}

struct Message {
    op: Op,
    cancel: CancellationToken,
}

/// One owned sandbox. Drop requests cleanup without claiming cleanup succeeded.
pub struct Sandbox {
    tx: mpsc::Sender<Message>,
    shutdown: CancellationToken,
    cleanup: CleanupReceipt,
    max_file_bytes: usize,
}

impl Sandbox {
    /// Interrupts queued or running operations and observes full owned cleanup.
    pub(crate) fn revoke(&self) -> CleanupReceipt {
        self.shutdown.cancel();
        self.cleanup.clone()
    }

    /// Validates `backend` and `config`, prepares the backend, and creates one
    /// owned instance.
    ///
    /// Backend-neutral and backend-specific values are rejected before any
    /// instance allocation. Capability requirements (pause/resume, guest file
    /// transfer, workspace archive capture) are checked before creating an
    /// instance. The backend then verifies its environment and that the image
    /// already exists locally; this crate never pulls or contacts a registry.
    ///
    /// The owner label and instance name are fixed before the create request.
    /// Dropping this future, including while the create response is outstanding,
    /// makes the worker remove only that name after confirming the label. A
    /// single not-found during the request window is not treated as finished.
    ///
    /// # Errors
    ///
    /// Returns [`SandboxError::BackendConfig`] for an invalid descriptor,
    /// [`SandboxError::Config`] for invalid neutral configuration,
    /// [`SandboxError::UnsupportedCapability`] for an unsupported operation set,
    /// [`SandboxError::BackendImageUnavailable`] when the local image is absent,
    /// and [`SandboxError::Backend`] or [`SandboxError::Workspace`] for other
    /// preparation failures. If creation leaves cleanup outstanding,
    /// [`SandboxError::CreationCleanupRequired`] carries the original error and
    /// a receipt for instance and staging cleanup. Its worker retries
    /// independently of the receipt or this future's lifetime.
    pub async fn create(backend: BackendConfig, config: SandboxConfig) -> Result<Self, SandboxError> {
        let docker = backend_config(&backend)?;
        let spec = common_spec(&config);
        docker
            .validate_common(&spec)
            .map_err(|err| SandboxError::BackendConfig(map_docker_config(err)))?;
        let backend = DockerBackend::connect(docker).await.map_err(engine_err)?;
        Self::create_with_backend(backend, spec, config).await
    }

    async fn create_with_backend<B: Backend>(
        backend: B,
        spec: CommonSpec,
        config: SandboxConfig,
    ) -> Result<Self, SandboxError> {
        require_capabilities(&backend)?;
        match backend.prepare().await {
            Ok(()) => {}
            Err(PrepareError::ImageUnavailable { image }) => {
                return Err(SandboxError::BackendImageUnavailable { image });
            }
            Err(PrepareError::Engine(err)) => return Err(engine_err(err)),
        }
        let nonce = uuid::Uuid::new_v4().to_string();
        let name = format!("circus-sandbox-{nonce}");
        let max_file = config.limits().max_file_bytes();
        let workspace = spawn_block(move || Workspace::new(max_file)).await??;
        let (tx, rx) = mpsc::channel(QUEUE);
        let shutdown = CancellationToken::new();
        let (ready_tx, ready_rx) = oneshot::channel();
        let (cleanup_tx, cleanup) = cleanup::channel(name.clone());
        let state = State {
            backend,
            spec,
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

    /// Starts the instance after validating all staged files.
    ///
    /// [`SandboxError::Staging`] leaves the instance in `Created`, with its
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

    /// Force-removes the owned instance. Repeat calls after confirmed absence
    /// return `Ok` and do not call the backend again. Failure keeps the id.
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
            return Err(crate::workspace::WorkspaceError::LimitExceeded {
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
    /// Cancelling an in-flight operation destroys the owned instance.
    pub async fn remove_file(&self, path: impl AsRef<Path>) -> Result<(), SandboxError> {
        let path = path_text(path.as_ref())?;
        self.call(|reply| Op::Unlink { path, reply }).await?
    }

    /// Runs `request` inside the running instance.
    ///
    /// Dropping this future, including while the exec request has not returned,
    /// destroys the owned instance. A pending response is not treated as
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

    /// Returns the phase, retained instance identity, and inspected enforcement
    /// evidence. Requested limits are never reported as enforced.
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

impl Drop for Sandbox {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl std::fmt::Debug for Sandbox {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Sandbox")
            .field("max_file_bytes", &self.max_file_bytes)
            .finish_non_exhaustive()
    }
}

fn backend_config(backend: &BackendConfig) -> Result<DockerConfig, SandboxError> {
    match backend {
        BackendConfig::Docker(descriptor) => DockerConfig::new(
            descriptor.endpoint(),
            descriptor.image(),
            descriptor.runtime(),
        )
        .map_err(|err| SandboxError::BackendConfig(map_docker_config(err))),
    }
}

fn map_docker_config(err: DockerConfigError) -> BackendConfigError {
    match err {
        DockerConfigError::UnsupportedEndpoint { value } => {
            BackendConfigError::UnsupportedEndpoint { value }
        }
        DockerConfigError::UnsupportedRuntime { name } => {
            BackendConfigError::UnsupportedRuntime { name }
        }
        DockerConfigError::ImageRequired => BackendConfigError::ImageRequired,
        DockerConfigError::SupervisorNeedsPid { pids } => {
            BackendConfigError::SupervisorNeedsPid { pids }
        }
    }
}

pub(super) struct State<B: Backend> {
    backend: B,
    spec: CommonSpec,
    workspace: Arc<Mutex<Workspace>>,
    config: SandboxConfig,
    name: String,
    nonce: String,
    user: String,
    id: Option<InstanceId>,
    phase: SandboxPhase,
    shutdown: CancellationToken,
    create_unsettled: bool,
    cleanup: tokio::sync::watch::Sender<cleanup::Progress>,
}

async fn worker<B: Backend>(
    mut state: State<B>,
    mut rx: mpsc::Receiver<Message>,
    ready: oneshot::Sender<Result<(), SandboxError>>,
) {
    let created = state
        .backend
        .create_owned(&state.name, &state.spec, &state.nonce)
        .await;
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
                state.id = Some(InstanceId::new(id));
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
        // A cancelled queued request must never reach a backend or workspace.
        if message.cancel.is_cancelled() {
            continue;
        }
        dispatch(&mut state, message).await;
    }
}

async fn dispatch<B: Backend>(state: &mut State<B>, message: Message) {
    let Message { op, cancel } = message;
    let shutdown = state.shutdown.clone();
    let file_timeout = state.config.file_timeout();
    // Drop an outstanding backend future before attempting instance removal.
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
                    operations::destroy_for_cancel(
                        state,
                        "operation cancelled while acceptance was uncertain",
                    )
                    .await
                }
                Err(false) => {
                    let report = destroy_report(state).await;
                    Err(SandboxError::TimedOut {
                        instance_id: report.instance_id,
                        ownership: report.ownership,
                        detail: report.detail,
                    })
                }
            }
        }};
    }
    match op {
        Op::Start(reply) => {
            let result = run!(operations::start(state), file_timeout);
            let _ = reply.send(result);
        }
        Op::Pause(reply) => {
            let result = run!(operations::pause(state), file_timeout);
            let _ = reply.send(result);
        }
        Op::Resume(reply) => {
            let result = run!(operations::resume(state), file_timeout);
            let _ = reply.send(result);
        }
        // Never cancel cleanup itself: a lost caller cannot relinquish ownership.
        Op::Destroy(reply) => {
            let report = destroy_report(state).await;
            let result = if state.phase == SandboxPhase::Destroyed {
                Ok(())
            } else {
                Err(SandboxError::CleanupRequired {
                    instance_id: state
                        .id
                        .as_ref()
                        .map_or_else(|| state.name.clone(), |id| id.as_str().to_string()),
                    detail: report.detail,
                })
            };
            let _ = reply.send(result);
        }
        Op::Write { path, bytes, reply } => {
            let result = run!(operations::write_file(state, &path, bytes), file_timeout);
            let _ = reply.send(result);
        }
        Op::Read { path, reply } => {
            let result = run!(operations::read_file(state, &path), file_timeout);
            let _ = reply.send(result);
        }
        Op::Mkdir { path, reply } => {
            let result = run!(operations::mkdir(state, &path), file_timeout);
            let _ = reply.send(result);
        }
        Op::Unlink { path, reply } => {
            let result = run!(operations::unlink(state, &path), file_timeout);
            let _ = reply.send(result);
        }
        Op::Exec { request, reply } => {
            let timeout = request.timeout;
            let result = run!(operations::exec(state, request), timeout);
            let _ = reply.send(result);
        }
        Op::Status(reply) => {
            let result = run!(operations::status(state), file_timeout);
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    async fn rejected_without_worker(request: CommandRequest, expected: CommandError) {
        let (tx, mut rx) = mpsc::channel(QUEUE);
        let sandbox = Sandbox {
            tx,
            shutdown: CancellationToken::new(),
            cleanup: cleanup::channel("unused-test-instance".to_string()).1,
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
