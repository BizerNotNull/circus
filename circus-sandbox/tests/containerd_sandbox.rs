//! Real containerd lifecycle, exec, file, pause/resume, and ownership suites.

mod containerd_support;

use std::time::Duration;

use circus_sandbox::{Sandbox, SandboxError, SandboxPhase, WorkspaceError};
use containerd_support as support;

#[tokio::test]
#[ignore = "requires an explicit real containerd daemon"]
async fn containerd_linux_runc_lifecycle() -> support::Result {
    support::enabled();
    let sandbox = support::create().await?;
    let id = sandbox.status().await?.instance_id.ok_or("missing id")?;

    sandbox.write_file("hello.txt", b"hello").await?;
    sandbox.create_dir_all("nested").await?;
    sandbox.write_file("nested/data.txt", b"staged").await?;
    sandbox.start().await?;

    let status = sandbox.status().await?;
    assert_eq!(status.phase, SandboxPhase::Running);
    assert_eq!(status.instance_id.as_deref(), Some(id.as_str()));
    let observed = status
        .enforcement
        .as_ref()
        .expect("running sandbox exposes inspected spec evidence");
    assert_eq!(observed.runtime.as_deref(), Some("io.containerd.runc.v2"));
    assert_eq!(observed.readonly_root, Some(true));
    assert_eq!(observed.network_mode.as_deref(), Some("private"));
    assert_eq!(observed.cap_drop, Some(vec!["ALL".to_string()]));
    assert!(
        observed
            .security_opt
            .as_ref()
            .is_some_and(|options| options.iter().any(|option| option == "no-new-privileges"))
    );
    assert_eq!(observed.bind_count, Some(0));
    let tmpfs = observed
        .tmpfs_workspace
        .as_deref()
        .expect("workspace tmpfs");
    assert!(tmpfs.contains("nosuid"), "{tmpfs}");
    assert!(tmpfs.contains("uid=65534"), "{tmpfs}");
    // containerd exposes no effective cgroup values, so these stay absent.
    assert_eq!(observed.memory_bytes, None);
    assert_eq!(observed.nano_cpus, None);
    assert_eq!(observed.pids_limit, None);

    let output = sandbox
        .exec(support::python(&["-c", "print('guest-ok')"]))
        .await?;
    assert_eq!(output.exit_code, 0);
    assert_eq!(output.stdout, b"guest-ok\n");
    assert!(output.stderr.is_empty());

    assert_eq!(sandbox.read_file("hello.txt").await?, b"hello");
    assert_eq!(sandbox.read_file("nested/data.txt").await?, b"staged");

    sandbox.write_file("live.txt", b"running write").await?;
    assert_eq!(sandbox.read_file("live.txt").await?, b"running write");
    sandbox.create_dir_all("created-while-running").await?;
    sandbox
        .write_file("created-while-running/file.txt", b"nested live")
        .await?;
    assert_eq!(
        sandbox.read_file("created-while-running/file.txt").await?,
        b"nested live"
    );

    sandbox.remove_file("live.txt").await?;
    match sandbox.read_file("live.txt").await {
        Err(SandboxError::Workspace(WorkspaceError::Io(error))) => {
            assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
        }
        other => panic!("expected a not-found workspace error, got {other:?}"),
    }

    sandbox.pause().await?;
    assert_eq!(sandbox.status().await?.phase, SandboxPhase::Paused);
    assert!(matches!(
        sandbox.exec(support::python(&["-c", "print(1)"])).await,
        Err(SandboxError::InvalidPhase { .. })
    ));
    sandbox.resume().await?;
    assert_eq!(sandbox.status().await?.phase, SandboxPhase::Running);
    assert_eq!(
        sandbox
            .exec(support::python(&["-c", "print('resumed')"]))
            .await?
            .stdout,
        b"resumed\n"
    );

    support::destroy_and_assert(&sandbox, &id).await?;
    let after = sandbox.status().await?;
    assert_eq!(after.phase, SandboxPhase::Destroyed);
    assert_eq!(after.instance_id, None);
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real containerd daemon"]
async fn containerd_exec_exit_codes_and_shared_output_budget() -> support::Result {
    support::enabled();
    let sandbox = support::create().await?;
    let id = sandbox.status().await?.instance_id.ok_or("missing id")?;
    sandbox.start().await?;

    let mut failing = support::python(&[
        "-c",
        "import sys; sys.stdout.write('out'); sys.stderr.write('err'); sys.exit(7)",
    ]);
    failing.output_limit_bytes = 16;
    let output = sandbox.exec(failing).await?;
    assert_eq!(output.exit_code, 7);
    assert_eq!(output.stdout, b"out");
    assert_eq!(output.stderr, b"err");

    // Exactly-at-limit output is accepted.
    let mut exact = support::python(&["-c", "import sys; sys.stdout.write('12345678')"]);
    exact.output_limit_bytes = 8;
    assert_eq!(sandbox.exec(exact).await?.stdout, b"12345678");

    // The first byte over the shared stdout+stderr budget destroys the instance.
    let mut over = support::python(&["-c", "import sys; sys.stdout.write('123456789')"]);
    over.output_limit_bytes = 8;
    match sandbox.exec(over).await {
        Err(SandboxError::OutputLimit {
            limit, ownership, ..
        }) => {
            assert_eq!(limit, 8);
            assert_eq!(ownership, circus_sandbox::Ownership::ConfirmedAbsent);
        }
        other => panic!("expected a typed output-limit error, got {other:?}"),
    }
    assert_eq!(sandbox.status().await?.phase, SandboxPhase::Destroyed);
    support::assert_absent(&support::namespace(), &id)?;
    assert!(sandbox.destroy().await.is_ok());
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real containerd daemon"]
async fn containerd_missing_image_is_typed_and_allocates_nothing() -> support::Result {
    support::enabled();
    let namespace = format!("circus-missing-{}", std::process::id());
    support::create_namespace(&namespace)?;
    let result = Sandbox::create(
        support::backend_in(&namespace, "docker.io/library/circus-does-not-exist:1"),
        support::config()?,
    )
    .await;
    match result {
        Err(SandboxError::BackendImageUnavailable { image }) => {
            assert_eq!(image, "docker.io/library/circus-does-not-exist:1");
        }
        Err(other) => panic!("expected a typed missing-image error, got {other:?}"),
        Ok(_) => panic!("missing image unexpectedly created a sandbox"),
    }
    // Prepare runs before an instance name or snapshot exists.
    support::assert_namespace_empty(&namespace)?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real containerd daemon"]
async fn containerd_second_concurrent_create_succeeds() -> support::Result {
    support::enabled();
    let (first, second) = tokio::join!(support::create(), support::create());
    let first = first?;
    let second = second?;
    let first_id = first
        .status()
        .await?
        .instance_id
        .ok_or("missing first id")?;
    let second_id = second
        .status()
        .await?
        .instance_id
        .ok_or("missing second id")?;
    assert_ne!(first_id, second_id);

    first.start().await?;
    second.start().await?;
    let (one, two) = tokio::join!(
        first.exec(support::python(&["-c", "print('one')"])),
        second.exec(support::python(&["-c", "print('two')"])),
    );
    assert_eq!(one?.stdout, b"one\n");
    assert_eq!(two?.stdout, b"two\n");

    support::destroy_and_assert(&first, &first_id).await?;
    support::destroy_and_assert(&second, &second_id).await?;
    Ok(())
}

#[tokio::test]
#[ignore = "requires an explicit real containerd daemon"]
async fn containerd_status_is_read_only_under_a_deadline() -> support::Result {
    support::enabled();
    let sandbox = support::create().await?;
    let id = sandbox.status().await?.instance_id.ok_or("missing id")?;
    sandbox.start().await?;
    // Repeated observation never changes the phase or destroys the instance.
    for _ in 0..3 {
        let status = tokio::time::timeout(Duration::from_secs(5), sandbox.status())
            .await
            .expect("status returned")?;
        assert_eq!(status.phase, SandboxPhase::Running);
        assert_eq!(status.instance_id.as_deref(), Some(id.as_str()));
    }
    support::destroy_and_assert(&sandbox, &id).await
}
