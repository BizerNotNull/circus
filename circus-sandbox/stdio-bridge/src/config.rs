use std::path::{Path, PathBuf};

use crate::secrets::{KeychainRef, read_bounded, read_secret};
use circus_agent_runner::{AgentKind, AgentRunner, ImagePreparation, ModelConfig, RunnerConfig};
#[cfg(all(unix, feature = "containerd"))]
use circus_sandbox::environment::ContainerdImageRequest;
use circus_sandbox::environment::{ImagePlatform, ImagePullPolicy, ImageRequest};
use circus_sandbox::{BackendConfig, ResourceLimits};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    version: u8,
    #[serde(default)]
    agent: Agent,
    backend: Backend,
    image: Image,
    python: PathBuf,
    resources: Resources,
    model: Model,
    #[serde(default)]
    mcp_connections: Vec<crate::mcp_config::Connection>,
}

#[derive(Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
enum Backend {
    Docker {
        endpoint: String,
        image: String,
        runtime: String,
    },
    Containerd {
        socket: String,
        namespace: String,
        image: String,
        runtime: String,
        snapshotter: String,
    },
}

impl Backend {
    fn descriptor(self, preparation: &ImagePreparation) -> Result<BackendConfig, ()> {
        match self {
            Self::Docker {
                endpoint,
                image,
                runtime,
            } => {
                ImageRequest::new(
                    endpoint.clone(),
                    image.clone(),
                    preparation.platform.clone(),
                    preparation.timeout,
                )
                .map_err(|_| ())?;
                if runtime != "runc" {
                    return Err(());
                }
                Ok(BackendConfig::docker(endpoint, image, runtime))
            }
            Self::Containerd {
                socket,
                namespace,
                image,
                runtime,
                snapshotter,
            } => {
                #[cfg(all(unix, feature = "containerd"))]
                {
                    ContainerdImageRequest::new(
                        socket.clone(),
                        namespace.clone(),
                        image.clone(),
                        preparation.platform.clone(),
                        preparation.timeout,
                    )
                    .map_err(|_| ())?;
                    if runtime != "runc" || snapshotter != "overlayfs" {
                        return Err(());
                    }
                    Ok(BackendConfig::containerd(
                        socket,
                        namespace,
                        image,
                        runtime,
                        snapshotter,
                    ))
                }
                #[cfg(not(all(unix, feature = "containerd")))]
                {
                    let _ = (socket, namespace, image, runtime, snapshotter);
                    Err(())
                }
            }
        }
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Image {
    platform: Platform,
    pull_policy: PullPolicy,
    prepare_timeout_ms: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Platform {
    os: String,
    architecture: String,
    variant: Option<String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
enum PullPolicy {
    LocalOnly,
    IfMissing,
}

impl Image {
    fn preparation(self) -> Result<ImagePreparation, ()> {
        let platform = ImagePlatform::with_variant(
            self.platform.os,
            self.platform.architecture,
            self.platform.variant,
        )
        .map_err(|_| ())?;
        let timeout = std::time::Duration::from_millis(self.prepare_timeout_ms);
        if timeout.is_zero() || std::time::Instant::now().checked_add(timeout).is_none() {
            return Err(());
        }
        Ok(ImagePreparation {
            platform,
            timeout,
            pull_policy: match self.pull_policy {
                PullPolicy::LocalOnly => ImagePullPolicy::LocalOnly,
                PullPolicy::IfMissing => ImagePullPolicy::IfMissing,
            },
        })
    }
}

#[derive(Default, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum Agent {
    #[default]
    Codex,
    ClaudeCode,
    OpenCode,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Resources {
    memory_bytes: u64,
    cpu_millis: u32,
    max_processes: u32,
    workspace_bytes: u64,
    max_file_bytes: usize,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Model {
    base_url: String,
    model: String,
    api_key_file: Option<PathBuf>,
    api_key_keychain: Option<KeychainRef>,
    ca_file: Option<PathBuf>,
}

pub async fn load(path: &Path) -> Result<AgentRunner, ()> {
    let config: Config =
        sonic_rs::from_slice(&read_bounded(path, 64 * 1024).await?).map_err(|_| ())?;
    if config.version != 1 || !config.python.is_absolute() {
        return Err(());
    }
    // Validate only descriptors here. Daemon preparation belongs to the owned
    // runner task, but invalid or unavailable backends must not read secrets.
    let preparation = config.image.preparation()?;
    let backend = config.backend.descriptor(&preparation)?;
    let credential = String::from_utf8(
        read_secret(
            config.model.api_key_file.as_deref(),
            config.model.api_key_keychain.as_ref(),
            4098,
        )
        .await?,
    )
    .map_err(|_| ())?;
    let mut model = ModelConfig::new(
        config.model.base_url,
        config.model.model,
        credential.trim_end_matches(['\r', '\n']).into(),
    )
    .map_err(|_| ())?;
    if let Some(path) = config.model.ca_file {
        if !path.is_absolute() {
            return Err(());
        }
        model = model.with_ca_file(path);
    }
    let r = config.resources;
    let resources = ResourceLimits::new(
        r.memory_bytes,
        r.cpu_millis,
        r.max_processes,
        r.workspace_bytes,
        r.max_file_bytes,
    )
    .map_err(|_| ())?;
    let connections = crate::mcp_config::load(config.mcp_connections).await?;
    AgentRunner::new(RunnerConfig {
        backend,
        image_preparation: Some(preparation),
        resources,
        model,
        python: config.python,
    })
    .and_then(|runner| runner.with_mcp_connections(connections))
    .map(|runner| {
        runner.with_agent(match config.agent {
            Agent::Codex => AgentKind::Codex,
            Agent::ClaudeCode => AgentKind::ClaudeCode,
            Agent::OpenCode => AgentKind::OpenCode,
        })
    })
    .map_err(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_selects_opencode_without_changing_default_agent() {
        let base = r#"{"version":1,"dockerEndpoint":"unix:///unused/docker.sock","agentImage":"image","python":"/usr/bin/python3","resources":{"memoryBytes":1,"cpuMillis":1,"maxProcesses":1,"workspaceBytes":1,"maxFileBytes":1},"model":{"baseUrl":"https://example.invalid/v1","model":"gpt-4.1"}}"#;
        let default: Config = sonic_rs::from_str(base).unwrap();
        assert!(matches!(default.agent, Agent::Codex));

        let selected: Config = sonic_rs::from_str(
            &base.replace(r#""version":1"#, r#""version":1,"agent":"open-code""#),
        )
        .unwrap();
        assert!(matches!(selected.agent, Agent::OpenCode));
    }
}
