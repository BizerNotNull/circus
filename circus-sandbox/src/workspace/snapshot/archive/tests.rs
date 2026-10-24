use std::io::{Seek, SeekFrom, Write};

use super::*;

fn record(file: &mut std::fs::File, path: &[u8], kind: u8, body: &[u8], declared: Option<u64>) {
    let mut header = tar::Header::new_ustar();
    header.set_mode(0o600);
    header.set_uid(1000);
    header.set_gid(1000);
    header.set_size(declared.unwrap_or(body.len() as u64));
    header.set_entry_type(tar::EntryType::new(kind));
    header.as_mut_bytes()[..100].fill(0);
    header.as_mut_bytes()[..path.len()].copy_from_slice(path);
    header.set_cksum();
    file.write_all(header.as_bytes()).unwrap();
    file.write_all(body).unwrap();
    let padding = (512 - body.len() % 512) % 512;
    file.write_all(&vec![0; padding]).unwrap();
}

fn archive(items: &[(&[u8], u8, &[u8])]) -> std::fs::File {
    let mut file = tempfile::tempfile().unwrap();
    for &(path, kind, body) in items {
        record(&mut file, path, kind, body, None);
    }
    file.write_all(&[0; 1024]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    file
}

fn capture(
    file: std::fs::File,
    bytes: usize,
    entries: usize,
) -> Result<CodeSnapshot, SnapshotError> {
    from_workspace_archive(
        file,
        "workspace",
        64,
        SnapshotBudget::new(bytes, entries).unwrap(),
    )
}

#[test]
fn real_tar_round_trip_including_empty_directory() {
    let file = archive(&[
        (b"workspace/", b'5', b""),
        (b"workspace/empty/", b'5', b""),
        (b"workspace/note", b'0', b"hello"),
    ]);
    let snapshot = capture(file, 5, 2).unwrap();
    assert_eq!(snapshot.total_bytes(), 5);
    assert_eq!(snapshot.entry_count(), 2);
    let mut workspace = snapshot.restore().unwrap();
    assert_eq!(workspace.read_file("note").unwrap(), b"hello");
    assert_eq!(
        workspace
            .capture_code_snapshot(SnapshotBudget::new(5, 2).unwrap())
            .unwrap()
            .entry_count(),
        2
    );
    workspace.cleanup().unwrap();
}

#[test]
fn root_is_required_but_not_charged() {
    assert_eq!(
        capture(archive(&[(b"workspace/", b'5', b"")]), 1, 1)
            .unwrap()
            .entry_count(),
        0
    );
    assert!(capture(archive(&[(b"other/", b'5', b"")]), 1, 1).is_err());
    assert!(capture(archive(&[(b"workspace/file", b'0', b"")]), 1, 1).is_err());
    assert!(capture(archive(&[(b"workspace", b'0', b"")]), 1, 1).is_err());
}

#[test]
fn rejects_escape_and_nonportable_raw_paths() {
    for path in [
        b"/workspace/a".as_slice(),
        b"workspace/../a",
        b"workspace/./a",
        b"workspace//a",
        b"workspace/C:drive",
        b"workspace/a\\b",
        b"workspace/\xff",
    ] {
        assert!(
            capture(
                archive(&[(b"workspace/", b'5', b""), (path, b'0', b"x")]),
                64,
                8
            )
            .is_err(),
            "accepted {path:?}"
        );
    }
}

#[test]
fn rejects_links_special_nodes_and_extension_metadata() {
    for kind in [
        b'1', b'2', b'3', b'4', b'6', b'7', b'x', b'g', b'L', b'K', b'S',
    ] {
        assert!(
            capture(
                archive(&[(b"workspace/", b'5', b""), (b"workspace/node", kind, b"")]),
                64,
                8
            )
            .is_err(),
            "accepted {kind}"
        );
    }
}

#[test]
fn rejects_duplicate_and_directory_payload() {
    assert!(
        capture(
            archive(&[
                (b"workspace/", b'5', b""),
                (b"workspace/a", b'0', b"a"),
                (b"workspace/a", b'0', b"b")
            ]),
            64,
            8
        )
        .is_err()
    );
    assert!(capture(archive(&[(b"workspace/", b'5', b"x")]), 64, 8).is_err());
}

#[test]
fn payload_and_entry_caps_fail_before_large_entry_reads() {
    let file = archive(&[(b"workspace/", b'5', b""), (b"workspace/a", b'0', b"abc")]);
    assert!(matches!(
        capture(file, 2, 8),
        Err(SnapshotError::ByteLimit {
            limit: 2,
            actual: 3
        })
    ));
    let file = archive(&[
        (b"workspace/", b'5', b""),
        (b"workspace/a", b'0', b""),
        (b"workspace/b", b'0', b""),
    ]);
    assert!(matches!(
        capture(file, 64, 1),
        Err(SnapshotError::EntryLimit {
            limit: 1,
            actual: 2
        })
    ));
    let mut file = tempfile::tempfile().unwrap();
    record(&mut file, b"workspace/", b'5', b"", None);
    record(&mut file, b"workspace/large", b'0', b"", Some(1 << 40));
    file.seek(SeekFrom::Start(0)).unwrap();
    assert!(matches!(
        capture(file, 64, 8),
        Err(SnapshotError::Workspace(
            super::super::WorkspaceError::LimitExceeded { limit: 64, .. }
        ))
    ));
}

#[test]
fn truncated_payload_fails_closed() {
    let mut file = tempfile::tempfile().unwrap();
    record(&mut file, b"workspace/", b'5', b"", None);
    record(&mut file, b"workspace/file", b'0', b"", Some(10));
    file.seek(SeekFrom::Start(0)).unwrap();
    assert!(capture(file, 64, 8).is_err());
}

#[test]
fn restoration_uses_target_limits_not_source_limits() {
    let snapshot = capture(
        archive(&[
            (b"workspace/", b'5', b""),
            (b"workspace/a", b'0', b"abc"),
            (b"workspace/b", b'0', b"def"),
        ]),
        6,
        2,
    )
    .unwrap();
    assert!(matches!(
        snapshot.restore_with_limits(2, 64),
        Err(SnapshotError::Workspace(
            super::super::WorkspaceError::LimitExceeded {
                limit: 2,
                actual: 3
            }
        ))
    ));
    assert!(matches!(
        snapshot.restore_with_limits(64, 5),
        Err(SnapshotError::ByteLimit {
            limit: 5,
            actual: 6
        })
    ));
    snapshot
        .restore_with_limits(3, 6)
        .unwrap()
        .cleanup()
        .unwrap();
}

#[test]
fn native_gnu_long_path_round_trip_and_parent_budget() {
    let component = "long".repeat(30);
    let directory = format!("workspace/{component}");
    let filename = format!("{directory}/note");
    let mut file = tempfile::tempfile().unwrap();
    record(&mut file, b"workspace/", b'5', b"", None);
    let mut long_directory = directory.as_bytes().to_vec();
    long_directory.push(0);
    record(&mut file, b"././@LongLink", b'L', &long_directory, None);
    record(&mut file, b"placeholder-dir", b'5', b"", None);
    let mut long_file = filename.as_bytes().to_vec();
    long_file.push(0);
    record(&mut file, b"././@LongLink", b'L', &long_file, None);
    record(&mut file, b"placeholder-file", b'0', b"ok", None);
    file.write_all(&[0; 1024]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let snapshot = capture(file, 2, 2).unwrap();
    let mut restored = snapshot.restore().unwrap();
    assert_eq!(
        restored.read_file(format!("{component}/note")).unwrap(),
        b"ok"
    );
    restored.cleanup().unwrap();
    assert!(
        capture(
            archive(&[
                (b"workspace/", b'5', b""),
                (b"workspace/a/b/file", b'0', b"x")
            ]),
            1,
            1
        )
        .is_err()
    );
}

#[test]
fn oversized_metadata_fails_before_allocating_declared_payload() {
    let mut file = tempfile::tempfile().unwrap();
    record(&mut file, b"metadata", b'L', b"", Some(1 << 40));
    file.seek(SeekFrom::Start(0)).unwrap();
    assert!(capture(file, 1, 1).is_err());
}

#[test]
fn pax_path_extensions_round_trip() {
    let path = format!("workspace/{}", "p".repeat(120));
    let mut file = tempfile::tempfile().unwrap();
    {
        let mut builder = tar::Builder::new(&mut file);
        let mut root = tar::Header::new_ustar();
        root.set_size(0);
        root.set_mode(0o700);
        root.set_entry_type(tar::EntryType::Directory);
        root.set_cksum();
        builder
            .append_data(&mut root, "workspace", &[][..])
            .unwrap();
        builder
            .append_pax_extensions([("path", path.as_bytes())])
            .unwrap();
        let mut header = tar::Header::new_ustar();
        header.set_size(1);
        header.set_mode(0o600);
        header.set_cksum();
        builder
            .append_data(&mut header, "placeholder", &b"x"[..])
            .unwrap();
        builder.finish().unwrap();
    }
    file.seek(SeekFrom::Start(0)).unwrap();
    let snapshot = capture(file, 1, 1).unwrap();
    let mut restored = snapshot.restore().unwrap();
    assert_eq!(restored.read_file(&path[10..]).unwrap(), b"x");
    restored.cleanup().unwrap();
}

#[test]
fn many_native_long_paths_exceed_old_aggregate_metadata_cap() {
    let mut file = tempfile::tempfile().unwrap();
    record(&mut file, b"workspace/", b'5', b"", None);
    let mut metadata_bytes = 0usize;
    for index in 0..600 {
        let path = format!("workspace/{index:04}-{}", "p".repeat(120));
        let mut long_path = path.into_bytes();
        long_path.push(0);
        metadata_bytes += long_path.len();
        record(&mut file, b"././@LongLink", b'L', &long_path, None);
        record(&mut file, b"placeholder", b'0', b"x", None);
    }
    assert!(metadata_bytes > 64 * 1024);
    file.write_all(&[0; 1024]).unwrap();
    file.seek(SeekFrom::Start(0)).unwrap();
    let snapshot = capture(file, 600, 600).unwrap();
    assert_eq!(snapshot.entry_count(), 600);
    assert_eq!(snapshot.total_bytes(), 600);
    let mut restored = snapshot.restore().unwrap();
    assert_eq!(
        restored
            .read_file(format!("0599-{}", "p".repeat(120)))
            .unwrap(),
        b"x"
    );
    restored.cleanup().unwrap();
}

#[test]
fn repeated_local_extensions_and_global_state_are_rejected() {
    let file = archive(&[
        (b"././@LongLink", b'L', b"workspace/a\0"),
        (b"././@LongLink", b'L', b"workspace/b\0"),
        (b"placeholder", b'0', b""),
    ]);
    assert!(capture(file, 1, 1).is_err());
    assert!(capture(archive(&[(b"global", b'g', b"")]), 1, 1).is_err());
}
