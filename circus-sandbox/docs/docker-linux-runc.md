# Linux Docker + runc slice

This checkout implements one real backend: an explicit local Docker Engine endpoint running a Linux image with the `runc` runtime. The SDK does not pull images and does not expose bollard types. Callers provide the image, endpoint, resource limits, isolation policy, and output bounds; the owner label is generated internally per sandbox.

The adapter sets memory and swap, milli-CPU to nano-CPU, pids, a bounded tmpfs workspace, read-only root, `none` networking, `no-new-privileges`, and `ALL` capability drop. It creates no host binds, volumes, credentials, or ports. Workspace files are staged from the existing capability-scoped `Workspace`. The guest file transport requires Python 3 and rejects symlinks during running-phase file operations.

`CommandRequest` uses argv form, bounded stdout and stderr, and a required timeout. Timeout, output overflow, and cancellation destroy the entire owned container so descendants cannot survive. Dropping the create future retains the generated name and owner label and recovers only a matching container. Cleanup errors retain ownership and are returned; retry `destroy` is required. Start and lifecycle operations also use the configured I/O timeout and may destroy the container when the engine call cannot be accepted safely.

## Real-engine gate and local reproduction

`Sandbox Rust` adds a separate `docker-integration` job on `ubuntu-latest`, using Rust 1.99.1 and a local Linux Docker Engine with cgroup v2 and `runc`. It explicitly pulls `python:3.12-slim` before running the ignored real-engine integration test and the `docker_sandbox` example. Pull, engine preconditions, test, and example failures fail the job; there is no mock fallback or `continue-on-error`.

The ordinary `rust (ubuntu-latest)` and `rust (windows-latest)` jobs compile and test the crate but do not opt into this ignored test. Their success is not real-engine isolation evidence. Configuring `docker-integration` likewise does not claim a passing run; results belong to the corresponding commit's PR.

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

Snapshot, Docker commit, HTTP, Git, environment resolution/pull, pools, and other runtimes are planned but not implemented. A read-only root plus tmpfs workspace would not preserve workspace writes in a Docker commit; a future snapshot must archive and restore files.
