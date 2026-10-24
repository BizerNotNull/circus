//! Real Codex and Docker, but a scripted Responses peer: no inference or credentials.

#[allow(dead_code)]
#[path = "../examples/codex_agent/runner.rs"]
mod runner;

use std::process::Command;
use std::time::Duration;

use runner::{Config, Result};

fn config() -> Result<Config> {
    assert_eq!(std::env::var("CIRCUS_DOCKER_SANDBOX").as_deref(), Ok("1"));
    Ok(Config {
        endpoint: std::env::var("CIRCUS_DOCKER_ENDPOINT")?,
        image: std::env::var("CIRCUS_AGENT_IMAGE")?,
        python: std::env::var("CIRCUS_HOST_PYTHON").unwrap_or_else(|_| "python3".into()),
        base_url: "https://unused.invalid/v1".into(),
        model: "circus-fixture".into(),
        api_key: "host-only-test-sentinel".into(),
        prompt: "Create answer.py containing add(a, b), test add(20, 22), and write 42 to result.txt. Use the shell tool to run the test.".into(),
        result_path: "project/result.txt".into(),
        timeout: Duration::from_secs(45),
    })
}

fn owned_containers(endpoint: &str) -> Result<String> {
    let output = Command::new("docker")
        .args([
            "--host",
            endpoint,
            "ps",
            "-aq",
            "--filter",
            "label=circus.sandbox.owner",
        ])
        .output()?;
    if !output.status.success() {
        return Err("docker ps failed".into());
    }
    Ok(String::from_utf8(output.stdout)?)
}

#[tokio::test]
#[ignore = "requires a local Codex image, Python 3.11+ on the host, and a real Linux Docker Engine"]
async fn codex_turn_uses_only_the_scoped_model_channel_and_cleans_up() -> Result {
    let endpoint = config()?.endpoint;
    let before = owned_containers(&endpoint)?;
    let controller = format!(
        "{}\n{}",
        runner::BRIDGE,
        include_str!("fixtures/codex_responses.py")
    );
    let result = runner::run(config()?, &controller).await;
    assert_eq!(
        owned_containers(&endpoint)?,
        before,
        "successful task leaked a container"
    );
    assert_eq!(result?, b"42\n");

    // A failed upstream must produce a failed task and still reclaim the container.
    let rejected = format!(
        "{}\nclass RejectPeer:\n def forward(self, body, emit):\n  raise RuntimeError('upstream-unavailable')\nmain_host(RejectPeer())\n",
        runner::BRIDGE
    );
    assert!(runner::run(config()?, &rejected).await.is_err());
    assert_eq!(
        owned_containers(&endpoint)?,
        before,
        "failed task leaked a container"
    );

    // A stuck host driver must be killed/reaped and its container reclaimed.
    let mut limited = config()?;
    limited.timeout = Duration::from_secs(3);
    let started = tempfile::NamedTempFile::new()?;
    let driver = format!(
        "from pathlib import Path\nimport os, time\nPath({:?}).write_text(str(os.getpid()))\ntime.sleep(300)\n",
        started.path().to_str().ok_or("non-UTF-8 test path")?,
    );
    let began = std::time::Instant::now();
    assert!(runner::run(limited, &driver).await.is_err());
    let pid: u32 = std::fs::read_to_string(started.path())?.parse()?;
    assert!(pid > 0, "host driver did not start");
    assert!(began.elapsed() >= Duration::from_secs(3));
    #[cfg(unix)]
    {
        let status = Command::new(&config()?.python)
            .args(["-I", "-B", "-c", "import os, sys\ntry: os.kill(int(sys.argv[1]), 0)\nexcept ProcessLookupError: sys.exit(0)\nsys.exit(1)", &pid.to_string()])
            .status()?;
        assert!(status.success(), "timed out host controller was not reaped");
    }
    assert_eq!(
        owned_containers(&endpoint)?,
        before,
        "timed out task leaked a container"
    );
    Ok(())
}
