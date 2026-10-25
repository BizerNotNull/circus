use std::collections::{BTreeMap, BTreeSet};
use std::time::Duration;

use base64::{Engine, engine::general_purpose::STANDARD};
use circus_agent_runner::{TaskEvent, TaskPhase, TaskRequest, ToolPhase};
use serde::{Deserialize, Serialize};

pub const VERSION: u8 = 1;
pub const MAX_FRAME: usize = 12 * 1024 * 1024;
pub const MAX_EVENT: usize = 512 * 1024;
const MAX_BYTES: usize = 8 * 1024 * 1024;

#[derive(Deserialize)]
#[serde(tag = "type", deny_unknown_fields)]
pub enum Request {
    #[serde(rename = "start", rename_all = "camelCase")]
    Start {
        v: u8,
        request_id: String,
        request: TaskInput,
    },
    #[serde(rename = "cancel", rename_all = "camelCase")]
    Cancel {
        v: u8,
        request_id: String,
        task_id: String,
    },
    #[serde(rename = "cleanup_retry", rename_all = "camelCase")]
    CleanupRetry {
        v: u8,
        request_id: String,
        task_id: String,
    },
}

impl Request {
    pub fn validate(&self) -> Result<(), &'static str> {
        let (v, request_id, task_id) = match self {
            Self::Start { v, request_id, .. } => (*v, request_id, None),
            Self::Cancel {
                v,
                request_id,
                task_id,
            }
            | Self::CleanupRetry {
                v,
                request_id,
                task_id,
            } => (*v, request_id, Some(task_id)),
        };
        if v != VERSION || !valid_id(request_id) || task_id.is_some_and(|id| !valid_id(id)) {
            return Err("invalid_frame");
        }
        Ok(())
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= 64 && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaskInput {
    pub prompt: String,
    pub input_files: Vec<File>,
    pub output_paths: Vec<String>,
    pub timeout_ms: u64,
    #[serde(default)]
    pub mcp_grants: Vec<circus_agent_runner::McpGrant>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    pub path: String,
    pub data: String,
}

pub fn valid_path(path: &str) -> bool {
    path.len() <= 1024
        && !path.contains(['\\', ':'])
        && !path.chars().any(char::is_control)
        && path
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
}

impl TaskInput {
    pub fn into_task(self) -> Result<TaskRequest, &'static str> {
        if self.prompt.is_empty()
            || self.prompt.len() > 65536
            || self.prompt.contains('\0')
            || self.timeout_ms == 0
            || self.timeout_ms > 3_600_000
            || self.input_files.len() > 64
            || self.output_paths.len() > 32
        {
            return Err("invalid_request");
        }
        let mut input_files = BTreeMap::new();
        let mut total = 0usize;
        for file in self.input_files {
            if !valid_path(&file.path) || file.data.len() > MAX_BYTES.div_ceil(3) * 4 {
                return Err("invalid_request");
            }
            let bytes = STANDARD.decode(&file.data).map_err(|_| "invalid_request")?;
            if STANDARD.encode(&bytes) != file.data {
                return Err("invalid_request");
            }
            total += bytes.len();
            if total > MAX_BYTES || input_files.insert(file.path, bytes).is_some() {
                return Err("invalid_request");
            }
        }
        let mut paths = BTreeSet::new();
        for path in &self.output_paths {
            if !valid_path(path) || !paths.insert(path) {
                return Err("invalid_request");
            }
        }
        Ok(TaskRequest {
            prompt: self.prompt,
            input_files,
            output_paths: self.output_paths,
            timeout: Duration::from_millis(self.timeout_ms),
            // The v1 stdio protocol remains single-turn: durable archives and
            // native session resume are caller-only library features here.
            workspace: None,
            agent_session: None,
        })
    }
}

#[derive(Serialize)]
#[serde(tag = "type")]
pub enum Response<'a> {
    #[serde(rename = "ready", rename_all = "camelCase")]
    Ready { v: u8, max_frame_bytes: usize },
    #[serde(rename = "accepted", rename_all = "camelCase")]
    Accepted {
        v: u8,
        request_id: &'a str,
        task_id: &'a str,
    },
    #[serde(rename = "event", rename_all = "camelCase")]
    Event {
        v: u8,
        request_id: &'a str,
        task_id: &'a str,
        seq: u32,
        event: Event,
    },
    #[serde(rename = "result", rename_all = "camelCase")]
    Result {
        v: u8,
        request_id: &'a str,
        task_id: &'a str,
        result: FinalResult,
    },
    #[serde(rename = "cancel_ack", rename_all = "camelCase")]
    CancelAck {
        v: u8,
        request_id: &'a str,
        task_id: &'a str,
    },
    #[serde(rename = "cleanup_result", rename_all = "camelCase")]
    CleanupResult {
        v: u8,
        request_id: &'a str,
        task_id: &'a str,
        cleanup: Cleanup,
    },
    #[serde(rename = "error")]
    Error {
        v: u8,
        code: &'static str,
        message: &'static str,
    },
}

