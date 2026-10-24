//! OCI runtime-spec construction and inspected-spec reporting for containerd.
//!
//! The spec is plain JSON (the OCI runtime-spec is JSON by definition) placed
//! in a protobuf `Any`. Nothing here is a containerd SDK type; only the `Any`
//! envelope is needed to submit it.

use std::collections::BTreeMap;

use prost_types::Any;
use serde_json::{Value, json};

use super::backend::CommonSpec;
use super::engine::{EngineError, EngineErrorKind, HostReport, file_error};

pub(crate) const SPEC_TYPE_URL: &str = "types.containerd.io/opencontainers/runtime-spec/1/Spec";
pub(crate) const PROCESS_TYPE_URL: &str =
    "types.containerd.io/opencontainers/runtime-spec/1/Process";

const DEFAULT_PATH: &str = "/usr/local/bin:/usr/local/sbin:/usr/sbin:/usr/bin:/sbin:/bin";
const WORKSPACE_MODE: u32 = 0o755;
const CPU_PERIOD: u64 = 100_000;

/// Builds the one container init spec. The init process parks in `sleep` so the
/// sandbox stays alive until lifecycle starts, pauses, resumes, or destroys it.
pub(crate) fn container_any(spec: &CommonSpec, hostname: &str) -> Any {
    let env = default_env(&[]);
    let args = vec![
        "python3".to_string(),
        "-I".to_string(),
        "-B".to_string(),
        "-c".to_string(),
        "import time; time.sleep(31536000)".to_string(),
    ];
    let process = process_value(
        spec.guest_uid,
        spec.guest_gid,
        &spec.workspace_path,
        &args,
        &env,
    );
    let spec = json!({
        "ociVersion": "1.0.2",
        "process": process,
        "root": {"path": "rootfs", "readonly": true},
        "hostname": hostname,
        "mounts": system_mounts(spec),
        "linux": {
            "namespaces": [
                {"type": "pid"},
                {"type": "ipc"},
                {"type": "uts"},
                {"type": "mount"},
                {"type": "network"},
                {"type": "cgroup"},
            ],
            "resources": {
                "memory": {
                    "limit": spec.memory,
                    // Equal to the limit: no swap beyond the memory bound.
                    "swap": spec.memory,
                },
                "cpu": {
                    "quota": spec.nano_cpus / 10_000,
                    "period": CPU_PERIOD,
                },
                "pids": {"limit": spec.pids},
                "devices": device_rules(),
            },
            "devices": devices(spec.guest_uid, spec.guest_gid),
            "maskedPaths": [
                "/proc/acpi",
                "/proc/asound",
                "/proc/kcore",
                "/proc/keys",
                "/proc/latency_stats",
                "/proc/timer_list",
                "/proc/timer_stats",
                "/proc/sched_debug",
                "/sys/firmware",
                "/proc/scsi",
            ],
            "readonlyPaths": [
                "/proc/bus",
                "/proc/fs",
                "/proc/irq",
                "/proc/sys",
                "/proc/sysrq-trigger",
            ],
        },
    });
    any(SPEC_TYPE_URL, &spec)
}

/// Builds one exec process spec. `user` is the lifecycle-owned `uid:gid` pair.
pub(crate) fn process_any(
    user: &str,
    workdir: &str,
    argv: &[String],
    env: &[String],
) -> Result<Any, EngineError> {
    let (uid, gid) = parse_user(user)?;
    let env = default_env(env);
    let process = process_value(uid, gid, workdir, argv, &env);
    Ok(any(PROCESS_TYPE_URL, &process))
}

fn any(type_url: &str, value: &Value) -> Any {
    Any {
        type_url: type_url.to_string(),
        value: value.to_string().into_bytes(),
    }
}

fn process_value(uid: u32, gid: u32, cwd: &str, argv: &[String], env: &[String]) -> Value {
    json!({
        "terminal": false,
        "user": {"uid": uid, "gid": gid, "additionalGids": []},
        "args": argv,
        "env": env,
        "cwd": cwd,
        "capabilities": {
            "bounding": [],
            "effective": [],
            "inheritable": [],
            "permitted": [],
            "ambient": [],
        },
        "noNewPrivileges": true,
    })
}

/// Explicit overrides first, then the fixed defaults they did not replace.
fn default_env(explicit: &[String]) -> Vec<String> {
    let mut env = explicit.to_vec();
    if !env.iter().any(|entry| entry.starts_with("PATH=")) {
        env.push(format!("PATH={DEFAULT_PATH}"));
    }
    if !env.iter().any(|entry| entry.starts_with("LANG=")) {
        env.push("LANG=C.UTF-8".to_string());
    }
    env
}

/// Parses the lifecycle-owned `uid:gid` user string into numeric ids.
pub(crate) fn parse_user(user: &str) -> Result<(u32, u32), EngineError> {
    let invalid = || {
        file_error(
            EngineErrorKind::Other,
            format!("guest user {user:?} is not a numeric uid:gid pair"),
        )
    };
    let (uid, gid) = user.split_once(':').ok_or_else(invalid)?;
    Ok((
        uid.parse().map_err(|_| invalid())?,
        gid.parse().map_err(|_| invalid())?,
    ))
}

