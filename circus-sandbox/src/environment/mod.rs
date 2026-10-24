//! Explicit OCI image resolution against a local Docker Engine.
//!
//! Preparation is async and requires a Tokio runtime. An explicit local
//! endpoint, image reference, Linux platform, and whole-operation timeout are
//! required; no Docker context, environment credentials, or Docker
//! configuration authentication is loaded. Nothing is pulled and no registry
//! is contacted: a missing local image is a typed failure. A successful result
//! identifies inspected image contents, not a complete dependency environment
//! or a guarantee that sandbox creation will accept the image (for example, the
//! guest Python requirement still applies).
//!
//! Dropping the preparation future or reaching its deadline stops observing the
//! daemon request without spawning detached work. Docker owns its shared image
//! cache: concurrent users may move tags or remove images. Pass
//! [`PreparedImage::id`] to [`crate::BackendConfig::docker`] to avoid using a
//! mutable tag; callers remain responsible for retaining the image in the
//! daemon.

use std::time::Duration;

use crate::infrastructure::{DockerEndpoint, Engine, EngineError, ImageInfo};

/// An explicit Linux OS and canonical Docker architecture, without a variant.
///
/// Architecture names are Docker names such as `amd64` and `arm64`, not host
/// aliases such as `x86_64` or `aarch64`. No host-platform default is inferred.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImagePlatform {
    os: String,
    architecture: String,
}

impl ImagePlatform {
    /// Validates the explicitly requested platform.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::InvalidPlatform`] for a non-Linux OS or an
    /// unsupported/noncanonical Docker architecture. Variants are not supported.
    pub fn new(
        os: impl Into<String>,
        architecture: impl Into<String>,
    ) -> Result<Self, EnvironmentError> {
        let os = os.into();
        let architecture = architecture.into();
        if os != "linux"
            || !matches!(
                architecture.as_str(),
                "386"
                    | "amd64"
                    | "arm"
                    | "arm64"
                    | "mips"
                    | "mipsle"
                    | "mips64"
                    | "mips64le"
                    | "ppc64"
                    | "ppc64le"
                    | "riscv64"
                    | "s390x"
            )
        {
            return Err(EnvironmentError::InvalidPlatform { os, architecture });
        }
        Ok(Self { os, architecture })
    }

    /// Requested OS (always `linux`).
    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    /// Canonical Docker architecture.
    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
}

/// Validated immutable inputs for one image resolution operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageRequest {
    endpoint: String,
    reference: String,
    platform: ImagePlatform,
    timeout: Duration,
}

impl ImageRequest {
    /// Validates all inputs without connecting to Docker.
    ///
    /// `endpoint` must name an authorized local Unix socket or Windows named
    /// pipe. `timeout` bounds connection/version negotiation and inspection as a
    /// single operation. References must be nonempty and contain no
    /// whitespace/control characters; Docker performs the remaining reference
    /// syntax validation. No pull policy exists because registry access is not
    /// implemented.
    ///
    /// # Errors
    ///
    /// Returns [`EnvironmentError::InvalidEndpoint`],
    /// [`EnvironmentError::InvalidReference`], or
    /// [`EnvironmentError::InvalidTimeout`].
    pub fn new(
        endpoint: impl Into<String>,
        reference: impl Into<String>,
        platform: ImagePlatform,
        timeout: Duration,
    ) -> Result<Self, EnvironmentError> {
        let raw_endpoint = endpoint.into();
        let endpoint = DockerEndpoint::parse(&raw_endpoint).map_err(|_| {
            EnvironmentError::InvalidEndpoint {
                value: raw_endpoint,
            }
        })?;
        let reference = reference.into();
        if reference.is_empty()
            || reference
                .chars()
                .any(|value| value.is_whitespace() || value.is_control())
        {
            return Err(EnvironmentError::InvalidReference);
        }
        if timeout.is_zero() {
            return Err(EnvironmentError::InvalidTimeout);
        }
        Ok(Self {
            endpoint: endpoint.path().to_string(),
            reference,
            platform,
            timeout,
        })
    }

    /// Explicit local Docker endpoint.
    #[must_use]
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Reference whose final contents will be inspected.
    #[must_use]
    pub fn reference(&self) -> &str {
        &self.reference
    }

    /// Required OS and architecture, checked against the final image inspection.
    #[must_use]
    pub fn platform(&self) -> &ImagePlatform {
        &self.platform
    }

    /// Whole-operation deadline duration.
    #[must_use]
    pub fn timeout(&self) -> Duration {
        self.timeout
    }
}

/// Immutable contents and actual platform reported by the final image inspect.
///
/// This value does not own the daemon cache and does not prevent external image
/// removal. Repository digests can be empty for locally built images.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedImage {
    id: String,
    repo_digests: Vec<String>,
    os: String,
    architecture: String,
}

