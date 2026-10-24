# circus-sandbox

Rust 库，包名 `circus-sandbox`。当前提供 capability-scoped workspace 文件能力、受预算约束的进程内代码快照、`runtime::ResourceLimits` 请求校验、本地 OCI 镜像解析，以及后端中立的 `Sandbox::create(BackendConfig, SandboxConfig)` API。该 API 目前只有一个已实现后端：显式本地 Linux Docker Engine + `runc`。workspace 文件操作在 tempfile 新建的临时目录里，通过 cap-std 目录句柄做相对路径的同步读写；容器后端通过私有 lifecycle/infrastructure 模块运行 Linux 容器，`BackendConfig::Docker` 只是公开的后端选择描述，不暴露 bollard 或 Engine 类型。

容器后端不是通用进程沙箱：`BackendConfig::Docker` 只接受本地 Unix socket 或 Windows named pipe，要求调用方提供已存在且包含 Python 3 的 Linux 镜像，拒绝拉取镜像。`environment::prepare_docker_image` 只做本地镜像 inspect，把显式 reference 解析成不可变 image ID 并检查平台；它不会访问 registry，也不会读取凭据。`environment::EnvironmentPlan` 执行显式有序、逐步固定且不经过 shell 的准备工作，并在返回前确认容器与 staging 清理；`environment::EnvironmentId` 从解析后的镜像、平台、固定步骤和显式 tenant 域派生不透明 SHA-256 标识，不提供缓存或持久化。`http` feature（默认启用）提供认证 JSON HTTP 传输，见 [HTTP API](docs/http-api.md)。Kubernetes、containerd、gVisor、Kata、Firecracker、registry pull/认证仍未实现；没有 Paseo 接入。这些仍属于[完整交付目标](docs/delivery-plan.md)，不能把当前 Docker + runc slice 当作全部七领域交付。

workspace API 本身不是进程隔离，也不隔离 CPU、网络、系统调用或进程；它不接受调用方传入的宿主目录，公开 API 也不返回工作区根路径。Docker 后端不改变 workspace API 的同步语义。资源请求校验不等于资源预留、聚合准入或内核强制；Docker 设置的实际效果需要真实 Linux Engine 验证，独立 workspace 仍没有总字节或宿主磁盘配额执行机制。`CodeSource::open` 仅供可信库调用方显式授予源目录读取能力，再复制到新建的 `Workspace`，不是挂载或直接接管源目录。`GitCheckout` 用受控宿主 Git 进程准备固定提交并可创建独立 worktree，属于可信准备流程，不是对抗性进程沙箱。

工具链基线是 Rust **1.99.1**、edition **2024**。直接依赖还包括 bollard **0.21.1**、tokio **1.x**、tokio-util **0.7.x**、uuid **1.x**、futures-util **0.3.x**、thiserror **2.0.21**，以及 `http` feature 使用的 axum **0.8**、serde **1.x**、serde_json **1.x**、base64 **0.22**；测试客户端使用 reqwest **0.12**（仅 JSON feature）。`Cargo.lock` 固定解析版本，检查和运行都加 `--locked`。workspace 仍使用 cap-std **4.0.3**、cap-fs-ext **4.0.3**、tempfile **3.27.0**。

这里描述当前检出的真实实现，不以文件或工作流存在代替发布、合并和运行证据。完整目标及交付记录入口见 [交付计划](docs/delivery-plan.md)，QA、安全、Rust 与架构域门禁的实际范围见 [工程说明](docs/engineering.md)，安全扫描约定见 [安全说明](../.github/sandbox-security.md)。

## 同步文件 API

```rust
use circus_sandbox::Workspace;

fn prepare() -> Result<(), circus_sandbox::WorkspaceError> {
    let mut workspace = Workspace::new(1024)?;
    workspace.create_dir_all("notes")?;
    workspace.write_file("notes/hello.txt", b"hello")?;
    let bytes = workspace.read_file("notes/hello.txt")?;
    assert_eq!(bytes, b"hello");
    workspace.remove_file("notes/hello.txt")?;
    workspace.cleanup()?;
    Ok(())
}
```

