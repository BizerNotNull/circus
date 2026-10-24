//! Generic lifecycle state operations, independent of any backend SDK.

use std::path::Path;
use std::sync::Arc;

use crate::infrastructure::{
    Backend, CommonSpec, EngineError, EngineErrorKind, InstanceId, StagedBytes,
};
use crate::runtime::{check_relative, SandboxConfig};
use crate::workspace::{StagedNode, WorkspaceError};

use super::{
    CleanupReport, CommandOutput, CommandRequest, EnforcementReport, Ownership, SandboxError,
    SandboxPhase, SandboxStatus, State,
};

/// Builds backend-neutral create values once per sandbox.
pub(super) fn common_spec(config: &SandboxConfig) -> CommonSpec {
    let limits = config.limits();
    CommonSpec {
        memory: i64::try_from(limits.memory_bytes()).unwrap_or(i64::MAX),
        nano_cpus: i64::from(limits.cpu_millis()) * 1_000_000,
        pids: i64::from(limits.pids()),
        workspace_path: config.isolation().workspace_path().to_string(),
        workspace_bytes: limits.workspace_bytes(),
        guest_uid: config.isolation().guest_uid(),
        guest_gid: config.isolation().guest_gid(),
    }
}

/// Rejects a configuration the selected backend cannot honor.
pub(super) fn require_capabilities<B: Backend>(backend: &B) -> Result<(), SandboxError> {
    let capabilities = backend.capabilities();
    if !capabilities.pause_resume {
        return Err(SandboxError::UnsupportedCapability {
            capability: "pause and resume",
        });
    }
    if !capabilities.file_transport {
        return Err(SandboxError::UnsupportedCapability {
            capability: "transfer guest files",
        });
    }
    if !capabilities.workspace_archive {
        return Err(SandboxError::UnsupportedCapability {
            capability: "capture a workspace archive",
        });
    }
    Ok(())
}

pub(super) async fn start<B: Backend>(state: &mut State<B>) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Created, "start")?;
    // Validate and read every staged node before an instance can run.
    let entries = staged(state).await?;
    let id = required_id(state)?;
    if let Err(err) = state.backend.start(&id).await {
        return cleanup_engine_error(state, err).await;
    }
    if let Err(err) = state
        .backend
        .upload(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &entries,
            state.config.limits().workspace_bytes(),
        )
        .await
    {
        return cleanup_engine_error(state, err).await;
    }
    state.phase = SandboxPhase::Running;
    Ok(())
}

pub(super) async fn pause<B: Backend>(state: &mut State<B>) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Running, "pause")?;
    let id = required_id(state)?;
    if let Err(err) = state.backend.pause(&id).await {
        return cleanup_engine_error(state, err).await;
    }
    state.phase = SandboxPhase::Paused;
    Ok(())
}

pub(super) async fn resume<B: Backend>(state: &mut State<B>) -> Result<(), SandboxError> {
    need(state, SandboxPhase::Paused, "resume")?;
    let id = required_id(state)?;
    if let Err(err) = state.backend.resume(&id).await {
        return cleanup_engine_error(state, err).await;
    }
    state.phase = SandboxPhase::Running;
    Ok(())
}

pub(super) async fn cleanup_engine_error<B: Backend, T>(
    state: &mut State<B>,
    err: EngineError,
) -> Result<T, SandboxError> {
    let report = destroy_report(state).await;
    if state.phase == SandboxPhase::CleanupRequired {
        return Err(SandboxError::CleanupRequired {
            instance_id: state.id.as_ref().map_or_else(
                || state.name.clone(),
                |id| id.as_str().to_string(),
            ),
            detail: format!("{}; {}", err.message, report.detail),
        });
    }
    Err(engine_err(err))
}

pub(super) async fn destroy_state<B: Backend>(state: &mut State<B>) -> Result<(), SandboxError> {
    if state.phase == SandboxPhase::Destroyed {
        return Ok(());
    }
    state.phase = SandboxPhase::CleanupRequired;
    let recovery = state.backend.recover_owned(&state.name, &state.nonce).await;
    match recovery {
        Ok(crate::infrastructure::Recovered::Removed) => {}
        Ok(crate::infrastructure::Recovered::Absent) if state.create_unsettled => {
            return Err(SandboxError::CleanupRequired {
                instance_id: state.name.clone(),
                detail: "create response is unresolved; owned instance may materialize later"
                    .to_string(),
            });
        }
        Ok(crate::infrastructure::Recovered::Absent) => {}
        Err(err) => {
            return Err(SandboxError::CleanupRequired {
                instance_id: state.id.as_ref().map_or_else(
                    || state.name.clone(),
                    |id| id.as_str().to_string(),
                ),
                detail: err.message,
            });
        }
    }
    state.create_unsettled = false;
    let workspace = Arc::clone(&state.workspace);
    // Do not clear the owned identity or discard staging on a failed cleanup.
    spawn_block(move || {
        workspace
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .cleanup()
    })
    .await??;
    state.id = None;
    state.phase = SandboxPhase::Destroyed;
    Ok(())
}