fn system_mounts(spec: &CommonSpec) -> Vec<Value> {
    vec![
        json!({
            "destination": "/proc",
            "type": "proc",
            "source": "proc",
            "options": ["nosuid", "noexec", "nodev"],
        }),
        json!({
            "destination": "/dev",
            "type": "tmpfs",
            "source": "tmpfs",
            "options": ["nosuid", "strictatime", "mode=755", "size=65536k"],
        }),
        json!({
            "destination": "/dev/pts",
            "type": "devpts",
            "source": "devpts",
            "options": [
                "nosuid",
                "noexec",
                "newinstance",
                "ptmxmode=0666",
                "mode=0620",
                "gid=5",
            ],
        }),
        json!({
            "destination": "/dev/shm",
            "type": "tmpfs",
            "source": "shm",
            "options": ["nosuid", "noexec", "nodev", "mode=1777", "size=65536k"],
        }),
        json!({
            "destination": "/sys",
            "type": "sysfs",
            "source": "sysfs",
            "options": ["nosuid", "noexec", "nodev", "ro"],
        }),
        json!({
            "destination": "/sys/fs/cgroup",
            "type": "cgroup",
            "source": "cgroup",
            "options": ["nosuid", "noexec", "nodev", "relatime", "ro"],
        }),
        json!({
            "destination": spec.workspace_path,
            "type": "tmpfs",
            "source": "tmpfs",
            "options": [
                "rw",
                "nosuid",
                "nodev",
                format!("uid={}", spec.guest_uid),
                format!("gid={}", spec.guest_gid),
                format!("mode={WORKSPACE_MODE:o}"),
                format!("size={}", spec.workspace_bytes),
            ],
        }),
    ]
}

fn devices(uid: u32, gid: u32) -> Vec<Value> {
    [
        ("/dev/null", 1, 3, 0o666),
        ("/dev/zero", 1, 5, 0o666),
        ("/dev/full", 1, 7, 0o666),
        ("/dev/random", 1, 8, 0o666),
        ("/dev/urandom", 1, 9, 0o666),
        ("/dev/tty", 5, 0, 0o666),
        ("/dev/ptmx", 5, 2, 0o666),
    ]
    .into_iter()
    .map(|(path, major, minor, mode)| {
        json!({
            "path": path,
            "type": "c",
            "major": major,
            "minor": minor,
            "fileMode": mode,
            "uid": uid,
            "gid": gid,
        })
    })
    .collect()
}

/// Device cgroup policy: deny by default, then allow the standard pseudo
/// devices every Linux container needs. Without the explicit allow rules the
/// cgroup v2 eBPF device controller rejects reopening `/dev/null` at start.
fn device_rules() -> Vec<Value> {
    let mut rules = vec![json!({"allow": false, "access": "rwm"})];
    let allowed = [
        ("c", Some(1), Some(3)),
        ("c", Some(1), Some(5)),
        ("c", Some(1), Some(7)),
        ("c", Some(1), Some(8)),
        ("c", Some(1), Some(9)),
        ("c", Some(5), Some(0)),
        ("c", Some(5), Some(1)),
        ("c", Some(5), Some(2)),
        ("c", Some(136), None),
        ("c", Some(10), Some(200)),
        ("c", Some(10), Some(229)),
        ("b", Some(7), None),
    ];
    for (kind, major, minor) in allowed {
        let mut rule = serde_json::Map::new();
        rule.insert("allow".to_string(), json!(true));
        rule.insert("type".to_string(), json!(kind));
        if let Some(major) = major {
            rule.insert("major".to_string(), json!(major));
        }
        if let Some(minor) = minor {
            rule.insert("minor".to_string(), json!(minor));
        }
        rule.insert("access".to_string(), json!("rwm"));
        rules.push(Value::Object(rule));
    }
    rules
}

/// Reads back the spec this adapter asked containerd to store.
///
/// Only structural facts are reported. containerd exposes no effective cgroup
/// controller values, so memory, CPU and PID numbers are left absent. Values
/// here are the stored OCI spec (what runc receives), not a kernel observation.
pub(crate) fn report_from_any(runtime: Option<String>, spec: Option<&Any>) -> HostReport {
    let mut report = HostReport {
        runtime,
        ..HostReport::default()
    };
    let Some(any) = spec else {
        return report;
    };
    let Ok(value) = serde_json::from_slice::<Value>(&any.value) else {
        return report;
    };
    report.readonly_root = value.pointer("/root/readonly").and_then(Value::as_bool);
    report.cap_drop =
        empty_capabilities(value.pointer("/process/capabilities")).then(|| vec!["ALL".to_string()]);
    report.security_opt = match value
        .pointer("/process/noNewPrivileges")
        .and_then(Value::as_bool)
    {
        Some(true) => Some(vec!["no-new-privileges".to_string()]),
        _ => None,
    };
    let namespaces = value
        .pointer("/linux/namespaces")
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| item.get("type").and_then(Value::as_str))
                .collect::<Vec<_>>()
        });
    report.network_mode = namespaces.map(|items| {
        if items.contains(&"network") {
            "private"
        } else {
            "host"
        }
        .to_string()
    });
    if let Some(mounts) = value.get("mounts").and_then(Value::as_array) {
        report.bind_count = Some(
            mounts
                .iter()
                .filter(|mount| mount.get("type").and_then(Value::as_str) == Some("bind"))
                .count(),
        );
        let mut tmpfs = BTreeMap::new();
        for mount in mounts {
            if mount.get("type").and_then(Value::as_str) != Some("tmpfs") {
                continue;
            }
            let Some(destination) = mount.get("destination").and_then(Value::as_str) else {
                continue;
            };
            // `/dev` and `/dev/shm` are runtime plumbing, not the workspace.
            if destination == "/dev" || destination == "/dev/shm" {
                continue;
            }
            let options = mount
                .get("options")
                .and_then(Value::as_array)
                .map(|options| {
                    options
                        .iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(",")
                })
                .unwrap_or_default();
            tmpfs.insert(destination.to_string(), options);
        }
        report.tmpfs = Some(tmpfs);
    }
    report
}

