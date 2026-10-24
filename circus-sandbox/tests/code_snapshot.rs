use circus_sandbox::{SnapshotBudget, SnapshotError, Workspace, WorkspaceError};

#[test]
fn captures_nested_files_and_empty_directories() {
    let mut source = Workspace::new(64).unwrap();
    source.create_dir_all("src/empty").unwrap();
    source.write_file("src/main.rs", b"fn main() {}\0").unwrap();
    let budget = SnapshotBudget::new(64, 8).unwrap();
    let snapshot = source.capture_code_snapshot(budget).unwrap();

    assert_eq!(snapshot.total_bytes(), 13);
    assert_eq!(snapshot.entry_count(), 3);
    let mut restored = snapshot.restore().unwrap();
    assert_eq!(
        restored.read_file("src/main.rs").unwrap(),
        b"fn main() {}\0"
    );
    restored.write_file("src/empty/new.txt", b"new").unwrap();
    restored.cleanup().unwrap();
    source.cleanup().unwrap();
}

#[test]
fn restore_survives_source_cleanup_and_isolation() {
    let mut source = Workspace::new(32).unwrap();
    source.create_dir_all("dir").unwrap();
    source.write_file("dir/value", b"original").unwrap();
    let snapshot = source
        .capture_code_snapshot(SnapshotBudget::new(32, 4).unwrap())
        .unwrap();
    source.cleanup().unwrap();

    let mut first = snapshot.restore().unwrap();
    first.write_file("dir/value", b"changed").unwrap();
    let second = snapshot.restore().unwrap();
    assert_eq!(second.read_file("dir/value").unwrap(), b"original");
    assert_eq!(first.read_file("dir/value").unwrap(), b"changed");
    first.cleanup().unwrap();
    let mut second = second;
    second.cleanup().unwrap();
}

#[test]
fn budget_boundaries_and_zero_values_are_enforced() {
    assert!(matches!(
        SnapshotBudget::new(0, 1),
        Err(SnapshotError::InvalidBudget { .. })
    ));
    assert!(matches!(
        SnapshotBudget::new(1, 0),
        Err(SnapshotError::InvalidBudget { .. })
    ));

    let mut source = Workspace::new(8).unwrap();
    source.create_dir_all("d").unwrap();
    source.write_file("d/a", b"1234").unwrap();
    assert!(
        source
            .capture_code_snapshot(SnapshotBudget::new(4, 2).unwrap())
            .is_ok()
    );
    assert!(matches!(
        source.capture_code_snapshot(SnapshotBudget::new(3, 2).unwrap()),
        Err(SnapshotError::ByteLimit {
            limit: 3,
            actual: 4
        })
    ));
    assert!(matches!(
        source.capture_code_snapshot(SnapshotBudget::new(4, 1).unwrap()),
        Err(SnapshotError::EntryLimit {
            limit: 1,
            actual: 2
        })
    ));
    source.cleanup().unwrap();
}

#[test]
fn closed_workspace_capture_is_rejected() {
    let mut workspace = Workspace::new(8).unwrap();
    workspace.cleanup().unwrap();
    assert!(matches!(
        workspace.capture_code_snapshot(SnapshotBudget::new(8, 8).unwrap()),
        Err(SnapshotError::Workspace(WorkspaceError::Closed))
    ));
}