pub(super) async fn write_file<B: Backend>(
    state: &mut State<B>,
    path: &str,
    bytes: Vec<u8>,
) -> Result<(), SandboxError> {
    check_relative(path)?;
    if !matches!(state.phase, SandboxPhase::Created | SandboxPhase::Running) {
        return Err(phase_err(state, "write_file"));
    }
    let limit = state.config.limits().max_file_bytes();
    if bytes.len() > limit {
        return Err(WorkspaceError::LimitExceeded {
            limit,
            actual: bytes.len(),
        }
        .into());
    }
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .write_file(path, &bytes)
        })
        .await??);
    }
    let id = required_id(state)?;
    let result = state
        .backend
        .upload(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &[StagedBytes {
                path: path.to_string(),
                is_dir: false,
                bytes,
            }],
            state.config.limits().workspace_bytes(),
        )
        .await;
    file_result(state, path, result).await
}

pub(super) async fn read_file<B: Backend>(
    state: &mut State<B>,
    path: &str,
) -> Result<Vec<u8>, SandboxError> {
    check_relative(path)?;
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .read_file(path)
        })
        .await??);
    }
    need(state, SandboxPhase::Running, "read_file")?;
    let id = required_id(state)?;
    let result = state
        .backend
        .download_file(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            path,
            state.config.limits().max_file_bytes(),
        )
        .await;
    file_result(state, path, result).await
}

pub(super) async fn mkdir<B: Backend>(
    state: &mut State<B>,
    path: &str,
) -> Result<(), SandboxError> {
    check_relative(path)?;
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .create_dir_all(path)
        })
        .await??);
    }
    need(state, SandboxPhase::Running, "create_dir_all")?;
    let id = required_id(state)?;
    let result = state
        .backend
        .upload(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &[StagedBytes {
                path: path.to_string(),
                is_dir: true,
                bytes: Vec::new(),
            }],
            state.config.limits().workspace_bytes(),
        )
        .await;
    file_result(state, path, result).await
}

pub(super) async fn unlink<B: Backend>(
    state: &mut State<B>,
    path: &str,
) -> Result<(), SandboxError> {
    check_relative(path)?;
    if state.phase == SandboxPhase::Created {
        let workspace = Arc::clone(&state.workspace);
        let path = path.to_string();
        return Ok(spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .remove_file(path)
        })
        .await??);
    }
    need(state, SandboxPhase::Running, "remove_file")?;
    let id = required_id(state)?;
    let result = state
        .backend
        .remove_file(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            path,
        )
        .await;
    file_result(state, path, result).await
}

pub(super) async fn exec<B: Backend>(
    state: &mut State<B>,
    request: CommandRequest,
) -> Result<CommandOutput, SandboxError> {
    need(state, SandboxPhase::Running, "exec")?;
    if request.output_limit_bytes > state.config.output_limit_bytes() {
        return Err(SandboxError::OutputLimit {
            limit: state.config.output_limit_bytes(),
            ownership: Ownership::StillPresent,
            detail: "request exceeds configured output limit".to_string(),
        });
    }
    let id = required_id(state)?;
    let result = state
        .backend
        .exec(
            &id,
            &state.user,
            state.config.isolation().workspace_path(),
            &request.argv,
            request.output_limit_bytes,
        )
        .await;
    match result {
        Ok(done) => Ok(CommandOutput {
            exit_code: done.exit_code,
            stdout: done.stdout,
            stderr: done.stderr,
        }),
        Err(err) if err.kind == EngineErrorKind::Limit => {
            let report = destroy_report(state).await;
            Err(SandboxError::OutputLimit {
                limit: request.output_limit_bytes,
                ownership: report.ownership,
                detail: report.detail,
            })
        }
        Err(err) => cleanup_engine_error(state, err).await,
    }
}

pub(super) async fn destroy_for_cancel<B: Backend, T>(
    state: &mut State<B>,
    detail: &str,
) -> Result<T, SandboxError> {
    let report = destroy_report(state).await;
    Err(SandboxError::Cancelled {
        instance_id: report.instance_id,
        ownership: report.ownership,
        detail: format!("{detail}; {}", report.detail),
    })
}

pub(super) async fn destroy_report<B: Backend>(state: &mut State<B>) -> CleanupReport {
    let id = state.id.as_ref().map(|id| id.as_str().to_string());
    let report = match destroy_state(state).await {
        Ok(()) => CleanupReport {
            instance_id: None,
            ownership: Ownership::ConfirmedAbsent,
            detail: "owned instance confirmed absent".to_string(),
        },
        Err(err) => CleanupReport {
            instance_id: id.or_else(|| Some(state.name.clone())),
            ownership: Ownership::Unknown,
            detail: err.to_string(),
        },
    };
    state.cleanup.send_replace(super::cleanup::Progress {
        report: report.clone(),
        complete: state.phase == SandboxPhase::Destroyed,
        attempted: true,
    });
    report
}

