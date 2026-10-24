# containerd + runc 后端

本文记录 `circus-sandbox` 中 containerd 后端的真实范围、配置方式、已执行命令与未验证语义。它只描述当前检出；工作流存在或单元测试通过不代表隔离已在本机验证。

## 实测环境

| 项目                | 值                                                                                                                          |
| ------------------- | --------------------------------------------------------------------------------------------------------------------------- |
| containerd          | 2.2.1（服务端与客户端）                                                                                                     |
| runc                | 1.3.4                                                                                                                       |
| snapshotter         | `overlayfs`                                                                                                                 |
| containerd socket   | `/run/containerd/containerd.sock`                                                                                           |
| namespace           | `default`（测试可用 `CIRCUS_CONTAINERD_NAMESPACE` 覆盖）                                                                    |
| OCI runtime handler | `io.containerd.runc.v2`                                                                                                     |
| 内核                | WSL2 Linux 6.6（cgroup v2，无 `/dev/kvm`）                                                                                  |
| Rust                | 1.99.1                                                                                                                      |
| protoc              | 3.21.12（`containerd-client` 构建期需要）                                                                                   |
| 测试镜像            | `docker.io/library/python:3.12-slim`（OCI index `sha256:a0f57064cbdcd6285fa45f51237ab0b35ea8ebaed06a1f5973e89e19dfb86704`） |

本机 image 已解包为四个 committed snapshot，最终 chain id 为 `sha256:241d372eec8f05a802bb5ca99fba52dd93480b904cb0ca4f591e4f2e27ffd6da`。chain-id 算法在 `containerd_image.rs` 单元测试中与这些真实 snapshot key 对照。

## 构建特性

containerd 后端是可选 Cargo feature：`--features containerd`。它引入 `containerd-client`，该依赖在构建期调用 `protoc` 生成 gRPC 绑定，因此构建机必须安装 protobuf compiler（实测 3.21.12）。未启用 feature 时 `BackendConfig::Containerd` 仍可构造，但 `Sandbox::create` 返回 `SandboxError::UnsupportedCapability`，不会静默降级到 Docker。

```bash
cargo build --locked --features containerd
CIRCUS_CONTAINERD=1 cargo test --locked --features containerd \
  --test containerd_sandbox --test containerd_snapshot --test containerd_process \
  --test containerd_pool -- --ignored --test-threads=1
```

## 配置

```rust
let backend = BackendConfig::containerd(
    "/run/containerd/containerd.sock",
    "default",
    "python:3.12-slim",
    "runc",
    "overlayfs",
);
```

- socket 必须是以 `/` 开头的绝对 Unix socket 路径；`unix://` URL 形式被类型化拒绝。
- namespace 必须非空，仅允许 ASCII 字母数字、`.`、`_`、`-`。
- image 是本地 reference；本 crate 从不 pull。熟悉短名会按 containerd 客户端约定回退查询：`python:3.12-slim` 先查原名，再查 `docker.io/library/python:3.12-slim`。`library/python:3.12-slim` 回退到 `docker.io/library/python:3.12-slim`；带 registry 前缀的名字不重写。
- runtime 只接受 `runc`；`runsc`（gVisor）在本机不可用，返回 `BackendConfigError::RuntimeUnavailable`，不会静默替换。
- snapshotter 只接受 `overlayfs`。
- 校验在分配任何 snapshot/container/task 之前完成，错误变体为 `BackendConfigError::{InvalidSocketPath, InvalidNamespace, RuntimeUnavailable, UnsupportedSnapshotter, ImageRequired}`。

## 创建与运行时语义

`prepare` 只调用 `Version` 与 `Images.Get`，验证 daemon 可达且镜像记录存在；不存在时映射为既有 `SandboxError::BackendImageUnavailable`。它不创建 snapshot。

`create_owned`：

