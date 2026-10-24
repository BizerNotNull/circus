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

async fn run_verified(config: Config, controller: &str) -> Result<Vec<u8>> {
    let endpoint = config.endpoint.clone();
    let mut owned = None;
    let result = runner::run_observed(config, controller, |id| owned = Some(id.to_owned())).await;
    let instance = owned.ok_or("task did not report its owned instance")?;
    let output = Command::new("docker")
        .args(["--host", &endpoint, "inspect", &instance])
        .output()?;
    let diagnostic = String::from_utf8(output.stderr)?.to_lowercase();
    assert!(
        !output.status.success(),
        "this task's instance was not removed"
    );
    assert!(
        diagnostic.contains("no such object") || diagnostic.contains("no such container"),
        "Docker could not confirm this task's instance absence"
    );
    result
}

#[tokio::test]
#[ignore = "requires a local Codex image, Python 3.11+ on the host, and a real Linux Docker Engine"]
async fn codex_turn_uses_only_the_scoped_model_channel_and_cleans_up() -> Result {
    let controller = format!(
        "{}\n{}",
        runner::BRIDGE,
        include_str!("fixtures/codex_responses.py")
    );
    // Concurrent runs must never mistake each other's containers for leaks.
    let (first, second) = tokio::join!(
        run_verified(config()?, &controller),
        run_verified(config()?, &controller),
    );
    assert_eq!(first?, b"42\n");
    assert_eq!(second?, b"42\n");

    // A failed upstream must produce a failed task and still reclaim the container.
    let rejected = format!(
        "{}\nclass RejectPeer:\n def forward(self, body, emit):\n  raise RuntimeError('upstream-unavailable')\nmain_host(RejectPeer())\n",
        runner::BRIDGE
    );
    assert!(run_verified(config()?, &rejected).await.is_err());

    // A stuck host driver must be killed/reaped and its container reclaimed.
    let mut limited = config()?;
    limited.timeout = Duration::from_secs(3);
    let started = tempfile::NamedTempFile::new()?;
    let driver = format!(
        "from pathlib import Path\nimport os, time\nPath({:?}).write_text(str(os.getpid()))\ntime.sleep(300)\n",
        started.path().to_str().ok_or("non-UTF-8 test path")?,
    );
    let began = std::time::Instant::now();
    assert!(run_verified(limited, &driver).await.is_err());
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
    Ok(())
}
