//! Explicit sandbox configuration. Security flags are required inputs.

use std::time::Duration;

use crate::runtime::{ResourceLimits, ResourceLimitsError};

/// Local Docker Engine endpoint. TCP, HTTP, and SSH are rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DockerEndpoint {
    /// Unix socket. `unix:///var/run/docker.sock` or `/var/run/docker.sock`.
    UnixSocket { path: String },
    /// Windows named pipe, including `npipe:////./pipe/dockerDesktopLinuxEngine`.
    NamedPipe { path: String },
}

impl DockerEndpoint {
    /// Parses a local socket or named-pipe endpoint.
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnsupportedEndpoint`] for empty, TCP, HTTP, or SSH values.
    pub fn parse(value: &str) -> Result<Self, ConfigError> {
        let value = value.trim();
        if value.is_empty() {
            return Err(ConfigError::UnsupportedEndpoint {
                value: value.to_string(),
            });
        }
        if value.starts_with("tcp://")
            || value.starts_with("http://")
            || value.starts_with("https://")
            || value.starts_with("ssh://")
            || value.starts_with("fd://")
        {
            return Err(ConfigError::UnsupportedEndpoint {
                value: value.to_string(),
            });
        }
        if let Some(rest) = value.strip_prefix("npipe://") {
            if !rest.starts_with("//./pipe/") || rest.len() == "//./pipe/".len() {
                return Err(ConfigError::UnsupportedEndpoint {
                    value: value.to_string(),
                });
            }
            return Ok(Self::NamedPipe {
                path: value.to_string(),
            });
        }
        if let Some(rest) = value.strip_prefix("unix://") {
            if !rest.starts_with('/') {
                return Err(ConfigError::UnsupportedEndpoint {
                    value: value.to_string(),
                });
            }
            return Ok(Self::UnixSocket {
                path: value.to_string(),
            });
        }
        if value.starts_with('/') {
            return Ok(Self::UnixSocket {
                path: format!("unix://{value}"),
            });
        }
        Err(ConfigError::UnsupportedEndpoint {
            value: value.to_string(),
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
    ///
    /// # Errors
    ///
    /// Returns [`ConfigError::UnsupportedRuntime`] for any name other than `runc`.
    pub fn parse(name: &str) -> Result<Self, ConfigError> {
        if name == "runc" {
            Ok(Self)
        } else {
            Err(ConfigError::UnsupportedRuntime {
                name: name.to_string(),
            })
        }
    }

    /// Returns `runc`.
    #[must_use]
    pub fn as_str(self) -> &'static str {
        "runc"
    }
}

/// Isolation flags that must all be enabled. There is no insecure constructor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IsolationConfig {
    workspace_path: String,
    guest_uid: u32,
    guest_gid: u32,
}

impl IsolationConfig {
    /// Builds a Linux isolation policy.
    ///
    /// `workspace_path` is an absolute container path, never a host path. It is
    /// mounted as a bounded tmpfs. The root filesystem is read-only, so a later
    /// Docker commit would not preserve workspace writes. Snapshot is unsupported.
    ///
    /// # Errors
    ///
    /// Rejects any disabled flag, a non-absolute workspace path, or a path that
    /// contains `..`, `\`, or a Windows drive prefix in any component.
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

    /// Absolute container workspace path.
    #[must_use]
    pub fn workspace_path(&self) -> &str {
        &self.workspace_path
    }

    /// Guest user id. Passed explicitly to the engine; not inherited.
    #[must_use]
    pub fn guest_uid(&self) -> u32 {
        self.guest_uid
    }

    /// Guest group id.
    #[must_use]
    pub fn guest_gid(&self) -> u32 {
        self.guest_gid
    }
}

/// Explicit bounds for captured output and each guest file operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoLimits {
    output_limit_bytes: usize,
    file_timeout: Duration,
}

impl IoLimits {
    /// Rejects a zero/unprobeable output bound or a zero file-operation timeout.
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

/// Create-time configuration. No field is filled from the daemon or the image.
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
    /// Validates a complete configuration.
    ///
    /// Each create call generates its own ownership nonce before requesting a container.
    /// `pids` must fit the init, supervisor, and a command (at least 3).
    /// The image is not pulled and must provide Python 3 for guest file operations.
    ///
    /// # Errors
    ///
    /// Returns a typed config or resource-limit error.
    pub fn new(
        endpoint: DockerEndpoint,
        image: impl Into<String>,
        runtime: RuntimeName,
        limits: ResourceLimits,
        isolation: IsolationConfig,
        io_limits: IoLimits,
    ) -> Result<Self, ConfigError> {
        let endpoint = DockerEndpoint::parse(endpoint.bollard_path())?;
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

/// Why configuration was rejected before any container was created.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ConfigError {
    #[error("endpoint is not a local unix socket or npipe: {value}")]
    UnsupportedEndpoint { value: String },
    #[error("runtime {name} is not supported; only runc is accepted")]
    UnsupportedRuntime { name: String },
    #[error(
        "no-new-privileges, capability drop, readonly root, and disabled network must all be set"
    )]
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
    #[error("relative path rejected: {reason}")]
    RelativePath { reason: &'static str },
}

pub(crate) fn check_container_dir(path: &str) -> Result<(), ConfigError> {
    if !path.starts_with('/') || path.contains('\\') || path.starts_with("//") {
        return Err(ConfigError::InvalidWorkspacePath);
    }
    if path
        .strip_prefix('/')
        .is_none_or(|rest| rest.is_empty() || rest.split('/').any(bad_component))
    {
        return Err(ConfigError::InvalidWorkspacePath);
    }
    Ok(())
}

pub(crate) fn check_relative(path: &str) -> Result<(), ConfigError> {
    if path.is_empty() {
        return Err(ConfigError::RelativePath { reason: "empty" });
    }
    if path.contains('\\') || path.starts_with('/') {
        return Err(ConfigError::RelativePath {
            reason: "not a relative slash path",
        });
    }
    if path.split('/').any(bad_component) {
        return Err(ConfigError::RelativePath {
            reason: "rejected component",
        });
    }
    Ok(())
}

fn bad_component(component: &str) -> bool {
    component.is_empty() || component == "." || component == ".." || drive_prefix(component)
}

fn drive_prefix(component: &str) -> bool {
    let bytes = component.as_bytes();
    bytes.len() >= 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

/// Label key written before the create request. Cleanup matches this exact key.
pub const OWNER_LABEL_KEY: &str = "circus.sandbox.owner";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_runtime_and_drive_component() {
        assert!(RuntimeName::parse("runsc").is_err());
        assert!(RuntimeName::parse("runc").is_ok());
        assert!(check_relative("notes/C:secret").is_err());
        assert!(check_relative("notes/hello.txt").is_ok());
        assert!(DockerEndpoint::parse("tcp://127.0.0.1:2375").is_err());
        assert!(matches!(
            DockerEndpoint::parse("npipe:////./pipe/dockerDesktopLinuxEngine").unwrap(),
            DockerEndpoint::NamedPipe { .. }
        ));
    }
}
