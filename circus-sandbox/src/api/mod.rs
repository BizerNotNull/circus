//! In-process sandbox API. Backend SDK types are not re-exported.

pub use crate::lifecycle::{
    CleanupReceipt, CleanupReport, CommandError, CommandOutput, CommandRequest, ConfigError,
    DockerEndpoint, DockerSandbox, IoLimits, IsolationConfig, OWNER_LABEL_KEY, Ownership,
    RuntimeName, SandboxConfig, SandboxError, SandboxPhase, SandboxStatus, StagingError,
};

pub use crate::workspace::{CodeSnapshot, SnapshotBudget, SnapshotError};
