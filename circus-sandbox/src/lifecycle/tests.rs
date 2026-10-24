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
        std::error::Error::source(&err).and_then(|source| source.downcast_ref::<CommandError>()),
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

use std::collections::VecDeque;
use std::future::Future;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::infrastructure::{
    Backend, Capabilities, CommonSpec, EngineError, EngineErrorKind, ExecDone, HostReport,
    InstanceId, OutputLimit as EngineOutputLimit, OutputPhase as EngineOutputPhase, PolicyMismatch,
    PrepareError, Recovered, StagedBytes,
};
use crate::runtime::{IoLimits, IsolationConfig, ResourceLimits, SandboxConfig};
use crate::workspace::Workspace;

#[derive(Clone)]
struct FakeBackend {
    recoveries: Arc<Mutex<VecDeque<Recovered>>>,
    recover_calls: Arc<AtomicUsize>,
    create_error: Arc<Mutex<Option<EngineError>>>,
    exec_result: Arc<Mutex<Option<Result<ExecDone, EngineError>>>>,
    report: Arc<Mutex<HostReport>>,
    slow_reports: Arc<AtomicUsize>,
}

impl FakeBackend {
    fn new() -> Self {
        Self {
            recoveries: Arc::new(Mutex::new(VecDeque::new())),
            recover_calls: Arc::new(AtomicUsize::new(0)),
            create_error: Arc::new(Mutex::new(None)),
            exec_result: Arc::new(Mutex::new(None)),
            report: Arc::new(Mutex::new(HostReport::default())),
            slow_reports: Arc::new(AtomicUsize::new(0)),
        }
    }

    fn push_recovery(&self, outcome: Recovered) {
        self.recoveries
            .lock()
            .expect("recoveries lock")
            .push_back(outcome);
    }

    fn set_create_error(&self, error: EngineError) {
        *self.create_error.lock().expect("create lock") = Some(error);
    }

    fn set_exec_result(&self, result: Result<ExecDone, EngineError>) {
        *self.exec_result.lock().expect("exec lock") = Some(result);
    }

    fn sleep_next_report(&self) {
        self.slow_reports.store(1, Ordering::SeqCst);
    }
}

// The production trait returns explicit `impl Future + Send` bounds.
#[allow(clippy::manual_async_fn)]
impl Backend for FakeBackend {
    fn capabilities(&self) -> Capabilities {
        Capabilities {
            pause_resume: true,
            file_transport: true,
            workspace_archive: true,
        }
    }

    fn prepare(&self) -> impl Future<Output = Result<(), PrepareError>> + Send {
        async { Ok(()) }
    }

    fn create_owned(
        &self,
        _name: &str,
        _spec: &CommonSpec,
        _nonce: &str,
    ) -> impl Future<Output = Result<InstanceId, EngineError>> + Send {
        let failure = self.create_error.lock().expect("create lock").take();
        async move {
            match failure {
                Some(error) => Err(error),
                None => Ok(InstanceId::new("fake-instance".to_string())),
            }
        }
    }

    fn recover_owned(
        &self,
        _name: &str,
        _nonce: &str,
        _retained_id: Option<&InstanceId>,
    ) -> impl Future<Output = Recovered> + Send {
        self.recover_calls.fetch_add(1, Ordering::SeqCst);
        let outcome = self
            .recoveries
            .lock()
            .expect("recoveries lock")
            .pop_front()
            .unwrap_or(Recovered::Removed);
        async move { outcome }
    }

    fn start(&self, _id: &InstanceId) -> impl Future<Output = Result<(), EngineError>> + Send {
        async { Ok(()) }
    }

    fn pause(&self, _id: &InstanceId) -> impl Future<Output = Result<(), EngineError>> + Send {
        async { Ok(()) }
    }

    fn resume(&self, _id: &InstanceId) -> impl Future<Output = Result<(), EngineError>> + Send {
        async { Ok(()) }
    }

    fn upload(
        &self,
        _id: &InstanceId,
        _user: &str,
        _workspace: &str,
        _entries: &[StagedBytes],
        _max_bytes: u64,
    ) -> impl Future<Output = Result<(), EngineError>> + Send {
        async { Ok(()) }
    }

    fn download_file(
        &self,
        _id: &InstanceId,
        _user: &str,
        _workspace: &str,
        _relative: &str,
        _max_bytes: usize,
    ) -> impl Future<Output = Result<Vec<u8>, EngineError>> + Send {
        async { unused_engine("download_file") }
    }

