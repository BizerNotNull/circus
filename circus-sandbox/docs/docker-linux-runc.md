# Linux Docker + runc slice

This checkout implements one real backend: an explicit local Docker Engine endpoint running a Linux image with the `runc` runtime. The SDK does not pull images and does not expose bollard types. Callers provide the image, endpoint, resource limits, isolation policy, and output bounds; the owner label is generated internally per sandbox.

The adapter sets memory and swap, milli-CPU to nano-CPU, pids, a bounded tmpfs workspace, read-only root, `no-new-privileges`, and `ALL` capability drop. Network access is explicit: `Disabled` sets Docker `network_disabled=true` and network mode `none`; `Bridge` sets `false` and `bridge`. Every existing callsite selects `Disabled`. `Bridge` is ordinary Docker bridge networking with unrestricted destination egress, not an allowlist or MCP-only channel; network/MCP workloads can leak data. A future MCP gateway needs a separate, narrower restricted channel and is not implemented. The adapter creates no host binds, volumes, credentials, or ports. Workspace files are staged from the existing capability-scoped `Workspace`. The guest file transport requires Python 3 and rejects symlinks during running-phase file operations.

To opt in to unrestricted bridge egress, pass `NetworkAccess::Bridge` to `IsolationConfig::linux_enforced`:

```rust
let isolation = IsolationConfig::linux_enforced(
    true,
    true,
    true,
    NetworkAccess::Bridge,
    "/workspace",
    65534,
    65534,
)?;
```

`CommandRequest` uses argv form, bounded stdout and stderr, and a required timeout. Timeout, output overflow, and cancellation destroy the entire owned container so descendants cannot survive. Dropping the create future retains the generated name and owner label and recovers only a matching container. Cleanup errors retain ownership and are returned; retry `destroy` is required. Start and lifecycle operations also use the configured I/O timeout and may destroy the container when the engine call cannot be accepted safely.

## Managed streaming process sessions

`DockerSandbox::open_process(ProcessRequest)` starts one argv process in a running sandbox and returns a `ProcessSession`. The request supplies a wall-clock timeout and a byte limit shared by stdout and stderr. `write_stdin` sends bytes through a bounded pipe; `close_stdin` sends EOF while output can still be read. `recv` returns bounded chunks tagged as stdout or stderr, in arrival order. Chunk boundaries are transport-defined; callers must not assume one chunk per line or write.

After output closes, `wait` returns the process exit code. A non-zero exit code is an ordinary completed process and does not itself destroy the sandbox. The process-session timeout or combined output-limit breach cancels the process and destroys its owning sandbox. Docker provides no exec-scoped kill API here: explicit `close` and dropping a still-active session detach its stream and destroy the entire owned container to guarantee the child cannot survive. Explicit close waits for attached I/O cleanup. Only one process session may be active per sandbox. Session cleanup is scoped to its owner and does not affect other sandboxes. Use the existing `exec` API when fully buffered output is preferred.

## Real-engine gate and local reproduction

`Sandbox Rust` adds a separate `docker-integration` job on `ubuntu-latest`, using Rust 1.99.1 and a local Linux Docker Engine with cgroup v2 and `runc`. It explicitly pulls `python:3.12-slim` before running the ignored real-engine integration test and the `docker_sandbox` example. Pull, engine preconditions, test, and example failures fail the job; there is no mock fallback or `continue-on-error`.

The ordinary `rust (ubuntu-latest)` and `rust (windows-latest)` jobs compile and test the crate but do not opt into this ignored test. Their success is not real-engine isolation evidence. Configuring `docker-integration` likewise does not claim a passing run; results belong to the corresponding commit's PR.

On a failed `docker-integration` job, a diagnostic step collects only the current boot's local `docker.service` and `containerd.service` journal entries from the preceding 30 minutes, capped at 200 entries and a 15-second collection timeout. Missing entries, inaccessible journals, and collection failures are reported explicitly; they do not clear the original job failure. These bounded logs can help diagnose Docker or containerd failures. Collection does not inspect container configuration or credentials, contact remote endpoints, retry tests, or change runtime settings or resource limits.

On a Linux host with Docker CLI access to the local Engine, cgroup v2, and the `runc` runtime, run from `circus-sandbox/`:

```bash
export CIRCUS_DOCKER_SANDBOX=1
export CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock
export CIRCUS_DOCKER_IMAGE=python:3.12-slim
docker --host "$CIRCUS_DOCKER_ENDPOINT" pull "$CIRCUS_DOCKER_IMAGE"
cargo test --locked --test docker_sandbox -- --ignored
cargo run --locked --example docker_sandbox
```

The test requires both the explicit opt-in `CIRCUS_DOCKER_SANDBOX=1` and `CIRCUS_DOCKER_ENDPOINT`; the example requires the endpoint but does not check the opt-in. Both accept `CIRCUS_DOCKER_IMAGE`, defaulting to `python:3.12-slim`, and require a locally available Linux image containing Python 3. Image preparation is a caller/CI responsibility, not an SDK pull feature. The example stages, reads, and executes against a workspace file, pauses/resumes, then destroys its sandbox. Only clean up containers owned by the current run; never use global prune.

The HTTPS egress smoke inside the ignored suite needs outbound Internet. To run only that case, use:

```bash
cargo test --locked --test docker_sandbox docker_bridge_allows_outbound_https -- --ignored
```

Snapshot, Docker commit, HTTP, Git, environment resolution/pull, pools, and other runtimes are planned but not implemented. A read-only root plus tmpfs workspace would not preserve workspace writes in a Docker commit; a future snapshot must archive and restore files.
