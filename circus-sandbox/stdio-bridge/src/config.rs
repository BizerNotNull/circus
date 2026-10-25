use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use circus_agent_runner::{AgentRunner, ModelConfig, RunnerConfig};
use circus_sandbox::{BackendConfig, ResourceLimits};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Config {
    version: u8,
    docker_endpoint: String,
    agent_image: String,
    python: PathBuf,
    resources: Resources,
    model: Model,
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
    api_key_file: PathBuf,
    ca_file: Option<PathBuf>,
}

fn read_bounded(path: &Path, limit: u64) -> Result<Vec<u8>, ()> {
    if !path.is_absolute() {
        return Err(());
    }
    let file = File::open(path).map_err(|_| ())?;
    if !file.metadata().map_err(|_| ())?.is_file() {
        return Err(());
    }
    let mut bytes = Vec::new();
    file.take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ())?;
    if bytes.len() as u64 > limit {
        return Err(());
    }
    Ok(bytes)
}

pub fn load(path: &Path) -> Result<AgentRunner, ()> {
    let config: Config = sonic_rs::from_slice(&read_bounded(path, 64 * 1024)?).map_err(|_| ())?;
    if config.version != 1 || !config.python.is_absolute() {
        return Err(());
    }
    let credential =
        String::from_utf8(read_bounded(&config.model.api_key_file, 4098)?).map_err(|_| ())?;
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
    AgentRunner::new(RunnerConfig {
        backend: BackendConfig::docker(config.docker_endpoint, config.agent_image, "runc"),
        resources,
        model,
        python: config.python,
    })
    .map_err(|_| ())
}