    fn remove_file(
        &self,
        _id: &InstanceId,
        _user: &str,
        _workspace: &str,
        _relative: &str,
    ) -> impl Future<Output = Result<(), EngineError>> + Send {
        async { unused_engine("remove_file") }
    }

    fn exec(
        &self,
        _id: &InstanceId,
        _user: &str,
        _workdir: &str,
        _argv: &[String],
        _output_limit: usize,
    ) -> impl Future<Output = Result<ExecDone, EngineError>> + Send {
        let result = self.exec_result.lock().expect("exec lock").take();
        async move {
            result.unwrap_or(Ok(ExecDone {
                exit_code: 0,
                stdout: Vec::new(),
                stderr: Vec::new(),
            }))
        }
    }

    fn host_report(
        &self,
        _id: &InstanceId,
    ) -> impl Future<Output = Result<HostReport, EngineError>> + Send {
        let slow = self.slow_reports.swap(0, Ordering::SeqCst) > 0;
        let report = self.report.lock().expect("report lock").clone();
        async move {
            if slow {
                tokio::time::sleep(Duration::from_secs(3600)).await;
            }
            Ok(report)
        }
    }

    fn download_workspace_archive(
        &self,
        _id: &InstanceId,
        _workspace: &str,
        _max_archive_bytes: u64,
    ) -> impl Future<Output = Result<std::fs::File, EngineError>> + Send {
        async { unused_engine("download_workspace_archive") }
    }
}

fn unused_engine<T>(operation: &str) -> Result<T, EngineError> {
    Err(engine_error(&format!("unused fake operation: {operation}")))
}

fn engine_error(message: &str) -> EngineError {
    EngineError {
        message: message.to_string(),
        not_found: false,
        kind: EngineErrorKind::Other,
        create_pending: false,
        create_id: None,
        output: None,
        policy: None,
    }
}

fn config() -> SandboxConfig {
    let limits = ResourceLimits::new(1024 * 1024, 100, 8, 8 * 1024, 1024).unwrap();
    let isolation =
        IsolationConfig::linux_enforced(true, true, true, true, "/workspace", 1000, 1000).unwrap();
    let io = IoLimits::with_deadlines(
        4096,
        Duration::from_millis(200),
        Duration::from_millis(200),
        Duration::from_millis(50),
    )
    .unwrap();
    SandboxConfig::new(limits, isolation, io).unwrap()
}

fn test_state(backend: FakeBackend) -> State<FakeBackend> {
    let config = config();
    let spec = operations::common_spec(&config);
    let workspace = Workspace::new(1024).unwrap();
    workspace.write_file("kept.txt", b"kept").unwrap();
    let (cleanup, _) = cleanup::channel("fake-name".to_string());
    State {
        backend,
        spec,
        workspace: Arc::new(Mutex::new(workspace)),
        config,
        name: "fake-name".to_string(),
        nonce: "nonce".to_string(),
        user: "1000:1000".to_string(),
        id: Some(InstanceId::new("fake-instance".to_string())),
        phase: SandboxPhase::Created,
        shutdown: CancellationToken::new(),
        create_unsettled: false,
        cleanup,
    }
}

fn sandbox_config_and_spec() -> (SandboxConfig, CommonSpec) {
    let config = config();
    let spec = operations::common_spec(&config);
    (config, spec)
}

#[tokio::test]
async fn destroy_retains_identity_and_staging_until_absence_is_confirmed() {
    let backend = FakeBackend::new();
    backend.push_recovery(Recovered::StillPresent {
        detail: "engine still reports the container".to_string(),
    });
    backend.push_recovery(Recovered::Removed);
    let mut state = test_state(backend);

    let (report, error) = operations::destroy_attempt(&mut state).await;
    assert_eq!(report.ownership, Ownership::StillPresent);
    assert_eq!(report.instance_id.as_deref(), Some("fake-instance"));
    let error = error.expect("destroy must not claim success while present");
    assert!(matches!(
        std::error::Error::source(&error)
            .and_then(|source| source.downcast_ref::<CleanupFailure>()),
        Some(CleanupFailure::StillPresent { .. })
    ));
    match error {
        SandboxError::CleanupRequired {
            ownership, failure, ..
        } => {
            assert_eq!(ownership, Ownership::StillPresent);
            assert!(matches!(failure, CleanupFailure::StillPresent { .. }));
        }
        other => panic!("unexpected destroy error: {other:?}"),
    }
    assert_eq!(state.phase, SandboxPhase::CleanupRequired);
    assert_eq!(
        state.id.as_ref().map(InstanceId::as_str),
        Some("fake-instance")
    );
    assert_eq!(
        state
            .workspace
            .lock()
            .expect("workspace lock")
            .read_file("kept.txt")
            .unwrap(),
        b"kept"
    );

    let (report, error) = operations::destroy_attempt(&mut state).await;
    assert!(error.is_none(), "{error:?}");
    assert_eq!(report.ownership, Ownership::ConfirmedAbsent);
    assert_eq!(state.phase, SandboxPhase::Destroyed);
    assert!(state.id.is_none());
    assert!(
        state
            .workspace
            .lock()
            .expect("workspace lock")
            .read_file("kept.txt")
            .is_err()
    );
}