1. 解析镜像 manifest/config（OCI index 按 host 架构选择 `linux/<arch>`），读取 image config 的 `rootfs.diff_ids`，按 containerd 的 chain-id 规则计算每一层的 committed snapshot key。若目标 committed snapshot 不存在，使用本地 content + diff service 解包（`Snapshots.Prepare` → `Diff.Apply` → `Snapshots.Commit`）；不访问 registry。进程内用一把异步互斥锁串行化解包，避免并发 create 重复 apply。
2. 以容器名（`circus-sandbox-<uuid>`）为 snapshot key，从最终 chain snapshot prepare 一个 active snapshot，并写入 owner label。
3. 以真实 OCI runtime-spec JSON 调用 `Containers.Create`：runtime handler `io.containerd.runc.v2`、`snapshotter=overlayfs`、`snapshot_key=<id>`。
4. 用 `Snapshots.Prepare` 返回的 mounts 调用 `Tasks.Create`，**不启动** task；`Sandbox::start` 才调用 `Tasks.Start`。

OCI spec 的安全设置：

- `root.readonly = true`。
- `process.capabilities` 的 bounding/effective/inheritable/permitted/ambient 全为空；`noNewPrivileges = true`。
- 新建 pid/ipc/uts/mount/network/cgroup namespace；network namespace 没有任何接口，因此没有网络连通性。
- workspace 为 tmpfs：`rw,nosuid,nodev,uid=<guest_uid>,gid=<guest_gid>,mode=0755,size=<workspace_bytes>`，不是 host bind。
- 额外挂载 `/proc`、`/dev`、`/dev/pts`、`/dev/shm`、`/sys`、`/sys/fs/cgroup`；`maskedPaths`/`readonlyPaths` 与 Docker 默认集合一致。
- 设备 cgroup：默认拒绝，再显式允许标准伪设备（null/zero/full/random/urandom/tty/console/ptmx/pts、loop、tun、fuse）。缺少该 allow list 时 cgroup v2 设备控制器会拒绝 runc 重新打开 `/dev/null`。
- cgroup 资源：memory limit 与 swap 都等于 `memory_bytes`；CPU quota = `cpu_millis * 100`、period = 100000；pids limit = `pids`。
- 未应用 seccomp profile，也未使用 user namespace；见“未验证/未实现”。

exec 与交互进程通过 runc shim 的 FIFO：

- 每个 exec 创建独立临时目录和三只 FIFO，`Tasks.Exec` 传入路径，然后并发打开 reader 与 `Tasks.Start`。
- stdin writer 是自定义 `FifoWriter`：`shutdown` 时真正关闭 fd，guest 才能看到 EOF（`tokio::fs::File` 只 flush 不关闭）。
- 一次性 exec 的 stdout/stderr 使用与 Docker 适配器相同的 `OutputCollector`，强制 per-stream 与 combined 预算；超限时 kill 该 exec、返回类型化 Limit 错误，由 lifecycle 销毁整个实例。
- 交互进程返回有序 `ProcessChunk` 流、可写 stdin 与 exit future；exit 只在输出流结束后调用 `Tasks.Wait`，并 best-effort `DeleteProcess`。

## 所有权与清理

- 容器与 snapshot 都带 `circus.sandbox.owner=<nonce>` 与 `circus.sandbox.name=<id>` label。
- `recover_owned` 先 `Containers.Get` 与 `Snapshots.Stat`。任一资源存在但 owner label 不匹配时返回 `Recovered::Unknown` 并拒绝删除。只有 owner 确认后才 `Tasks.Kill(all=true, SIGKILL)` → `Tasks.Delete` → `Containers.Delete` → `Snapshots.Remove`，最后重新 Get/Stat 确认两者都不存在；未确认时返回 `StillPresent`/`Unknown`，lifecycle 保留 staging 与身份供重试。
- create 中途失败时，已存在的资源由同一恢复路径删除；传输层不确定的失败标记 `create_pending`，避免把可能迟到的实例当作已清理。
- lifecycle 仍是唯一状态机；后端不修改 phase，也不保存 receipt。

## 未验证 / 未实现

