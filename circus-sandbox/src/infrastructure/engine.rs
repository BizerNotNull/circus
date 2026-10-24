//! Docker Engine calls. Bollard types stay in this module.

use std::collections::HashMap;

use futures_util::StreamExt;

use tokio::io::AsyncWriteExt;

const GUEST_FILES: &str = include_str!("guest_files.py");

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct StagedBytes {
    pub path: String,
    pub is_dir: bool,
    pub bytes: Vec<u8>,
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
}

const CLIENT_TIMEOUT_SECS: u64 = 90;

pub(crate) const OWNER_LABEL_KEY: &str = "circus.sandbox.owner";

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
}

impl EngineError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            not_found: false,
            kind: EngineErrorKind::Other,
            create_pending: false,
            create_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recovered {
    Removed,
    Absent,
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

pub(crate) struct HostReport {
    pub runtime: String,
    pub memory_bytes: i64,
    pub nano_cpus: i64,
    pub pids_limit: i64,
    pub readonly_root: bool,
    pub network_mode: String,
    pub cap_drop: Vec<String>,
    pub security_opt: Vec<String>,
    pub bind_count: usize,
    pub tmpfs_workspace: Option<String>,
}

pub(crate) struct ExecDone {
    pub exit_code: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

struct FileRequest<'a> {
    relative: &'a str,
    mode: &'static str,
    input: &'a [u8],
    max_bytes: usize,
}

struct ExecIo<'a> {
    stdin: Option<&'a [u8]>,
    output_limit: usize,
    stderr_limit: usize,
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
        let mut body = container_body(name, spec);
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
        validate_created(&inspected, spec).map_err(|err| EngineError {
            create_id: Some(created_id.clone()),
            ..err
        })?;
        Ok(created_id)
    }

    pub(crate) async fn recover_owned(
        &self,
        name: &str,
        owner_label: &str,
    ) -> Result<Recovered, EngineError> {
        match self.confirm_owner(name, owner_label).await {
            Ok(id) => {
                self.force_remove(&id).await?;
                match self.confirm_owner(name, owner_label).await {
                    Err(err) if err.not_found => Ok(Recovered::Removed),
                    Ok(_) => Err(EngineError::new(format!(
                        "container {name} still present after forced removal"
                    ))),
                    Err(err) => Err(err),
                }
            }
            Err(err) if err.not_found => Ok(Recovered::Absent),
            Err(err) => Err(err),
        }
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

    pub(crate) async fn upload(
        &self,
        id: &str,
        user: &str,
        workspace: &str,
        entries: &[StagedBytes],
        max_bytes: u64,
    ) -> Result<(), EngineError> {
        let total = entries
            .iter()
            .filter(|entry| !entry.is_dir)
            .try_fold(0u64, |n, entry| n.checked_add(entry.bytes.len() as u64));
        if total.is_none_or(|total| total > max_bytes) {
            return Err(file_error(
                EngineErrorKind::Limit,
                "staged bytes exceed workspace limit",
            ));
        }
        for entry in entries {
            self.file_helper(
                id,
                user,
                workspace,
                FileRequest {
                    relative: &entry.path,
                    mode: if entry.is_dir { "mkdir" } else { "write" },
                    input: if entry.is_dir { &[] } else { &entry.bytes },
                    max_bytes: 0,
                },
            )
            .await?;
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
                    input: &[],
                    max_bytes,
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
                input: &[],
                max_bytes: 0,
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
            input,
            max_bytes,
        } = request;
        let argv = vec![
            "python3".to_string(),
            "-I".to_string(),
            "-B".to_string(),
            "-c".to_string(),
            GUEST_FILES.to_string(),
            mode.to_string(),
            workspace.to_string(),
            relative.to_string(),
            if mode == "write" {
                input.len()
            } else {
                max_bytes
            }
            .to_string(),
        ];
        let result = self
            .exec_input(
                id,
                user,
                "/",
                &argv,
                ExecIo {
                    stdin: Some(input),
                    output_limit: max_bytes.saturating_add(1),
                    stderr_limit: 4096,
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
                stdin: None,
                output_limit,
                stderr_limit: output_limit,
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
            output_limit,
            stderr_limit,
        } = io;
        let created = self
            .docker
            .create_exec(
                id,
                bollard::models::ExecConfig {
                    attach_stdin: Some(stdin.is_some()),
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
        let send = async {
            let result = async {
                if let Some(bytes) = stdin {
                    input
                        .write_all(bytes)
                        .await
                        .map_err(|err| EngineError::new(err.to_string()))?;
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
            let mut stdout = Vec::new();
            let mut stderr = Vec::new();
            while let Some(item) = output.next().await {
                match item.map_err(map_err)? {
                    bollard::container::LogOutput::StdOut { message }
                    | bollard::container::LogOutput::Console { message } => {
                        push_bounded(&mut stdout, &message, output_limit)?;
                    }
                    bollard::container::LogOutput::StdErr { message } => {
                        push_bounded(&mut stderr, &message, stderr_limit)?;
                    }
                    bollard::container::LogOutput::StdIn { .. } => {}
                }
            }
            Ok::<_, EngineError>((stdout, stderr))
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
        let host = inspected.host_config.unwrap_or_default();
        Ok(HostReport {
            runtime: host.runtime.unwrap_or_default(),
            memory_bytes: host.memory.unwrap_or(0),
            nano_cpus: host.nano_cpus.unwrap_or(0),
            pids_limit: host.pids_limit.unwrap_or(0),
            readonly_root: host.readonly_rootfs.unwrap_or(false),
            network_mode: host.network_mode.unwrap_or_default(),
            cap_drop: host.cap_drop.unwrap_or_default(),
            security_opt: host.security_opt.unwrap_or_default(),
            bind_count: host.binds.unwrap_or_default().len(),
            tmpfs_workspace: host.tmpfs.and_then(|map| map.into_values().next()),
        })
    }

    async fn confirm_owner(&self, name: &str, owner_label: &str) -> Result<String, EngineError> {
        let inspected = match self.docker.inspect_container(name, None).await {
            Ok(value) => value,
            Err(err) if is_not_found(&err) => {
                return Err(EngineError {
                    message: "owned container is absent".to_string(),
                    not_found: true,
                    kind: EngineErrorKind::Other,
                    create_pending: false,
                    create_id: None,
                });
            }
            Err(err) => return Err(map_err(err)),
        };
        let labels = inspected
            .config
            .and_then(|config| config.labels)
            .unwrap_or_default();
        if labels.get(OWNER_LABEL_KEY).map(String::as_str) != Some(owner_label) {
            return Err(EngineError {
                message: format!("container {name} does not carry the owner label"),
                not_found: false,
                kind: EngineErrorKind::Other,
                create_pending: false,
                create_id: None,
            });
        }
        inspected
            .id
            .filter(|id| !id.is_empty())
            .ok_or_else(|| EngineError::new("inspect returned no container id"))
    }

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

// Inspect is checked before lifecycle can start the owned container. A mismatch
// returns its create id to the existing lifecycle recovery path.
fn validate_created(
    inspected: &bollard::models::ContainerInspectResponse,
    spec: &CreateSpec,
) -> Result<(), EngineError> {
    let reject =
        || EngineError::new("engine did not preserve required resource limits and isolation");
    let host = inspected.host_config.as_ref().ok_or_else(reject)?;
    let config = inspected.config.as_ref().ok_or_else(reject)?;
    if host.memory != Some(spec.memory)
        || host.memory_swap != Some(spec.memory)
        || host.nano_cpus != Some(spec.nano_cpus)
        || host.pids_limit != Some(spec.pids)
        || host.readonly_rootfs != Some(true)
        || host.init != Some(true)
        || host.runtime.as_deref() != Some(spec.runtime.as_str())
        || host.network_mode.as_deref() != Some("none")
        || config.network_disabled != Some(true)
        || config.user.as_deref() != Some(format!("{}:{}", spec.guest_uid, spec.guest_gid).as_str())
        || config.working_dir.as_deref() != Some(spec.workspace_path.as_str())
        || host.privileged.unwrap_or(false)
        || host.publish_all_ports.unwrap_or(false)
        || !empty(&host.cap_add)
        || !host
            .cap_drop
            .as_ref()
            .is_some_and(|caps| caps.len() == 1 && caps[0].eq_ignore_ascii_case("ALL"))
        || !host.security_opt.as_ref().is_some_and(|opts| {
            opts.len() == 1
                && matches!(
                    opts[0].as_str(),
                    "no-new-privileges" | "no-new-privileges:true" | "no-new-privileges=true"
                )
        })
        || !empty(&host.binds)
        || !empty(&host.mounts)
        || !empty(&host.volumes_from)
        || !empty(&config.volumes)
        || !empty(&host.devices)
        || !empty(&host.device_requests)
        || !empty(&host.device_cgroup_rules)
        || host
            .port_bindings
            .as_ref()
            .is_some_and(|ports| !ports.is_empty())
        || !empty(&host.extra_hosts)
        || !empty(&host.dns)
        || host.ipc_mode.as_deref() != Some("private")
        || host.cgroupns_mode != Some(bollard::models::HostConfigCgroupnsModeEnum::PRIVATE)
        || host
            .pid_mode
            .as_deref()
            .is_some_and(|mode| !mode.is_empty())
        || host
            .uts_mode
            .as_deref()
            .is_some_and(|mode| !mode.is_empty())
        || host
            .userns_mode
            .as_deref()
            .is_some_and(|mode| !mode.is_empty())
        || host.auto_remove.unwrap_or(false)
        || host.oom_kill_disable.unwrap_or(false)
        || host.shm_size != Some(65_536)
        || !host.restart_policy.as_ref().is_some_and(|policy| {
            policy.name == Some(bollard::models::RestartPolicyNameEnum::NO)
                && policy.maximum_retry_count.unwrap_or(0) == 0
        })
    {
        return Err(reject());
    }
    let expected = workspace_options(spec);
    if !host.tmpfs.as_ref().is_some_and(|tmpfs| {
        tmpfs.len() == 1
            && tmpfs.get(&spec.workspace_path).is_some_and(|options| {
                options.split(',').count() == expected.split(',').count()
                    && expected
                        .split(',')
                        .all(|option| options.split(',').any(|value| value == option))
            })
    }) {
        return Err(reject());
    }
    // Docker may omit HostConfig.Tmpfs entries from Mounts. If returned, only
    // that same writable, host-source-free tmpfs is permitted.
    if inspected.mounts.as_ref().is_some_and(|mounts| {
        mounts.len() > 1
            || mounts.iter().any(|mount| {
                mount.typ.as_deref() != Some("tmpfs")
                    || mount.destination.as_deref() != Some(spec.workspace_path.as_str())
                    || mount
                        .source
                        .as_deref()
                        .is_some_and(|source| !source.is_empty())
                    || mount.rw != Some(true)
            })
    }) {
        return Err(reject());
    }
    Ok(())
}

fn empty<T>(items: &Option<Vec<T>>) -> bool {
    items.as_ref().is_none_or(Vec::is_empty)
}

fn workspace_options(spec: &CreateSpec) -> String {
    format!(
        "rw,nosuid,nodev,uid={},gid={},mode=0755,size={}",
        spec.guest_uid, spec.guest_gid, spec.workspace_bytes
    )
}

fn container_body(name: &str, spec: &CreateSpec) -> bollard::models::ContainerCreateBody {
    let mut labels = HashMap::new();
    labels.insert(OWNER_LABEL_KEY.to_string(), spec.owner_label.clone());
    labels.insert("circus.sandbox.name".to_string(), name.to_string());
    let mut tmpfs = HashMap::new();
    tmpfs.insert(spec.workspace_path.clone(), workspace_options(spec));
    bollard::models::ContainerCreateBody {
        image: Some(spec.image.clone()),
        user: Some(format!("{}:{}", spec.guest_uid, spec.guest_gid)),
        tty: Some(false),
        open_stdin: Some(false),
        attach_stdin: Some(false),
        env: Some(Vec::new()),
        cmd: Some(Vec::new()),
        entrypoint: Some(vec![
            "python3".to_string(),
            "-I".to_string(),
            "-B".to_string(),
            "-c".to_string(),
            "import time; time.sleep(31536000)".to_string(),
        ]),
        working_dir: Some(spec.workspace_path.clone()),
        network_disabled: Some(true),
        exposed_ports: Some(Vec::new()),
        volumes: Some(Vec::new()),
        labels: Some(labels),
        stop_signal: Some("SIGKILL".to_string()),
        stop_timeout: Some(1),
        host_config: Some(bollard::models::HostConfig {
            memory: Some(spec.memory),
            memory_swap: Some(spec.memory),
            nano_cpus: Some(spec.nano_cpus),
            pids_limit: Some(spec.pids),
            readonly_rootfs: Some(true),
            network_mode: Some("none".to_string()),
            cap_drop: Some(vec!["ALL".to_string()]),
            cap_add: Some(Vec::new()),
            security_opt: Some(vec!["no-new-privileges:true".to_string()]),
            privileged: Some(false),
            publish_all_ports: Some(false),
            binds: Some(Vec::new()),
            mounts: Some(Vec::new()),
            tmpfs: Some(tmpfs),
            runtime: Some(spec.runtime.clone()),
            auto_remove: Some(false),
            init: Some(true),
            ipc_mode: Some("private".to_string()),
            cgroupns_mode: Some(bollard::models::HostConfigCgroupnsModeEnum::PRIVATE),
            oom_kill_disable: Some(false),
            shm_size: Some(65_536),
            restart_policy: Some(bollard::models::RestartPolicy {
                name: Some(bollard::models::RestartPolicyNameEnum::NO),
                maximum_retry_count: Some(0),
            }),
            devices: Some(Vec::new()),
            extra_hosts: Some(Vec::new()),
            dns: Some(Vec::new()),
            ..Default::default()
        }),
        ..Default::default()
    }
}

fn file_error(kind: EngineErrorKind, message: impl Into<String>) -> EngineError {
    EngineError {
        message: message.into(),
        not_found: false,
        kind,
        create_pending: false,
        create_id: None,
    }
}

fn push_bounded(buf: &mut Vec<u8>, chunk: &[u8], limit: usize) -> Result<(), EngineError> {
    if buf.len().saturating_add(chunk.len()) > limit {
        return Err(file_error(
            EngineErrorKind::Limit,
            "command output exceeded the byte limit",
        ));
    }
    buf.extend_from_slice(chunk);
    Ok(())
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

#[cfg(test)]
mod isolation_tests {
    use super::*;
    use bollard::models::{ContainerConfig, ContainerInspectResponse, MountPoint};

    fn fixture() -> (CreateSpec, ContainerInspectResponse) {
        let spec = CreateSpec {
            image: "local-image".into(),
            runtime: "runc".into(),
            memory: 64 * 1024 * 1024,
            nano_cpus: 100_000_000,
            pids: 8,
            workspace_path: "/workspace".into(),
            workspace_bytes: 8 * 1024 * 1024,
            guest_uid: 65534,
            guest_gid: 65534,
            owner_label: "owned".into(),
        };
        let body = container_body("sandbox", &spec);
        let inspected = ContainerInspectResponse {
            config: Some(ContainerConfig {
                user: body.user,
                working_dir: body.working_dir,
                network_disabled: body.network_disabled,
                volumes: body.volumes,
                ..Default::default()
            }),
            host_config: body.host_config,
            ..Default::default()
        };
        (spec, inspected)
    }

    #[test]
    fn accepts_safe_policy_and_engine_normalization() {
        let (spec, mut inspected) = fixture();
        assert!(validate_created(&inspected, &spec).is_ok());
        let host = inspected.host_config.as_mut().unwrap();
        host.cap_add = None;
        host.binds = None;
        host.mounts = None;
        host.cap_drop = Some(vec!["all".into()]);
        host.tmpfs.as_mut().unwrap().insert(
            spec.workspace_path.clone(),
            "size=8388608,mode=0755,gid=65534,uid=65534,nodev,nosuid,rw".into(),
        );
        inspected.mounts = Some(vec![MountPoint {
            typ: Some("tmpfs".into()),
            destination: Some(spec.workspace_path.clone()),
            source: Some(String::new()),
            rw: Some(true),
            ..Default::default()
        }]);
        for option in [
            "no-new-privileges",
            "no-new-privileges:true",
            "no-new-privileges=true",
        ] {
            inspected.host_config.as_mut().unwrap().security_opt = Some(vec![option.into()]);
            assert!(validate_created(&inspected, &spec).is_ok(), "{option}");
        }
    }

    #[test]
    fn rejects_effective_host_policy_deviations() {
        type Change = fn(&mut bollard::models::HostConfig);
        let changes: &[(&str, Change)] = &[
            ("memory", |h| h.memory = Some(0)),
            ("swap", |h| h.memory_swap = Some(-1)),
            ("cpu", |h| h.nano_cpus = None),
            ("pids", |h| h.pids_limit = Some(-1)),
            ("root", |h| h.readonly_rootfs = Some(false)),
            ("init", |h| h.init = None),
            ("runtime", |h| h.runtime = Some("other".into())),
            ("network", |h| h.network_mode = Some("host".into())),
            ("privileged", |h| h.privileged = Some(true)),
            ("caps added", |h| h.cap_add = Some(vec!["SYS_ADMIN".into()])),
            ("caps retained", |h| h.cap_drop = None),
            ("nnp absent", |h| h.security_opt = None),
            ("nnp false", |h| {
                h.security_opt = Some(vec!["no-new-privileges:false".into()])
            }),
            ("security weakened", |h| {
                h.security_opt
                    .as_mut()
                    .unwrap()
                    .push("seccomp=unconfined".into())
            }),
            ("bind", |h| h.binds = Some(vec!["/:/host".into()])),
            ("mount", |h| h.mounts = Some(vec![Default::default()])),
            ("inherited volume", |h| {
                h.volumes_from = Some(vec!["other".into()])
            }),
            ("device", |h| h.devices = Some(vec![Default::default()])),
            ("device request", |h| {
                h.device_requests = Some(vec![Default::default()])
            }),
            ("device rule", |h| {
                h.device_cgroup_rules = Some(vec!["a *:* rwm".into()])
            }),
            ("ports", |h| h.publish_all_ports = Some(true)),
            ("ipc", |h| h.ipc_mode = Some("host".into())),
            ("pid", |h| h.pid_mode = Some("host".into())),
            ("cgroup", |h| {
                h.cgroupns_mode = Some(bollard::models::HostConfigCgroupnsModeEnum::HOST)
            }),
            ("tmpfs absent", |h| h.tmpfs = None),
            ("tmpfs unsafe", |h| {
                h.tmpfs
                    .as_mut()
                    .unwrap()
                    .insert("/workspace".into(), "rw,size=8388608".into());
            }),
            ("extra tmpfs", |h| {
                h.tmpfs
                    .as_mut()
                    .unwrap()
                    .insert("/extra".into(), "rw".into());
            }),
        ];
        for (name, change) in changes {
            let (spec, mut inspected) = fixture();
            change(inspected.host_config.as_mut().unwrap());
            assert!(validate_created(&inspected, &spec).is_err(), "{name}");
        }
    }

    #[test]
    fn rejects_missing_config_and_effective_mounts() {
        type Change = fn(&mut ContainerInspectResponse);
        let changes: &[Change] = &[
            |i| i.host_config = None,
            |i| i.config = None,
            |i| i.config.as_mut().unwrap().network_disabled = Some(false),
            |i| i.config.as_mut().unwrap().user = Some("0:0".into()),
            |i| i.config.as_mut().unwrap().volumes = Some(vec!["/data".into()]),
            |i| {
                i.mounts = Some(vec![MountPoint {
                    typ: Some("bind".into()),
                    source: Some("/".into()),
                    destination: Some("/host".into()),
                    rw: Some(true),
                    ..Default::default()
                }])
            },
            |i| {
                i.mounts = Some(vec![MountPoint {
                    typ: Some("volume".into()),
                    destination: Some("/data".into()),
                    ..Default::default()
                }])
            },
        ];
        for change in changes {
            let (spec, mut inspected) = fixture();
            change(&mut inspected);
            assert!(validate_created(&inspected, &spec).is_err());
        }
    }
}
