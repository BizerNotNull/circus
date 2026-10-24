//! Docker deployment adapter. Not a public SDK surface.

mod backend;
mod docker;
mod engine;
pub(crate) mod git_process;

#[cfg(test)]
pub(crate) use backend::Capabilities;
pub(crate) use backend::{Backend, CommonSpec, InstanceId, PrepareError};
pub(crate) use docker::{DockerBackend, DockerConfig, DockerConfigError, DockerEndpoint};
pub(crate) use engine::{
    Engine, EngineError, EngineErrorKind, ImageInfo, OutputPhase, Recovered, StagedBytes,
};
#[cfg(test)]
pub(crate) use engine::{ExecDone, HostReport, OutputLimit, PolicyMismatch};
