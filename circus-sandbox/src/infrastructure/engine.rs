//! Docker Engine calls. Bollard types stay in this module.

#[path = "images.rs"]
mod images;
pub(crate) use images::ImageInfo;

#[path = "engine_output.rs"]
mod output;
pub(crate) use output::{OutputLimit, OutputPhase};

#[path = "engine_manifest.rs"]
mod manifest;
pub(crate) use manifest::{WorkspaceEntry, WorkspaceEntryKind};

#[path = "engine_policy.rs"]
mod policy;
pub(crate) use policy::{HostReport, OWNER_LABEL_KEY, PolicyMismatch, create_body};

#[path = "engine_recovery.rs"]
mod recovery;
pub(crate) use recovery::{OwnerCheck, Recovered, RemovalControl, recover_with};

mod process;
pub(crate) use process::{EngineProcess, ProcessChunk, ProcessSpec};

use std::future::Future;

use futures_util::StreamExt;

use tokio::io::{AsyncReadExt, AsyncWriteExt};

const GUEST_FILES: &str = include_str!("guest_files.py");

/// One bounded host-to-guest copy buffer. The guest helper reads stdin in the
/// same 64 KiB units, so neither side needs a whole-file buffer.
const TRANSFER_CHUNK_BYTES: usize = 64 * 1024;

/// One staging entry presented to a backend.
///
/// Payloads are never accumulated for the whole tree. A directory carries no
/// payload, an in-memory value is used for a single API write, and a staged
/// file is transferred from an already-opened host handle in bounded chunks.
pub(crate) struct UploadEntry {
    pub path: String,
    pub source: UploadSource,
}

pub(crate) enum UploadSource {
    Directory,
    Bytes(Vec<u8>),
    File { file: std::fs::File, len: u64 },
}

/// Stdin framing for one guest exec.
enum Stdin<'a> {
    None,
    Bytes(&'a [u8]),
    File { file: std::fs::File, len: u64 },
}

impl Stdin<'_> {
    fn attached(&self) -> bool {
        !matches!(self, Self::None)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EngineErrorKind {
    Other,
    FileIo,
    NotFound,
    PermissionDenied,
    UnsafeFile,
    Limit,
    InvalidPath,
    Protocol,
    /// The guest stopped listing after the caller's entry bound.
    EntryLimit,
}

const CLIENT_TIMEOUT_SECS: u64 = 90;

#[derive(Clone, Copy)]
pub(crate) enum SocketKind {
    Unix,
    Pipe,
}

#[derive(Clone)]
pub(crate) struct Engine {
    docker: bollard::Docker,
}

#[derive(Debug)]
pub(crate) struct EngineError {
    pub message: String,
    pub not_found: bool,
    pub kind: EngineErrorKind,
    pub create_pending: bool,
    pub create_id: Option<String>,
    /// Typed output-budget violation when this error was produced while
    /// collecting stdout/stderr.
    pub output: Option<OutputLimit>,
    /// Typed effective-policy mismatch when creation verification failed.
    pub policy: Option<Box<PolicyMismatch>>,
}

impl EngineError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            not_found: false,
            kind: EngineErrorKind::Other,
            create_pending: false,
            create_id: None,
            output: None,
            policy: None,
        }
    }
}

pub(crate) struct CreateSpec {
    pub image: String,
    pub runtime: String,
    pub memory: i64,
    pub nano_cpus: i64,
    pub pids: i64,
    pub workspace_path: String,
    pub workspace_bytes: u64,
    pub guest_uid: u32,
    pub guest_gid: u32,
    pub owner_label: String,
}

pub(crate) struct ExecDone {
    pub exit_code: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

struct FileRequest<'a> {
    relative: &'a str,
    mode: &'static str,
    stdin: Stdin<'a>,
    max_bytes: usize,
    /// Only `list` carries one; it becomes the helper's entry bound.
    max_entries: Option<usize>,
}

struct ExecIo<'a> {
    stdin: Stdin<'a>,
    /// Per-stream allowance for stdout.
    stdout_limit: usize,
    /// Per-stream allowance for stderr.
    stderr_limit: usize,
    /// Combined stdout+stderr budget. Always the explicit sum of the two
    /// allowances, never an accidental doubling.
    combined_limit: usize,
}

