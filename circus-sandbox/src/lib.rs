//! Capability-scoped workspace files, resource requests, and a Linux Docker sandbox.
//!
//! [`Workspace`] confines file access to a directory it creates. [`DockerSandbox`]
//! is a separate async API: it stages those files into a bounded tmpfs and runs
//! commands in a Linux container through an explicit local Docker endpoint.
//! Public types do not expose the Docker SDK.
//!
//! [`CodeSource`] copies an explicitly authorized source directory into a fresh
//! owned workspace. [`CodeSnapshot`] captures workspace contents in memory.
//! Neither is a process/container checkpoint. Pool, Git, HTTP, Kubernetes,
//! gVisor, Kata, and Firecracker backends are not implemented.
//!
//! # Blocking
//!
//! Workspace methods block the calling thread. [`DockerSandbox`] methods are
//! async and must be called from a Tokio runtime. They run workspace I/O with
//! `spawn_blocking`.
//!
//! # Limits
//!
//! [`Workspace::new`] takes a positive byte cap for a single file. It is not a
//! workspace total, file-count, or host disk quota, and this crate has no
//! total-quota API. Reads never allocate from untrusted file metadata and stop
//! after proving the file exceeds the cap. Writes that exceed the cap fail
//! before any create or truncate. A closed workspace, a rejected path, a
//! missing parent, or a non-regular node also returns before the file is
//! created or truncated. Replacing a regular file is not atomic and not
//! crash-safe: after it is opened, its length is set to zero and then the
//! bytes are written. An I/O error after that open can leave the file
//! truncated or only partly written. This crate has no atomic-write API.
//!
//! Calls on one [`Workspace`] share an internal lock so a read does not observe
//! another call on that value mid-truncate. That is not process isolation:
//! other processes can still change the directory.
//!
//! ```
//! use circus_sandbox::Workspace;
//!
//! let mut workspace = Workspace::new(64)?;
//! workspace.write_file("note.txt", b"hello")?;
//! assert_eq!(workspace.read_file("note.txt")?, b"hello");
//! workspace.remove_file("note.txt")?;
//! workspace.cleanup()?;
//! # Ok::<(), circus_sandbox::WorkspaceError>(())
//! ```

#![forbid(unsafe_code)]

pub mod api;
mod infrastructure;
mod lifecycle;
pub mod runtime;
mod workspace;

#[doc(inline)]
pub use api::{
    CleanupReport, CommandError, CommandOutput, CommandRequest, ConfigError, DockerEndpoint,
    DockerSandbox, IoLimits, IsolationConfig, OWNER_LABEL_KEY, Ownership, RuntimeName,
    SandboxConfig, SandboxError, SandboxPhase, SandboxStatus, StagingError,
};
#[doc(inline)]
pub use workspace::{
    CodeCopyError, CodeSnapshot, CodeSource, GitCheckout, GitCleanup, GitError, GitOptions, GitRevision,
    GitSource, PathRejectReason, SnapshotBudget, SnapshotError, Workspace, WorkspaceError,
};

#[doc(inline)]
pub use runtime::{ResourceLimits, ResourceLimitsError};