fn empty_capabilities(capabilities: Option<&Value>) -> bool {
    let Some(object) = capabilities.and_then(Value::as_object) else {
        return false;
    };
    [
        "bounding",
        "effective",
        "inheritable",
        "permitted",
        "ambient",
    ]
    .iter()
    .all(|key| {
        object
            .get(*key)
            .and_then(Value::as_array)
            .is_none_or(|items| items.is_empty())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> CommonSpec {
        CommonSpec {
            memory: 64 * 1024 * 1024,
            nano_cpus: 100_000_000,
            pids: 32,
            workspace_path: "/workspace".to_string(),
            workspace_bytes: 8 * 1024 * 1024,
            guest_uid: 65_534,
            guest_gid: 65_534,
        }
    }

    #[test]
    fn spec_is_read_only_capability_free_and_workspace_bounded() {
        let any = container_any(&spec(), "circus");
        assert_eq!(any.type_url, SPEC_TYPE_URL);
        let value: Value = serde_json::from_slice(&any.value).unwrap();
        assert_eq!(value.pointer("/root/readonly"), Some(&Value::Bool(true)));
        assert_eq!(
            value.pointer("/process/noNewPrivileges"),
            Some(&Value::Bool(true))
        );
        assert_eq!(
            value.pointer("/linux/resources/pids/limit"),
            Some(&json!(32))
        );
        assert_eq!(
            value.pointer("/linux/resources/cpu/quota"),
            Some(&json!(10_000))
        );
        let mounts = value.get("mounts").and_then(Value::as_array).unwrap();
        let workspace = mounts
            .iter()
            .find(|mount| mount.get("destination").and_then(Value::as_str) == Some("/workspace"))
            .unwrap();
        let options = workspace.get("options").unwrap().as_array().unwrap();
        for required in [
            "rw",
            "nosuid",
            "nodev",
            "uid=65534",
            "gid=65534",
            "mode=755",
        ] {
            assert!(
                options
                    .iter()
                    .any(|option| option.as_str() == Some(required)),
                "missing {required}"
            );
        }
        assert!(
            options
                .iter()
                .any(|option| option.as_str() == Some("size=8388608"))
        );
    }

    #[test]
    fn report_reads_back_structural_facts_only() {
        let any = container_any(&spec(), "circus");
        let report = report_from_any(Some("io.containerd.runc.v2".to_string()), Some(&any));
        assert_eq!(report.runtime.as_deref(), Some("io.containerd.runc.v2"));
        assert_eq!(report.readonly_root, Some(true));
        assert_eq!(report.network_mode.as_deref(), Some("private"));
        assert_eq!(report.cap_drop, Some(vec!["ALL".to_string()]));
        assert_eq!(
            report.security_opt,
            Some(vec!["no-new-privileges".to_string()])
        );
        assert_eq!(report.bind_count, Some(0));
        assert_eq!(report.memory_bytes, None);
        assert_eq!(report.nano_cpus, None);
        assert_eq!(report.pids_limit, None);
        let tmpfs = report.tmpfs.unwrap();
        assert_eq!(tmpfs.len(), 1);
        assert!(tmpfs.contains_key("/workspace"));
    }

    #[test]
    fn process_spec_carries_explicit_env_and_numeric_user() {
        let any = process_any(
            "65534:65534",
            "/workspace",
            &["python3".to_string()],
            &["A=1".to_string()],
        )
        .unwrap();
        let value: Value = serde_json::from_slice(&any.value).unwrap();
        let env = value.get("env").unwrap().as_array().unwrap();
        assert_eq!(env[0], json!("A=1"));
        assert!(
            env.iter()
                .any(|entry| entry.as_str().is_some_and(|v| v.starts_with("PATH=")))
        );
        assert_eq!(value.pointer("/user/uid"), Some(&json!(65_534)));
    }

    #[test]
    fn non_numeric_user_is_rejected() {
        assert!(process_any("nobody", "/workspace", &["true".to_string()], &[]).is_err());
    }
}
