use std::time::Duration;

use circus_sandbox::runtime::ResourceLimits;
use circus_sandbox::{
    DockerEndpoint, IoLimits, IsolationConfig, NetworkAccess, RuntimeName, SandboxConfig,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let isolation = IsolationConfig::linux_enforced(
        true,
        true,
        true,
        NetworkAccess::Disabled,
        "/workspace",
        1000,
        1000,
    )?;
    let limits = ResourceLimits::new(64 * 1024 * 1024, 250, 3, 16 * 1024 * 1024, 1024 * 1024)?;
    let valid = SandboxConfig::new(
        DockerEndpoint::parse("/var/run/docker.sock")?,
        "alpine:3.20",
        RuntimeName::runc(),
        limits,
        isolation,
        IoLimits::new(1024 * 1024, Duration::from_secs(5))?,
    )?;
    println!(
        "valid image={} runtime={} workspace={}",
        valid.image(),
        valid.runtime().as_str(),
        valid.isolation().workspace_path()
    );

    match DockerEndpoint::parse("tcp://127.0.0.1:2375") {
        Err(error) => println!("typed invalid request: {error}"),
        Ok(_) => return Err("remote endpoint unexpectedly accepted".into()),
    }
    Ok(())
}
