//! Docker deployment adapter. Not a public SDK surface.

mod backend;
#[cfg(unix)]
mod containerd;
mod containerd_config;
#[cfg(unix)]
mod containerd_engine;
#[cfg(unix)]
mod containerd_grpc;
#[cfg(unix)]
mod containerd_image;
#[cfg(unix)]
mod containerd_io;
#[cfg(unix)]
mod containerd_spec;
mod docker;
mod engine;
pub(crate) mod git_process;
mod guest_transport;

#[cfg(test)]
pub(crate) use backend::Capabilities;
pub(crate) use backend::{Backend, BackendProcess, CommonSpec, InstanceId, PrepareError};
#[cfg(unix)]
pub(crate) use containerd::ContainerdBackend;
pub(crate) use containerd_config::{ContainerdConfig, ContainerdConfigError};
pub(crate) use docker::{DockerBackend, DockerConfig, DockerConfigError, DockerEndpoint};
pub(crate) use engine::{
    Engine, EngineError, EngineErrorKind, ImageInfo, OutputPhase, ProcessChunk, ProcessSpec,
    Recovered, UploadEntry, UploadSource, WorkspaceEntry, WorkspaceEntryKind,
};
#[cfg(test)]
pub(crate) use engine::{ExecDone, HostReport, OutputLimit, PolicyMismatch};
