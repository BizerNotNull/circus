use std::time::Duration;

use circus_sandbox::environment::{
    EnvironmentError, ImagePlatform, ImageRequest, prepare_docker_image,
};
use circus_sandbox::{
    BackendConfig, CommandRequest, IoLimits, IsolationConfig, ResourceLimits, Sandbox,
    SandboxConfig, SandboxPhase,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn endpoint() -> TestResult<String> {
    assert_eq!(std::env::var("CIRCUS_DOCKER_SANDBOX").as_deref(), Ok("1"));
    Ok(std::env::var("CIRCUS_DOCKER_ENDPOINT")?)
}

fn platform() -> TestResult<ImagePlatform> {
    Ok(ImagePlatform::new(
        "linux",
        std::env::var("CIRCUS_DOCKER_ARCH")?,
    )?)
}

fn local_image() -> String {
    std::env::var("CIRCUS_DOCKER_IMAGE").unwrap_or_else(|_| "python:3.12-slim".into())
}

fn request(reference: String) -> TestResult<ImageRequest> {
    Ok(ImageRequest::new(
        endpoint()?,
        reference,
        platform()?,
        Duration::from_secs(120),
    )?)
}

fn absent_reference() -> String {
    // Public repository, unique nonexistent tag. Never remove daemon images to create a miss.
    format!(
        "docker.io/library/python:circus-missing-{}",
        uuid::Uuid::new_v4()
    )
}

#[tokio::test]
#[ignore = "requires explicit real Linux Docker Engine and local Python image"]
async fn local_image_resolves_immutable_id_and_runs() -> TestResult {
    let prepared = prepare_docker_image(request(local_image())?).await?;
    assert!(prepared.id().starts_with("sha256:"));
    assert_eq!(prepared.id().len(), 71);
    assert!(
        prepared.id()[7..]
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit())
    );
    assert_eq!(prepared.os(), "linux");
    assert_eq!(prepared.architecture(), platform()?.architecture());
    let repeated = prepare_docker_image(request(prepared.id().into())?).await?;
    assert_eq!(prepared.id(), repeated.id());
    assert_eq!(prepared.repo_digests(), repeated.repo_digests());

    let config = SandboxConfig::new(
        ResourceLimits::new(64 * 1024 * 1024, 100, 32, 8 * 1024 * 1024, 1024)?,
        IsolationConfig::linux_enforced(true, true, true, true, "/workspace", 65534, 65534)?,
        IoLimits::new(64 * 1024, Duration::from_secs(10))?,
    )?;
    // Sandbox generates a unique owner/name; this test owns exactly this container.
    let sandbox = Sandbox::create(
        BackendConfig::docker(endpoint()?, prepared.id(), "runc"),
        config,
    )
    .await?;
    let execution: TestResult<_> = async {
        sandbox.start().await?;
        let output = sandbox
            .exec(CommandRequest {
                argv: vec![
                    "python3".into(),
                    "-c".into(),
                    "print('prepared-image')".into(),
                ],
                timeout: Duration::from_secs(10),
                output_limit_bytes: 1024,
            })
            .await?;
        Ok(output)
    }
    .await;
    // Explicit cleanup runs even when start/exec returns an error, before checking output.
    sandbox.destroy().await?;
    assert_eq!(sandbox.status().await?.phase, SandboxPhase::Destroyed);
    let output = execution?;
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, b"prepared-image\n");
    assert!(output.stderr.is_empty());
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicit real Linux Docker Engine"]
async fn missing_local_image_never_pulls() -> TestResult {
    let reference = absent_reference();
    let result = prepare_docker_image(request(reference.clone())?).await;
    assert!(
        matches!(result, Err(EnvironmentError::ImageNotFound { reference: found }) if found == reference)
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicit real Linux Docker Engine and local Python image"]
async fn local_platform_mismatch_is_an_error() -> TestResult {
    let actual = prepare_docker_image(request(local_image())?).await?;
    let wrong_arch = if actual.architecture() == "amd64" {
        "arm64"
    } else {
        "amd64"
    };
    let result = prepare_docker_image(ImageRequest::new(
        endpoint()?,
        actual.id(),
        ImagePlatform::new("linux", wrong_arch)?,
        Duration::from_secs(30),
    )?)
    .await;
    assert!(
        matches!(result, Err(EnvironmentError::PlatformMismatch { actual_os, actual_architecture, .. })
        if actual_os == actual.os() && actual_architecture == actual.architecture())
    );
    Ok(())
}
