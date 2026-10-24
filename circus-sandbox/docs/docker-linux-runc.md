# Linux Docker + runc slice

This checkout implements one real backend: an explicit local Docker Engine endpoint running a Linux image with the `runc` runtime. The SDK does not pull images and does not expose bollard types. Callers provide the image, endpoint, resource limits, isolation policy, and output bounds; the owner label is generated internally per sandbox.

The adapter sets memory and swap, milli-CPU to nano-CPU, pids, a bounded tmpfs workspace, read-only root, `none` networking, `no-new-privileges`, and `ALL` capability drop. It creates no host binds, volumes, credentials, or ports. Workspace files are staged from the existing capability-scoped `Workspace`. The guest file transport requires Python 3 and rejects symlinks during running-phase file operations.

`CommandRequest` uses argv form, bounded stdout and stderr, and a required timeout. Timeout, output overflow, and cancellation destroy the entire owned container so descendants cannot survive. Dropping the create future retains the generated name and owner label and recovers only a matching container. Cleanup errors retain ownership and are returned; retry `destroy` is required. Start and lifecycle operations also use the configured I/O timeout and may destroy the container when the engine call cannot be accepted safely.

The requested PID limit is enforced as supplied; the adapter does not silently increase it. Its budget includes the sandbox init, supervisor, commands, and any runtime/helper tasks charged to the container's PID cgroup, including temporary OCI exec startup tasks. Configuration validation requires at least 3 PIDs as a structural init/supervisor/command floor, not a guarantee that arbitrary commands or guest file operations can launch under every Docker/runc version. A low limit can therefore pass validation yet cause Docker to reject an exec. Such failures remain subject to the existing lifecycle safety cleanup described above; callers must handle reported errors and retained ownership.

The Docker integration test fixture requests 32 PIDs to leave room for temporary OCI startup tasks while exercising commands and guest file transport. This is an explicit test budget, not a product minimum or a universal launch guarantee. Applications must choose a PID budget that accommodates their commands and runtime's helper-task requirements.

Invalid command inputs are distinct from path failures: `SandboxError::Command(CommandError::EmptyArgv)`, `CommandError::InvalidTimeout { timeout }`, and `CommandError::InvalidOutputLimit { output_limit_bytes }` reject empty argv, zero timeout, and zero output bound before queueing. They preserve the phase, container identity, and staged files. A request above the configured output bound is separately rejected as `SandboxError::OutputLimit` with `Ownership::StillPresent`; it does not execute or destroy the container. Actual command output overflow still destroys it.

`start` counts staged file payload bytes before starting the container. `SandboxError::Staging(StagingError::LimitExceeded { limit, actual })` reports the configured workspace byte bound and the first detected sum above it: `actual` is a lower bound on the complete staged payload total, not a claimed exact total. Equality is permitted by this check. Defensive `StagingError::SizeOverflow { accumulated, next, limit }` retains the operands and bound if byte counting cannot fit `u64`. These are resource errors, not `RelativePath`. Both nested error types are exported through `api` and the crate root and retained as the standard error source; callers can match their variants rather than parse text.

A staging validation failure leaves the sandbox `Created`, retains its container identity and files, and allows removing or shrinking staged files before retrying `start`, or explicitly calling `destroy`. This payload check is not a host disk quota or a guarantee that tmpfs has room: filesystem metadata and allocation overhead can still cause a later upload to fail under the separately enforced tmpfs bound.

The ignored integration test requires `CIRCUS_DOCKER_SANDBOX=1` and `CIRCUS_DOCKER_ENDPOINT`; it must run against a real Linux Docker Engine with cgroup v2. `CIRCUS_DOCKER_IMAGE` optionally overrides the default `python:3.12-slim` image and must refer to a local Linux image containing Python 3. The example is `docker_sandbox` and also requires an endpoint. Run it with `cargo test --locked --test docker_sandbox -- --ignored`.

Snapshot, Docker commit, HTTP, Git, environment resolution/pull, pools, and other runtimes are planned but not implemented. A read-only root plus tmpfs workspace would not preserve workspace writes in a Docker commit; a future snapshot must archive and restore files.

## Interactive processes

`DockerSandbox::spawn_process(ProcessRequest)` returns a `ProcessSession` after Docker attaches the process. The request supplies argv, an optional workspace-relative working directory, explicit environment overrides, a lifetime timeout, and a total output limit for each of stdout and stderr. The output limit cannot exceed the sandbox's `IoLimits`. Environment names must be portable identifiers and values must be NUL-free. No host environment is copied; the image environment still applies. Environment overrides are not a secret store or an outbound network policy. A relative working directory uses container filesystem semantics, including container symlinks.

Use `write_stdin` repeatedly to send protocol messages and `next_output` to receive raw `ProcessOutput::Stdout` / `Stderr` chunks. Chunks may split lines or UTF-8 characters. `close_stdin` sends EOF; it does not terminate the process. Drain output concurrently with input and `wait`: the output queue holds eight chunks of at most 16 KiB each, then applies backpressure. Input calls are serialized and copied in 16-KiB chunks through a bounded queue. The transport may also retain an in-flight Docker frame. Total output limits and the lifetime timeout remain active under backpressure and while the container is paused.

One interactive process is admitted per sandbox. A second process and one-shot `exec` return `ProcessActive` until lifecycle records completion. File operations, status, pause/resume, and destroy remain available while the process runs. The I/O task reports completion to the lifecycle owner; it never deletes containers or changes sandbox state itself.

`wait` is repeatable and returns `ProcessExit::Exited` with the actual exit code, including nonzero codes. A normal exit preserves the container and workspace. Cancelling a `wait` or `next_output` future does not cancel the process. Dropping a session, calling `cancel`, or abandoning an accepted stdin operation requests whole-container cleanup. Timeout, output overflow, and transport failure also require cleanup. `ProcessExit::Terminated` reports the reason and observed `CleanupReport`; if ownership is unknown, retain the sandbox and retry `destroy`. `Drop` only requests cleanup while the Tokio runtime is alive; process crashes do not provide durable reconciliation.

Agent turn cancellation belongs in the agent's stdin protocol. Session cancellation and sandbox destruction force-remove the container and lose its tmpfs writes, including writes by descendant processes. Export results before deliberate destruction. There is no detached process recovery, TTY, process-group signal API, model access, or Paseo integration in this slice.

Run the two-message example with a local Python image and an explicit endpoint:

```bash
CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
  cargo run --locked --example interactive_process
```

Run the existing backend checks and the interactive process tests against a real Linux engine:

```bash
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
  cargo test --locked --test docker_sandbox --test docker_process -- --ignored --test-threads=1
```

The interactive tests cover repeated input, separate output streams, exit codes, concurrent file and lifecycle operations, one-process admission, output backpressure, timeout, cancellation, and owned-container cleanup. They are ignored without explicit invocation; ordinary Cargo test success is not Docker runtime evidence.