| 方法                             | 行为                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| -------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Workspace::new(max_file_bytes)` | 新建独立临时目录。`max_file_bytes` 必须是 `1..=usize::MAX-1`，否则返回 `InvalidLimit`。这是单个文件的读写上限，不是预分配大小，不是工作区总字节或文件数配额，也不是宿主磁盘配额或进程隔离。                                                                                                                                                                                                                                                                                                                                                 |
| `write_file(path, contents)`     | 写入或覆盖已有普通文件。不隐式创建父目录，也没有原子替换。`Closed`、非法路径、超限字节、缺失父目录和非普通文件都在截断之前返回，这些失败不修改已有文件，也不留下新文件。Unix 同样以 `nonblock(true)` 打开，Windows 不设置该标志，CreateFile 是 write+create 且不预先 truncate。确认是普通文件后先 `set_len(0)` 再 `write_all`。此后的 I/O 失败可能截断已有内容，或留下不完整或空文件。                                                                                                                                                      |
| `read_file(path)`                | 读取普通文件的全部字节。目录、FIFO、设备及其他非普通节点，包括工作区内指向它们的符号链接，返回 `NotRegularFile`，并保留调用方路径。指向普通文件的工作区符号链接仍跟随。Unix 用 cap-std `OpenOptions` 的 `nonblock(true)` 打开，所以 `open` 本身不等待 FIFO 对端；这不是路径 metadata 预检。Windows 不设置该标志，读取是只读打开。不会按不可信 metadata 无限分配。超过上限时最多探测 `limit + 1` 字节后返回 `LimitExceeded`，`actual` 是这次探测到的长度，不是未知的文件总长。逃出工作区的符号链接仍是 cap-std 的 I/O 错误，不是新错误变体。 |
| `create_dir_all(path)`           | 创建相对目录及其缺失的父目录。已存在时成功。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                |
| `remove_file(path)`              | 通过 cap-std `Dir::remove_file` 对最终组件 unlink。删除非目录项，包括普通文件、符号链接、FIFO 和 socket。不跟随符号链接目标，不删除目录。目录得到 `Io`，不是 `NotRegularFile`。删除前不做类型预检。                                                                                                                                                                                                                                                                                                                                         |
| `cleanup(&mut self)`             | 删除临时目录。可重复调用：已经删除视为成功。成功之后，上述文件操作都返回 `Closed`。失败时保留工作区归属，可以再调一次；失败不会把工作区标成已关闭。                                                                                                                                                                                                                                                                                                                                                                                         |

这些方法会阻塞当前线程；从 Tokio 异步上下文调用时，必须放到 `spawn_blocking` 上，不能直接占住异步 worker。Docker 异步后端使用 Tokio，并通过 `spawn_blocking` 处理 workspace I/O；不要引入第二套异步运行时。

`Drop` 只尽力删除临时目录，不返回错误。需要确认清理结果时调用 `cleanup`。

## 单文件上限、删除与并发

`max_file_bytes` 只约束一次 `write_file` 的字节数，以及一次 `read_file` 最多探测的长度。读和写共用这一个单文件上限，不是把多个文件的用量加总，也不是文件个数上限。`new` 不按该值预分配，也不向操作系统申请磁盘配额。多次各自未超限的写入仍可能写满宿主临时目录；那是磁盘耗尽，本库不会把它报成 `LimitExceeded`。独立 workspace 没有总配额 API，也不提供进程隔离；Docker 后端另行显式配置 CPU、内存、进程数和 tmpfs 工作区限制，不能把这个参数当成完整沙箱配额。

`remove_file` 就是 cap-std 的 unlink：去掉路径最终组件上的非目录项。普通文件、符号链接、FIFO、socket，以及该平台 unlink 会删除的其他非目录节点，都会被删掉。最终组件是符号链接时，删的是链接本身，不打开、不跟随目标，因此也不会删掉链接指向的目录或文件。目录删不掉，返回 `Io`，目录及其内容保留。这不是“只删除普通文件”。读写才用 `NotRegularFile` 拒绝非普通节点；删除前再做类型检查会和 unlink 竞态，本 API 不做。

同一 `Workspace` 的并发读写由内部 `RwLock` 保护，避免本进程里对这一个值的交错写入。这不是外部进程隔离：其他进程仍可改临时目录。这也不是崩溃安全的原子写。`write_file` 没有原子替换；只有已关闭、非法路径、超限、缺失父目录和非普通文件这些失败发生在截断之前，不修改已有文件。打开普通文件并 `set_len(0)` 之后的 I/O 失败，仍可能截断已有内容或留下不完整文件。这把锁不补上那些保证，也不构成磁盘配额。

可运行示例在 `examples/workspace.rs`。它实际执行写、读、删除和 `cleanup`。成功时标准输出只有读到的字节再加一个换行；失败写到标准错误并以状态码 1 退出，不打印与 `Result` 无关的成功字样：

```bash
cargo run --locked --example workspace
```

在 `circus-sandbox/` 目录执行。rustup 会按该目录的 `rust-toolchain.toml` 选择 1.99.1。

## 进程内代码快照

公开入口为 `Workspace::capture_code_snapshot(SnapshotBudget::new(max_total_bytes, max_entries)?)`，返回 `CodeSnapshot`；调用其 `restore()` 创建独立的新 `Workspace`，不会覆盖源工作区。

- 两项预算必须为正：字节预算只计算文件内容，条目预算计算文件和目录，不包括根目录；单文件仍受 workspace 的读写上限约束。这是一次捕获的预算，不是之后写入的工作区配额。
- 捕获保留普通文件与空目录，拒绝符号链接、特殊节点和不符合可移植相对路径规则的名称；失败返回类型化 `SnapshotError`。
- 快照只保存在当前进程内存中，不序列化、不持久化，不是进程/内存检查点、Docker commit、容器或 VM 快照，也不是池已实现的证据。恢复失败且清理也失败时，`RestoreCleanup` 返回目标 workspace 供调用方重试清理。

示例为 `examples/code_snapshot.rs`，公开 API 测试在 `tests/code_snapshot.rs`。

## 显式授权的代码副本

`CodeSource::open(path)` 打开调用方已经授权读取的源目录，返回只读 capability；不公开源根路径。路径字符串不是租户授权凭证：服务必须先自行决定调用方可读哪个目录，不能把不可信请求里的宿主路径直接传入。打开根目录按宿主路径解析语义处理；此后子项遍历和读取通过 cap-std 目录能力约束在源目录内，不修改源文件。

`source.copy_to_workspace(max_file_bytes, SnapshotBudget::new(max_total_bytes, max_entries)?)` 返回新的、独立拥有的 `Workspace`，不经过内存 `CodeSnapshot` 中间副本。原有 `Workspace::capture_code_snapshot` 仍是工作区到进程内快照的独立能力，不等同于外部源目录导入。

- `max_file_bytes` 沿用 workspace 单文件限制（拒绝 `0` 与 `usize::MAX`）；预算两个值必须为正。总字节只计文件内容；条目计目录和文件（包括空目录、零字节文件），不计根目录。恰好等于上限允许。
- 预算只约束这一次复制，不是以后写入的总配额、磁盘预留或原子配额。复制逐文件读取，不按不可信 metadata 分配整个源树。
- 拒绝所有源子项符号链接（包括指向目录内的链接）、非普通节点和不满足 workspace 可移植路径规则的名称；不复制所有权、权限或其他文件 metadata。
- `CodeCopyError` 可匹配 `ByteLimit { limit, actual }`、`EntryLimit { limit, actual }`、`Symlink { path }`、`NotRegular { path }`、`NotPortable { path }`、`Workspace(WorkspaceError)` 和 `SourceIo { path, source }`。单文件超限是 `Workspace(WorkspaceError::LimitExceeded { .. })`；遍历错误路径相对源根，打开失败的 `SourceIo.path` 是调用方传入的授权路径。失败不返回成功工作区；若回收部分目的工作区也失败，`CopyCleanup { copy, destination, cleanup }` 保留原始复制错误及 boxed 工作区所有权，调用方应取回 `destination` 并重试 `cleanup`，不能把 Drop 当成清理成功。
- 打开、复制和清理是同步阻塞调用；异步调用方应使用 `spawn_blocking`，已经开始的阻塞任务不能靠 abort 终止。外部仍可修改源文件，本 API 不承诺一致时间点快照、崩溃原子性或跨进程隔离。
- 不推断 Git clone/worktree、沙箱池或运行时能力；没有 Git metadata 特殊策略，也不会隐式分享源凭据。授权范围内的普通文件会按普通内容复制，调用方必须自行排除不应授予的秘密。

真实临时目录示例创建授权源、读取复制后的实际字节并显式清理双方；公开 API 测试覆盖独立副本、精确预算、嵌套文件与拒绝边界。在 `circus-sandbox/` 执行：

```bash
cargo run --locked --example code_copy
cargo test --locked --test code_copy
cargo run --locked --example git_worktree
cargo test --locked --test git_worktree
```

## Fixed-revision Git workspaces

`GitSource::local` and `GitSource::https` accept only explicitly authorized absolute local paths or HTTPS URLs without credentials, queries, fragments, or alternate protocols. `GitRevision` accepts only a complete 40- or 64-character commit ID. `GitCheckout::clone` prepares that commit with the configured absolute Git binary in a private temporary repository, disables ambient configuration and credentials, disables recursive submodules and tags, and verifies the checked-out object ID.

`GitCheckout::copy_to_workspace` returns a code-only `Workspace`: the checkout's private `.git` metadata is retained by `GitCheckout` and is never copied. The copy is independent and bounded by the per-file and one-shot snapshot budgets. The source path, repository root, private home, hooks, and configuration are not exposed publicly.

`GitCheckout::create_worktree` adds a detached worktree of that prepared repository at the same verified commit with `git worktree add --detach`, in a separate newly created private directory under the configured storage root, with its administrative entry recorded in the checkout's private repository. The checkout retains ownership of the shared repository and its primary directory; each returned `GitWorktree` owns only its own private directory and administrative entry. Two worktrees are independent, and removing one through `GitWorktree::cleanup` never removes the source repository, the primary checkout directory, or a sibling worktree. Concurrent worktrees are supported. Cleaning the checkout removes its private repository and therefore the administrative metadata of any live worktree, but it does not remove those worktrees' own directories; such a worktree can still be copied from and must still be cleaned explicitly (or by dropping its handle).

`GitWorktree::copy_to_workspace` uses the same filtered, code-only copy as the checkout: the worktree's root `.git` file and its private administrative metadata are excluded, so copies contain no Git state. Symlinks, non-regular nodes, path violations, and the per-file or snapshot budget fail the copy exactly as for the checkout, and each call produces an independent `Workspace`. Worktree creation uses the checkout's configured timeout as its whole-operation deadline; every worktree copy starts its own whole-operation deadline of the same length, covering queueing and the blocking copy. Dropping a worktree handle cancels its owner, and dropping a copy future cancels that copy; either path drains started blocking work, explicitly removes the created destination, and retains cleanup responsibility if removal cannot be confirmed.

`GitWorktree::cleanup` is explicit and idempotent and uses `git worktree remove --force` for that worktree, then `git worktree prune --expire now`, then removes its private directory. A dirty worktree is therefore removed with `--force`: modified and untracked files in that worktree are discarded, not preserved, and no other worktree is affected. A removal that cannot be confirmed returns `GitError::CleanupRequired` with an opaque `GitCleanup` receipt, and the retained owner keeps retrying; the same handle may call `cleanup` again while that retry is pending. Repeated calls after success return `Ok(())`, while copies after successful cleanup return `GitError::Closed`; creation after checkout cleanup also returns `GitError::Closed`. If the checkout's private repository has already been removed, worktree cleanup still removes its own private directory because no administrative entry remains. Each removal Git command has the checkout's configured per-process timeout.

Git preparation is trusted host-side work, not an OS process or filesystem isolation boundary. A local grant trusts the filesystem repository; an HTTPS grant is not an SSRF policy. The storage parent has no library-enforced disk quota and must be bounded by the deployer. A detached worker owns allocation, processes, blocking copies, and explicit removal. Dropping a clone/copy future cancels work but does not drop that owner; started blocking work drains before its destination is explicitly removed. Each clone or copy has one whole-operation deadline. Clone output has one combined stdout/stderr budget across all commands. Only the selected source protocol is enabled. Private home, config, temporary files, and empty hooks live beside, not inside, the repository.

Call `GitCheckout::cleanup().await` for observable, idempotent removal. `GitError::CleanupRequired { source, cleanup }` preserves the initiating error and an opaque `GitCleanup` receipt: `cleanup.wait().await` succeeds only after process termination and directory removal are confirmed. Cleanup retries retain directory identity and continue independently of receipt cancellation/drop. A failed ordinary copy can retain its partial destination in `CodeCopyError::CopyCleanup`; explicitly clean that destination. These are library ownership guarantees, not crash recovery or disk quota enforcement. Git behavior depends on the configured trusted binary.

## 后端选择与创建

公开创建入口是 `Sandbox::create(BackendConfig, SandboxConfig).await`：

- `runtime::SandboxConfig::new(limits, isolation, io_limits)` 只承载所有后端共有的要求：`ResourceLimits` 请求、`IsolationConfig` 隔离要求（no-new-privileges、drop all capabilities、read-only root、disabled network 必须全部为真）和 `IoLimits`。它不包含 endpoint、image 或 runtime。
- `runtime::BackendConfig` 是后端选择枚举，当前只有一个已实现变体 `BackendConfig::Docker(DockerBackendConfig)`。该描述包含显式本地 endpoint、已存在的 image reference 和 OCI runtime 名称；构造它不连接 Docker。
- Docker endpoint/image/runtime 的解析、仅 runc、以及 Docker 特有的 PID 下限等校验都在私有 `infrastructure::DockerConfig` 中完成。`Sandbox::create` 在分配任何容器之前返回 `SandboxError::BackendConfig(BackendConfigError)`；本地镜像不存在时返回 `SandboxError::BackendImageUnavailable { image }`；其他后端失败是 `SandboxError::Backend { message }`，不泄露 SDK 类型。真实 Engine 的 restrict/limit 结果由 create 时的 inspect 校验，不匹配会清理已创建容器，而不是降级。
- `Sandbox::create` 还会在连接前检查后端是否声明支持 pause/resume、guest 文件传输和 workspace 快照捕获；不支持时返回 `SandboxError::UnsupportedCapability`。这些检查不表示其他后端已实现。

`SandboxStatus` 只报告当前 phase、`instance_id: Option<String>` 和 `enforcement: Option<EnforcementReport>`。`EnforcementReport` 的每个字段都是 inspect 到的证据，类型为 `Option`；`None` 表示后端没有报告该项。请求值不会复制成“已强制”的值。

示例：`examples/sandbox_config.rs` 构造中立配置、展示 `BackendConfig::Docker` 描述，并观察无效 endpoint 与 Docker PID 下限在分配前被类型化拒绝；`examples/image_environment.rs` 解析本地不可变镜像 ID。两者都不需要连接成功，也不代表容器隔离已验证。

## 资源请求值模型

通过 `circus_sandbox::runtime::{ResourceLimits, ResourceLimitsError}` 使用。`ResourceLimits::new(memory_bytes, cpu_millis, pids, workspace_bytes, max_file_bytes)` 返回 `Result`；字段私有，提供同名 getter，支持 `Clone`、`Copy`、`Debug`、`PartialEq`、`Eq`，没有 `Default`。

- 内存和工作区用字节计，必须在 `1..=i64::MAX`，以便后端或持久化使用有符号 64 位数值。
- CPU 用 milli-CPU 计，`1000` 是一个 CPU，不是执行时间；CPU 与进程数必须为正，不附加宿主 CPU 数、Docker 最小内存或任意后端上限。
- `workspace_bytes <= memory_bytes`，`max_file_bytes <= workspace_bytes`。单文件上限范围为 `1..usize::MAX`，与 `Workspace::new` 一致，拒绝 `0` 和 `usize::MAX`。
- 标量错误为 `ZeroMemory`、`MemoryOutOfRange`、`ZeroCpu`、`ZeroPids`、`ZeroWorkspace`、`WorkspaceOutOfRange`、`InvalidMaxFileBytes`；关联错误为 `WorkspaceExceedsMemory`、`MaxFileExceedsWorkspace`。标量边界先于关联约束检查，错误为可匹配的 `ResourceLimitsError`，不是字符串判断。

构造成功只证明单个请求的数值与关联约束有效，不代表后端支持、资源已预留、多个请求的聚合准入成功、内核已经强制配额，或执行已隔离。它不启动运行时，也不为 `Workspace` 提供总配额执行机制。

`examples/resource_policy.rs` 构造有效请求、打印单位，再观察工作区超过内存的类型化拒绝。在 `circus-sandbox/` 执行：

```bash
cargo run --locked --example resource_policy
```

## Linux Docker + runc

可手动构建的 [Codex Agent 镜像](images/codex/README.md) 固定 CLI 与 Node 版本，包含 Python 3、Git、ripgrep，并提供可写 HOME/缓存/临时目录的启动脚本。镜像验收沿用当前禁网策略，范围是工具和 Codex stdio 离线
[Codex 单任务调用示例](docs/codex-agent.md) 通过 `ProcessSession` 转发受限模型请求，保持容器禁网、上游密钥留在宿主，并在任务完成后取回文件。需要显式配置支持 Responses 的模型服务；脚本化模型对端测试不代表真实模型验收或 Paseo 接入。

交互进程入口为 `DockerSandbox::spawn_process(ProcessRequest)`。返回的 `ProcessSession` 支持持续 stdin、分开的 stdout/stderr 流、EOF、退出通知和取消；每个沙箱同时允许一个交互进程，运行时仍可读写文件、查询状态及销毁。输出背压、超时、取消与工作区保留语义见下述后端文档。可运行示例为 `examples/interactive_process.rs`；Paseo 接入仍待实现。

容器后端的配置、生命周期、文件传输和已知限制见 [Linux Docker + runc 文档](docs/docker-linux-runc.md)，设计取舍见 [ADR-0001](docs/adr-0001-docker-linux-runc.md)。最小前置条件：Linux Docker Engine（本地 Unix socket；Windows 开发机可使用 Linux Docker Desktop named pipe）、已存在的 Linux 镜像、镜像内 Python 3，以及 cgroup v2 用于运行集成测试。镜像不会自动拉取；声明 `VOLUME` 的镜像不支持。

集成测试默认被忽略且不会连接 Docker。显式运行真实后端：

```bash
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
  cargo test --locked --test docker_sandbox --test docker_process -- --ignored
