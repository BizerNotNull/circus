use std::collections::BTreeMap;
use std::io::Write;
use std::process::Stdio;
use std::time::Duration;

use circus_sandbox::{
    DockerEndpoint, DockerSandbox, IoLimits, IsolationConfig, ProcessExit, ProcessOutput,
    ProcessRequest, ResourceLimits, RuntimeName, SandboxConfig,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const BRIDGE: &str = include_str!("bridge.py");
pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const OUTPUT_LIMIT: usize = 32 * 1024 * 1024;

// Example-only orchestration, deliberately outside the backend-neutral library API.
// Do not derive Debug: the upstream credential is host-only.
pub struct Config {
    pub endpoint: String,
    pub image: String,
    pub python: String,
    pub base_url: String,
    pub model: String,
    pub api_key: String,
    pub prompt: String,
    pub result_path: String,
    pub timeout: Duration,
}

impl Config {
    pub fn from_env() -> Result<Self> {
        let required = |name| -> Result<String> {
            let value = std::env::var(name).map_err(|_| format!("set {name} explicitly"))?;
            if value.is_empty() {
                return Err(format!("{name} must not be empty").into());
            }
            Ok(value)
        };
        Ok(Self {
            endpoint: required("CIRCUS_DOCKER_ENDPOINT")?,
            image: required("CIRCUS_AGENT_IMAGE")?,
            python: std::env::var("CIRCUS_HOST_PYTHON").unwrap_or_else(|_| "python3".into()),
            base_url: required("CIRCUS_MODEL_BASE_URL")?,
            model: required("CIRCUS_MODEL")?,
            api_key: required("CIRCUS_MODEL_API_KEY")?,
            prompt: required("CIRCUS_AGENT_PROMPT")?,
            result_path: required("CIRCUS_AGENT_RESULT")?,
            timeout: Duration::from_secs(180),
        })
    }
}

pub async fn run(config: Config, controller: &str) -> Result<Vec<u8>> {
    let sandbox = DockerSandbox::create(SandboxConfig::new(
        DockerEndpoint::parse(&config.endpoint)?,
        &config.image,
        RuntimeName::runc(),
        ResourceLimits::new(
            512 * 1024 * 1024,
            1000,
            128,
            128 * 1024 * 1024,
            2 * 1024 * 1024,
        )?,
        IsolationConfig::linux_enforced(true, true, true, true, "/workspace", 65534, 65534)?,
        IoLimits::new(OUTPUT_LIMIT, Duration::from_secs(15))?,
    )?)
    .await?;
    let exercise = async {
        sandbox.create_dir_all("project").await?;
        sandbox
            .write_file("bridge.py", format!("{BRIDGE}\nmain_guest()\n").as_bytes())
            .await?;
        sandbox.start().await?;
        let status = sandbox.status().await?;
        if status.network_mode != "none" || !status.readonly_root || status.bind_count != 0 {
            return Err("unexpected sandbox isolation policy".into());
        }
        let token = uuid::Uuid::new_v4().to_string();
        let session = sandbox
            .spawn_process(ProcessRequest {
                argv: vec![
                    "circus-agent".into(),
                    "python3".into(),
                    "-I".into(),
                    "-B".into(),
                    "/workspace/bridge.py".into(),
                ],
                working_dir: Some("project".into()),
                env: BTreeMap::from([
                    ("CIRCUS_TASK_TOKEN".into(), token.clone()),
                    ("CIRCUS_MODEL".into(), config.model.clone()),
                ]),
                timeout: config.timeout,
                output_limit_bytes: OUTPUT_LIMIT,
            })
            .await?;
        if let Err(error) = exchange(&session, &config, controller, &token).await {
            session.cancel();
            return Err(error);
        }
        match session.wait().await? {
            ProcessExit::Exited { exit_code: 0 } => sandbox
                .read_file(&config.result_path)
                .await
                .map_err(Into::into),
            other => Err(format!("agent process failed: {other:?}").into()),
        }
    }
    .await;
    // Always observe cleanup, including startup, controller, protocol and export failures.
    let cleanup = sandbox.destroy().await;
    match (exercise, cleanup) {
        (result, Ok(_)) => result,
        (Ok(_), Err(error)) => {
            Err(format!("task finished but sandbox cleanup failed: {error}").into())
        }
        (Err(error), Err(cleanup)) => {
            Err(format!("{error}; sandbox cleanup also failed: {cleanup}").into())
        }
    }
}

async fn exchange(
    session: &circus_sandbox::ProcessSession,
    config: &Config,
    controller: &str,
    token: &str,
) -> Result {
    let mut script = tempfile::NamedTempFile::new()?;
    script.write_all(controller.as_bytes())?;
    script.flush()?;
    let mut command = tokio::process::Command::new(&config.python);
    command
        .args(["-I", "-B"])
        .arg(script.path())
        .env_clear()
        .env("CIRCUS_MODEL_BASE_URL", &config.base_url)
        .env("CIRCUS_MODEL", &config.model)
        .env("CIRCUS_MODEL_API_KEY", &config.api_key)
        .env("CIRCUS_TASK_TOKEN", token)
        .env("CIRCUS_AGENT_PROMPT", &config.prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true);
    // Python's platform runtime may need these; no other host environment is copied.
    for name in ["PATH", "SYSTEMROOT"] {
        if let Some(value) = std::env::var_os(name) {
            command.env(name, value);
        }
    }
    let mut child = command.spawn()?;
    let mut input = child.stdin.take().ok_or("missing controller stdin")?;
    let mut output = child.stdout.take().ok_or("missing controller stdout")?;
    let forward_input = async {
        let mut buffer = [0; 16 * 1024];
        loop {
            let count = output.read(&mut buffer).await?;
            if count == 0 {
                break;
            }
            session.write_stdin(&buffer[..count]).await?;
        }
        // EOF also lets the guest clean up when the host controller fails early.
        match session.close_stdin().await {
            Ok(()) | Err(circus_sandbox::ProcessError::StdinClosed) => {}
            Err(error) => return Err(error.into()),
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let forward_output = async {
        while let Some(chunk) = session.next_output().await {
            if let ProcessOutput::Stdout(bytes) = chunk {
                input.write_all(&bytes).await?;
            }
            // Guest diagnostics are untrusted and can contain prompts or credentials.
        }
        input.shutdown().await?;
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let deadline = tokio::time::Instant::now() + config.timeout;
    let transfer = tokio::time::timeout_at(deadline, async {
        tokio::try_join!(forward_input, forward_output)
    })
    .await;
    let transfer = match transfer {
        Ok(result) => result,
        Err(_) => Err("agent task exceeded its deadline".into()),
    };
    if let Err(error) = transfer {
        // kill_on_drop is a fallback; explicitly reap on this error path.
        child.kill().await?;
        return Err(error);
    }
    let status = match tokio::time::timeout_at(deadline, child.wait()).await {
        Ok(status) => status?,
        Err(_) => {
            child.kill().await?;
            return Err("host controller did not exit before its deadline".into());
        }
    };
    if !status.success() {
        return Err(format!("host model controller failed ({status})").into());
    }
    Ok(())
}