impl PreparedImage {
    /// Immutable image ID suitable for [`crate::BackendConfig::docker`].
    #[must_use]
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Repository digests returned by Docker, if available.
    #[must_use]
    pub fn repo_digests(&self) -> &[String] {
        &self.repo_digests
    }

    /// Actual inspected OS.
    #[must_use]
    pub fn os(&self) -> &str {
        &self.os
    }

    /// Actual inspected Docker architecture.
    #[must_use]
    pub fn architecture(&self) -> &str {
        &self.architecture
    }
}

/// A resolution failure. None of these failures modify the daemon image cache.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EnvironmentError {
    #[error("image reference must be nonempty without whitespace or control characters")]
    InvalidReference,
    #[error("image preparation timeout must be non-zero")]
    InvalidTimeout,
    #[error("local endpoint is not an authorized unix socket or npipe: {value}")]
    InvalidEndpoint { value: String },
    #[error(
        "unsupported image platform {os}/{architecture}; explicit Linux Docker architecture required"
    )]
    InvalidPlatform { os: String, architecture: String },
    #[error("image {reference} is absent from the local daemon cache; this crate does not pull")]
    ImageNotFound { reference: String },
    #[error(
        "image platform {actual_os}/{actual_architecture} does not match requested {expected:?}"
    )]
    PlatformMismatch {
        expected: ImagePlatform,
        actual_os: String,
        actual_architecture: String,
    },
    #[error("Docker image preparation failed: {message}")]
    Engine { message: String },
    #[error("image preparation exceeded its whole-operation deadline")]
    Timeout,
}

/// Resolves and inspects an explicitly referenced local image.
///
/// The timeout includes connection negotiation and inspection. A missing image
/// is [`EnvironmentError::ImageNotFound`]; no pull is attempted and no fallback
/// image is selected. Platform mismatch is rejected after inspection. Dropping
/// this future stops client observation; no daemon-side cache mutation occurs.
///
/// # Errors
///
/// Returns [`EnvironmentError`] for endpoint/daemon failures, a missing local
/// image, a platform mismatch, or expiration of the operation's timeout.
pub async fn prepare_docker_image(
    request: ImageRequest,
) -> Result<PreparedImage, EnvironmentError> {
    tokio::time::timeout(request.timeout, prepare(&request))
        .await
        .map_err(|_| EnvironmentError::Timeout)?
}

async fn prepare(request: &ImageRequest) -> Result<PreparedImage, EnvironmentError> {
    let endpoint = DockerEndpoint::parse(&request.endpoint).map_err(|_| {
        EnvironmentError::InvalidEndpoint {
            value: request.endpoint.clone(),
        }
    })?;
    let engine = Engine::connect(endpoint.socket_kind(), endpoint.path())
        .await
        .map_err(engine_error)?;
    let info: ImageInfo = engine
        .inspect_image(&request.reference)
        .await
        .map_err(|error| {
            if error.not_found {
                EnvironmentError::ImageNotFound {
                    reference: request.reference.clone(),
                }
            } else {
                engine_error(error)
            }
        })?;
    if info.os != request.platform.os || info.architecture != request.platform.architecture {
        return Err(EnvironmentError::PlatformMismatch {
            expected: request.platform.clone(),
            actual_os: info.os,
            actual_architecture: info.architecture,
        });
    }
    Ok(PreparedImage {
        id: info.id,
        repo_digests: info.repo_digests,
        os: info.os,
        architecture: info.architecture,
    })
}

fn engine_error(error: EngineError) -> EnvironmentError {
    EnvironmentError::Engine {
        message: error.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_implicit_platforms_and_invalid_inputs() {
        assert!(ImagePlatform::new("windows", "amd64").is_err());
        assert!(ImagePlatform::new("linux", "x86_64").is_err());
        assert!(ImagePlatform::new("linux", "arm64/v8").is_err());
        let platform = ImagePlatform::new("linux", "amd64").unwrap();
        for reference in ["", " ", "image\ntag", "image\0tag"] {
            assert!(matches!(
                ImageRequest::new(
                    "unix:///var/run/docker.sock",
                    reference,
                    platform.clone(),
                    Duration::from_secs(1)
                ),
                Err(EnvironmentError::InvalidReference)
            ));
        }
        assert!(matches!(
            ImageRequest::new(
                "unix:///var/run/docker.sock",
                "image",
                platform.clone(),
                Duration::ZERO
            ),
            Err(EnvironmentError::InvalidTimeout)
        ));
        assert!(matches!(
            ImageRequest::new(
                "tcp://localhost:2375",
                "image",
                platform,
                Duration::from_secs(1)
            ),
            Err(EnvironmentError::InvalidEndpoint { .. })
        ));
    }
}
