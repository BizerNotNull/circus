use std::process::Command;
use std::time::Duration;

use circus_sandbox::{
    CommandRequest, DockerEndpoint, DockerSandbox, IoLimits, IsolationConfig, NetworkAccess,
    ProcessOutput, ProcessRequest, ResourceLimits, RuntimeName, SandboxConfig, SandboxError,
    SandboxPhase,
};

const PY: &str = "python3";
// OCI transient helper overhead measured under 8 processes requires fixture headroom.
const TEST_PIDS: u32 = 32;

fn enabled() {
    assert_eq!(
        std::env::var("CIRCUS_DOCKER_SANDBOX").as_deref(),
        Ok("1"),
        "set CIRCUS_DOCKER_SANDBOX=1 to opt into the real backend test"
    );
}

fn endpoint() -> Result<DockerEndpoint, Box<dyn std::error::Error>> {
    Ok(DockerEndpoint::parse(&std::env::var(
        "CIRCUS_DOCKER_ENDPOINT",
    )?)?)
}

fn config(network_access: NetworkAccess) -> Result<SandboxConfig, Box<dyn std::error::Error>> {
    let limits = ResourceLimits::new(64 * 1024 * 1024, 100, TEST_PIDS, 8 * 1024 * 1024, 1024)?;
    Ok(SandboxConfig::new(
        endpoint()?,
        std::env::var("CIRCUS_DOCKER_IMAGE").unwrap_or_else(|_| "python:3.12-slim".into()),
        RuntimeName::runc(),
        limits,
        IsolationConfig::linux_enforced(
            true,
            true,
            true,
            network_access,
            "/workspace",
            65534,
            65534,
        )?,
        IoLimits::new(64 * 1024, Duration::from_secs(3))?,
    )?)
}

struct Fixture {
    sandbox: Option<DockerSandbox>,
    endpoint: DockerEndpoint,
    id: Option<String>,
}

impl Fixture {
    async fn new() -> Result<Self, Box<dyn std::error::Error>> {
        Self::with_network(NetworkAccess::Disabled).await
    }

    async fn with_network(
        network_access: NetworkAccess,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let ep = endpoint()?;
        let sandbox = DockerSandbox::create(config(network_access)?).await?;
        let id = sandbox.status().await?.container_id;
        Ok(Self {
            sandbox: Some(sandbox),
            endpoint: ep,
            id,
        })
    }

    fn sandbox(&self) -> &DockerSandbox {
        self.sandbox.as_ref().expect("fixture is alive")
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        if let Some(id) = &self.id {
            let host = match &self.endpoint {
                DockerEndpoint::UnixSocket { path } | DockerEndpoint::NamedPipe { path } => path,
            };
            let _ = Command::new("docker")
                .args(["--host", host, "rm", "-f", id])
                .output();
        }
    }
}

async fn destroy(f: &mut Fixture) -> Result<(), Box<dyn std::error::Error>> {
    if let Some(sandbox) = f.sandbox.take() {
        sandbox.destroy().await?;
    }
    Ok(())
}

