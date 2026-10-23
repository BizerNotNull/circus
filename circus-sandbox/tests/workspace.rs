//! Public `Workspace` behavior against a real temporary directory.
//!
//! These tests use only the crate's public API. They do not read source
//! text, copy private fields, or expose the workspace root.

use std::io::ErrorKind;
use std::path::Path;

use circus_sandbox::{PathRejectReason, Workspace, WorkspaceError};

fn open(limit: usize) -> Workspace {
    Workspace::new(limit).unwrap_or_else(|err| panic!("Workspace::new({limit}) failed: {err}"))
}

fn assert_rejected(result: Result<(), WorkspaceError>, reason: PathRejectReason, raw: &str) {
    match result {
        Err(WorkspaceError::PathRejected {
            path,
            reason: actual,
        }) => {
            assert_eq!(actual, reason, "reason for {raw}");
            assert_eq!(path, Path::new(raw), "preserved path for {raw}");
        }
        other => panic!("expected PathRejected {reason:?} for {raw}, got {other:?}"),
    }
}

fn assert_all_ops_reject(ws: &Workspace, raw: &str, reason: PathRejectReason) {
    assert_rejected(ws.write_file(raw, b"x"), reason, raw);
    assert_rejected(ws.read_file(raw).map(|_| ()), reason, raw);
    assert_rejected(ws.create_dir_all(raw), reason, raw);
    assert_rejected(ws.remove_file(raw), reason, raw);
}

fn assert_io_kind(result: Result<(), WorkspaceError>, kind: ErrorKind) {
    match result {
        Err(WorkspaceError::Io(err)) => assert_eq!(err.kind(), kind, "{err}"),
        other => panic!("expected Io {kind:?}, got {other:?}"),
    }
}

fn assert_closed(result: Result<(), WorkspaceError>) {
    assert!(
        matches!(result, Err(WorkspaceError::Closed)),
        "expected Closed, got {result:?}"
    );
}

fn pid_name(label: &str) -> String {
    format!("circus-sandbox-{label}-{}", std::process::id())
}

/// Fail if `path` appeared, and remove it so a buggy build does not leave it behind.
fn assert_host_untouched(path: &Path, existed_before: bool) {
    let exists_now = path.exists();
    if !existed_before && exists_now {
        let _ = std::fs::remove_file(path);
        let _ = std::fs::remove_dir_all(path);
    }
    assert!(
        !exists_now || existed_before,
        "host path was created: {}",
        path.display()
    );
}