#[tokio::test]
async fn unconfirmed_removal_is_unknown_and_keeps_ownership() {
    let backend = FakeBackend::new();
    backend.push_recovery(Recovered::Unknown {
        detail: "inspect transport failed".to_string(),
    });
    let mut state = test_state(backend);

    let (report, error) = operations::destroy_attempt(&mut state).await;
    assert_eq!(report.ownership, Ownership::Unknown);
    match error.expect("unknown ownership is not success") {
        SandboxError::CleanupRequired {
            ownership, failure, ..
        } => {
            assert_eq!(ownership, Ownership::Unknown);
            assert!(matches!(failure, CleanupFailure::Unknown { .. }));
        }
        other => panic!("unexpected destroy error: {other:?}"),
    }
    assert!(state.id.is_some());
    assert_eq!(state.phase, SandboxPhase::CleanupRequired);
}

#[tokio::test]
async fn concurrent_destroy_calls_never_report_false_success() {
    let backend = FakeBackend::new();
    for _ in 0..2 {
        backend.push_recovery(Recovered::StillPresent {
            detail: "engine reports present".to_string(),
        });
    }
    backend.push_recovery(Recovered::Removed);
    let (config, spec) = sandbox_config_and_spec();
    let sandbox = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect("fake creation");

    let (first, second) = tokio::join!(sandbox.destroy(), sandbox.destroy());
    for result in [first, second] {
        match result {
            Err(SandboxError::CleanupRequired { ownership, .. }) => {
                assert_eq!(ownership, Ownership::StillPresent);
            }
            other => panic!("destroy reported a false result: {other:?}"),
        }
    }
    sandbox.destroy().await.expect("retry confirms absence");
    let status = sandbox.status().await.expect("status after removal");
    assert_eq!(status.phase, SandboxPhase::Destroyed);
    assert_eq!(status.instance_id, None);
    let (third, fourth) = tokio::join!(sandbox.destroy(), sandbox.destroy());
    assert!(third.is_ok() && fourth.is_ok());
}

