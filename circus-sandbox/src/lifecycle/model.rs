//! Public lifecycle models and matchable errors.

use std::time::Duration;

use crate::runtime::{BackendConfigError, ConfigError};
use crate::workspace::{SnapshotError, WorkspaceError};

use super::cleanup::CleanupReceipt;

/// Lifecycle phase. Only the worker mutates it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SandboxPhase {
    Created,
    Running,
    Paused,
    Destroyed,
    CleanupRequired,
}

/// Where the owned instance is after a cleanup attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    ConfirmedAbsent,
    StillPresent,
    Unknown,
}

/// Cleanup outcome. Success is never implied when ownership is not confirmed absent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CleanupReport {
    /// Backend instance identity while it may still exist.
    pub instance_id: Option<String>,
    pub ownership: Ownership,
    pub detail: String,
}

/// Backend-reported effective configuration for one instance.
///
/// Every field is inspected evidence, not the request that was sent. `None`
/// means the backend did not report that property. The lifecycle owner never
/// substitutes the requested value for enforcement evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnforcementReport {
    pub runtime: Option<String>,
    pub memory_bytes: Option<i64>,
    pub nano_cpus: Option<i64>,
    pub pids_limit: Option<i64>,
    pub readonly_root: Option<bool>,
    pub network_mode: Option<String>,
    pub cap_drop: Option<Vec<String>>,
    pub security_opt: Option<Vec<String>>,
    pub bind_count: Option<usize>,
    pub tmpfs_workspace: Option<String>,
}

/// Current phase, owned instance identity, and inspected enforcement evidence.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxStatus {
    pub phase: SandboxPhase,
    /// Opaque backend instance identity while one is retained.
    pub instance_id: Option<String>,
    /// `None` before an instance exists or after confirmed removal. This is
    /// evidence observed from the backend, never a copy of requested limits.
    pub enforcement: Option<EnforcementReport>,
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
    /// Backend-neutral configuration was rejected.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// The backend descriptor was rejected before allocation.
    #[error(transparent)]
    BackendConfig(#[from] BackendConfigError),
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
    Command(#[from] super::CommandError),
    /// Staged content exceeds its byte bound or cannot be counted.
    #[error("workspace staging failed: {0}")]
    Staging(#[from] super::StagingError),
    #[error("phase {current:?} cannot {operation}")]
    InvalidPhase {
        current: SandboxPhase,
        operation: &'static str,
    },
    /// The selected backend has no local copy of this image. This crate never
    /// pulls; supply an image that already exists or resolve one explicitly
    /// with [`crate::environment`].
    #[error("backend image {image} is not available locally; this crate does not pull")]
    BackendImageUnavailable { image: String },
    /// A backend operation failed. The message is backend-supplied text; no SDK
    /// type is exposed.
    #[error("backend operation failed: {message}")]
    Backend { message: String },
    #[error("command timed out; instance ownership is {ownership:?}: {detail}")]
    TimedOut {
        instance_id: Option<String>,
        ownership: Ownership,
        detail: String,
    },
    #[error("command cancelled; instance ownership is {ownership:?}: {detail}")]
    Cancelled {
        instance_id: Option<String>,
        ownership: Ownership,
        detail: String,
    },
    #[error("output exceeded {limit} bytes; instance ownership is {ownership:?}")]
    OutputLimit {
        limit: usize,
        ownership: Ownership,
        detail: String,
    },
    /// Creation failed without returning a sandbox handle. The worker retains
    /// instance and staging ownership; await `cleanup.wait()` before reusing
    /// capacity. Dropping the receipt does not stop cleanup.
    #[error("creation failed; cleanup for instance {instance_id} continues: {source}")]
    CreationCleanupRequired {
        instance_id: String,
        #[source]
        source: Box<SandboxError>,
        cleanup: CleanupReceipt,
    },
    #[error("cleanup failed; instance {instance_id} is still owned: {detail}")]
    CleanupRequired { instance_id: String, detail: String },
    #[error("sandbox worker is gone: {detail}")]
    WorkerGone { detail: String },
    #[error("relative path rejected: {reason}")]
    RelativePath { reason: &'static str },
    #[error("the selected backend cannot {capability}")]
    UnsupportedCapability { capability: &'static str },
}
