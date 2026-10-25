//! Public, SDK-independent handle types for a managed attached process.

use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use super::SandboxError;

use crate::infrastructure::{AttachedExec, EngineError, OutputStream};

pub(crate) const SESSION_QUEUE: usize = 8;

pub(crate) async fn run_attached(
    mut exec: AttachedExec,
    mut commands: mpsc::Receiver<SessionCommand>,
    output: mpsc::Sender<Result<ProcessOutput, SandboxError>>,
    completion: oneshot::Sender<Result<i64, SandboxError>>,
    cancel: CancellationToken,
    timeout: std::time::Duration,
    output_limit: usize,
) -> Result<(), SandboxError> {
    let mut used = 0usize;
    let mut stdin_open = true;
    let deadline = tokio::time::sleep(timeout);
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => return Ok(()),
            () = &mut deadline => {
                let detail = "attached process session timed out".to_string();
                let _ = completion.send(Err(SandboxError::TimedOut {
                    container_id: None,
                    ownership: super::Ownership::StillPresent,
                    detail: detail.clone(),
                }));
                return Err(SandboxError::TimedOut {
                    container_id: None,
                    ownership: super::Ownership::StillPresent,
                    detail,
                });
            }
            command = commands.recv(), if stdin_open => {
                match command {
                    Some(SessionCommand::Stdin(bytes)) => {
                        let result = tokio::select! {
                            biased;
                            () = cancel.cancelled() => return Ok(()),
                            result = exec.write_stdin(&bytes) => result,
                        };
                        if let Err(err) = result {
                            let message = err.message;
                            let _ = completion.send(Err(SandboxError::Engine { message: message.clone() }));
                            return Err(SandboxError::Engine { message });
                        }
                    }
                    Some(SessionCommand::CloseStdin) => {
                        stdin_open = false;
                        let result = tokio::select! {
                            biased;
                            () = cancel.cancelled() => return Ok(()),
                            result = exec.close_stdin() => result,
                        };
                        if let Err(err) = result {
                            let message = err.message;
                            let _ = completion.send(Err(SandboxError::Engine { message: message.clone() }));
                            return Err(SandboxError::Engine { message });
                        }
                    }
                    None => {
                        stdin_open = false;
                        if let Err(err) = exec.close_stdin().await {
                            let message = err.message;
                            let _ = completion.send(Err(SandboxError::Engine { message: message.clone() }));
                            return Err(SandboxError::Engine { message });
                        }
                    }
                }
            }
            result = exec.recv() => {
                match result {
                    Ok(Some(chunk)) => {
                        used = used.saturating_add(chunk.bytes.len());
                        if used > output_limit {
                            let detail = "attached process exceeded combined stdout/stderr limit".to_string();
                            let error = SandboxError::OutputLimit {
                                limit: output_limit,
                                ownership: super::Ownership::StillPresent,
                                detail: detail.clone(),
                            };
                            let _ = tokio::select! {
                                biased;
                                () = cancel.cancelled() => false,
                                result = output.send(Err(error)) => result.is_ok(),
                            };
                            let _ = completion.send(Err(SandboxError::OutputLimit {
                                limit: output_limit,
                                ownership: super::Ownership::StillPresent,
                                detail: detail.clone(),
                            }));
                            return Err(SandboxError::OutputLimit {
                                limit: output_limit,
                                ownership: super::Ownership::StillPresent,
                                detail,
                            });
                        }
                        let item = match chunk.stream {
                            OutputStream::Stdout => ProcessOutput::Stdout(chunk.bytes),
                            OutputStream::Stderr => ProcessOutput::Stderr(chunk.bytes),
                        };
                        let sent = tokio::select! {
                            biased;
                            () = cancel.cancelled() => false,
                            result = output.send(Ok(item)) => result.is_ok(),
                        };
                        if !sent {
                            if cancel.is_cancelled() {
                                return Ok(());
                            }
                            return Err(SandboxError::WorkerGone {
                                detail: "process output receiver was dropped".to_string(),
                            });
                        }
                    }
                    Ok(None) => {
                        let exit_code = loop {
                            tokio::select! {
                                biased;
                                () = cancel.cancelled() => return Ok(()),
                                () = &mut deadline => {
                                    let detail = "attached process session timed out".to_string();
                                    let _ = completion.send(Err(SandboxError::TimedOut {
                                        container_id: None,
                                        ownership: super::Ownership::StillPresent,
                                        detail: detail.clone(),
                                    }));
                                    return Err(SandboxError::TimedOut {
                                        container_id: None,
                                        ownership: super::Ownership::StillPresent,
                                        detail,
                                    });
                                }
                                result = exec.completion() => {
                                    match result.map_err(engine_error)? {
                                        Some(code) => break code,
                                        None => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
                                    }
                                }
                            }
                        };
                        let _ = completion.send(Ok(exit_code));
                        return Ok(());
                    }
                    Err(err) => {
                        let message = err.message;
                        let _ = output.send(Err(SandboxError::Engine { message: message.clone() })).await;
                        let _ = completion.send(Err(SandboxError::Engine { message: message.clone() }));
                        return Err(SandboxError::Engine { message });
                    }
                }
            }
        }
    }
}

