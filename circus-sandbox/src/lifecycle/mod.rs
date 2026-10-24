//! Validated lifecycle configuration models; no lifecycle backend is implemented.

mod config;

pub use config::{
    ConfigError, DockerEndpoint, IoLimits, IsolationConfig, RuntimeName, SandboxConfig,
};