impl Engine {
    pub(crate) async fn connect(kind: SocketKind, path: &str) -> Result<Self, EngineError> {
        let docker = connect_socket(kind, path)?;
        let docker = docker.negotiate_version().await.map_err(map_err)?;
        Ok(Self { docker })
    }

    pub(crate) async fn prepare(&self, image: &str, runtime: &str) -> Result<(), EngineError> {
        let info = self.docker.info().await.map_err(map_err)?;
        let os = info.os_type.unwrap_or_default();
        if !os.eq_ignore_ascii_case("linux") {
            return Err(EngineError::new(format!(
                "engine OS {os} is not linux; refusing to continue"
            )));
        }
        if info.memory_limit != Some(true)
            || info.swap_limit != Some(true)
            || info.pids_limit != Some(true)
            || info.cpu_cfs_period != Some(true)
            || info.cpu_cfs_quota != Some(true)
        {
            return Err(EngineError::new(
                "engine does not support required memory, swap, pids and CPU cgroup limits",
            ));
        }
        if !info.runtimes.unwrap_or_default().contains_key(runtime) {
            return Err(EngineError::new(
                "engine has no runc runtime; refusing to substitute another runtime",
            ));
        }
        let inspected = self.docker.inspect_image(image).await.map_err(map_err)?;
        if inspected.os.as_deref() != Some("linux") {
            return Err(EngineError::new(
                "local image is not a linux image; this crate does not pull",
            ));
        }
        if inspected
            .config
            .as_ref()
            .and_then(|config| config.volumes.as_ref())
            .is_some_and(|volumes| !volumes.is_empty())
        {
            return Err(EngineError::new("images declaring VOLUME are unsupported"));
        }
        Ok(())
    }

    pub(crate) async fn create_owned(
        &self,
        name: &str,
        spec: &CreateSpec,
    ) -> Result<String, EngineError> {
        // Pin the inspected image so a mutable tag cannot introduce volumes.
        let image = self
            .docker
            .inspect_image(&spec.image)
            .await
            .map_err(map_err)?;
        if image
            .config
            .as_ref()
            .and_then(|config| config.volumes.as_ref())
            .is_some_and(|volumes| !volumes.is_empty())
        {
            return Err(EngineError::new("images declaring VOLUME are unsupported"));
        }
        let mut body = create_body(name, spec);
        body.image = Some(
            image
                .id
                .filter(|id| !id.is_empty())
                .ok_or_else(|| EngineError::new("image inspect returned no immutable id"))?,
        );
        let options = bollard::query_parameters::CreateContainerOptionsBuilder::new()
            .name(name)
            .platform("linux")
            .build();
        let created = self
            .docker
            .create_container(Some(options), body)
            .await
            .map_err(map_create_err)?;
        if created.id.is_empty() {
            return Err(EngineError::new("engine create returned an empty id"));
        }
        let created_id = created.id;
        if !created.warnings.is_empty() {
            return Err(EngineError {
                message: format!("engine create warnings: {:?}", created.warnings),
                not_found: false,
                kind: EngineErrorKind::Other,
                create_pending: false,
                create_id: Some(created_id),
                output: None,
                policy: None,
            });
        }
        let inspected = self
            .docker
            .inspect_container(&created_id, None)
            .await
            .map_err(|err| EngineError {
                create_id: Some(created_id.clone()),
                ..map_err(err)
            })?;
        let Some(host) = inspected.host_config else {
            return Err(EngineError {
                create_id: Some(created_id),
                ..PolicyMismatch {
                    field: "host_config",
                    expected: "reported by engine".to_string(),
                    actual: "not reported".to_string(),
                }
                .error()
            });
        };
        let report = policy::report_from(Some(host));
        if let Err(mismatch) = policy::verify(spec, &report) {
            return Err(EngineError {
                create_id: Some(created_id),
                ..mismatch.error()
            });
        }
        Ok(created_id)
    }