```

测试使用 `python:3.12-slim`，可用 `CIRCUS_DOCKER_IMAGE` 指定另一个包含 Python 3 的本地 Linux 镜像。实际运行环境、命令、结果和限制应记录在对应 PR；不要把编译、忽略测试或 CI 通过写成隔离验证。GitHub 的 `Sandbox Docker` 工作流在 `ubuntu-latest` 上用相同环境变量逐个运行这些忽略套件，并先打印 Engine 版本与镜像证据；它只提供该次 GitHub 运行的真实 Engine 证据，不要求本地 Docker，未运行、取消或失败都不是通过声明。`agent_image` 仍只能手动验收：其镜像构建需要网络，不能离线构建。

本地镜像解析测试同样默认忽略，且需要 `CIRCUS_DOCKER_ARCH` 指定 Docker 架构名（例如 `amd64` 或 `arm64`）：

```bash
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
CIRCUS_DOCKER_ARCH=amd64 \
  cargo test --locked --test docker_environment -- --ignored
CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock CIRCUS_DOCKER_ARCH=amd64 \
  cargo run --locked --example image_environment
```

`prepare_docker_image` 只 inspect 本地镜像并返回不可变 ID；缺失镜像是 `EnvironmentError::ImageNotFound`，不会触发拉取，也不读取任何 registry 凭据。

### 环境准备计划与稳定标识

`EnvironmentPlan::new(ImageRequest, Vec<PrepareStep>)` 构造显式有序的准备计划。`PrepareStep::new(argv, PinnedVersion, timeout, output_limit_bytes)` 只接受分开的 argv，不经过 shell；`PrepareStep::unpinned(...)` 可以表示未固定的步骤，但 `EnvironmentPlan::new` 会在任何 Docker 连接或容器分配之前以 `PlanError::UnpinnedInput { index }` 拒绝它，不会静默接受不可复现的输入。

`EnvironmentPlan::execute(SandboxConfig)` 在私有 lifecycle 钩子中执行真实准备：只 inspect 本地镜像；创建并启动一个 sandbox；按顺序对每一步调用 `Sandbox::exec`，只传入该步 argv、超时和输出上限，不继承宿主环境变量或凭据；最后销毁 sandbox，并等待实例与 staging 清理确认。非零退出返回 `PreparationError::StepFailed { index, argv, exit_code, stdout, stderr, output_limit_bytes, cleanup }`，其中 stdout/stderr 不超过该步配置的 output limit；超时、其他执行失败和清理无法确认也各有类型化错误。调用方丢弃 `execute` future 时，lifecycle owner 的 Drop 撤销会继续后台清理，但被丢弃的 future 不能报告清理结果。

`PreparedEnvironment` 记录解析后的不可变 image ID、repo digests、请求平台、每一步的精确 argv、pinned version 和有界观察输出。`EnvironmentId::new(image_id, repo_digests, platform, steps, tenant)` 或 `PreparedEnvironment::identity(TenantDomain)` 用 SHA-256（FIPS 180-4，规范序列化格式版本 1）对这些输入派生 64 个字符的小写十六进制不透明标识。repo digest 按集合排序比较；步骤顺序、argv、pinned version、image ID/digest、平台 os/arch/variant（Docker inspect 只报告 os/arch，variant 会参与标识但不会被验证）或 tenant domain 任一变化都会产生新标识。tenant domain 是调用方显式提供的安全域，必须非空且不含空白/控制字符，用于避免跨租户碰撞；identity 不包含 endpoint、宿主路径、环境变量、凭据、超时或输出上限。任何输入变化都意味着旧标识失效；等价输入得到相同标识。该模块只派生标识，不实现缓存、registry、持久化或查询服务。

```bash
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
CIRCUS_DOCKER_ARCH=amd64 \
  cargo test --locked --test docker_environment_prep -- --ignored
CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock CIRCUS_DOCKER_ARCH=amd64 \
  cargo run --locked --example environment_identity
```

真实测试覆盖固定步骤的精确输出、失败步骤的 index/exit code 与容器清理、执行中取消后的清理、未固定输入在分配前被拒绝，以及标识稳定性/敏感性和 Debug/Display 不泄露输入。示例打印观察到的 image ID、步骤输出和 identity hex；`execute` 返回即表示已确认清理。示例的 pinned version 可用 `CIRCUS_PYTHON_VERSION`（默认 `3`）覆盖，tenant domain 可用 `CIRCUS_TENANT`（默认 `example-tenant`）覆盖。身份派生测试不需要 Docker，可直接随 `cargo test --locked` 运行。真实 Docker 用例保持 `#[ignore]`；本检出没有可用的 Docker Engine，因此代码存在不等于已运行验证。

### 预启动 Docker 池

`PoolConfig::new(backend: BackendConfig, sandbox: SandboxConfig, capacity, baseline, lease_ttl: Duration)` 要求正容量和显式正 TTL；`DockerPool::create(config).await` 在所有槽位完成实际 create、可选基线恢复、start 和 Running 检查后才成功。TTL 从租约分配时开始，不从预热或排队开始；到期请求撤销并中断仍在执行的命令，实际销毁是异步的，必须等待清理确认，不能把到期当作容器已不存在。`acquire(Duration)` 必须给出正超时；超时为 `PoolError::AcquireTimeout`，存活等待者 FIFO，取消等待不会占用租约。`PoolLease::sandbox()` 借用真实后端；租约不可克隆。每次租约只使用一次，`release().await`、Drop 或到期都销毁容器，确认不存在后才按不可变配置补充全新容器，绝不 reset 或跨租用复用脏容器。

