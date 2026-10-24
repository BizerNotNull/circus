//! Stable HTTP error bodies and neutral domain-error mapping.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

use crate::api::{SandboxError, StagingError};
use crate::workspace::{SnapshotError, WorkspaceError};

/// Stable JSON error body. Codes are part of the HTTP contract.
#[derive(Debug, Serialize)]
pub(super) struct ErrorBody {
    pub(super) code: &'static str,
    pub(super) message: &'static str,
}

/// One HTTP failure. The message is fixed per code and never derived from an
/// error's `Display`, so backend text and SDK types cannot cross the boundary.
#[derive(Debug, Clone, Copy)]
pub(super) struct ApiError {
    status: StatusCode,
    code: &'static str,
    message: &'static str,
}

impl ApiError {
    const fn new(status: StatusCode, code: &'static str, message: &'static str) -> Self {
        Self {
            status,
            code,
            message,
        }
    }

    pub(super) const fn invalid_request() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "request is invalid",
        )
    }

    pub(super) const fn invalid_json() -> Self {
        Self::new(
            StatusCode::BAD_REQUEST,
            "invalid_request",
            "request body is not valid JSON for this endpoint",
        )
    }

    pub(super) const fn unauthorized() -> Self {
        Self::new(
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            "a valid bearer token is required",
        )
    }

    pub(super) const fn forbidden() -> Self {
        Self::new(
            StatusCode::FORBIDDEN,
            "forbidden",
            "the tenant is not allowed to use this service",
        )
    }

    pub(super) const fn not_found() -> Self {
        Self::new(StatusCode::NOT_FOUND, "not_found", "resource not found")
    }

    pub(super) const fn method_not_allowed() -> Self {
        Self::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "method not allowed for this resource",
        )
    }

    pub(super) const fn conflict() -> Self {
        Self::new(
            StatusCode::CONFLICT,
            "conflict",
            "the sandbox is not in a phase that allows this operation",
        )
    }

    pub(super) const fn limit_exceeded() -> Self {
        Self::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "limit_exceeded",
            "the request exceeds a configured limit",
        )
    }

    pub(super) const fn cancelled() -> Self {
        Self::new(
            StatusCode::REQUEST_TIMEOUT,
            "cancelled",
            "the request was cancelled",
        )
    }

    pub(super) const fn timeout() -> Self {
        Self::new(
            StatusCode::GATEWAY_TIMEOUT,
            "timeout",
            "the operation timed out",
        )
    }

    pub(super) const fn cleanup_required() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "cleanup_required",
            "sandbox cleanup is not confirmed; retry DELETE",
        )
    }

    pub(super) const fn backend_unavailable() -> Self {
        Self::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "backend_unavailable",
            "the sandbox backend is unavailable",
        )
    }

    pub(super) const fn unsupported_backend() -> Self {
        Self::new(
            StatusCode::NOT_IMPLEMENTED,
            "unsupported_backend",
            "the requested backend or capability is not implemented",
        )
    }

    pub(super) const fn internal_error() -> Self {
        Self::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal_error",
            "the request could not be completed",
        )
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                code: self.code,
                message: self.message,
            }),
        )
            .into_response()
    }
}

pub(super) fn from_sandbox(error: SandboxError) -> ApiError {
    match error {
        SandboxError::InvalidProcessRequest { .. }
        | SandboxError::Command(_)
        | SandboxError::Config(_)
        | SandboxError::BackendConfig(_)
        | SandboxError::RelativePath { .. } => ApiError::invalid_request(),
        SandboxError::ProcessActive | SandboxError::InvalidPhase { .. } => ApiError::conflict(),
        SandboxError::Workspace(error) => from_workspace(error),
        SandboxError::Snapshot(error) => from_snapshot(error),
        SandboxError::SnapshotInterrupted { interruption, .. } => from_sandbox(*interruption),
        SandboxError::Staging(StagingError::LimitExceeded { .. })
        | SandboxError::Staging(StagingError::SizeOverflow { .. }) => ApiError::limit_exceeded(),
        SandboxError::BackendImageUnavailable { .. }
        | SandboxError::Backend { .. }
        | SandboxError::WorkerGone { .. } => ApiError::backend_unavailable(),
        SandboxError::TimedOut { .. } => ApiError::timeout(),
        SandboxError::Cancelled { .. } => ApiError::cancelled(),
        SandboxError::OutputLimit { .. } => ApiError::limit_exceeded(),
        SandboxError::PolicyMismatch { .. } => ApiError::internal_error(),
        SandboxError::CreationCleanupRequired { .. } | SandboxError::CleanupRequired { .. } => {
            ApiError::cleanup_required()
        }
        SandboxError::UnsupportedCapability { .. } => ApiError::unsupported_backend(),
    }
}

fn from_workspace(error: WorkspaceError) -> ApiError {
    match error {
        WorkspaceError::LimitExceeded { .. } => ApiError::limit_exceeded(),
        WorkspaceError::Closed => ApiError::conflict(),
        WorkspaceError::InvalidLimit { .. }
        | WorkspaceError::PathRejected { .. }
        | WorkspaceError::NotRegularFile { .. } => ApiError::invalid_request(),
        WorkspaceError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            ApiError::not_found()
        }
        WorkspaceError::Io(_) => ApiError::internal_error(),
    }
}

fn from_snapshot(error: SnapshotError) -> ApiError {
    match error {
        SnapshotError::EntryLimit { .. } | SnapshotError::ByteLimit { .. } => {
            ApiError::limit_exceeded()
        }
        SnapshotError::InvalidArchive { .. }
        | SnapshotError::InvalidArchiveLimit
        | SnapshotError::InvalidBudget { .. }
        | SnapshotError::Symlink { .. }
        | SnapshotError::NotRegular { .. }
        | SnapshotError::NotPortable { .. } => ApiError::invalid_request(),
        SnapshotError::Workspace(error) => from_workspace(error),
        SnapshotError::ArchiveCleanup { .. } | SnapshotError::RestoreCleanup { .. } => {
            ApiError::cleanup_required()
        }
        SnapshotError::Interrupted => ApiError::cancelled(),
    }
}