- containerd 的 API 不暴露 cgroup 控制器生效值。`SandboxStatus::enforcement` 只报告从**存储的 OCI spec** 读回的结构事实（runtime handler、readonly root、capability 全空、no-new-privileges、private network namespace、0 个 bind mount、workspace tmpfs 选项）。`memory_bytes`、`nano_cpus`、`pids_limit` 保持 `None`，不能当作内核已强制。
- 没有 seccomp 过滤、user namespace、AppArmor/SELinux profile、TTY、进程组信号或 detached recovery。
- 网络 namespace 为空，但没有 bandwith/连接审计；不能把它当作网络策略。
- image 解包路径只在 committed snapshot 缺失时执行。本机测试使用已解包的 python 镜像；CI 中 `ctr images pull` 会解包，因此该路径在 CI 会执行，但本地未单独制造“缺 snapshot”场景。
- 外部进程并发解包同一 chain 时，Prepare 返回 AlreadyExists 后只轮询等待 commit；没有跨进程锁。
- 未验证 Kubernetes/CRI、Kata、Firecracker、gVisor（`runsc` 在本机不可用）、VM/内存检查点、registry 认证与 pull。
- Windows 主机不编译 containerd 适配器；在非 Unix 主机上选择 `BackendConfig::Containerd` 会在连接前返回 typed backend error。

## 测试

四组集成测试默认忽略，只有显式设置 `CIRCUS_CONTAINERD=1` 才连接真实 daemon：

```bash
CIRCUS_CONTAINERD=1 \
CIRCUS_CONTAINERD_SOCKET=/run/containerd/containerd.sock \
CIRCUS_CONTAINERD_IMAGE=python:3.12-slim \
cargo test --locked --features containerd --test containerd_sandbox --test containerd_snapshot --test containerd_process --test containerd_pool -- --ignored
```

- `containerd_sandbox`：生命周期、exec argv/退出码/共享输出预算、文件写入/读取/删除、pause/resume、缺失镜像类型化错误且新 namespace 无残留、并发创建、只读 status。
- `containerd_snapshot`：运行中 guest tmpfs 的 capture，并 restore 到新容器；capture 后源容器仍可写。
- `containerd_process`：交互进程 stdout/stdin/stderr/退出码、取消导致整容器清理、输出超限导致整容器清理。
- `containerd_pool`：`SandboxPool`（后端中立，Docker 与 containerd 通用）在真实 containerd 上预热容量 2、`status()` 计数、租约写入私有标记后 release 并经 `ctr` 确认容器与 snapshot 均不存在且下一租约看不到标记、容量 1 的 `AcquireTimeout` 后重新获取、短 TTL 撤销等待睡眠 Python 子进程的 exec 并补充全新容器、持有租约时 `shutdown()` 销毁全部槽位且幂等、坏镜像准备失败留下类型化错误且无容器。租约单次使用，release、TTL 与 shutdown 都真实销毁；`ResourceLimits` 仍是每容器限制，不是宿主聚合准入。
- 每个 teardown 都用 `ctr -n <ns> containers list -q` 与 `ctr -n <ns> snapshots --snapshotter overlayfs list` 确认容器和 snapshot 不存在。

本地实际执行（2026-10-25；WSL2 root 运行 Rust 与 containerd，Windows Node 24.18.0 在仓库根运行 QA）：

```text
cargo fmt --check
cargo clippy --locked --all-targets -- -D warnings
cargo clippy --locked --all-targets --features containerd -- -D warnings
cargo test --locked
RUSTDOCFLAGS="-D warnings" cargo doc --locked --no-deps
CIRCUS_CONTAINERD=1 CIRCUS_CONTAINERD_SOCKET=/run/containerd/containerd.sock \
CIRCUS_CONTAINERD_NAMESPACE=default CIRCUS_CONTAINERD_IMAGE=python:3.12-slim \
  cargo test --locked --features containerd --test containerd_pool -- --ignored
npm --prefix circus-sandbox/qa run check
npm --prefix circus-sandbox/qa test
node circus-sandbox/qa/architecture.mjs
```

`containerd_pool` 的实际结果是 `6 passed; 0 failed`（约 13.5 秒）。运行前后 `ctr -n default containers list -q` 均为空；`ctr -n default snapshots --snapshotter overlayfs list` 在运行后只保留镜像的 5 个 committed 层（与运行前计数相同），没有残留 active snapshot。该套件自身用进程内互斥串行化，因此未额外传 `--test-threads=1`。