pub(super) async fn status<B: Backend>(
    state: &State<B>,
) -> Result<SandboxStatus, SandboxError> {
    let Some(id) = state.id.as_ref() else {
        return Ok(SandboxStatus {
            phase: state.phase,
            instance_id: None,
            enforcement: None,
        });
    };
    let report = state.backend.host_report(id).await.map_err(engine_err)?;
    Ok(SandboxStatus {
        phase: state.phase,
        instance_id: Some(id.as_str().to_string()),
        enforcement: Some(EnforcementReport {
            runtime: report.runtime,
            memory_bytes: report.memory_bytes,
            nano_cpus: report.nano_cpus,
            pids_limit: report.pids_limit,
            readonly_root: report.readonly_root,
            network_mode: report.network_mode,
            cap_drop: report.cap_drop,
            security_opt: report.security_opt,
            bind_count: report.bind_count,
            tmpfs_workspace: report.tmpfs_workspace,
        }),
    })
}

pub(super) async fn staged<B: Backend>(
    state: &State<B>,
) -> Result<Vec<StagedBytes>, SandboxError> {
    let workspace = Arc::clone(&state.workspace);
    let nodes = spawn_block(move || {
        workspace
            .lock()
            .unwrap_or_else(|err| err.into_inner())
            .staged_nodes()
    })
    .await??;
    let mut entries = Vec::new();
    let mut total = 0u64;
    for StagedNode { relative, is_dir } in nodes {
        check_relative(&relative)?;
        if is_dir {
            entries.push(StagedBytes {
                path: relative,
                is_dir: true,
                bytes: Vec::new(),
            });
            continue;
        }
        let workspace = Arc::clone(&state.workspace);
        let path = relative.clone();
        let bytes = spawn_block(move || {
            workspace
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .read_file(path)
        })
        .await??;
        total = super::input::staged_total(
            total,
            bytes.len() as u64,
            state.config.limits().workspace_bytes(),
        )?;
        entries.push(StagedBytes {
            path: relative,
            is_dir: false,
            bytes,
        });
    }
    Ok(entries)
}

pub(super) fn need<B: Backend>(
    state: &State<B>,
    phase: SandboxPhase,
    operation: &'static str,
) -> Result<(), SandboxError> {
    if state.phase == phase {
        Ok(())
    } else {
        Err(phase_err(state, operation))
    }
}

pub(super) fn phase_err<B: Backend>(
    state: &State<B>,
    operation: &'static str,
) -> SandboxError {
    SandboxError::InvalidPhase {
        current: state.phase,
        operation,
    }
}

pub(super) fn required_id<B: Backend>(state: &State<B>) -> Result<InstanceId, SandboxError> {
    state
        .id
        .clone()
        .ok_or_else(|| SandboxError::CleanupRequired {
            instance_id: state.name.clone(),
            detail: "instance id is not retained".to_string(),
        })
}

pub(super) fn path_text(path: &Path) -> Result<String, SandboxError> {
    path.to_str()
        .map(str::to_string)
        .ok_or(SandboxError::RelativePath {
            reason: "path is not utf-8",
        })
}

pub(super) async fn file_result<B: Backend, T>(
    state: &mut State<B>,
    path: &str,
    result: Result<T, EngineError>,
) -> Result<T, SandboxError> {
    match result {
        Ok(value) => Ok(value),
        Err(err) => match err.kind {
            EngineErrorKind::UnsafeFile => {
                Err(WorkspaceError::NotRegularFile { path: path.into() }.into())
            }
            EngineErrorKind::Limit => Err(WorkspaceError::LimitExceeded {
                limit: state.config.limits().max_file_bytes(),
                actual: state.config.limits().max_file_bytes().saturating_add(1),
            }
            .into()),
            EngineErrorKind::NotFound => Err(WorkspaceError::Io(std::io::Error::new(
                std::io::ErrorKind::NotFound,
                err.message,
            ))
            .into()),
            EngineErrorKind::PermissionDenied => Err(WorkspaceError::Io(std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                err.message,
            ))
            .into()),
            EngineErrorKind::FileIo => {
                Err(WorkspaceError::Io(std::io::Error::other(err.message)).into())
            }
            EngineErrorKind::InvalidPath => Err(SandboxError::RelativePath {
                reason: "guest transport rejected the relative path",
            }),
            EngineErrorKind::Other | EngineErrorKind::Protocol => {
                cleanup_engine_error(state, err).await
            }
        },
    }
}

pub(super) fn engine_err(err: EngineError) -> SandboxError {
    match err.kind {
        EngineErrorKind::InvalidPath => SandboxError::RelativePath {
            reason: "guest transport rejected the relative path",
        },
        EngineErrorKind::UnsafeFile
        | EngineErrorKind::Limit
        | EngineErrorKind::FileIo
        | EngineErrorKind::NotFound
        | EngineErrorKind::PermissionDenied
        | EngineErrorKind::Protocol
        | EngineErrorKind::Other => SandboxError::Backend {
            message: err.message,
        },
    }
}

pub(super) async fn spawn_block<T: Send + 'static>(
    f: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SandboxError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|err| SandboxError::WorkerGone {
            detail: err.to_string(),
        })
}
