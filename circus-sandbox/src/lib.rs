//! Capability-scoped temporary workspace files.
//!
//! [`Workspace`] confines file access to a directory it creates and opens as a
//! [`cap_std::fs::Dir`]. Callers pass relative paths only. The root path is not
//! exposed. This crate is **not** a process sandbox and does not provide
//! containers, OCI runtimes, mounts, or host isolation.
//!
//! # Blocking
//!
//! Every filesystem method blocks the calling thread. Async callers must run
//! them with `spawn_blocking` (or the equivalent) on their own runtime. This
//! crate does not start a runtime and does not depend on one.
//!
//! # Limits
//!
//! [`Workspace::new`] takes a positive byte cap. Reads never allocate from
//! untrusted file metadata and stop after proving the file exceeds the cap.
//! Writes that exceed the cap fail before any create or truncate. A closed
//! workspace, a rejected path, a missing parent, or a non-regular node also
//! returns before the file is created or truncated. Replacing a regular file
//! is not atomic: after it is opened, its length is set to zero and then the
//! bytes are written. An I/O error after that open can leave the file
//! truncated or only partly written. This crate has no atomic-write API.
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

mod workspace;

#[doc(inline)]
pub use workspace::{PathRejectReason, Workspace, WorkspaceError};
