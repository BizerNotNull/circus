# Kubernetes workload (runc) backend

This document records the B02 Kubernetes workload adapter: what it actually
does, the versions and commands used for the recorded run, least-privilege RBAC,
security settings, cleanup/ownership, and what remains unverified. It does not
claim true pause/resume, workspace snapshots, or a VM runtime.

## Scope

Implemented:

- `BackendConfig::Kubernetes(KubernetesBackendConfig)` behind the optional
  `kubernetes` crate feature.
- Explicit kubeconfig loading (never `KUBECONFIG`, `$HOME/.kube/config`, or
  in-cluster credentials), namespace-scoped API access, API preparation with a
  server-side dry run, Pod create/start, argv exec, bounded guest file transport,
  inspected host reporting, ownership-verified deletion, and typed unsupported
  operations.
- `tests/kubernetes_workload.rs`, ignored unless `CIRCUS_KUBERNETES=1`; a real
  k3s job in `.github/workflows/sandbox-kubernetes.yml`.

Not implemented:

- Pause/resume (B03 provider).
- Workspace snapshot capture/restore (`workspace_snapshot` capability is false).
- Interactive `spawn_process` sessions (`spawn_process` capability is false).
- pids limits, RuntimeClass selection, image pull credentials, NetworkPolicy
  creation, or KVM/gVisor/Kata runtimes.

## Versions used for the recorded run

Recorded in this WSL workstation against the running k3s cluster:

| Component          | Value                                                                                                                        |
| ------------------ | ---------------------------------------------------------------------------------------------------------------------------- |
| k3s                | `v1.36.5+k3s1` (`3dd98cc5`)                                                                                                  |
| Kubernetes node    | `v1.36.5+k3s1`                                                                                                               |
| containerd         | `2.3.4-k3s1.36` (reported by the node)                                                                                       |
| Node OS / kernel   | Ubuntu 24.04.4 LTS, `6.6.87.2-microsoft-standard-WSL2`                                                                       |
| Rust               | `1.99.1`, edition 2024                                                                                                       |
| kube / k8s-openapi | `4.2.0` / `0.28.0` (`v1_36`)                                                                                                 |
| Image              | `docker.io/library/python:3.12-slim`, index digest `sha256:a0f57064cbdcd6285fa45f51237ab0b35ea8ebaed06a1f5973e89e19dfb86704` |

The node was already Ready and the image was already present in k3s
containerd. The backend's `prepare` never pulls; kubelet pulls a missing image
with `imagePullPolicy: IfNotPresent`.

```text
$ /usr/local/bin/k3s kubectl get nodes -o wide
NAME              STATUS   ROLES           AGE   VERSION        INTERNAL-IP   CONTAINER-RUNTIME
desktop-8nqa5gc   Ready    control-plane   125m  v1.36.5+k3s1  100.100.4.15  containerd://2.3.4-k3s1.36
```

```text
$ /usr/local/bin/k3s ctr -n k8s.io images list | grep python:3.12-slim
docker.io/library/python:3.12-slim  application/vnd.oci.image.index.v1+json  sha256:a0f57064cbdcd6285fa45f51237ab0b35ea8ebaed06a1f5973e89e19dfb86704  41.3 MiB
```

## Descriptor and creation

```rust
use circus_sandbox::{BackendConfig, KubernetesBackendConfig};

let backend = BackendConfig::Kubernetes(
    KubernetesBackendConfig::new(
        "/etc/rancher/k3s/k3s.yaml",
        "default",
        "docker.io/library/python:3.12-slim",
    )
    .with_context("default")                 // optional
    .with_service_account("sandbox-runner"), // optional
);
```

Validation happens before any API allocation:

| Field             | Rule                                                              |
| ----------------- | ----------------------------------------------------------------- |
| `kubeconfig`      | absolute path (Unix or host-absolute)                             |
| `context`         | optional, nonempty, at most 253 characters, no control characters |
| `namespace`       | DNS-1123 label                                                    |
| `service_account` | optional DNS-1123 subdomain                                       |
| `image`           | nonempty, lowercase OCI/Docker reference (tag/digest accepted)    |
| guest uid         | non-zero because the Pod sets `runAsNonRoot`                      |

Failures are `SandboxError::BackendConfig(BackendConfigError)` with
`InvalidKubeconfigPath`, `InvalidKubernetesNamespace`, `InvalidContextName`,
`InvalidServiceAccount`, `InvalidImageReference`, `ImageRequired`, or
`NonRootGuestRequired`. The descriptor is plain data; `kube`/`k8s-openapi`
types never appear in the public API, and the default build does not compile
those crates.

