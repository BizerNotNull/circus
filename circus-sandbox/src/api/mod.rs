//! In-process sandbox API. Backend SDK types are not re-exported.

pub use crate::lifecycle::{
    CleanupReport, CommandOutput, CommandRequest, ConfigError, DockerEndpoint, DockerSandbox,
    IoLimits, IsolationConfig, NetworkAccess, OWNER_LABEL_KEY, Ownership, ProcessOutput,
    ProcessRequest, ProcessSession, RuntimeName, SandboxConfig, SandboxError, SandboxPhase,
    SandboxStatus,
};
