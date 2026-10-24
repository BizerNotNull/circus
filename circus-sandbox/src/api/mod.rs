//! In-process sandbox API. Backend SDK types are not re-exported.

pub use crate::lifecycle::{
    CleanupReceipt, CleanupReport, CommandError, CommandOutput, CommandRequest, EnforcementReport,
    Ownership, Sandbox, SandboxError, SandboxPhase, SandboxStatus, StagingError,
};

pub use crate::runtime::{
    BackendConfig, BackendConfigError, ConfigError, DockerBackendConfig, IoLimits, IsolationConfig,
    SandboxConfig,
};

pub use crate::workspace::{CodeSnapshot, SnapshotBudget, SnapshotError};