#[test]
fn byte_limit_boundaries_do_not_create_or_truncate() {
    assert!(matches!(
        Workspace::new(0),
        Err(WorkspaceError::InvalidLimit { max_file_bytes: 0 })
    ));
    assert!(matches!(
        Workspace::new(usize::MAX),
        Err(WorkspaceError::InvalidLimit {
            max_file_bytes: usize::MAX
        })
    ));

    // The limit is a cap, not a buffer to preallocate.
    let large = open(usize::MAX - 1);
    large
        .write_file("small.txt", b"ok")
        .expect("small write under a large cap");
    assert_eq!(large.read_file("small.txt").unwrap(), b"ok");

    let ws = open(4);
    ws.write_file("bound.txt", b"abcd").expect("exact limit");
    assert_eq!(ws.read_file("bound.txt").unwrap(), b"abcd");

    match ws.write_file("bound.txt", b"abcde") {
        Err(WorkspaceError::LimitExceeded {
            limit: 4,
            actual: 5,
        }) => {}
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
    assert_eq!(
        ws.read_file("bound.txt").unwrap(),
        b"abcd",
        "oversize write must not truncate an existing file"
    );

    match ws.write_file("missing.bin", b"abcde") {
        Err(WorkspaceError::LimitExceeded {
            limit: 4,
            actual: 5,
        }) => {}
        other => panic!("expected LimitExceeded, got {other:?}"),
    }
    assert_io_kind(ws.read_file("missing.bin").map(|_| ()), ErrorKind::NotFound);

    ws.write_file("empty.bin", b"").expect("empty file");
    assert_eq!(ws.read_file("empty.bin").unwrap(), b"");

    let one = open(1);
    one.write_file("one.bin", b"Z").unwrap();
    assert_eq!(one.read_file("one.bin").unwrap(), b"Z");
    assert!(matches!(
        one.write_file("one.bin", b"ZZ"),
        Err(WorkspaceError::LimitExceeded {
            limit: 1,
            actual: 2
        })
    ));
    assert_eq!(one.read_file("one.bin").unwrap(), b"Z");
}

#[test]
fn illegal_paths_are_rejected_on_every_operation() {
    let ws = open(32);
    let abs = format!("/{}", pid_name("abs"));
    let drive = format!("C:/{}", pid_name("drive"));
    let prefix = format!("//server/{}", pid_name("unc"));
    let parent = format!("foo/../../{}", pid_name("parent"));
    let slash = format!("dir\\{}", pid_name("slash"));

    let cases = [
        ("", PathRejectReason::Empty),
        (slash.as_str(), PathRejectReason::Backslash),
        (drive.as_str(), PathRejectReason::WindowsPrefix),
        ("C:relative", PathRejectReason::WindowsPrefix),
        (prefix.as_str(), PathRejectReason::WindowsPrefix),
        (abs.as_str(), PathRejectReason::Absolute),
        (parent.as_str(), PathRejectReason::ParentComponent),
        ("..", PathRejectReason::ParentComponent),
        ("foo/../bar", PathRejectReason::ParentComponent),
    ];
    for (raw, reason) in cases {
        assert_all_ops_reject(&ws, raw, reason);
    }

    // Only unique names we own. Never delete `..`, UNC, or a short relative path.
    let abs_path = Path::new(&abs);
    let abs_before = abs_path.exists();
    let abs_result = ws.write_file(abs_path, b"nope");
    assert_host_untouched(abs_path, abs_before);
    assert_rejected(abs_result, PathRejectReason::Absolute, &abs);

    let drive_path = Path::new(&drive);
    let drive_before = drive_path.exists();
    let drive_result = ws.write_file(drive_path, b"nope");
    assert_host_untouched(drive_path, drive_before);
    assert_rejected(drive_result, PathRejectReason::WindowsPrefix, &drive);

    let cwd_leak = std::env::current_dir()
        .expect("cwd")
        .join(pid_name("parent"));
    assert_host_untouched(&cwd_leak, cwd_leak.exists());

    let host_abs = std::env::temp_dir().join(pid_name("host-abs"));
    let host_before = host_abs.exists();
    let host_result = ws.write_file(&host_abs, b"nope");
    assert_host_untouched(&host_abs, host_before);
    match host_result {
        Err(WorkspaceError::PathRejected { path, reason }) => {
            assert_eq!(path, host_abs);
            assert!(
                matches!(
                    reason,
                    PathRejectReason::Absolute
                        | PathRejectReason::Backslash
                        | PathRejectReason::WindowsPrefix
                ),
                "host temp path must be rejected, got {reason:?}"
            );
        }
        other => panic!("expected host absolute path to be rejected, got {other:?}"),
    }
}

#[test]
fn nested_dirs_require_explicit_create_and_remove_only_files() {
    let ws = open(64);
    let cwd_name = pid_name("cwd-leak");
    let cwd_leak = std::env::current_dir().expect("cwd").join(&cwd_name);
    let cwd_before = cwd_leak.exists();

    ws.create_dir_all("proj/src").unwrap();
    ws.create_dir_all("proj/src").unwrap();
    ws.create_dir_all("proj/./src").unwrap();
    ws.write_file("proj/src/main.txt", b"data").unwrap();
    ws.write_file("proj/./src/main.txt", b"data").unwrap();
    assert_eq!(ws.read_file("proj/src/main.txt").unwrap(), b"data");
    assert_eq!(ws.read_file("proj/./src/main.txt").unwrap(), b"data");

    ws.write_file(&cwd_name, b"inside").unwrap();
    assert_eq!(ws.read_file(&cwd_name).unwrap(), b"inside");
    assert_host_untouched(&cwd_leak, cwd_before);

    assert_io_kind(
        ws.write_file("missing-parent/file.txt", b"x"),
        ErrorKind::NotFound,
    );
    assert_io_kind(
        ws.read_file("missing-parent/file.txt").map(|_| ()),
        ErrorKind::NotFound,
    );

    assert!(matches!(
        ws.read_file("proj"),
        Err(WorkspaceError::NotRegularFile { .. })
    ));
    assert!(matches!(
        ws.write_file("proj", b"no"),
        Err(WorkspaceError::NotRegularFile { .. })
    ));
    assert_eq!(ws.read_file("proj/src/main.txt").unwrap(), b"data");

    let remove_dir = ws.remove_file("proj");
    assert!(
        matches!(remove_dir, Err(WorkspaceError::Io(_))),
        "remove_file must not delete a directory, got {remove_dir:?}"
    );
    assert_eq!(ws.read_file("proj/src/main.txt").unwrap(), b"data");

    ws.write_file("proj/src/main.txt", b"next").unwrap();
    assert_eq!(ws.read_file("proj/src/main.txt").unwrap(), b"next");
    ws.remove_file("proj/src/main.txt").unwrap();
    assert_io_kind(
        ws.read_file("proj/src/main.txt").map(|_| ()),
        ErrorKind::NotFound,
    );
    assert_io_kind(ws.remove_file("proj/src/main.txt"), ErrorKind::NotFound);

    let other = open(64);
    other.write_file("same.txt", b"from-b").unwrap();
    ws.write_file("same.txt", b"from-a").unwrap();
    assert_eq!(ws.read_file("same.txt").unwrap(), b"from-a");
    assert_eq!(other.read_file("same.txt").unwrap(), b"from-b");
}

#[test]
fn cleanup_is_idempotent_and_then_closed() {
    let mut ws = open(16);
    ws.create_dir_all("keep").unwrap();
    ws.write_file("keep/a.txt", b"a").unwrap();
    ws.cleanup().expect("first cleanup");
    ws.cleanup()
        .expect("cleanup of an already removed workspace");

    assert_closed(ws.write_file("keep/a.txt", b"b"));
    assert_closed(ws.read_file("keep/a.txt").map(|_| ()));
    assert_closed(ws.create_dir_all("keep"));
    assert_closed(ws.remove_file("keep/a.txt"));
    assert_closed(ws.write_file("../escape", b"x"));
    assert_closed(ws.write_file("/circus-sandbox-closed", b"x"));
    assert_closed(ws.read_file("").map(|_| ()));
}
