use std::time::Duration;

use circus_sandbox::environment::{ImagePlatform, ImagePullPolicy};
use circus_sandbox::{BackendConfig, ResourceLimits};

use crate::{AgentRunner, ImagePreparation, ModelConfig, RunnerConfig, TaskError};

fn config(backend: BackendConfig) -> RunnerConfig {
    RunnerConfig {
        backend,
        image_preparation: Some(ImagePreparation {
            platform: ImagePlatform::new("linux", "amd64").unwrap(),
            timeout: Duration::from_secs(10),
            pull_policy: ImagePullPolicy::LocalOnly,
        }),
        resources: ResourceLimits::new(1024 * 1024, 1000, 32, 1024 * 1024, 4096).unwrap(),
        model: ModelConfig::new(
            "https://unused.invalid/v1".into(),
            "model".into(),
            "unused-key".into(),
        )
        .unwrap(),
        python: "python3".into(),
    }
}

async fn failure(config: RunnerConfig) -> std::sync::Arc<crate::TaskOutcome> {
    let task = AgentRunner::new(config)
        .unwrap()
        .start(super::request())
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(5), task.wait())
        .await
        .expect("owned task must settle");
    let Err(failure) = outcome.as_ref() else {
        panic!("invalid configuration unexpectedly succeeded");
    };
    assert!(failure.cleanup.is_none());
    outcome
}

#[tokio::test]
async fn invalid_preparation_configuration_fails_before_daemon_access() {
    for (endpoint, image, runtime) in [
        ("tcp://127.0.0.1:1", "test-image", "runc"),
        ("unix:///unused/docker.sock", "", "runc"),
        ("unix:///unused/docker.sock", "test-image", "runsc"),
    ] {
        assert!(matches!(
            failure(config(BackendConfig::docker(endpoint, image, runtime)))
                .await
                .as_ref()
                .as_ref()
                .unwrap_err()
                .error,
            TaskError::Invalid(_)
        ));
    }
    for timeout in [Duration::ZERO, Duration::MAX] {
        let mut config = config(BackendConfig::docker(
            "unix:///unused/docker.sock",
            "test-image",
            "runc",
        ));
        config.image_preparation.as_mut().unwrap().timeout = timeout;
        assert!(matches!(
            failure(config).await.as_ref().as_ref().unwrap_err().error,
            TaskError::Invalid(_)
        ));
    }
}

#[tokio::test]
async fn direct_prepared_backend_without_image_preparation_uses_sandbox_validation() {
    let mut config = config(BackendConfig::docker(
        "tcp://127.0.0.1:1",
        "already-prepared-image",
        "runc",
    ));
    config.image_preparation = None;
    let outcome = failure(config).await;
    assert!(matches!(
        outcome.as_ref().as_ref().unwrap_err().error,
        TaskError::Sandbox { .. }
    ));
}

#[cfg(not(all(unix, feature = "containerd")))]
#[tokio::test]
async fn unavailable_containerd_preparation_fails_closed() {
    let failure = failure(config(BackendConfig::containerd(
        "/unused/containerd.sock",
        "test",
        "test-image",
        "runc",
        "overlayfs",
    )))
    .await;
    assert!(matches!(
        failure.as_ref().as_ref().unwrap_err().error,
        TaskError::Invalid(_)
    ));
}

#[cfg(unix)]
mod unix {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    // Like the sandbox image-policy tests, exercise the actual Bollard HTTP
    // boundary over a controlled local socket, without a daemon or registry.
    struct Peer {
        endpoint: String,
        reached: Option<oneshot::Receiver<()>>,
        release: Option<oneshot::Sender<()>>,
        worker: tokio::task::JoinHandle<()>,
        _directory: tempfile::TempDir,
    }

    async fn header(socket: &mut tokio::net::UnixStream) -> String {
        let mut bytes = Vec::new();
        loop {
            let byte = socket.read_u8().await.expect("complete HTTP header");
            bytes.push(byte);
            assert!(bytes.len() <= 32768);
            if bytes.ends_with(b"\r\n\r\n") {
                return String::from_utf8(bytes).unwrap();
            }
        }
    }

