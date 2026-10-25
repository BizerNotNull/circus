use std::time::Duration;

use circus_sandbox::runtime::ResourceLimits;
use circus_sandbox::{
    ConfigError, DockerEndpoint, IoLimits, IsolationConfig, NetworkAccess, RuntimeName,
    SandboxConfig,
};

fn limits(pids: u32) -> ResourceLimits {
    ResourceLimits::new(64 * 1024, 100, pids, 32 * 1024, 1024).unwrap()
}

fn isolation() -> IsolationConfig {
    IsolationConfig::linux_enforced(
        true,
        true,
        true,
        NetworkAccess::Disabled,
        "/workspace",
        1000,
        1000,
    )
    .unwrap()
}

#[test]
fn rejects_insecure_flags_and_remote_values() {
    assert!(matches!(
        IsolationConfig::linux_enforced(
            false,
            true,
            true,
            NetworkAccess::Disabled,
            "/workspace",
            1000,
            1000
        ),
        Err(ConfigError::IsolationNotEnforced)
    ));
    assert!(DockerEndpoint::parse("tcp://127.0.0.1:2375").is_err());
    assert!(DockerEndpoint::parse("npipe://remote/pipe/name").is_err());
    assert!(RuntimeName::parse("runsc").is_err());
}

#[test]
fn revalidates_public_enum_variants_and_bounds() {
    let endpoint = DockerEndpoint::NamedPipe {
        path: "tcp://remote".into(),
    };
    assert!(matches!(
        SandboxConfig::new(
            endpoint,
            "image:tag",
            RuntimeName::runc(),
            limits(3),
            isolation(),
            IoLimits::new(1, Duration::from_secs(1)).unwrap()
        ),
        Err(ConfigError::UnsupportedEndpoint { .. })
    ));
    assert!(IoLimits::new(0, Duration::from_secs(1)).is_err());
    assert!(IoLimits::new(1, Duration::ZERO).is_err());
    assert!(matches!(
        SandboxConfig::new(
            DockerEndpoint::parse("/var/run/docker.sock").unwrap(),
            "image",
            RuntimeName::runc(),
            limits(2),
            isolation(),
            IoLimits::new(1, Duration::from_secs(1)).unwrap()
        ),
        Err(ConfigError::SupervisorNeedsPid { pids: 2 })
    ));
}

#[test]
fn accepts_valid_configuration_and_exposes_typed_values() {
    let config = SandboxConfig::new(
        DockerEndpoint::parse("/var/run/docker.sock").unwrap(),
        "alpine:3.20",
        RuntimeName::runc(),
        limits(3),
        isolation(),
        IoLimits::new(4096, Duration::from_secs(2)).unwrap(),
    )
    .unwrap();
    assert_eq!(config.image(), "alpine:3.20");
    assert_eq!(config.runtime().as_str(), "runc");
    assert_eq!(config.isolation().workspace_path(), "/workspace");
    assert_eq!(config.isolation().network_access(), NetworkAccess::Disabled);
    let bridge = IsolationConfig::linux_enforced(
        true,
        true,
        true,
        NetworkAccess::Bridge,
        "/workspace",
        1000,
        1000,
    )
    .unwrap();
    assert_eq!(bridge.network_access(), NetworkAccess::Bridge);
    assert_eq!(config.output_limit_bytes(), 4096);
}