fn py(argv: &[&str]) -> CommandRequest {
    CommandRequest {
        argv: std::iter::once(PY.to_string())
            .chain(argv.iter().map(|s| (*s).into()))
            .collect(),
        timeout: Duration::from_secs(5),
        output_limit_bytes: 16 * 1024,
    }
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_lifecycle() -> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut f = Fixture::new().await?;
    f.sandbox().write_file("hello.txt", b"hello").await?;
    f.sandbox().create_dir_all("staged").await?;
    f.sandbox().start().await?;
    let status = f.sandbox().status().await?;
    assert_eq!(status.phase, SandboxPhase::Running);
    assert_eq!(status.runtime, "runc");
    assert_eq!(status.memory_bytes, 64 * 1024 * 1024);
    assert_eq!(status.nano_cpus, 100_000_000);
    assert_eq!(status.pids_limit, i64::from(TEST_PIDS));
    assert!(status.readonly_root && status.network_mode == "none");
    assert!(status.bind_count == 0 && status.cap_drop.iter().any(|c| c == "ALL"));
    let security = f.sandbox().exec(py(&["-c", r###"from pathlib import Path; s=Path('/proc/self/status').read_text(); print(next(x for x in s.splitlines() if x.startswith('NoNewPrivs:'))); print(next(x for x in s.splitlines() if x.startswith('CapEff:')))"###])).await?;
    assert_eq!(security.exit_code, 0, "guest security probe: {security:?}");
    let security_text = String::from_utf8(security.stdout)?;
    assert!(
        security_text
            .lines()
            .any(|line| line.contains("NoNewPrivs:") && line.ends_with("1"))
    );
    assert!(
        security_text
            .lines()
            .any(|line| line.contains("CapEff:") && line.ends_with("0000000000000000"))
    );
    let limits = f.sandbox().exec(py(&["-c", r###"from pathlib import Path; root=Path('/sys/fs/cgroup'); print((root/'memory.max').read_text().strip()); print((root/'pids.max').read_text().strip()); print((root/'cpu.max').read_text().strip())"###])).await?;
    assert_eq!(limits.exit_code, 0, "guest cgroup limits probe: {limits:?}");
    let limit_text = String::from_utf8(limits.stdout)?;
    assert_eq!(limit_text.lines().next(), Some("67108864"));
    assert_eq!(limit_text.lines().nth(1), Some("32"));
    assert_eq!(limit_text.lines().nth(2), Some("10000 100000"));
    let out = f.sandbox().exec(py(&["-c", r###"from pathlib import Path; Path('/workspace/hello.txt').write_bytes(b'changed'); Path('/workspace/staged/guest.txt').write_bytes(b'guest')"###])).await?;
    assert_eq!(out.exit_code, 0);
    assert_eq!(f.sandbox().read_file("hello.txt").await?, b"changed");
    f.sandbox()
        .exec(py(&[
            "-c",
            r###"from pathlib import Path; Path('/workspace/newdir').mkdir()"###,
        ]))
        .await?;
    f.sandbox().write_file("newdir/sdk.txt", b"sdk").await?;
    f.sandbox().pause().await?;
    assert_eq!(f.sandbox().status().await?.phase, SandboxPhase::Paused);
    assert!(matches!(
        f.sandbox().write_file("paused", b"x").await,
        Err(SandboxError::InvalidPhase { .. })
    ));
    f.sandbox().resume().await?;
    f.sandbox().remove_file("staged/guest.txt").await?;
    f.sandbox().destroy().await?;
    assert_eq!(f.sandbox().status().await?.phase, SandboxPhase::Destroyed);
    f.sandbox().destroy().await?;
    f.sandbox = None;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_ownership_is_per_create() -> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut first = Fixture::new().await?;
    let mut second = Fixture::new().await?;
    assert_ne!(first.id, second.id);
    first.sandbox().start().await?;
    second.sandbox().start().await?;
    destroy(&mut second).await?;
    assert_eq!(first.sandbox().status().await?.phase, SandboxPhase::Running);
    destroy(&mut first).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_guest_paths_and_file_limits() -> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut f = Fixture::new().await?;
    f.sandbox().write_file("secret", b"secret").await?;
    f.sandbox().start().await?;
    f.sandbox().exec(py(&["-c", r###"import os, pathlib; pathlib.Path('/workspace/link').symlink_to('/workspace/secret'); pathlib.Path('/workspace/escape').symlink_to('/etc'); os.mkfifo('/workspace/pipe')"###])).await?;
    let link_read =
        tokio::time::timeout(Duration::from_secs(3), f.sandbox().read_file("link")).await?;
    assert!(link_read.is_err());
    let pipe_write =
        tokio::time::timeout(Duration::from_secs(3), f.sandbox().write_file("pipe", b"x")).await?;
    assert!(pipe_write.is_err());
    let escape_read = tokio::time::timeout(
        Duration::from_secs(3),
        f.sandbox().read_file("escape/passwd"),
    )
    .await?;
    assert!(escape_read.is_err());
    assert_eq!(f.sandbox().read_file("secret").await?, b"secret");
    assert!(f.sandbox().write_file("missing/file", b"x").await.is_err());
    f.sandbox().create_dir_all("missing").await?;
    f.sandbox().write_file("missing/file", b"x").await?;
    f.sandbox()
        .exec(py(&[
            "-c",
            r###"open('/workspace/too-big','wb').write(b'x'*2048)"###,
        ]))
        .await?;
    let too_big = f.sandbox().read_file("too-big").await;
    assert!(too_big.is_err());
    assert!(f.sandbox().read_file("secret").await? == b"secret");
    destroy(&mut f).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_exec_abort_destroys_only_owner() -> Result<(), Box<dyn std::error::Error>>
{
    enabled();
    let mut victim = Fixture::new().await?;
    let mut survivor = Fixture::new().await?;
    victim.sandbox().start().await?;
    survivor.sandbox().start().await?;
    let request = CommandRequest {
        argv: vec![
            PY.into(),
            "-c".into(),
            "import time; open('/workspace/started','w').write('yes'); time.sleep(30)".into(),
        ],
        timeout: Duration::from_secs(1),
        output_limit_bytes: 4096,
    };
    let err = victim.sandbox().exec(request).await;
    assert!(matches!(
        err,
        Err(SandboxError::TimedOut {
            ownership: circus_sandbox::Ownership::ConfirmedAbsent,
            ..
        })
    ));
    assert_eq!(
        victim.sandbox().status().await?.phase,
        SandboxPhase::Destroyed
    );
    let out = survivor
        .sandbox()
        .exec(py(&["-c", r###"print('alive')"###]))
        .await?;
    assert_eq!(out.stdout, b"alive\n");
    let output_err = survivor
        .sandbox()
        .exec(CommandRequest {
            argv: vec![PY.into(), "-c".into(), "print('x'*10000)".into()],
            timeout: Duration::from_secs(5),
            output_limit_bytes: 64,
        })
        .await;
    assert!(matches!(
        output_err,
        Err(SandboxError::OutputLimit {
            ownership: circus_sandbox::Ownership::ConfirmedAbsent,
            ..
        })
    ));
    assert_eq!(
        survivor.sandbox().status().await?.phase,
        SandboxPhase::Destroyed
    );
    destroy(&mut victim).await?;
    destroy(&mut survivor).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_process_session_streams_stdin_stdout_stderr_and_exit()
-> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut f = Fixture::new().await?;
    f.sandbox().start().await?;
    let mut session = f.sandbox().open_process(ProcessRequest {
        argv: vec![
            PY.into(),
            "-u".into(),
            "-c".into(),
            "import sys; line=sys.stdin.readline(); print('out:'+line, end=''); print('err:'+line, end='', file=sys.stderr); sys.exit(7)".into(),
        ],
        timeout: Duration::from_secs(10),
        output_limit_bytes: 4096,
    }).await?;
    session.write_stdin(b"hello\n").await?;
    session.close_stdin().await?;

    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    while let Some(output) = session.recv().await? {
        match output {
            ProcessOutput::Stdout(bytes) => stdout.extend(bytes),
            ProcessOutput::Stderr(bytes) => stderr.extend(bytes),
        }
    }
    assert_eq!(session.wait().await?, 7);
    assert_eq!(stdout, b"out:hello\n");
    assert_eq!(stderr, b"err:hello\n");
    assert_eq!(f.sandbox().status().await?.phase, SandboxPhase::Running);
    destroy(&mut f).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_process_session_close_destroys_only_its_sandbox()
-> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut victim = Fixture::new().await?;
    let mut survivor = Fixture::new().await?;
    victim.sandbox().start().await?;
    survivor.sandbox().start().await?;
    let session = victim
        .sandbox()
        .open_process(ProcessRequest {
            argv: vec![
                PY.into(),
                "-u".into(),
                "-c".into(),
                "import time; print('ready', flush=True); time.sleep(30)".into(),
            ],
            timeout: Duration::from_secs(60),
            output_limit_bytes: 4096,
        })
        .await?;
    let mut session = session;
    let mut ready = Vec::new();
    while !ready.ends_with(b"\n") {
        match session.recv().await? {
            Some(ProcessOutput::Stdout(bytes)) => ready.extend(bytes),
            Some(ProcessOutput::Stderr(_)) => {}
            None => break,
        }
    }
    assert_eq!(ready, b"ready\n");
    session.close().await?;

    assert_eq!(
        victim.sandbox().status().await?.phase,
        SandboxPhase::Destroyed
    );

    let out = survivor
        .sandbox()
        .exec(py(&["-c", "print('alive')"]))
        .await?;
    assert_eq!(out.stdout, b"alive\n");
    destroy(&mut victim).await?;
    destroy(&mut survivor).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_process_session_drop_destroys_its_sandbox()
-> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut f = Fixture::new().await?;
    f.sandbox().start().await?;
    let session = f
        .sandbox()
        .open_process(ProcessRequest {
            argv: vec![
                PY.into(),
                "-u".into(),
                "-c".into(),
                "import time; time.sleep(30)".into(),
            ],
            timeout: Duration::from_secs(60),
            output_limit_bytes: 4096,
        })
        .await?;
    drop(session);
    assert_eq!(f.sandbox().status().await?.phase, SandboxPhase::Destroyed);
    destroy(&mut f).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real Linux Docker Engine"]
async fn docker_linux_runc_process_session_timeout_destroys_its_sandbox()
-> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut f = Fixture::new().await?;
    f.sandbox().start().await?;
    let session = f
        .sandbox()
        .open_process(ProcessRequest {
            argv: vec![
                PY.into(),
                "-u".into(),
                "-c".into(),
                "import time; time.sleep(30)".into(),
            ],
            timeout: Duration::from_millis(500),
            output_limit_bytes: 4096,
        })
        .await?;
    assert!(matches!(
        session.wait().await,
        Err(SandboxError::TimedOut { .. })
    ));
    assert_eq!(f.sandbox().status().await?.phase, SandboxPhase::Destroyed);
    destroy(&mut f).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicit opt-in and a real Linux Docker Engine with outbound Internet"]
async fn docker_bridge_allows_outbound_https() -> Result<(), Box<dyn std::error::Error>> {
    enabled();
    let mut f = Fixture::with_network(NetworkAccess::Bridge).await?;
    f.sandbox().start().await?;
    let status = f.sandbox().status().await?;
    assert_eq!(status.network_mode, "bridge");
    let result = f
        .sandbox()
        .exec(CommandRequest {
            argv: vec![
                PY.into(),
                "-c".into(),
                "import urllib.request; r=urllib.request.urlopen('https://example.com/', timeout=15); print(r.status)"
                    .into(),
            ],
            timeout: Duration::from_secs(30),
            output_limit_bytes: 16 * 1024,
        })
        .await?;
    assert_eq!(result.exit_code, 0, "HTTPS egress failed: {result:?}");
    assert!(String::from_utf8(result.stdout)?.contains("200"));
    destroy(&mut f).await?;
    Ok(())
}