    async fn respond(socket: &mut tokio::net::UnixStream, status: u16, body: &str) {
        let response = format!(
            "HTTP/1.1 {status} peer\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        // A cancelled operation is permitted to close its HTTP connection.
        let _ = socket.write_all(response.as_bytes()).await;
        let _ = socket.shutdown().await;
    }

    impl Peer {
        fn new(status: u16, body: &'static str) -> Self {
            let directory = tempfile::tempdir().unwrap();
            let path = directory.path().join("engine.sock");
            let listener = tokio::net::UnixListener::bind(&path).unwrap();
            let endpoint = format!("unix://{}", path.display());
            let (reached, wait_reached) = oneshot::channel();
            let (release, wait_release) = oneshot::channel();
            let worker = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = header(&mut socket).await;
                assert!(request.starts_with("GET /version HTTP/1.1\r\n"));
                respond(
                    &mut socket,
                    200,
                    r#"{"ApiVersion":"1.47","MinAPIVersion":"1.24"}"#,
                )
                .await;
                let (mut socket, _) = listener.accept().await.unwrap();
                let request = header(&mut socket).await;
                let target = request.split_whitespace().nth(1).unwrap();
                let target = target.strip_prefix("/v1.47").unwrap_or(target);
                assert_eq!(target, "/images/test-image/json");
                assert!(request.starts_with("GET "));
                reached.send(()).unwrap();
                let _ = wait_release.await;
                respond(&mut socket, status, body).await;
                assert!(
                    tokio::time::timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err(),
                    "preparation failure must not pull or create a sandbox"
                );
            });
            Self {
                endpoint,
                reached: Some(wait_reached),
                release: Some(release),
                worker,
                _directory: directory,
            }
        }

        fn config(&self) -> RunnerConfig {
            config(BackendConfig::docker(&self.endpoint, "test-image", "runc"))
        }

        async fn reached(&mut self) {
            tokio::time::timeout(Duration::from_secs(5), self.reached.take().unwrap())
                .await
                .expect("image inspection boundary must be reached")
                .unwrap();
        }

        fn release(&mut self) {
            self.release.take().unwrap().send(()).unwrap();
        }

        async fn finish(mut self) {
            tokio::time::timeout(Duration::from_secs(5), &mut self.worker)
                .await
                .expect("peer must settle")
                .expect("peer boundary assertions");
        }
    }

    impl Drop for Peer {
        fn drop(&mut self) {
            self.worker.abort();
        }
    }

    #[tokio::test]
    async fn cancellation_during_image_preparation_prevents_sandbox_creation() {
        let mut peer = Peer::new(
            200,
            r#"{"Id":"unused","Os":"linux","Architecture":"amd64"}"#,
        );
        let task = AgentRunner::new(peer.config())
            .unwrap()
            .start(super::super::request())
            .unwrap();
        peer.reached().await;
        task.cancel();
        let outcome = tokio::time::timeout(Duration::from_secs(5), task.wait())
            .await
            .unwrap();
        let failure = outcome.as_ref().as_ref().unwrap_err();
        assert!(matches!(failure.error, TaskError::Cancelled));
        assert!(failure.cleanup.is_none());
        peer.release();
        peer.finish().await;
    }

    #[tokio::test]
    async fn task_deadline_during_image_preparation_prevents_sandbox_creation() {
        let mut peer = Peer::new(200, "{}");
        let mut request = super::super::request();
        request.timeout = Duration::from_secs(2);
        let task = AgentRunner::new(peer.config())
            .unwrap()
            .start(request)
            .unwrap();
        peer.reached().await;
        let outcome = tokio::time::timeout(Duration::from_secs(5), task.wait())
            .await
            .unwrap();
        let failure = outcome.as_ref().as_ref().unwrap_err();
        assert!(matches!(failure.error, TaskError::TimedOut));
        assert!(failure.cleanup.is_none());
        peer.release();
        peer.finish().await;
    }

    #[tokio::test]
    async fn preparation_timeout_propagates_without_sandbox_creation() {
        let mut peer = Peer::new(200, "{}");
        let mut config = peer.config();
        config.image_preparation.as_mut().unwrap().timeout = Duration::from_secs(2);
        let task = AgentRunner::new(config)
            .unwrap()
            .start(super::super::request())
            .unwrap();
        peer.reached().await;
        let outcome = tokio::time::timeout(Duration::from_secs(5), task.wait())
            .await
            .unwrap();
        let failure = outcome.as_ref().as_ref().unwrap_err();
        assert!(matches!(failure.error, TaskError::ImagePreparation(_)));
        assert!(failure.cleanup.is_none());
        peer.release();
        peer.finish().await;
    }

    #[tokio::test]
    async fn local_only_image_miss_and_daemon_error_propagate_without_creation() {
        for (status, body) in [
            (404, r#"{"message":"No such image"}"#),
            (500, r#"{"message":"controlled inspection failure"}"#),
        ] {
            let mut peer = Peer::new(status, body);
            let task = AgentRunner::new(peer.config())
                .unwrap()
                .start(super::super::request())
                .unwrap();
            peer.reached().await;
            peer.release();
            let outcome = tokio::time::timeout(Duration::from_secs(5), task.wait())
                .await
                .unwrap();
            let failure = outcome.as_ref().as_ref().unwrap_err();
            assert!(
                matches!(&failure.error, TaskError::ImagePreparation(message) if !message.is_empty())
            );
            assert!(failure.cleanup.is_none());
            peer.finish().await;
        }
    }
}
