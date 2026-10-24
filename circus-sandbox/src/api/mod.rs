//! In-process sandbox API. Backend SDK types are not re-exported.

pub use crate::lifecycle::{
    CleanupReport, CommandError, CommandOutput, CommandRequest, ConfigError, DockerEndpoint,
    DockerSandbox, IoLimits, IsolationConfig, OWNER_LABEL_KEY, Ownership, ProcessError,
    ProcessExit, ProcessOutput, ProcessRequest, ProcessSession, ProcessTermination, RuntimeName,
    SandboxConfig, SandboxError, SandboxPhase, SandboxStatus, StagingError,
};