基线 `Option<Arc<CodeSnapshot>>` 只能包含调用者明确授权共享的输入；它不是凭据扫描器，也不是进程/内存检查点。容量约束包含 ready、creating、leased、retiring 和 cleanup_required；`status().await` 的这些互斥计数连同关闭后的 absent 合计为 capacity。`ResourceLimits` 是每容器限制，不是宿主聚合资源预留。

`shutdown().await` 关闭准入、取消等待并**撤销尚未归还的租约**，销毁所有池拥有的容器；成功才证明全部已不存在。取消 shutdown future 不会停止清理。最后一个池客户端 Drop 只请求同样的关闭，不证明成功；失败清理仍由后台所有者持有并重试。构造失败若返回 `PoolError::CleanupRequired { pool, source }`，必须保留该 pool 并重试 shutdown，可沿标准错误 source 链查看原因。真实测试与计时示例在 `circus-sandbox/` 执行（默认镜像须已存在，测试请求 32 PIDs）：

```bash
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
  cargo test --locked --test docker_pool -- --ignored
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
  cargo run --locked --example docker_pool
```

示例分别观察 cold create + restore + start、完整预热和 warm acquire 的 Instant 耗时，并执行基线字节、显式 release/shutdown；不是承诺加速比。Windows 端点可设为 `npipe:////./pipe/dockerDesktopLinuxEngine`；环境变量由所用 shell 显式设置。

<<<<<<< HEAD
## 可观测性（调用方计数与 tracing 事件）

`observability::SandboxMetrics` 是调用方显式创建、可克隆的固定计数器句柄；`SandboxMetrics::new()` 分配一组原子计数，`SandboxMetrics::disabled()`（也是 `Default`）不分配且每次记录立即返回。把句柄传给 `Sandbox::create_with_metrics(backend, config, metrics)` 或 `PoolConfig::with_metrics(metrics)` 后，lifecycle、pool 和 backend 会记录 create 成功/失败、create cleanup pending/completed、按 `OperationKind` 与 `OperationOutcome` 分类的操作、cleanup 尝试/失败、pool ready/leased/retired/refilled、snapshot capture/restore，以及按 `TerminationKind` 分类的进程会话结束。`metrics.snapshot()` 返回纯数据 `MetricsSnapshot`；计数饱和而不是回绕，pending cleanup 是永不为负的 gauge。没有句柄时是分支级 no-op，不存在进程级注册表，也不接受租户或调用方自定义标签。

同一批路径通过 `tracing` 发出稳定名称事件和 `sandbox.create`/`backend.create` span，例如 `sandbox.create.started`、`sandbox.create.completed`、`sandbox.transition.completed`、`sandbox.exec.completed`、`sandbox.process.opened`/`closed`、`sandbox.snapshot.capture.completed`/`restore.completed`、`sandbox.cleanup.attempted`、`sandbox.cleanup.retry`、`pool.acquire.completed`、`pool.lease.handed_off`/`released`/`expired`、`pool.slot.ready`/`retired`/`refilled`，以及 `backend.*.completed`。字段只包含固定标签、计数、布尔值、持续时间、退出码和有界程序名；环境变量值、完整 argv、文件内容、宿主路径、镜像凭据和 endpoint URL 不会进入事件。`cleanup_pending`/`cleanup_required` 字段和 `create_cleanup_pending` 计数用于区分“能力不可用”与“清理仍未完成”。

本 crate 是库：它不安装全局 `tracing` subscriber，不提供 exporter，也没有 health/readiness/metrics HTTP endpoint；是否采集、采到哪里由嵌入应用决定。事件与计数只描述已观察到的事实，不表示 Docker 隔离已在当前机器验证。
=======
## HTTP API

`circus-sandbox` 的 `http` feature（默认启用）把同一领域 `Sandbox` API 暴露为认证 JSON HTTP 服务。`HttpConfig::new(bind, token, allowed_tenants)` 要求显式非空 bearer token 和非空租户 allow-list，没有环境默认值或匿名模式。每个路由（含 `GET /healthz`）都先验证 token；缺失/错误 token 是 `401`，租户缺失或不在 allow-list 是 `403`。sandbox 与 snapshot id 均为随机 UUID，注册表按租户隔离，跨租户 id 返回 `404`。