    /// Confirms absence of the owned instance and force-removes it.
    ///
    /// Uses the retained identity when a transient inspection failure prevents
    /// confirming the name, and always attempts forced removal even when the
    /// best-effort stop call fails. Returns an explicit ownership outcome.
    pub(crate) async fn recover_owned(
        &self,
        name: &str,
        owner_label: &str,
        retained_id: Option<&str>,
    ) -> Recovered {
        recover_with(self, name, owner_label, retained_id).await
    }

    pub(crate) async fn start(&self, id: &str) -> Result<(), EngineError> {
        self.docker
            .start_container(id, None::<bollard::query_parameters::StartContainerOptions>)
            .await
            .map_err(map_err)
    }

    pub(crate) async fn pause(&self, id: &str) -> Result<(), EngineError> {
        self.docker.pause_container(id).await.map_err(map_err)
    }

    pub(crate) async fn resume(&self, id: &str) -> Result<(), EngineError> {
        self.docker.unpause_container(id).await.map_err(map_err)
    }

    /// Enumerates only the configured workspace through the confined guest
    /// helper, never the container root and never Docker's archive API.
    ///
    /// The helper walks the workspace without following links and refuses
    /// symlinks, hardlinks, FIFOs, devices, sockets, non-UTF-8 names, and names
    /// that are not portable relative paths. It emits a sorted NUL-delimited
    /// manifest; `max_bytes` bounds that manifest (the helper fails with the
    /// limit exit when a record would cross it) and `max_entries` bounds the
    /// number of listed entries with a typed entry-limit exit. A zero byte bound
    /// is rejected before exec. Payloads are not transferred here; callers read
    /// them one file at a time with [`Self::download_file`].
    pub(crate) async fn list_workspace(
        &self,
        id: &str,
        user: &str,
        workspace: &str,
        max_bytes: u64,
        max_entries: usize,
    ) -> Result<Vec<WorkspaceEntry>, EngineError> {
        if max_bytes == 0 {
            return Err(file_error(
                EngineErrorKind::Limit,
                "workspace manifest byte limit must be positive",
            ));
        }
        let bytes = self
            .file_helper(
                id,
                user,
                workspace,
                FileRequest {
                    relative: "",
                    mode: "list",
                    stdin: Stdin::None,
                    max_bytes: usize::try_from(max_bytes).unwrap_or(usize::MAX),
                    max_entries: Some(max_entries),
                },
            )
            .await?;
        manifest::decode(&bytes)
    }

    /// Transfers one entry. A file source is streamed from its host handle in
    /// [`TRANSFER_CHUNK_BYTES`] chunks; the payload is never rebuilt as one
    /// buffer. The lifecycle owner calls this once per planned entry, so live
    /// payload memory is bounded by one chunk.
    pub(crate) async fn upload(
        &self,
        id: &str,
        user: &str,
        workspace: &str,
        entry: UploadEntry,
        max_bytes: u64,
    ) -> Result<(), EngineError> {
        let UploadEntry { path, source } = entry;
        match source {
            UploadSource::Directory => {
                self.file_helper(
                    id,
                    user,
                    workspace,
                    FileRequest {
                        relative: &path,
                        mode: "mkdir",
                        stdin: Stdin::None,
                        max_bytes: 0,
                        max_entries: None,
                    },
                )
                .await?;
            }
            UploadSource::Bytes(bytes) => {
                if bytes.len() as u64 > max_bytes {
                    return Err(file_error(
                        EngineErrorKind::Limit,
                        "staged bytes exceed workspace limit",
                    ));
                }
                self.file_helper(
                    id,
                    user,
                    workspace,
                    FileRequest {
                        relative: &path,
                        mode: "write",
                        stdin: Stdin::Bytes(&bytes),
                        max_bytes: 0,
                        max_entries: None,
                    },
                )
                .await?;
            }
            UploadSource::File { file, len } => {
                if len > max_bytes {
                    return Err(file_error(
                        EngineErrorKind::Limit,
                        "staged bytes exceed workspace limit",
                    ));
                }
                self.file_helper(
                    id,
                    user,
                    workspace,
                    FileRequest {
                        relative: &path,
                        mode: "write",
                        stdin: Stdin::File { file, len },
                        max_bytes: 0,
                        max_entries: None,
                    },
                )
                .await?;
            }
        }
        Ok(())
    }

