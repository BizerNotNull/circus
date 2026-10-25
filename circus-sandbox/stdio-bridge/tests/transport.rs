use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use serde::Deserialize;

struct Peer {
    process: Child,
    input: Option<ChildStdin>,
    output: BufReader<ChildStdout>,
    _directory: tempfile::TempDir,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct Frame {
    #[serde(rename = "type")]
    kind: String,
    code: Option<String>,
    task_id: Option<String>,
    result: Option<ResultFrame>,
    event: Option<EventFrame>,
}

#[derive(Deserialize, Debug)]
#[serde(rename_all = "camelCase")]
struct EventFrame {
    instance_id: Option<String>,
}

#[derive(Deserialize, Debug)]
struct ResultFrame {
    status: String,
    cleanup: String,
    error: Option<FailureFrame>,
}

#[derive(Deserialize, Debug)]
struct FailureFrame {
    code: String,
    message: String,
}

impl Peer {
    fn missing_endpoint() -> &'static str {
        if cfg!(windows) {
            "npipe:////./pipe/circus-stdio-missing-engine"
        } else {
            "unix:///circus-stdio-missing-engine.sock"
        }
    }

    fn new() -> Self {
        Self::with_backend(
            Self::missing_endpoint(),
            "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            &std::env::current_exe().unwrap(),
        )
    }