主要端点：`POST /v1/sandboxes`、`GET /v1/sandboxes/{id}`、`POST .../{id}/{start,pause,resume}`、`DELETE /v1/sandboxes/{id}`、`POST .../{id}/exec`、`GET|PUT .../{id}/files/{path}`、`POST .../{id}/snapshots`、`POST .../{id}/snapshots/{snapshot_id}/restore`。`GET /healthz` 只表示进程存活，不探测 Docker 或任何 sandbox；后端相关失败通过 sandbox 响应的 `503 backend_unavailable` 等错误码表达。

错误体固定为 `{"code","message"}`，不使用内部错误文本，也不泄露 SDK 类型。仅支持 `docker` + `runc`；其他后端或 runtime 返回 `501 unsupported_backend`。清理未确认时 `DELETE` 返回 `500 cleanup_required` 并保留句柄供重试；优雅关闭会销毁全部保留 sandbox，并分别报告失败。

完整端点、请求/响应字段、限额、取消与清理归属及 curl 示例见 [HTTP API](docs/http-api.md)，机器可读契约见 [openapi.json](docs/openapi.json)。示例只启动传输，不虚构 Docker 可用性：

```bash
CIRCUS_HTTP_TOKEN=example-token CIRCUS_HTTP_TENANT=local \
  cargo run --locked --example http_api
```
>>>>>>> origin/main

## 命令

同样在 `circus-sandbox/` 下执行：

| 目的              | 命令                                                                                            |
| ----------------- | ----------------------------------------------------------------------------------------------- |
| 格式检查 / 格式化 | `cargo fmt --check` / `cargo fmt`                                                               |
| Clippy            | `cargo clippy --locked --all-targets -- -D warnings`                                            |
| 构建              | `cargo build --locked`                                                                          |
| 测试              | `cargo test --locked`                                                                           |
| 只跑文件 API 测试 | `cargo test --locked --test workspace`                                                          |
| 文档              | `RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps`                                         |
| 运行示例          | `cargo run --locked --example workspace`                                                        |
| 资源请求示例      | `cargo run --locked --example resource_policy`                                                  |
| 镜像解析示例      | `cargo run --locked --example image_environment`                                                |
| 镜像解析测试      | `cargo test --locked --test docker_environment -- --ignored`                                    |
| 环境准备示例      | `cargo run --locked --example environment_identity`                                             |
| 环境准备测试      | `cargo test --locked --test docker_environment_prep -- --ignored`                               |
| 代码副本测试      | `cargo test --locked --test code_copy`                                                          |
| 代码副本示例      | `cargo run --locked --example code_copy`                                                        |
| Git worktree 测试 | `cargo test --locked --test git_worktree`                                                       |
| Git worktree 示例 | `cargo run --locked --example git_worktree`                                                     |
| 依赖策略          | 先 `cargo install --locked --version 0.20.2 cargo-deny`，再在本目录 `cargo deny --locked check` |

`cargo test --locked` 覆盖库测试和 `tests/`。要连示例一起编译，使用 `cargo test --locked --all-targets`。不要手改 `Cargo.lock` 来绕过解析失败。

Windows 本机目标是 `x86_64-pc-windows-msvc`。这里的编译成功不表示 Linux 容器隔离已经验证；真实 Docker 后端必须在 Linux Docker Engine 上按上面的显式命令运行。符号链接测试需要创建符号链接的权限（开发者模式，或进程持有 `SeCreateSymbolicLinkPrivilege`）。没有该权限时，相关测试应失败，不能静默跳过；不要修改宿主安全设置来让它们通过。

### 架构域检查

架构域门禁用真实 `cargo modules dependencies` 生成 DOT，按语义 ID 投影到顶层域并检查固定矩阵；它不读 Rust 源码，也不使用 `--acyclic`。先安装固定开发工具，再运行 QA：

```bash
cargo install --locked --version 0.28.0 cargo-modules
npm --prefix circus-sandbox/qa ci
npm --prefix circus-sandbox/qa run architecture
```

