//! Explicit sandbox configuration validation. No backend is started.

use std::time::Duration;

use crate::runtime::{ResourceLimits, ResourceLimitsError};

/// Local Docker Engine endpoint. TCP, HTTP, and SSH are rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerEndpoint {
    /// Unix socket, such as `/var/run/docker.sock` or `unix:///var/run/docker.sock`.
    UnixSocket { path: String },
    /// Local Windows named pipe, such as `npipe:////./pipe/dockerDesktopLinuxEngine`.
    NamedPipe { path: String },
}

impl DockerEndpoint {
    /// Parses a local Unix socket or Windows named pipe endpoint.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        let value = value.trim();
        if value.is_empty()
            || value.starts_with("tcp://")
            || value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("ssh://")
            || value.starts_with("fd://")
        {
            return Err(ConfigError::UnsupportedEndpoint {
                value: value.into(),
            });
        }
        if let Some(name) = value.strip_prefix("npipe:////./pipe/") {
            if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
                return Err(ConfigError::UnsupportedEndpoint {
                    value: value.into(),
                });
            }
            return Ok(Self::NamedPipe { path: value.into() });
        }
        if let Some(rest) = value.strip_prefix("unix://") {
            if rest.starts_with('/') && !rest.contains('\0') {
                return Ok(Self::UnixSocket { path: value.into() });
            }
            return Err(ConfigError::UnsupportedEndpoint {
                value: value.into(),
            });
        }
        if value.starts_with('/') && !value.contains('\0') {
            return Ok(Self::UnixSocket {
                path: format!("unix://{value}"),
            });
        }
        Err(ConfigError::UnsupportedEndpoint {
            value: value.into(),
        })
    }

    pub(crate) fn bollard_path(&self) -> &str {
        match self {
            Self::UnixSocket { path } | Self::NamedPipe { path } => path,
        }
    }
}

/// OCI runtime name. Only `runc` is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RuntimeName;

impl RuntimeName {
    /// The only supported runtime.
    #[must_use]
    pub fn runc() -> Self {
        Self
    }

    /// Parses a runtime name.
    pub fn parse(name: &str) -> Result<Self, ConfigError> {
        if name == "runc" {
            Ok(Self)
        } else {
            Err(ConfigError::UnsupportedRuntime { name: name.into() })
        }
    }

    #[must_use]
    pub fn as_str(self) -> &'static str {
        "runc"
    }
}

/// Explicit isolation flags. Every security flag must be enabled.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationConfig {
    workspace_path: String,
    guest_uid: u32,
    guest_gid: u32,
}

impl IsolationConfig {
    /// Builds an enforced Linux isolation policy.
    pub fn linux_enforced(
        no_new_privileges: bool,
        drop_all_capabilities: bool,
        readonly_root: bool,
        network_disabled: bool,
        workspace_path: impl Into<String>,
        guest_uid: u32,
        guest_gid: u32,
    ) -> Result<Self, ConfigError> {
        if !no_new_privileges || !drop_all_capabilities || !readonly_root || !network_disabled {
            return Err(ConfigError::IsolationNotEnforced);
        }
        let workspace_path = workspace_path.into();
        check_container_dir(&workspace_path)?;
        Ok(Self {
            workspace_path,
            guest_uid,
            guest_gid,
        })
    }

    #[must_use]
    pub fn workspace_path(&self) -> &str {
        &self.workspace_path
    }
    #[must_use]
    pub fn guest_uid(&self) -> u32 {
        self.guest_uid
    }
    #[must_use]
    pub fn guest_gid(&self) -> u32 {
        self.guest_gid
    }
}

/// Explicit bounds for captured output and guest file operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoLimits {
    output_limit_bytes: usize,
    file_timeout: Duration,
}