    pub(crate) async fn download_file(
        &self,
        id: &str,
        user: &str,
        workspace: &str,
        relative: &str,
        max_bytes: usize,
    ) -> Result<Vec<u8>, EngineError> {
        let bytes = self
            .file_helper(
                id,
                user,
                workspace,
                FileRequest {
                    relative,
                    mode: "read",
                    stdin: Stdin::None,
                    max_bytes,
                    max_entries: None,
                },
            )
            .await?;
        if bytes.len() > max_bytes {
            return Err(file_error(
                EngineErrorKind::Limit,
                "file exceeds byte limit",
            ));
        }
        Ok(bytes)
    }

    pub(crate) async fn remove_file(
        &self,
        id: &str,
        user: &str,
        workspace: &str,
        relative: &str,
    ) -> Result<(), EngineError> {
        self.file_helper(
            id,
            user,
            workspace,
            FileRequest {
                relative,
                mode: "unlink",
                stdin: Stdin::None,
                max_bytes: 0,
                max_entries: None,
            },
        )
        .await?;
        Ok(())
    }

    async fn file_helper(
        &self,
        id: &str,
        user: &str,
        workspace: &str,
        request: FileRequest<'_>,
    ) -> Result<Vec<u8>, EngineError> {
        let FileRequest {
            relative,
            mode,
            stdin,
            max_bytes,
            max_entries,
        } = request;
        let bound = match &stdin {
            Stdin::None => 0,
            Stdin::Bytes(bytes) => u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            Stdin::File { len, .. } => *len,
        };
        let mut argv = vec![
            "python3".to_string(),
            "-I".to_string(),
            "-B".to_string(),
            "-c".to_string(),
            GUEST_FILES.to_string(),
            mode.to_string(),
            workspace.to_string(),
            relative.to_string(),
            if mode == "write" {
                bound
            } else {
                u64::try_from(max_bytes).unwrap_or(u64::MAX)
            }
            .to_string(),
        ];
        if let Some(max_entries) = max_entries {
            argv.push(max_entries.to_string());
        }
        // The helper contract is explicit: stdout may carry up to
        // `max_bytes + 1` (the extra byte proves an over-limit file), stderr may
        // carry at most 4 KiB of diagnostics, and both share exactly their sum.
        let stdout_limit = max_bytes.saturating_add(1);
        let stderr_limit = 4096;
        let result = self
            .exec_input(
                id,
                user,
                "/",
                &argv,
                ExecIo {
                    stdin,
                    stdout_limit,
                    stderr_limit,
                    combined_limit: stdout_limit.saturating_add(stderr_limit),
                },
            )
            .await?;
        let kind = match result.exit_code {
            0 => return Ok(result.stdout),
            81 => EngineErrorKind::UnsafeFile,
            82 => EngineErrorKind::Limit,
            83 => EngineErrorKind::InvalidPath,
            84 => EngineErrorKind::Protocol,
            85 => EngineErrorKind::FileIo,
            86 => EngineErrorKind::NotFound,
            87 => EngineErrorKind::PermissionDenied,
            88 => EngineErrorKind::EntryLimit,
            _ => EngineErrorKind::Other,
        };
        Err(file_error(
            kind,
            format!(
                "guest file helper failed (exit {}); Python3 is required",
                result.exit_code
            ),
        ))
    }

    pub(crate) async fn exec(
        &self,
        id: &str,
        user: &str,
        workdir: &str,
        argv: &[String],
        output_limit: usize,
    ) -> Result<ExecDone, EngineError> {
        self.exec_input(
            id,
            user,
            workdir,
            argv,
            ExecIo {
                stdin: Stdin::None,
                stdout_limit: output_limit,
                stderr_limit: output_limit,
                combined_limit: output_limit,
            },
        )
        .await
    }