`cargo install` 在 `circus-sandbox/` 执行，rustup 按该目录的 `rust-toolchain.toml` 选择 Rust 1.99.1；npm 命令在仓库根目录执行。脚本还会验证 cargo-modules 精确为 0.28.0。cargo-modules 是 MPL-2.0 的开发期 CLI 工具，不是 crate 的运行时依赖。完整域矩阵、根门面规则、失败条件和已知边界见[工程说明](docs/engineering.md#架构域门禁)。该门禁不证明外部 SDK 暴露面，也不检查域内循环。

## CI

`.github/workflows/sandbox-rust.yml`（`Sandbox Rust`）在每个 pull request、`main` push、merge queue 和手动运行时启动，没有路径过滤，也不调用 Paseo CI。`rust` job 在 `ubuntu-latest` 与 `windows-latest` 上跑上表的 fmt、clippy、build、test、doc，以及 `workspace` 和 `resource_policy` 两个 example。`dependencies` job 只在 Ubuntu 安装 cargo-deny 0.20.2 并执行 `cargo deny --locked check`。`codeql` job 只在 Ubuntu，以 Rust `build-mode: none` 分析 `circus-sandbox` 源码，不编译，配置是 `.github/codeql/sandbox-rust.yml`。

`.github/workflows/sandbox-architecture.yml`（`Sandbox architecture`）使用同样的触发条件，没有路径过滤。`architecture (ubuntu-latest)` 与 `architecture (windows-latest)` 安装 Node.js 24.18.0、Rust 1.99.1 和 cargo-modules 0.28.0，执行 `npm ci` 与 `npm run architecture`，对真实 crate 图检查固定跨域矩阵；超时 20 分钟，不构建或测试 crate，也不要求 Docker。

许可证 allow 只有 Apache-2.0、MIT、`Apache-2.0 WITH LLVM-exception`（winx 0.36.4）和 Unicode-3.0（unicode-ident 1.0.27）。没有例外列表。`multiple-versions` 只警告。当前 lock 里去不掉的传递重复是 `io-lifetimes` 2.0.4 与 3.0.1，以及 Windows SDK 绑定 `windows-sys` 0.59.0、0.60.2 与 0.61.2，都来自 cap-std / cap-fs-ext 4.0.3 的依赖闭包。细节见 [AGENTS.md](AGENTS.md)。工作流已配置不等于这些检查已经通过；结果看对应 PR 的 Actions。

当前检出还包含质量与安全门禁：`Sandbox quality` 检查文档、链接、1200 行手写 Rust 上限和 QA 回归测试，并运行 npm audit 与 actionlint；`CodeQL QA` 分析 QA JavaScript；`Gitleaks` 扫描完整 Git 历史；`Dependency review` 只审查 PR 依赖差异；`CodeQL (actions)` 只分析安全工作流。Dependabot 每周更新 GitHub Actions、QA npm 和 Cargo，但不批准或合并。QA 固定 Node.js 24.18.0，使用现有 checker 与锁文件；根目录命令见工程说明。这些门禁不等于隔离测试，也不等于仓库全部漏洞已解决。

合并使用 PR，不要求非作者批准；任务授权后的自动 squash merge 仍须最新 CI 通过、适用审查问题闭环且无冲突，不绕过保护。

## 路径

拒绝发生在打开句柄之前，并且 Windows 与 Linux 使用同一规则。非法路径是：空路径（`Empty`）、含 `\`（`Backslash`）、以盘符 `A:`–`Z:` 开头或以 `//` 开头（`WindowsPrefix`）、以 `/` 开头或 `Path::is_absolute()`（`Absolute`）、任一组件是 `..`（`ParentComponent`）。`cleanup` 成功后的文件操作是 `Closed`，不是这些路径类别。

`foo/bar`、`foo/./bar` 可以通过。`create_dir_all` 之外不会自动创建父目录。

通过检查之后，读写都从临时目录的 cap-std `Dir` 句柄出发。cap-std 会拒绝逃出该目录的 `..` 和符号链接，避免“先检查字符串再打开路径”的 TOCTOU。符号链接用例在 crate 私有测试里，因为公开 API 不暴露工作区根，也不能创建符号链接。这只约束文件访问路径，不能阻止进程打开其他宿主路径。

## 错误

`WorkspaceError` 实现了 `std::error::Error`，可以用 `match` 区分：

| 变体                              | 何时出现                                                                                                                                                                                                                                                                                                   |
| --------------------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `Closed`                          | `cleanup` 已经成功。优先于路径和长度检查。                                                                                                                                                                                                                                                                 |
| `InvalidLimit { max_file_bytes }` | `new(0)` 或 `new(usize::MAX)`。                                                                                                                                                                                                                                                                            |
| `PathRejected { path, reason }`   | 上一节的非法路径。`path` 是调用方原始路径。                                                                                                                                                                                                                                                                |
| `LimitExceeded { limit, actual }` | 写入或读取超过 `max_file_bytes`。写入时 `actual` 是 `contents.len()`。读取时最多探测 `limit + 1` 字节，`actual` 是探测长度，不是未知的文件总长。                                                                                                                                                           |
| `NotRegularFile { path }`         | 把目录、FIFO 或其他非普通文件当作文件读写，包括工作区内指向它们的符号链接。不用于 `remove_file`。Unix 上不会为了等 FIFO 对端而卡住。拒绝发生在截断之前。                                                                                                                                                   |
| `Io(std::io::Error)`              | 缺失文件、缺失父目录、`remove_file` 遇到目录、临时目录创建/删除失败，以及写入过程中的文件系统错误。缺失路径是 `ErrorKind::NotFound`，发生在截断之前。unlink 掉符号链接、FIFO 或 socket 是成功，不是这个变体，也不是 `NotRegularFile`。写入时若错误发生在打开普通文件之后，文件可能已被截断或只写入一部分。 |

`cleanup` 的 I/O 失败也是 `Io`。此时工作区仍然打开，应保留同一个 `Workspace` 再调用 `cleanup`，不要丢弃错误后假设目录已消失。

## 变更

### 未发布：调用方拥有的观测计数与结构化追踪事件

- 新增公开 `observability` 模块：`SandboxMetrics`/`MetricsSnapshot` 是调用方创建并可显式传入的固定计数器，`PoolConfig::with_metrics` 与 `Sandbox::create_with_metrics` 接入 lifecycle 和 pool；无句柄时为零分配 no-op。
- lifecycle、pool 与 Docker backend 发出稳定名称的 `tracing` 事件和 span，覆盖 create、start/pause/resume、exec、进程会话、snapshot、cleanup 重试和池租约；字段只含固定标签、计数、时长与程序名，不记录环境值、完整 argv、文件内容、宿主路径或凭据。
- 新增 `tests/observability.rs` 与本地 `Subscriber` 单元测试（断言事件名/字段、哨兵 env/argv 不泄漏、计数器饱和与 no-op）；新增 `tracing` 依赖。crate 不安装全局 subscriber，不提供 health/readiness 或导出 endpoint。

### 未发布：后端中立的创建 API 与本地镜像解析

- 公开创建入口改为 `Sandbox::create(BackendConfig, SandboxConfig)`；`SandboxConfig` 只保留 limits、isolation 与 I/O 上限，Docker endpoint/image/runtime 与 Docker 专用校验移入私有 infrastructure。旧的容器 sandbox 公共类型名和 Docker 专用配置/endpoint 公共名称已移除，没有别名。
- `SandboxStatus` 改为 `phase`、`instance_id` 与 inspected `enforcement` 证据；未报告的字段是 `None`，请求值不再显示为已强制。`SandboxError` 保留可匹配含义：`BackendConfig`、`BackendImageUnavailable { image }`、`Backend { message }`，以及原有的阶段、staging、snapshot 与清理归属错误。
- 新增私有 `Backend` trait、`DockerBackend` 和静态泛型 `State<B>`；lifecycle 仍独占状态转换、取消、清理回执与 create 未决归属。`Sandbox::create` 在分配前检查后端能力并拒绝不支持的组合。
- 新增 `environment`：`prepare_docker_image(ImageRequest)` 只 inspect 本地镜像，返回不可变 ID、repo digests 与平台；不拉取、不访问 registry、不读取凭据。平台不匹配和本地缺失都是类型化错误。示例 `image_environment`、真实集成测试 `docker_environment` 与单元测试均不声明 registry pull 或认证能力。

### 未发布：环境准备计划与稳定标识

- 新增 `EnvironmentPlan`/`PrepareStep`：有序、带 pinned version 的准备步骤，使用分开的 argv、逐步超时和输出上限；未固定步骤在连接 Docker 或分配容器前以 `PlanError::UnpinnedInput` 拒绝。
- `EnvironmentPlan::execute(SandboxConfig)` 通过既有 `Sandbox::create`/`start`/`exec`/`destroy` 执行真实准备，成功或失败都等待实例与 staging 清理确认；失败步骤保留 index、argv、exit code 和有界 stdout/stderr。取消时由 lifecycle Drop 撤销继续清理。
- 新增 `EnvironmentId`/`TenantDomain`：SHA-256（格式版本 1）对 image ID、repo digests 集合、平台 os/arch/variant、有序 argv/pinned version 和显式 tenant domain 派生不透明十六进制标识；不缓存、不持久化，不包含宿主路径、endpoint、凭据或环境变量。
- 新增示例 `environment_identity`、真实集成测试 `docker_environment_prep`，并扩展 `docker_environment`；身份派生测试不需要 Docker。

### 未发布：Docker 工作区文件快照

- 新增 `capture_workspace_snapshot` 和 `restore_workspace_snapshot`：通过受限 guest helper 枚举运行中 tmpfs 的普通文件/目录（每项逐文件读取），并替换新建 sandbox 的启动暂存区；复用 `Arc<CodeSnapshot>`，不复制队列中的快照载荷。Docker archive API 对 `--tmpfs` 只返回挂载点本身，因此不能用于捕获。
- 运行态容器保持运行：`docker exec` 在暂停容器上会被拒绝，因此捕获是活动工作区的尽力一致读取，不是冻结检查点；清单与逐个文件长度校验在并发修改时失败关闭。已暂停的 sandbox 会短暂恢复枚举后重新暂停。独立限制清单传输、文件载荷与条目数量，预算按条目数、单文件上限、总字节的顺序在读取任何文件前检查；不安全节点与非可移植名以类型化快照错误失败并保留可用 sandbox。恢复使用目标配置限制。清理失败保留所有权，完整语义见[后端文档](docs/docker-linux-runc.md)。
- 不代表进程、内存、VM checkpoint 或 Docker commit，不声明本次改动已经通过真实 Docker 验证。

### 未发布：显式授权的代码副本与固定提交 Git 检出

- 新增只读 `CodeSource`，将调用方显式授权的源目录逐文件复制到新建的能力工作区；原有内存 `CodeSnapshot` 不变。
- 使用单文件限制与单次内容/条目预算，拒绝链接、特殊节点和不可移植名称；清理失败保留部分工作区所有权以供重试。
- 新增受控宿主 Git 进程、固定 40/64 位提交校验、代码-only 工作区复制和 `GitCleanup` 清理回执；恢复环境隔离边界与清理责任不适用项。
- 新增 `GitCheckout::create_worktree` 与独立 `GitWorktree`：固定提交的 detached worktree、代码-only 复制、并发 worktree 与幂等清理；`--force` 删除脏 worktree 会丢弃其未提交修改，不删除源仓库、主检出或其他 worktree，也不声明进程/磁盘隔离。
- 新增真实临时目录公开 API 测试和 `code_copy`、`git_clone`、`git_worktree` 示例；不声明 Git 进程隔离、池、运行时隔离、一致快照或未来写入总配额。

### 未发布：预启动 Docker 租约池

- 新增 `DockerPool`/`PoolLease`：固定容量内预启动新鲜容器，租约单次使用，释放、Drop、TTL 到期或 shutdown 都走真实销毁与确认回收。
- 新增 `PoolConfig::new(backend, sandbox, capacity, baseline, lease_ttl)`、显式 acquire 超时、FIFO 等待者、容量统计与 `CleanupReceipt` 归属观察；不代表聚合宿主资源预留或进程检查点。

### 未发布：声明依赖架构门禁

- 新增 `npm --prefix circus-sandbox/qa run architecture` 与 Windows/Linux CI，验证真实工具报告的跨架构域 `uses` 边满足固定矩阵。
- 固定 cargo-modules 0.28.0 与 dotparser 1.1.1；避免内建 `--acyclic` 把合法 getter 误判成环，保留失败关闭与未知命名空间拒绝。
- 这是声明依赖的补充检查，不是完整函数调用图或运行时隔离证明；具体范围见[工程说明](docs/engineering.md#架构域门禁)。

### 未发布：资源请求值模型

- 新增 `runtime::ResourceLimits` 与类型化 `ResourceLimitsError`，校验单个请求，不声称执行后端或隔离已实现。
- 内存/工作区为正字节数且不超过 `i64::MAX`；CPU 为正 milli-CPU（`1000` 等于一个 CPU），进程数为正。工作区不超过内存，单文件不超过工作区；单文件上限范围为 `1..usize::MAX`，拒绝 `0` 和 `usize::MAX`，与 `Workspace` 一致。
- 错误区分零值、超出有符号数值范围、`InvalidMaxFileBytes`、`WorkspaceExceedsMemory` 和 `MaxFileExceedsWorkspace`，不施加 Docker 或宿主容量约束。不做聚合资源预留/准入，也没有内核强制或总磁盘配额。
- 新增真实示例：`cargo run --locked --example resource_policy`；构造合法请求并观察冲突请求的类型化拒绝。

### 0.1.0

以下记录 0.1.0 workspace 文件能力的初始范围，不是当前检出全部 API 的清单；当前新增能力与边界见上方未发布记录。发布与验证记录以对应提交和发布说明为准。

可用：

- `Workspace::new`、`write_file`、`read_file`、`create_dir_all`、`remove_file`、`cleanup`。这些文件方法阻塞调用线程；独立 Docker API 是异步的。
- 临时目录由库自己创建。公开 API 不接受宿主目录，也不返回工作区根路径。
- 空路径、绝对路径、`..`、反斜杠和 Windows 盘符/UNC 前缀在打开前拒绝。通过后的访问从 cap-std 目录句柄出发。
- 读和写使用同一个 `max_file_bytes`，含义是单个文件的上限，不是工作区总字节、文件数或宿主磁盘配额。超限写入在创建或截断之前失败。读取不按不可信 metadata 分配；超限时 `actual` 是探测到的长度，不是未知总长。当前没有总配额 API。
- `cleanup` 成功后文件操作返回 `Closed`，并可重复调用。清理失败保留工作区，可以再试。`Drop` 只尽力删除，不返回错误。

限制：workspace 本身不提供命令执行、Git 或沙箱池；当前支持的快照仅为上述进程内代码副本，不支持容器/VM 快照。Docker + runc 后端提供独立的异步命令执行和生命周期 API，具体范围见 [Linux Docker + runc](docs/docker-linux-runc.md)。

- `write_file` 不是原子替换，也没有单独的原子写入 API。只有关闭、非法路径、超限、缺失父目录和非普通文件这类前置失败保证不改文件。打开普通文件之后的 I/O 失败可能截断已有文件，或留下部分内容。
- `remove_file` 按 cap-std unlink 删除非目录项，包括符号链接、FIFO 和 socket。不跟随符号链接目标，不删除目录，删除前也不做类型预检。
- 同一 `Workspace` 的并发读写由内部 `RwLock` 保护。这不是外部进程隔离，也不是崩溃安全的原子写。
- 独立 workspace 没有总字节或宿主磁盘配额；Docker 后端的资源设置需要另行显式配置。

## 许可证

本 crate 以 [Apache License 2.0](LICENSE) 授权。依赖保持各自的许可证。本目录没有、也不编造 `NOTICE` 文件。