impl IoLimits {
    /// Rejects unprobeable output bounds and zero file-operation timeouts.
    pub fn new(output_limit_bytes: usize, file_timeout: Duration) -> Result<Self, ConfigError> {
        if output_limit_bytes == 0 || output_limit_bytes == usize::MAX {
            return Err(ConfigError::InvalidOutputLimit { output_limit_bytes });
        }
        if file_timeout.is_zero() {
            return Err(ConfigError::InvalidFileTimeout);
        }
        Ok(Self {
            output_limit_bytes,
            file_timeout,
        })
    }
}

/// Complete create-time configuration. Validation does not start a backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SandboxConfig {
    endpoint: DockerEndpoint,
    image: String,
    runtime: RuntimeName,
    limits: ResourceLimits,
    isolation: IsolationConfig,
    io_limits: IoLimits,
}

impl SandboxConfig {
    /// Validates a complete six-argument configuration.
    ///
    /// The minimum of three PIDs is the supported future supervisor profile,
    /// not a backend-independent property of [`ResourceLimits`]. Images are
    /// referenced, never implicitly pulled, by this validation-only crate.
    pub fn new(
        endpoint: DockerEndpoint,
        image: impl Into<String>,
        runtime: RuntimeName,
        limits: ResourceLimits,
        isolation: IsolationConfig,
        io_limits: IoLimits,
    ) -> Result<Self, ConfigError> {
        let endpoint = DockerEndpoint::parse(endpoint.bollard_path())?;
        let runtime = RuntimeName::parse(runtime.as_str())?;
        let image = image.into();
        if image.is_empty() || image.chars().any(char::is_whitespace) {
            return Err(ConfigError::ImageRequired);
        }
        if limits.pids() < 3 {
            return Err(ConfigError::SupervisorNeedsPid {
                pids: limits.pids(),
            });
        }
        Ok(Self {
            endpoint,
            image,
            runtime,
            limits,
            isolation,
            io_limits,
        })
    }
    #[must_use]
    pub fn endpoint(&self) -> &DockerEndpoint {
        &self.endpoint
    }
    #[must_use]
    pub fn image(&self) -> &str {
        &self.image
    }
    #[must_use]
    pub fn runtime(&self) -> RuntimeName {
        self.runtime
    }
    #[must_use]
    pub fn limits(&self) -> ResourceLimits {
        self.limits
    }
    #[must_use]
    pub fn isolation(&self) -> &IsolationConfig {
        &self.isolation
    }
    #[must_use]
    pub fn output_limit_bytes(&self) -> usize {
        self.io_limits.output_limit_bytes
    }
    #[must_use]
    pub fn file_timeout(&self) -> Duration {
        self.io_limits.file_timeout
    }
}

/// Why configuration was rejected before any backend operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("endpoint is not a local unix socket or npipe: {value}")]
    UnsupportedEndpoint { value: String },
    #[error("runtime {name} is not supported; only runc is accepted")]
    UnsupportedRuntime { name: String },
    #[error("all isolation security flags must be enabled")]
    IsolationNotEnforced,
    #[error("workspace path must be an absolute container path without host prefixes")]
    InvalidWorkspacePath,
    #[error("image reference is required and is not pulled by this crate")]
    ImageRequired,
    #[error("output_limit_bytes must be in 1..usize::MAX, got {output_limit_bytes}")]
    InvalidOutputLimit { output_limit_bytes: usize },
    #[error("file-operation timeout must be non-zero")]
    InvalidFileTimeout,
    #[error("pids {pids} cannot fit init, supervisor and command; minimum is 3")]
    SupervisorNeedsPid { pids: u32 },
    #[error(transparent)]
    Limits(#[from] ResourceLimitsError),
}

fn check_container_dir(path: &str) -> Result<(), ConfigError> {
    if !path.starts_with('/')
        || path.starts_with("//")
        || path.contains('\\')
        || path.contains('\0')
    {
        return Err(ConfigError::InvalidWorkspacePath);
    }
    let rest = &path[1..];
    if rest.is_empty()
        || rest.split('/').any(|component| {
            component.is_empty() || component == "." || component == ".." || drive_prefix(component)
        })
    {
        return Err(ConfigError::InvalidWorkspacePath);
    }
    Ok(())
}

fn drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}
