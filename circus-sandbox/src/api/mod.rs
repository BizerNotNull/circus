//! In-process sandbox API. Backend SDK types are not re-exported.

pub use crate::lifecycle::{
    CleanupFailure, CleanupReceipt, CleanupReport, CommandError, CommandOutput, CommandRequest,
    EnforcementReport, OutputPhase, Ownership, ProcessError, ProcessExit, ProcessOutput,
    ProcessRequest, ProcessSession, ProcessTermination, Sandbox, SandboxError, SandboxOperation,
    SandboxPhase, SandboxStatus, StagingError,
};

pub use crate::runtime::{
    BackendConfig, BackendConfigError, ConfigError, DockerBackendConfig, IoLimits, IsolationConfig,
    SandboxConfig,
};

pub use crate::workspace::{CodeSnapshot, SnapshotBudget, SnapshotError};
