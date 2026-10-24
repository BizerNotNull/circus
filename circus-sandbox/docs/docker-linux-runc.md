# Linux Docker + runc slice

This checkout implements one real backend: an explicit local Docker Engine endpoint running a Linux image with the `runc` runtime. The SDK does not pull images and does not expose bollard types. Callers provide the image, endpoint, resource limits, isolation policy, and output bounds; the owner label is generated internally per sandbox.

The adapter sets memory and swap, milli-CPU to nano-CPU, pids, a bounded tmpfs workspace, read-only root, `none` networking, `no-new-privileges`, and `ALL` capability drop. It creates no host binds, volumes, credentials, or ports. Workspace files are staged from the existing capability-scoped `Workspace`. The guest file transport requires Python 3 and rejects symlinks during running-phase file operations.

`CommandRequest` uses argv form, bounded stdout and stderr, and a required timeout. Timeout, output overflow, and cancellation destroy the entire owned container so descendants cannot survive. Dropping the create future retains the generated name and owner label and recovers only a matching container. Operation cleanup errors retain ownership in the existing sandbox handle; retry `destroy` is required. Failed creation instead exposes the background cleanup receipt described below. Start and lifecycle operations also use the configured I/O timeout and may destroy the container when the engine call cannot be accepted safely.

The requested PID limit is enforced as supplied; the adapter does not silently increase it. Its budget includes the sandbox init, supervisor, commands, and any runtime/helper tasks charged to the container's PID cgroup, including temporary OCI exec startup tasks. Configuration validation requires at least 3 PIDs as a structural init/supervisor/command floor, not a guarantee that arbitrary commands or guest file operations can launch under every Docker/runc version. A low limit can therefore pass validation yet cause Docker to reject an exec. Such failures remain subject to the existing lifecycle safety cleanup described above; callers must handle reported errors and retained ownership.

The Docker integration test fixture requests 32 PIDs to leave room for temporary OCI startup tasks while exercising commands and guest file transport. This is an explicit test budget, not a product minimum or a universal launch guarantee. Applications must choose a PID budget that accommodates their commands and runtime's helper-task requirements.

Invalid command inputs are distinct from path failures: `SandboxError::Command(CommandError::EmptyArgv)`, `CommandError::InvalidTimeout { timeout }`, and `CommandError::InvalidOutputLimit { output_limit_bytes }` reject empty argv, zero timeout, and zero output bound before queueing. They preserve the phase, container identity, and staged files. A request above the configured output bound is separately rejected as `SandboxError::OutputLimit` with `Ownership::StillPresent`; it does not execute or destroy the container. Actual command output overflow still destroys it.

`start` counts staged file payload bytes before starting the container. `SandboxError::Staging(StagingError::LimitExceeded { limit, actual })` reports the configured workspace byte bound and the first detected sum above it: `actual` is a lower bound on the complete staged payload total, not a claimed exact total. Equality is permitted by this check. Defensive `StagingError::SizeOverflow { accumulated, next, limit }` retains the operands and bound if byte counting cannot fit `u64`. These are resource errors, not `RelativePath`. Both nested error types are exported through `api` and the crate root and retained as the standard error source; callers can match their variants rather than parse text.

A staging validation failure leaves the sandbox `Created`, retains its container identity and files, and allows removing or shrinking staged files before retrying `start`, or explicitly calling `destroy`. This payload check is not a host disk quota or a guarantee that tmpfs has room: filesystem metadata and allocation overhead can still cause a later upload to fail under the separately enforced tmpfs bound.

The ignored integration test requires `CIRCUS_DOCKER_SANDBOX=1` and `CIRCUS_DOCKER_ENDPOINT`; it must run against a real Linux Docker Engine with cgroup v2. `CIRCUS_DOCKER_IMAGE` optionally overrides the default `python:3.12-slim` image and must refer to a local Linux image containing Python 3. The example is `docker_sandbox` and also requires an endpoint. Run it with `cargo test --locked --test docker_sandbox -- --ignored`.

## Failed creation and cleanup receipts

`DockerSandbox::create` can return `SandboxError::CreationCleanupRequired { container_id, source, cleanup }` when creation fails without returning a sandbox handle. The boxed `source` preserves the original `SandboxError` and its standard error chain. `cleanup` is a cloneable, debug-printable `CleanupReceipt`, exported from both `api` and the crate root. This differs from operation-level `CleanupRequired`, where the caller already owns a sandbox on which it can retry `destroy`.

The detached lifecycle worker retains both container identity and staging, and retries real cleanup with backoff from 100 ms to 30 seconds. Every retry updates the receipt. Dropping the create future, ready receiver, receipt, or a pending `wait()` does not stop that worker. A single not-found response while creation is unresolved is still not proof of absence: a late container could materialize. No receipt exposes the ownership nonce or permits adopting an arbitrary container ID.

