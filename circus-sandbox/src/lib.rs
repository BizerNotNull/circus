//! Capability-scoped temporary workspace files and validated resource requests.
//!
//! [`Workspace`] confines file access to a directory it creates and opens as a
//! [`cap_std::fs::Dir`]. Callers pass relative paths only. The root path is not
//! exposed. This crate is **not** a process sandbox and does not provide
//! containers, OCI runtimes, mounts, or host isolation.
//!
//! [`runtime::ResourceLimits`] validates backend-neutral CPU, memory, process,
//! workspace, and per-file requests. Validation does not enforce limits, provide
//! isolation, reserve resources, or perform aggregate admission control.
//!
//! # Blocking
//!
//! Every filesystem method blocks the calling thread. Async callers must run
//! them with `spawn_blocking` (or the equivalent) on their own runtime. This
//! crate does not start a runtime and does not depend on one.
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
//! another call on that value mid-truncate. That is not process isolation. On
//! Unix the workspace root is created with mode `0o700`, so other host users
//! cannot traverse it; same-account and privileged processes still can change
//! the directory. Windows keeps tempfile's platform default.
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

pub mod runtime;
mod workspace;

#[doc(inline)]
pub use workspace::{PathRejectReason, Workspace, WorkspaceError};
