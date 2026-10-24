//! Fixed-revision Git preparation and code-only workspace import.
mod owner;
#[cfg(test)]
mod owner_tests;
use std::{path::PathBuf, time::Duration};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_util::sync::CancellationToken;
use url::Url;
use crate::runtime::ProcessError;
use super::{CodeCopyError, SnapshotBudget, Workspace};

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("invalid git options: {0}")]
    Options(String),
    #[error("invalid source: {0}")]
    Source(String),
    #[error("invalid revision: {0}")]
    Revision(String),
    #[error("git process failed: {0}")]
    Process(#[from] ProcessError),
    #[error("git exited unsuccessfully ({status})")]
    Command { status: std::process::ExitStatus },
    #[error("code copy failed: {0}")]
    Copy(#[from] CodeCopyError),
    #[error("cleanup failed: {0}")]
    Cleanup(#[source] std::io::Error),
    #[error("Git cleanup remains owned by its worker: {source}")]
    CleanupRequired { #[source] source: Box<GitError>, cleanup: GitCleanup },
    #[error("Git owner exited without confirming cleanup")]
    WorkerGone,
    #[error("checkout is closed")]
    Closed,
}

/// Opaque receipt. Dropping it never cancels the independent cleanup owner.
#[derive(Clone, Debug)]
pub struct GitCleanup { done: watch::Receiver<bool> }
impl GitCleanup {
    /// Wait for both process termination and all owned directories to be removed.
    pub async fn wait(&self) -> Result<(), GitError> {
        let mut done = self.done.clone();
        loop {
            if *done.borrow_and_update() { return Ok(()); }
            done.changed().await.map_err(|_| GitError::WorkerGone)?;
        }
    }
}

#[derive(Clone)]
pub struct GitOptions {
    executable: PathBuf,
    storage_root: PathBuf,
    timeout: Duration,
    max_output_bytes: usize,
}
impl std::fmt::Debug for GitOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitOptions").field("timeout", &self.timeout).field("max_output_bytes", &self.max_output_bytes).finish_non_exhaustive()
    }
}
impl GitOptions {
    pub fn new(executable: PathBuf, storage_root: PathBuf, timeout: Duration, max_output_bytes: usize) -> Result<Self, GitError> {
        if !executable.is_absolute() { return Err(GitError::Options("git executable must be absolute".into())); }
        if !storage_root.is_absolute() { return Err(GitError::Options("storage root must be absolute".into())); }
        if timeout.is_zero() || max_output_bytes == 0 { return Err(GitError::Options("timeout and output budget must be positive".into())); }
        Ok(Self { executable, storage_root, timeout, max_output_bytes })
    }
}

pub struct GitSource(SourceKind);
enum SourceKind { Local(PathBuf), Https(Url) }
impl std::fmt::Debug for GitSource { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("GitSource").finish_non_exhaustive() } }
impl GitSource {
    /// Grants access to a trusted filesystem repository, not filesystem confinement.
    pub fn local(path: PathBuf) -> Result<Self, GitError> {
        if !path.is_absolute() { return Err(GitError::Source("local source must be absolute".into())); }
        Ok(Self(SourceKind::Local(path)))
    }
    /// Grants HTTPS access; this is not an SSRF or network isolation policy.
    pub fn https(url: Url) -> Result<Self, GitError> {
        if url.scheme() != "https" || url.cannot_be_a_base() || url.username() != "" || url.password().is_some() || url.query().is_some() || url.fragment().is_some() {
            return Err(GitError::Source("URL must be HTTPS without credentials, query, or fragment".into()));
        }
        Ok(Self(SourceKind::Https(url)))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitRevision(String);
impl GitRevision {
    pub fn parse(value: &str) -> Result<Self, GitError> {
        if (value.len() != 40 && value.len() != 64) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(GitError::Revision("expected a full hexadecimal commit".into()));
        }
        Ok(Self(value.to_ascii_lowercase()))
    }
}

/// A message handle; only the detached worker owns host resources.
pub struct GitCheckout {
    requests: mpsc::UnboundedSender<owner::Request>,
    shutdown: CancellationToken,
    revision: GitRevision,
}
impl Drop for GitCheckout { fn drop(&mut self) { self.shutdown.cancel(); } }
impl std::fmt::Debug for GitCheckout { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.debug_struct("GitCheckout").field("revision", &self.revision).finish_non_exhaustive() } }
impl GitCheckout {
    /// Prepare and verify an exact commit. Cancellation retains an independent
    /// owner until started processes and blocking stages have drained and cleaned.
    pub async fn clone(source: GitSource, revision: GitRevision, options: GitOptions) -> Result<Self, GitError> {
        let shutdown = CancellationToken::new();
        let guard = shutdown.clone().drop_guard();
        let (requests, receive) = mpsc::unbounded_channel();
        let (ready, result) = oneshot::channel();
        tokio::spawn(owner::serve(source, revision.clone(), options, shutdown.clone(), receive, ready));
        result.await.map_err(|_| GitError::WorkerGone)??;
        guard.disarm();
        Ok(Self { requests, shutdown, revision })
    }
    pub fn revision(&self) -> &GitRevision { &self.revision }
    /// Copy only repository code. The deadline includes queued and blocking work;
    /// cancellation drains a started copy and explicitly removes its destination.
    pub async fn copy_to_workspace(&self, max_file_bytes: usize, budget: SnapshotBudget) -> Result<Workspace, GitError> {
        let cancel = self.shutdown.child_token();
        let _guard = cancel.clone().drop_guard();
        let (reply, result) = oneshot::channel();
        let slot = std::sync::Arc::new(std::sync::Mutex::new(None));
        let (accepted, acknowledgment) = oneshot::channel();
        self.requests.send(owner::Request::Copy { max_file_bytes, budget, cancel, started: tokio::time::Instant::now(), reply, slot: slot.clone(), acknowledgment }).map_err(|_| GitError::Closed)?;
        result.await.map_err(|_| GitError::WorkerGone)??;
        let result = slot.lock().expect("Git delivery lock poisoned").take().ok_or(GitError::WorkerGone)?;
        let _ = accepted.send(());
        result
    }
    /// Idempotent explicit cleanup. A failed removal retains its identity in the
    /// worker and returns a receipt; retries never consume a failed TempDir.
    pub async fn cleanup(&mut self) -> Result<(), GitError> {
        let (reply, result) = oneshot::channel();
        self.requests.send(owner::Request::Cleanup(reply)).map_err(|_| GitError::Closed)?;
        result.await.map_err(|_| GitError::WorkerGone)?
    }
}