fn engine_error(err: EngineError) -> SandboxError {
    SandboxError::Engine {
        message: err.message,
    }
}

/// Command used to start a long-lived process in a running sandbox.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessRequest {
    /// Executable and arguments, passed separately (never interpreted by a shell).
    pub argv: Vec<String>,
    /// Maximum wall-clock time before the owning sandbox is destroyed.
    pub timeout: std::time::Duration,
    /// Maximum combined stdout and stderr bytes accepted for this process.
    pub output_limit_bytes: usize,
}

/// One output stream item, preserving stdout/stderr identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutput {
    Stdout(Vec<u8>),
    Stderr(Vec<u8>),
}

pub(crate) enum SessionCommand {
    Stdin(Vec<u8>),
    CloseStdin,
}

/// Opaque handle to one process whose lifetime is owned by its sandbox.
///
/// Dropping or closing the handle cancels the process session. The lifecycle
/// worker destroys the owning container and waits for attached I/O cleanup.
pub struct ProcessSession {
    pub(crate) commands: mpsc::Sender<SessionCommand>,
    pub(crate) output: mpsc::Receiver<Result<ProcessOutput, SandboxError>>,
    pub(crate) completion: Option<oneshot::Receiver<Result<i64, SandboxError>>>,
    pub(crate) cancel: CancellationToken,
    pub(crate) close_reply: Option<oneshot::Receiver<Result<(), SandboxError>>>,
    pub(crate) armed: bool,
}

impl ProcessSession {
    /// Send bytes to the process stdin. Calls are backpressured by the bounded session pipe.
    pub async fn write_stdin(&self, bytes: &[u8]) -> Result<(), SandboxError> {
        self.commands
            .send(SessionCommand::Stdin(bytes.to_vec()))
            .await
            .map_err(|_| SandboxError::WorkerGone {
                detail: "process session is no longer accepting stdin".to_string(),
            })
    }

    /// Close process stdin while continuing to read output and completion.
    pub async fn close_stdin(&self) -> Result<(), SandboxError> {
        self.commands
            .send(SessionCommand::CloseStdin)
            .await
            .map_err(|_| SandboxError::WorkerGone {
                detail: "process session is no longer accepting stdin".to_string(),
            })
    }

    /// Read the next bounded stdout/stderr chunk. Returns `None` after output closes.
    pub async fn recv(&mut self) -> Result<Option<ProcessOutput>, SandboxError> {
        match self.output.recv().await {
            Some(Ok(chunk)) => Ok(Some(chunk)),
            Some(Err(err)) => Err(err),
            None => Ok(None),
        }
    }

    /// Wait for the child exit code. Non-zero is a normal completed process.
    pub async fn wait(mut self) -> Result<i64, SandboxError> {
        self.armed = false;
        self.completion
            .take()
            .expect("completion receiver exists until wait")
            .await
            .map_err(|_| SandboxError::WorkerGone {
                detail: "process session completion was dropped".to_string(),
            })?
    }
    /// Cancel the process and await cleanup of its owning sandbox.
    pub async fn close(mut self) -> Result<(), SandboxError> {
        self.cancel.cancel();
        match self.close_reply.take() {
            Some(reply) => reply.await.map_err(|_| SandboxError::WorkerGone {
                detail: "process session cleanup reply was dropped".to_string(),
            })?,
            None => Ok(()),
        }
    }
}

impl Drop for ProcessSession {
    fn drop(&mut self) {
        if self.armed {
            self.cancel.cancel();
        }
    }
}