#[tokio::test]
async fn aggregate_output_limit_reports_stream_and_reclaims_instance() {
    let backend = FakeBackend::new();
    backend.set_exec_result(Err(EngineError {
        output: Some(EngineOutputLimit {
            limit: 8,
            phase: EngineOutputPhase::Stderr,
        }),
        kind: EngineErrorKind::Limit,
        ..engine_error("combined budget exceeded")
    }));
    backend.push_recovery(Recovered::Removed);
    let (config, spec) = sandbox_config_and_spec();
    let sandbox = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect("fake creation");
    sandbox.start().await.expect("fake start");

    let error = sandbox
        .exec(CommandRequest {
            argv: vec!["/bin/true".into()],
            timeout: Duration::from_secs(1),
            output_limit_bytes: 8,
        })
        .await
        .expect_err("output limit is an error");
    match error {
        SandboxError::OutputLimit {
            limit,
            phase,
            ownership,
            ..
        } => {
            assert_eq!(limit, 8);
            assert_eq!(phase, OutputPhase::Stderr);
            assert_eq!(ownership, Ownership::ConfirmedAbsent);
        }
        other => panic!("unexpected output error: {other:?}"),
    }
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn request_over_configured_output_bound_is_not_started_or_destroyed() {
    let backend = FakeBackend::new();
    let (config, spec) = sandbox_config_and_spec();
    let sandbox = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect("fake creation");
    sandbox.start().await.expect("fake start");

    match sandbox
        .exec(CommandRequest {
            argv: vec!["/bin/true".into()],
            timeout: Duration::from_secs(1),
            output_limit_bytes: 5000,
        })
        .await
    {
        Err(SandboxError::OutputLimit {
            limit,
            phase,
            ownership,
            ..
        }) => {
            assert_eq!(limit, 4096);
            assert_eq!(phase, OutputPhase::NotStarted);
            assert_eq!(ownership, Ownership::StillPresent);
        }
        other => panic!("unexpected result: {other:?}"),
    }
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn status_timeout_preserves_phase_and_ownership() {
    let backend = FakeBackend::new();
    backend.sleep_next_report();
    let (config, spec) = sandbox_config_and_spec();
    let sandbox = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect("fake creation");
    sandbox.start().await.expect("fake start");

    let error = tokio::time::timeout(Duration::from_secs(2), sandbox.status())
        .await
        .expect("status deadline is bounded by the worker")
        .expect_err("slow status is a deadline error");
    match error {
        SandboxError::TimedOut {
            operation,
            ownership,
            ..
        } => {
            assert_eq!(operation, SandboxOperation::Status);
            assert_eq!(ownership, Ownership::StillPresent);
        }
        other => panic!("unexpected status error: {other:?}"),
    }
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 0);
    let status = sandbox.status().await.expect("retry status");
    assert_eq!(status.phase, SandboxPhase::Running);
    assert_eq!(status.instance_id.as_deref(), Some("fake-instance"));
    // The instance was not destroyed: another transition is still legal.
    sandbox
        .pause()
        .await
        .expect("timeout must leave the phase retryable");
}

#[tokio::test]
async fn policy_mismatch_fails_creation_and_destroys_the_instance() {
    let backend = FakeBackend::new();
    backend.set_create_error(EngineError {
        create_id: Some("fake-instance".to_string()),
        policy: Some(Box::new(PolicyMismatch {
            field: "network_mode",
            expected: "none".to_string(),
            actual: "bridge".to_string(),
        })),
        ..engine_error("effective policy mismatch")
    });
    backend.push_recovery(Recovered::Removed);
    let (config, spec) = sandbox_config_and_spec();

    let error = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect_err("mismatched policy must not create a sandbox");
    match error {
        SandboxError::PolicyMismatch {
            field,
            expected,
            actual,
        } => {
            assert_eq!(field, "network_mode");
            assert_eq!(expected, "none");
            assert_eq!(actual, "bridge");
        }
        other => panic!("unexpected create error: {other:?}"),
    }
    assert_eq!(backend.recover_calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn policy_mismatch_retains_cleanup_ownership_when_removal_is_unconfirmed() {
    let backend = FakeBackend::new();
    backend.set_create_error(EngineError {
        create_id: Some("fake-instance".to_string()),
        policy: Some(Box::new(PolicyMismatch {
            field: "tmpfs",
            expected: "size=8192".to_string(),
            actual: "size=1".to_string(),
        })),
        ..engine_error("effective policy mismatch")
    });
    backend.push_recovery(Recovered::StillPresent {
        detail: "removal not confirmed".to_string(),
    });
    backend.push_recovery(Recovered::Removed);
    let (config, spec) = sandbox_config_and_spec();

    let error = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect_err("mismatched policy must not create a sandbox");
    match error {
        SandboxError::CreationCleanupRequired {
            instance_id,
            source,
            cleanup,
        } => {
            assert_eq!(instance_id, "fake-instance");
            assert!(matches!(
                source.as_ref(),
                SandboxError::PolicyMismatch { field, .. } if field == "tmpfs"
            ));
            let report = cleanup.wait().await.expect("retry confirms absence");
            assert_eq!(report.ownership, Ownership::ConfirmedAbsent);
        }
        other => panic!("unexpected create error: {other:?}"),
    }
}

#[tokio::test]
async fn cancelled_cleanup_wait_does_not_stop_owned_retry() {
    let backend = FakeBackend::new();
    backend.set_create_error(EngineError {
        create_id: Some("fake-instance".to_string()),
        policy: Some(Box::new(PolicyMismatch {
            field: "cap_drop",
            expected: "ALL".to_string(),
            actual: "none".to_string(),
        })),
        ..engine_error("effective policy mismatch")
    });
    backend.push_recovery(Recovered::StillPresent {
        detail: "first removal unconfirmed".to_string(),
    });
    backend.push_recovery(Recovered::Removed);
    let (config, spec) = sandbox_config_and_spec();

    let error = Sandbox::create_with_backend(backend.clone(), spec, config)
        .await
        .expect_err("mismatched policy must not create a sandbox");
    let SandboxError::CreationCleanupRequired { cleanup, .. } = error else {
        panic!("expected a creation cleanup receipt");
    };
    // Cancelling a wait is only an observation change; the worker keeps going.
    assert!(
        tokio::time::timeout(Duration::from_millis(1), cleanup.wait())
            .await
            .is_err(),
        "cleanup completed before the wait could be cancelled"
    );
    let report = cleanup.wait().await.expect("retained retry completes");
    assert_eq!(report.ownership, Ownership::ConfirmedAbsent);
}
