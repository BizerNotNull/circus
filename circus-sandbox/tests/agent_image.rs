//! Opt-in acceptance of the locally built image under the real sandbox policy.

use std::process::Command;
use std::time::Duration;

use circus_sandbox::{
    BackendConfig, CommandRequest, IoLimits, IsolationConfig, ResourceLimits, Sandbox,
    SandboxConfig, SandboxPhase,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct ContainerGuard<'a> {
    host: &'a str,
    id: String,
}

impl Drop for ContainerGuard<'_> {
    fn drop(&mut self) {
        // Assertions can unwind before async destroy; remove only this test's container.
        let _ = Command::new("docker")
            .args(["--host", self.host, "rm", "-f", &self.id])
            .output();
    }
}

#[tokio::test]
#[ignore = "requires a locally built Codex image and an explicit real Linux Docker Engine"]
async fn codex_image_starts_offline_in_a_fresh_bounded_workspace() -> Result {
    assert_eq!(std::env::var("CIRCUS_DOCKER_SANDBOX").as_deref(), Ok("1"));
    let backend = BackendConfig::docker(
        std::env::var("CIRCUS_DOCKER_ENDPOINT")?,
        std::env::var("CIRCUS_AGENT_IMAGE")?,
        "runc",
    );
    let host = match &backend {
        BackendConfig::Docker(docker) => match docker.endpoint() {
            endpoint if endpoint.starts_with('/') => format!("unix://{endpoint}"),
            endpoint => endpoint.to_string(),
        },
    };
    let config = SandboxConfig::new(
        ResourceLimits::new(
            512 * 1024 * 1024,
            1000,
            128,
            128 * 1024 * 1024,
            1024 * 1024,
        )?,
        IsolationConfig::linux_enforced(true, true, true, true, "/workspace", 65534, 65534)?,
        IoLimits::new(256 * 1024, Duration::from_secs(15))?,
    )?;

    // A second container must initialize its own home and project from scratch.
    for _ in 0..2 {
        let sandbox = Sandbox::create(backend.clone(), config.clone()).await?;
        let guard = ContainerGuard {
            host: &host,
            id: sandbox.status().await?.instance_id.ok_or("missing id")?,
        };
        let exercise = async {
            sandbox.start().await?;
            let enforcement = sandbox
                .status()
                .await?
                .enforcement
                .ok_or("missing inspected enforcement")?;
            assert_eq!(enforcement.readonly_root, Some(true));
            assert_eq!(enforcement.network_mode.as_deref(), Some("none"));
            // `None` means the engine reported no host binds at all.
            assert!(enforcement.bind_count.is_none_or(|count| count == 0));
            // Sandbox overrides image ENTRYPOINT, so initialization is explicit.
            let output = sandbox
                .exec(CommandRequest {
                    argv: vec![
                        "circus-agent".into(),
                        "python3".into(),
                        "/opt/circus/verify.py".into(),
                    ],
                    timeout: Duration::from_secs(45),
                    output_limit_bytes: 128 * 1024,
                })
                .await?;
            assert_eq!(
                output.exit_code,
                0,
                "image check failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            let stdout = String::from_utf8(output.stdout)?;
            assert!(stdout.contains("\"app_server\": \"ready\""), "{stdout}");
            assert_eq!(
                sandbox.read_file("project/image-check.txt").await?,
                b"circus image tools ready\n"
            );
            println!("{stdout}");
            Ok::<_, Box<dyn std::error::Error>>(())
        }
        .await;
        // Observe cleanup even when an API call fails.
        let cleanup = sandbox.destroy().await;
        cleanup?;
        exercise?;
        assert_eq!(sandbox.status().await?.phase, SandboxPhase::Destroyed);
        let inspected = Command::new("docker")
            .args(["--host", &host, "inspect", &guard.id])
            .output()?;
        assert!(!inspected.status.success());
        assert!(String::from_utf8_lossy(&inspected.stderr).contains("No such"));
    }
    Ok(())
}