`cleanup.status()` returns the latest actual cleanup report. `cleanup.wait().await` returns `Ok(report)` only after the worker reaches `Destroyed`, which requires both confirmed container removal/absence and successful staging cleanup. Do not refill pool capacity based merely on container absence or receipt delivery. Concurrent cloned receipts and cancelled/restarted waits are supported. If the worker disappears without confirmed completion, waiting returns `SandboxError::WorkerGone`; it must not be treated as safe capacity. Cleanup can remain pending indefinitely when the engine is unavailable or creation remains unresolved. These guarantees require the process and Tokio runtime to remain alive.

The same worker-owned progress channel is initialized before container creation and also tracks ordinary destruction and internal revocation. Before any cleanup attempt its report is conservatively `Unknown`. Internal pool revocation cancels the sandbox shutdown token, interrupting already-running operations, and returns a receipt without queueing behind those operations. The receipt confirms completion only after container and staging cleanup, including any retries.

```rust,no_run
use circus_sandbox::{DockerSandbox, SandboxConfig, SandboxError};

async fn create_with_cleanup(config: SandboxConfig) -> Result<DockerSandbox, SandboxError> {
    match DockerSandbox::create(config).await {
        Err(SandboxError::CreationCleanupRequired { source, cleanup, .. }) => {
            // Keep capacity reserved until both container and staging are gone.
            cleanup.wait().await?;
            // Cleanup succeeded; creation still failed with its original cause.
            Err(*source)
        }
        result => result,
    }
}
```

Pre-create connection/image failures and settled creation failures whose cleanup is already confirmed keep their original error. Real Docker create-response-loss and staging-cleanup failure scenarios have not been exercised for this receipt change; the currently unavailable Docker environment is not evidence of successful cleanup. No simulated backend is used as a substitute for that integration proof.

## Workspace file snapshots

`DockerSandbox::capture_workspace_snapshot(budget, max_archive_bytes)` returns an `Arc<CodeSnapshot>` of regular files and directories, including empty directories. `SnapshotBudget::new(payload_bytes, entries)` requires positive limits; the workspace root does not count as an entry. The separate positive `max_archive_bytes: u64` caps downloaded tar bytes including headers and padding. A zero archive cap returns `SandboxError::Snapshot(SnapshotError::InvalidArchiveLimit)` before queueing. Equality with any payload/entry cap is allowed. The configured per-file limit also applies.

In `Created`, capture reads host staging. In `Running`, the lifecycle worker pauses the guest, downloads only the configured workspace using Docker's archive API, validates it, then resumes, including after an ordinary archive validation failure. A guest already `Paused` remains paused. Other phases reject capture with `InvalidPhase`. The archive streams into an anonymous temporary file, not an all-archive RAM buffer or public host path. Blocking tar import uses the same capability-scoped workspace operations as ordinary files. Only the expected root and portable relative regular files/directories are accepted; links, special nodes, escaping paths and unsupported archive metadata fail closed.

`DockerSandbox::restore_workspace_snapshot(Arc<CodeSnapshot>)` is only valid in `Created`. It shares the immutable snapshot rather than cloning its file payloads. It validates the target configuration's per-file and workspace payload bounds, creates replacement staging, and explicitly removes old staging before committing the replacement. It does not start a container or overwrite a running guest. Validation failures preserve old staging. If old cleanup fails, old staging remains owned and installed, but may have been partially removed; replacement staging is explicitly cleaned. A replacement cleanup failure is returned as nested `SnapshotError::RestoreCleanup`, owning that workspace for retry. Domain capture/import cleanup failures likewise retain temporary workspace ownership in their matchable snapshot error.

Both APIs serialize with all other sandbox operations and use the configured file timeout for the complete operation. Cancellation and timeout invoke whole-container cleanup, not process-only cancellation, and retain ownership when removal cannot be confirmed. Engine pause, archive download and resume errors also use owned cleanup. Ordinary archive, budget and target-limit failures use `SandboxError::Snapshot`; a running source is resumed before an ordinary parse error is returned. `CleanupRequired` preserves container identity for explicit `destroy` retry.

The deadline initiates cancellation, not a guarantee of cleanup-completion time. Already-started bounded local filesystem work is drained rather than detached, and produced replacement staging is explicitly removed before container cleanup. Network archive requests can be dropped promptly. Only the lifecycle worker installs staging, never a detached filesystem task. `SandboxError::SnapshotInterrupted { interruption, snapshot }` preserves a timeout/cancellation or lifecycle failure together with a domain failure and its cleanup-owned workspace.

Native GNU long-path and local PAX path/size/time/identity/xattr extensions are supported through the tar parser after a raw pre-scan caps each extension payload at 1 MiB. At most one GNU long-path and one local PAX extension may precede a real entry, bounding live parser metadata without imposing an aggregate metadata cap across independent files. Global PAX state, repeated local extensions, sparse/link extensions and unknown dangerous metadata fail closed before application. File import reuses a bounded payload buffer across entries. Each directory must have its own preceding archive entry, so implicit parent creation cannot bypass the entry budget; normal Docker workspace archives include these directories.

These are process-local file snapshots, not durable serialization, process/memory state, VM checkpointing, CRIU or Docker commits. The readonly root and tmpfs workspace mean Docker commit would omit workspace writes; actual archive capture is required. HTTP, Git, environment resolution/pull, pools and other runtimes remain unimplemented.