    async fn exec_input(
        &self,
        id: &str,
        user: &str,
        workdir: &str,
        argv: &[String],
        io: ExecIo<'_>,
    ) -> Result<ExecDone, EngineError> {
        let ExecIo {
            stdin,
            stdout_limit,
            stderr_limit,
            combined_limit,
        } = io;
        let created = self
            .docker
            .create_exec(
                id,
                bollard::models::ExecConfig {
                    attach_stdin: Some(stdin.attached()),
                    attach_stdout: Some(true),
                    attach_stderr: Some(true),
                    cmd: Some(argv.to_vec()),
                    user: Some(user.to_string()),
                    working_dir: Some(workdir.to_string()),
                    tty: Some(false),
                    privileged: Some(false),
                    ..Default::default()
                },
            )
            .await
            .map_err(map_err)?;
        let started = self
            .docker
            .start_exec(&created.id, None)
            .await
            .map_err(map_err)?;
        let bollard::exec::StartExecResults::Attached {
            mut output,
            mut input,
        } = started
        else {
            return Err(EngineError::new("exec did not attach stdout"));
        };
        let stdin_attached = stdin.attached();
        let send = async {
            let result = async {
                match stdin {
                    Stdin::None => {}
                    Stdin::Bytes(bytes) => {
                        input
                            .write_all(bytes)
                            .await
                            .map_err(|err| EngineError::new(err.to_string()))?;
                    }
                    Stdin::File { file, len } => {
                        let mut source = tokio::fs::File::from_std(file);
                        let mut chunk = vec![0u8; TRANSFER_CHUNK_BYTES];
                        let mut remaining = len;
                        while remaining > 0 {
                            let want = usize::try_from(remaining)
                                .unwrap_or(TRANSFER_CHUNK_BYTES)
                                .min(chunk.len());
                            let read = source.read(&mut chunk[..want]).await.map_err(|err| {
                                file_error(
                                    EngineErrorKind::FileIo,
                                    format!("staged file read: {err}"),
                                )
                            })?;
                            if read == 0 {
                                return Err(file_error(
                                    EngineErrorKind::FileIo,
                                    format!(
                                        "staged file ended before its declared {len} byte bound"
                                    ),
                                ));
                            }
                            input
                                .write_all(&chunk[..read])
                                .await
                                .map_err(|err| EngineError::new(err.to_string()))?;
                            remaining -= u64::try_from(read).unwrap_or(0);
                        }
                    }
                }
                if stdin_attached {
                    input
                        .shutdown()
                        .await
                        .map_err(|err| EngineError::new(err.to_string()))?;
                }
                Ok::<(), EngineError>(())
            }
            .await;
            // A rejected path can close stdin early; preserve its typed exit code.
            Ok::<_, EngineError>(result)
        };
        let receive = async {
            let mut collector =
                output::OutputCollector::new(stdout_limit, stderr_limit, combined_limit);
            while let Some(item) = output.next().await {
                match item.map_err(map_err)? {
                    bollard::container::LogOutput::StdOut { message }
                    | bollard::container::LogOutput::Console { message } => {
                        collector.push(OutputPhase::Stdout, &message)?;
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        collector.push(OutputPhase::Stderr, &message)?;
                    }
                    bollard::container::LogOutput::StdIn { .. } => {}
                }
            }
            Ok::<_, EngineError>(collector.finish())
        };
        // An output-limit/transport error cancels a blocked stdin writer immediately.
        let (sent, (stdout, stderr)) = tokio::try_join!(send, receive)?;
        // Inspect the helper status even when it closed stdin early on unsafe input.
        let inspected = self
            .docker
            .inspect_exec(&created.id)
            .await
            .map_err(map_err)?;
        if inspected.exit_code == Some(0) {
            sent?;
        }
        Ok(ExecDone {
            exit_code: inspected.exit_code.unwrap_or(-1),
            stdout,
            stderr,
        })
    }

    pub(crate) async fn host_report(&self, id: &str) -> Result<HostReport, EngineError> {
        let inspected = self
            .docker
            .inspect_container(id, None)
            .await
            .map_err(map_err)?;
        Ok(policy::report_from(inspected.host_config))
    }

    async fn owner_check(&self, name: &str, owner_label: &str) -> OwnerCheck {
        let inspected = match self.docker.inspect_container(name, None).await {
            Ok(value) => value,
            Err(err) if is_not_found(&err) => return OwnerCheck::Absent,
            Err(err) => {
                return OwnerCheck::Unknown {
                    message: err.to_string(),
                };
            }
        };
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        if labels.get(OWNER_LABEL_KEY).map(String::as_str) != Some(owner_label) {
            return OwnerCheck::Foreign {
                name: name.to_string(),
            };
        }
        match inspected.id.filter(|id| !id.is_empty()) {
            Some(id) => OwnerCheck::Owned(id),
            None => OwnerCheck::Unknown {
                message: "inspect returned no container id".to_string(),
            },
        }
    }