#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Event {
    Phase {
        phase: &'static str,
    },
    #[serde(rename_all = "camelCase")]
    SandboxCreated {
        instance_id: String,
    },
    TextDelta {
        text: String,
    },
    #[serde(rename_all = "camelCase")]
    Tool {
        id: String,
        tool: String,
        phase: &'static str,
        exit_code: Option<i64>,
    },
    ToolOutput {
        id: String,
        text: String,
    },
}

impl From<TaskEvent> for Event {
    fn from(event: TaskEvent) -> Self {
        match event {
            TaskEvent::Phase(phase) => Self::Phase {
                phase: match phase {
                    TaskPhase::Preparing => "preparing",
                    TaskPhase::Running => "running",
                    TaskPhase::Exporting => "exporting",
                    TaskPhase::Cleaning => "cleaning",
                },
            },
            TaskEvent::SandboxCreated { instance_id } => Self::SandboxCreated { instance_id },
            TaskEvent::TextDelta { text } => Self::TextDelta { text },
            TaskEvent::Tool {
                id,
                kind,
                phase,
                exit_code,
            } => Self::Tool {
                id,
                tool: kind,
                phase: match phase {
                    ToolPhase::Started => "started",
                    ToolPhase::Completed => "completed",
                },
                exit_code,
            },
            TaskEvent::ToolOutput { id, text } => Self::ToolOutput { id, text },
        }
    }
}

#[derive(Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Cleanup {
    Confirmed,
    Pending,
    Unknown,
}

#[derive(Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum FinalResult {
    Succeeded {
        cleanup: Cleanup,
        artifacts: Vec<File>,
    },
    Failed {
        cleanup: Cleanup,
        error: Failure,
    },
    Cancelled {
        cleanup: Cleanup,
        error: Failure,
    },
}

#[derive(Serialize)]
pub struct Failure {
    pub code: &'static str,
    pub message: &'static str,
}

pub fn artifacts(files: &BTreeMap<String, Vec<u8>>) -> Vec<File> {
    files
        .iter()
        .map(|(path, data)| File {
            path: path.clone(),
            data: STANDARD.encode(data),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unknown_authority_version_and_duplicate_keys() {
        for input in [
            r#"{"v":1,"type":"cancel","requestId":"a","taskId":"b","command":"sh"}"#,
            r#"{"v":1,"v":1,"type":"cancel","requestId":"a","taskId":"b"}"#,
        ] {
            assert!(sonic_rs::from_str::<Request>(input).is_err());
        }
        let request: Request =
            sonic_rs::from_str(r#"{"v":2,"type":"cancel","requestId":"a","taskId":"b"}"#).unwrap();
        assert_eq!(request.validate(), Err("invalid_frame"));
    }

    #[test]
    fn paths_and_base64_are_portable_and_unambiguous() {
        for path in ["", "../x", "/x", "C:x", "a\\b", "a//b", "./a", "a\0b"] {
            assert!(!valid_path(path));
        }
        assert!(valid_path("src/你好.rs"));
        for data in ["YQ", "YQ==!", "YR=="] {
            let request = TaskInput {
                prompt: "fix".into(),
                input_files: vec![File {
                    path: "a".into(),
                    data: data.into(),
                }],
                output_paths: vec![],
                timeout_ms: 10,
                mcp_grants: Vec::new(),
            };
            assert!(request.into_task().is_err());
        }
    }

    #[test]
    fn input_preserves_bytes_and_rejects_duplicate_files() {
        let input = || File {
            path: "a".into(),
            data: "AP8=".into(),
        };
        let request = TaskInput {
            prompt: "fix".into(),
            input_files: vec![input()],
            output_paths: vec!["a".into()],
            timeout_ms: 10,
            mcp_grants: Vec::new(),
        };
        assert_eq!(request.into_task().unwrap().input_files["a"], [0, 255]);
        let duplicate = TaskInput {
            prompt: "fix".into(),
            input_files: vec![input(), input()],
            output_paths: vec![],
            timeout_ms: 10,
            mcp_grants: Vec::new(),
        };
        assert!(duplicate.into_task().is_err());
    }
}