    fn with_backend(endpoint: &str, image: &str, python: &std::path::Path) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let credential = directory.path().join("key");
        std::fs::write(&credential, "host-only-test-sentinel").unwrap();
        let json = config_json(endpoint, image, python, &credential);
        let mut peer = Self::spawn(directory, &json);
        assert_eq!(peer.read().kind, "ready");
        peer
    }

    fn spawn(directory: tempfile::TempDir, json: &str) -> Self {
        let config = directory.path().join("config.json");
        std::fs::write(&config, json).unwrap();
        let mut process = Command::new(env!("CARGO_BIN_EXE_circus-stdio-bridge"))
            .args(["--config", config.to_str().unwrap()])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let input = process.stdin.take();
        let output = BufReader::new(process.stdout.take().unwrap());
        Self {
            process,
            input,
            output,
            _directory: directory,
        }
    }

    fn write(&mut self, frame: &str) {
        self.input
            .as_mut()
            .unwrap()
            .write_all(frame.as_bytes())
            .unwrap();
    }
    fn read(&mut self) -> Frame {
        let mut line = String::new();
        assert!(self.output.read_line(&mut line).unwrap() > 0);
        assert!(!line.contains("host-only-test-sentinel"));
        sonic_rs::from_str(&line).unwrap()
    }
    fn close(&mut self) {
        drop(self.input.take());
    }
    fn wait(&mut self) -> std::process::ExitStatus {
        for _ in 0..500 {
            if let Some(status) = self.process.try_wait().unwrap() {
                return status;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        self.process.kill().unwrap();
        panic!("bridge did not exit after stdin EOF");
    }
}

#[test]
#[ignore = "requires explicit real Linux Docker and the pinned Codex image"]
fn docker_cancellation_and_eof_confirm_owned_container_removal() {
    assert_eq!(std::env::var("CIRCUS_DOCKER_SANDBOX").as_deref(), Ok("1"));
    let endpoint = std::env::var("CIRCUS_DOCKER_ENDPOINT").unwrap();
    let image = std::env::var("CIRCUS_AGENT_IMAGE").unwrap();
    let python = std::env::var("CIRCUS_HOST_PYTHON").unwrap();
    for cancel in [true, false] {
        let mut peer = Peer::with_backend(&endpoint, &image, std::path::Path::new(&python));
        peer.write(&START.replace("\"timeoutMs\":1000", "\"timeoutMs\":60000"));
        let task_id = peer.read().task_id.unwrap();
        let instance = loop {
            let frame = peer.read();
            assert_ne!(
                frame.kind, "result",
                "task ended before sandbox creation: {:?}",
                frame.result
            );
            if let Some(EventFrame {
                instance_id: Some(id),
            }) = frame.event
            {
                break id;
            }
        };
        if cancel {
            peer.write(&format!("{{\"v\":1,\"type\":\"cancel\",\"requestId\":\"client-a\",\"taskId\":\"{task_id}\"}}\n"));
        } else {
            peer.close();
        }
        loop {
            let frame = peer.read();
            if frame.kind == "result" {
                let result = frame.result.unwrap();
                assert_eq!(result.status, "cancelled");
                assert_eq!(result.cleanup, "confirmed");
                let error = result.error.unwrap();
                assert_eq!(error.code, "cancelled");
                assert_eq!(error.message, "Agent task cancelled");
                break;
            }
        }
        peer.close();
        assert!(peer.wait().success());
        let inspection = Command::new("docker")
            .args(["--host", &endpoint, "inspect", &instance])
            .output()
            .unwrap();
        assert!(!inspection.status.success());
        assert!(
            String::from_utf8(inspection.stderr)
                .unwrap()
                .to_lowercase()
                .contains("no such")
        );
    }
}

impl Drop for Peer {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

fn config_json(
    endpoint: &str,
    image: &str,
    python: &std::path::Path,
    credential: &std::path::Path,
) -> String {
    format!(
        r#"{{"version":1,"backend":{{"kind":"docker","endpoint":{},"image":{},"runtime":"runc"}},"image":{{"platform":{{"os":"linux","architecture":"amd64"}},"pullPolicy":"local-only","prepareTimeoutMs":30000}},"python":{},"resources":{{"memoryBytes":536870912,"cpuMillis":1000,"maxProcesses":128,"workspaceBytes":134217728,"maxFileBytes":2097152}},"model":{{"baseUrl":"https://unused.invalid/v1","model":"test","apiKeyFile":{}}}}}"#,
        sonic_rs::to_string(endpoint).unwrap(),
        sonic_rs::to_string(image).unwrap(),
        sonic_rs::to_string(python).unwrap(),
        sonic_rs::to_string(credential).unwrap()
    )
}

fn reject_configuration(configure: impl FnOnce(String) -> String) {
    let directory = tempfile::tempdir().unwrap();
    let credential = directory.path().join("key");
    std::fs::write(&credential, "host-only-test-sentinel").unwrap();
    let config = config_json(
        Peer::missing_endpoint(),
        "circus-agent:test",
        &std::env::current_exe().unwrap(),
        &credential,
    );
    let mut peer = Peer::spawn(directory, &configure(config));
    peer.close();
    assert!(!peer.wait().success());
    let frame = peer.read();
    assert_eq!(frame.kind, "error");
    assert_eq!(frame.code.as_deref(), Some("configuration"));
    let mut extra = String::new();
    assert_eq!(peer.output.read_line(&mut extra).unwrap(), 0);
    use std::io::Read;
    let mut diagnostics = String::new();
    peer.process
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut diagnostics)
        .unwrap();
    assert!(!diagnostics.contains("host-only-test-sentinel"));
}

#[test]
fn obsolete_runner_configuration_is_rejected_before_ready() {
    reject_configuration(|config| {
        let start = config.find(r#""backend":"#).unwrap();
        let end = config.find(r#","python":"#).unwrap();
        format!(
            "{}{}{}",
            &config[..start],
            r#""dockerEndpoint":"unix:///unused.sock","agentImage":"circus-agent:test""#,
            &config[end..]
        )
    });
    for obsolete in ["dockerEndpoint", "agentImage"] {
        reject_configuration(|config| {
            config.replacen('{', &format!(r#"{{"{obsolete}":"obsolete","#), 1)
        });
    }
}

#[test]
fn invalid_backend_and_image_configuration_is_rejected_before_ready() {
    for (valid, invalid) in [
        (r#""image":"circus-agent:test""#, r#""image":"""#),
        (
            r#""image":"circus-agent:test""#,
            r#""image":"invalid host-only-test-sentinel""#,
        ),
        (r#""runtime":"runc""#, r#""runtime":"runsc""#),
        (r#""os":"linux""#, r#""os":"windows""#),
        (r#""architecture":"amd64""#, r#""architecture":"x86_64""#),
        (
            r#""architecture":"amd64""#,
            r#""architecture":"amd64","variant":"""#,
        ),
        (r#""prepareTimeoutMs":30000"#, r#""prepareTimeoutMs":0"#),
        (r#""prepareTimeoutMs":30000"#, r#""prepareTimeoutMs":-1"#),
        (r#""pullPolicy":"local-only""#, r#""pullPolicy":"always""#),
    ] {
        reject_configuration(|config| config.replacen(valid, invalid, 1));
    }
    reject_configuration(|config| {
        config.replace(
            &sonic_rs::to_string(Peer::missing_endpoint()).unwrap(),
            r#""https://user:host-only-test-sentinel@daemon.invalid""#,
        )
    });
}

fn containerd_config(config: String) -> String {
    let start = config.find(r#""backend":"#).unwrap();
    let end = config.find(r#","image":{"platform""#).unwrap();
    format!(
        "{}{}{}",
        &config[..start],
        r#""backend":{"kind":"containerd","socket":"/run/circus-missing-containerd.sock","namespace":"circus-test","image":"circus-agent:test","runtime":"runc","snapshotter":"overlayfs"}"#,
        &config[end..]
    )
}

#[cfg(not(all(unix, feature = "containerd")))]
#[test]
fn unavailable_containerd_is_rejected_before_ready() {
    reject_configuration(containerd_config);
}

#[cfg(all(unix, feature = "containerd"))]
#[test]
fn invalid_containerd_descriptors_are_rejected_before_ready() {
    for (valid, invalid) in [
        (r#""runtime":"runc""#, r#""runtime":"runsc""#),
        (r#""snapshotter":"overlayfs""#, r#""snapshotter":"native""#),
        (
            r#""namespace":"circus-test""#,
            r#""namespace":"invalid/namespace""#,
        ),
        (
            r#""socket":"/run/circus-missing-containerd.sock""#,
            r#""socket":"relative.sock""#,
        ),
    ] {
        reject_configuration(|config| containerd_config(config).replace(valid, invalid));
    }
}

const START: &str = "{\"v\":1,\"type\":\"start\",\"requestId\":\"client-a\",\"request\":{\"prompt\":\"test\",\"inputFiles\":[],\"outputPaths\":[],\"timeoutMs\":1000}}\n";

#[test]
fn real_entry_reports_image_preparation_failure_without_allocating_a_sandbox() {
    let mut peer = Peer::new();
    peer.write(START);
    assert_eq!(peer.read().kind, "accepted");
    loop {
        let frame = peer.read();
        assert!(
            frame
                .event
                .as_ref()
                .and_then(|event| event.instance_id.as_ref())
                .is_none(),
            "image preparation must fail before sandbox creation"
        );
        if frame.kind == "result" {
            let result = frame.result.unwrap();
            assert_eq!(result.status, "failed");
            assert_eq!(result.cleanup, "confirmed");
            let error = result.error.unwrap();
            assert_eq!(error.code, "image_preparation");
            assert!(!error.message.contains("host-only-test-sentinel"));
            assert!(!error.message.contains(Peer::missing_endpoint()));
            break;
        }
    }
    peer.close();
    assert!(peer.wait().success());
}

#[test]
fn foreign_task_identity_is_rejected_and_owner_is_settled() {
    let mut peer = Peer::new();
    peer.write(START);
    let accepted = peer.read();
    assert_eq!(accepted.kind, "accepted");
    assert!(accepted.task_id.is_some());
    peer.write(
        "{\"v\":1,\"type\":\"cancel\",\"requestId\":\"client-b\",\"taskId\":\"another-task\"}\n",
    );
    loop {
        let frame = peer.read();
        if frame.kind == "error" {
            assert_eq!(frame.code.as_deref(), Some("wrong_task"));
            break;
        }
    }
    assert!(!peer.wait().success());
}

#[test]
fn malformed_and_truncated_frames_fail_closed() {
    for invalid in [
        "{no-json}\n",
        "{\"v\":1",
        "{\"v\":2,\"type\":\"cancel\",\"requestId\":\"a\",\"taskId\":\"b\"}\n",
    ] {
        let mut peer = Peer::new();
        peer.write(invalid);
        peer.close();
        let frame = peer.read();
        assert_eq!(frame.kind, "error");
        assert_eq!(frame.code.as_deref(), Some("invalid_frame"));
        assert!(!peer.wait().success());
    }
}

#[test]
fn invalid_path_never_starts_a_task() {
    let mut peer = Peer::new();
    peer.write(&START.replace("\"outputPaths\":[]", "\"outputPaths\":[\"../host\"]"));
    assert_eq!(peer.read().code.as_deref(), Some("invalid_request"));
    assert!(!peer.wait().success());
}