    async fn stop_best_effort(&self, id: &str) -> Result<(), EngineError> {
        let options = bollard::query_parameters::StopContainerOptionsBuilder::new()
            .t(1)
            .build();
        self.docker
            .stop_container(id, Some(options))
            .await
            .map_err(map_err)
    }

    /// Forced removal with dependent anonymous volumes; an already-absent
    /// instance counts as removed.
    async fn force_remove(&self, id: &str) -> Result<(), EngineError> {
        match self
            .docker
            .remove_container(
                id,
                Some(
                    bollard::query_parameters::RemoveContainerOptionsBuilder::new()
                        .force(true)
                        .v(true)
                        .build(),
                ),
            )
            .await
        {
            Ok(()) => Ok(()),
            Err(err) if is_not_found(&err) => Ok(()),
            Err(err) => Err(map_err(err)),
        }
    }
}

// Explicit `impl Future + Send` bounds keep this private seam statically
// dispatched inside the lifecycle worker's spawned future.
#[allow(clippy::manual_async_fn)]
impl RemovalControl for Engine {
    fn inspect_owner(
        &self,
        name: &str,
        owner_label: &str,
    ) -> impl Future<Output = OwnerCheck> + Send {
        async move { Engine::owner_check(self, name, owner_label).await }
    }

    fn stop(&self, id: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        async move { Engine::stop_best_effort(self, id).await }
    }

    fn remove(&self, id: &str) -> impl Future<Output = Result<(), EngineError>> + Send {
        async move { Engine::force_remove(self, id).await }
    }
}

fn file_error(kind: EngineErrorKind, message: impl Into<String>) -> EngineError {
    EngineError {
        message: message.into(),
        not_found: false,
        kind,
        create_pending: false,
        create_id: None,
        output: None,
        policy: None,
    }
}

fn connect_socket(kind: SocketKind, path: &str) -> Result<bollard::Docker, EngineError> {
    match kind {
        SocketKind::Unix => {
            #[cfg(unix)]
            {
                bollard::Docker::connect_with_socket(
                    path,
                    CLIENT_TIMEOUT_SECS,
                    bollard::API_DEFAULT_VERSION,
                )
                .map_err(map_err)
            }
            #[cfg(not(unix))]
            {
                let _ = path;
                Err(EngineError::new(
                    "unix socket endpoints are only supported when the client OS is unix",
                ))
            }
        }
        SocketKind::Pipe => {
            #[cfg(windows)]
            {
                bollard::Docker::connect_with_socket(
                    path,
                    CLIENT_TIMEOUT_SECS,
                    bollard::API_DEFAULT_VERSION,
                )
                .map_err(map_err)
            }
            #[cfg(not(windows))]
            {
                let _ = path;
                Err(EngineError::new(
                    "named pipe endpoints are only supported when the client OS is windows",
                ))
            }
        }
    }
}

fn map_err(err: bollard::errors::Error) -> EngineError {
    EngineError {
        not_found: is_not_found(&err),
        kind: EngineErrorKind::Other,
        message: err.to_string(),
        create_pending: false,
        create_id: None,
        output: None,
        policy: None,
    }
}

fn map_create_err(err: bollard::errors::Error) -> EngineError {
    let pending = matches!(
        &err,
        bollard::errors::Error::DockerResponseServerError { status_code, .. }
            if *status_code >= 500
    ) || matches!(
        &err,
        bollard::errors::Error::RequestTimeoutError
            | bollard::errors::Error::IOError { .. }
            | bollard::errors::Error::DockerStreamError { .. }
    );
    let mut mapped = map_err(err);
    mapped.create_pending = pending;
    mapped
}

fn is_not_found(err: &bollard::errors::Error) -> bool {
    matches!(
        err,
        bollard::errors::Error::DockerResponseServerError {
            status_code: 404,
            ..
        }
    )
}