`Sandbox::create` performs `prepare` before allocating:

1. Load the explicit kubeconfig and resolve the requested context.
2. Override the default namespace with the descriptor's namespace.
3. List Pods in that namespace (`limit=1`) to prove reachability and RBAC read.
4. Create the full Pod spec with `dryRun=All` under a generated name. This runs
   schema/admission validation and proves `pods create` without persisting.
5. Return the owned Pod. Readiness is awaited by `start`, not by `create`.

An unreachable API, unreadable kubeconfig, unknown context, or forbidden
namespace is a typed `SandboxError::Backend { message }` and leaves no Pod. A
well-formed but unresolvable image cannot be detected without a pull; it fails
during `start` (kubelet `ErrImagePull`/`ImagePullBackOff`) and lifecycle removes
the Pod. The suite asserts this with `registry.invalid/...`.

## Pod security settings

One Pod per sandbox, one container named `sandbox`:

- `metadata.name = circus-sandbox-<uuid>`; labels
  `circus.sandbox.owner=<nonce>`, `circus.sandbox.name=<name>`,
  `app.kubernetes.io/managed-by=circus-sandbox`.
- `restartPolicy: Never`, `activeDeadlineSeconds: 86400` as a hard backstop.
- Entrypoint `python3 -I -B -c 'import time; time.sleep(31536000)'`; Python 3 is
  required for the entrypoint, exec trampoline, and guest file helper.
- `securityContext` (Pod and container): `runAsNonRoot: true`, explicit
  `runAsUser`/`runAsGroup` from `IsolationConfig`, `fsGroup` with
  `OnRootMismatch`, `seccompProfile: RuntimeDefault`,
  `allowPrivilegeEscalation: false`, `readOnlyRootFilesystem: true`,
  `privileged: false`, capabilities `drop: ["ALL"]` and `add: []`.
- `serviceAccountName` only when configured; `automountServiceAccountToken:
false`; `enableServiceLinks: false`; `hostNetwork`/`hostPID`/`hostIPC` false;
  `shareProcessNamespace: false`; `terminationGracePeriodSeconds: 0`.
- Workspace is an `emptyDir` with `sizeLimit` from `ResourceLimits`, mounted at
  the configured guest path. There are no `hostPath` volumes and no host mounts.
- Memory and CPU `limits` and `requests` are copied from `ResourceLimits`.
  pids limits do not exist in a Pod spec for a normal container; the adapter
  deliberately reports `pids_limit: None` instead of pretending.
- `imagePullPolicy: IfNotPresent`; `create_owned` verifies that the API server
  preserved the owner labels, image, restart policy, deadline, security fields,
  workspace volume, and the absence of `hostPath` volumes. A mismatch destroys
  the Pod through the normal cleanup path.

`emptyDir.medium` is left unset, so the node chooses its storage. It is not
reported as tmpfs. Network isolation is not implemented by this adapter: pods
use the cluster CNI, and no NetworkPolicy is created. `hostNetwork: false` only
means the pod does not join the host network namespace.

## Exec, files, and status

- `exec` uses `pods/exec` with argv only. Kubernetes has no per-exec working
  directory or user field, so a small Python trampoline (`os.chdir`, then
  `os.execvp`) changes to the requested guest directory. `/` uses the command
  unchanged. The container's security context already fixes the uid/gid; the
  `user` argument is therefore inert.
- stdout and stderr are drained concurrently through the shared
  `OutputCollector`. The request's byte bound is a single aggregate budget;
  exceeding it is `SandboxError::OutputLimit` and lifecycle removes the Pod.
- `upload`, `download_file`, `remove_file`, and `list_workspace` run the same
  `guest_transport` argv and `guest_files.py` exit-code mapping as Docker and
  containerd. Staged files are streamed in 64 KiB chunks and bounded by the
  workspace per-file and aggregate limits; the manifest format is unchanged.
- `status().enforcement` reports only fields read back from the Pod spec:
  memory limit, CPU limit, read-only root, capability drop list, observed
  security options (`allow-privilege-escalation:false`,
  `seccomp:RuntimeDefault`), host-path bind count, and mount count. `runtime`,
  `network_mode`, `pids_limit`, and `tmpfs_workspace` stay `None`.

## Ownership and cleanup

The instance id is the Pod name. Deletion always starts with a live `GET`:

1. Pod absent → absence confirmed.
2. Pod present with the same `circus.sandbox.owner` nonce and
   `circus.sandbox.name` label → delete with `PropagationPolicy::Foreground`
   and `gracePeriodSeconds: 0`, then poll until `GET` returns 404. Five seconds
   without 404 is `StillPresent`; an inspection failure is `Unknown`.
3. Pod present with different labels → never delete. The sandbox retains
   ownership and reports `CleanupRequired` with `Ownership::Unknown`.
4. Any other inspection failure → retain ownership. A retained id alone never
   authorizes deletion because the Pod's labels are the only ownership proof.

`Sandbox::destroy` retries after the operator removes a foreign Pod; the suite
covers a foreign Pod with the same name and no owner label. `Drop` still asks
lifecycle to clean up, but only `destroy` confirms absence.

## Least-privilege RBAC

The backend only needs, in the target namespace:

```yaml
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  namespace: sandboxes
  name: circus-sandbox-workload
rules:
  - apiGroups: [""]
    resources: ["pods"]
    verbs: ["get", "list", "watch", "create", "delete"]
  - apiGroups: [""]
    resources: ["pods/exec"]
    verbs: ["create"]
  # Optional: not used by the adapter today, but useful for operator diagnosis.
  - apiGroups: [""]
    resources: ["pods/log"]
    verbs: ["get"]
```

Notes:

- `list` is used by `prepare`; `get` by start/status/recovery; `create` by both
  the dry run and the real Pod; `delete` by cleanup.
- `pods/exec` `create` is the exec subresource requirement.
- No deployments, replicasets, secrets, configmaps, nodes, namespaces, or
  service accounts are read. The kubeconfig user must be allowed to resolve its
  context and authenticate.
- The test suite's `kubectl` assertions use the same kubeconfig and need
  `pods get/delete` in that namespace; the CI job uses the k3s admin kubeconfig
  copied for the runner user.

## Recorded commands and results

In WSL (Rust 1.99.1, k3s already Ready):

```bash
export CIRCUS_KUBERNETES=1
cargo test --locked --features kubernetes --test kubernetes_workload -- \
  --ignored --test-threads=1
```

Result on 2026-10-25:

```text
running 7 tests
test kubernetes_absent_image_fails_start_without_leaking ... ok
test kubernetes_exec_output_limit_destroys_and_confirms_absence ... ok
test kubernetes_foreign_pod_with_same_name_is_never_deleted ... ok
test kubernetes_invalid_image_is_rejected_before_allocation ... ok
test kubernetes_linux_runc_lifecycle_exec_files_and_observables ... ok
test kubernetes_unreachable_api_is_typed_and_allocates_nothing ... ok
test kubernetes_unsupported_operations_are_typed ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 34.61s
```

The suite creates real Pods in `default`, runs exec and file operations, reads
`kubectl` state, replaces one owned Pod with a same-named foreign Pod to verify
cleanup refusal, and checks absence with `kubectl get pod` after destroy.

Environment overrides:

| Variable                         | Default                                |
| -------------------------------- | -------------------------------------- |
| `CIRCUS_KUBERNETES`              | must be `1` to opt in                  |
| `CIRCUS_KUBERNETES_KUBECONFIG`   | `/etc/rancher/k3s/k3s.yaml`            |
| `CIRCUS_KUBERNETES_NAMESPACE`    | `default`                              |
| `CIRCUS_KUBERNETES_IMAGE`        | `docker.io/library/python:3.12-slim`   |
| `CIRCUS_KUBERNETES_ABSENT_IMAGE` | `registry.invalid/circus/absent:never` |

## What is not verified

- True pause/resume: no node-runtime or workload-provider pause exists. Pod
  recreation is not pause. `pause`/`resume` return
  `SandboxError::UnsupportedCapability { capability: "pause and resume" }`.
- Workspace snapshot capture/restore: `list_workspace` exists, but API capture
  and restore are unsupported for Kubernetes, so the same-manifest-format claim
  is only exercised at the trait level, not through snapshot round trips.
- Interactive process sessions, pids limits, RuntimeClass selection, gVisor,
  Kata, Firecracker/KVM, registry authentication, image pull policy changes,
  multi-node scheduling behavior, and CNI/NetworkPolicy isolation.
- Node-level cgroup readback. `enforcement` reports the Pod spec, not the
  kubelet's effective cgroup values.
- Tenant isolation across namespaces. The adapter scopes to one configured
  namespace; callers must not share one sandbox handle across tenants, and RBAC
  must be granted per namespace.
