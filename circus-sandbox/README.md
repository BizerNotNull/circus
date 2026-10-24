# circus-sandbox

Rust 库，包名 `circus-sandbox`。当前提供 capability-scoped workspace 文件能力、受预算约束的进程内代码快照、`runtime::ResourceLimits` 请求校验，以及显式配置的本地 Linux Docker Engine + `runc` 异步容器后端。workspace 文件操作在 tempfile 新建的临时目录里，通过 cap-std 目录句柄做相对路径的同步读写；容器后端通过私有 lifecycle/infrastructure 模块运行 Linux 容器。

容器后端不是通用进程沙箱：它只接受本地 Unix socket 或 Windows named pipe，要求调用方提供已存在且包含 Python 3 的 Linux 镜像，拒绝拉取镜像。Kubernetes、containerd、gVisor、Kata、Firecracker、OCI 环境准备、HTTP/RPC、Git、容器/VM 快照与预启动/沙箱池仍未实现；没有 Paseo 接入。这些仍属于[完整交付目标](docs/delivery-plan.md)，不能把当前 Docker + runc slice 当作全部七领域交付。

workspace API 本身不是进程隔离，也不隔离 CPU、网络、系统调用或进程；它不接受调用方传入的宿主目录，公开 API 也不返回工作区根路径。Docker 后端不改变 workspace API 的同步语义。资源请求校验不等于资源预留、聚合准入或内核强制；Docker 设置的实际效果需要真实 Linux Engine 验证，独立 workspace 仍没有总字节或宿主磁盘配额执行机制。

工具链基线是 Rust **1.99.1**、edition **2024**。直接依赖还包括 bollard **0.21.1**、tokio **1.x**、tokio-util **0.7.x**、uuid **1.x**、futures-util **0.3.x** 和 thiserror **2.0.21**；`Cargo.lock` 固定解析版本，检查和运行都加 `--locked`。workspace 仍使用 cap-std **4.0.3**、cap-fs-ext **4.0.3**、tempfile **3.27.0**。

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

容器后端的配置、生命周期、文件传输和已知限制见 [Linux Docker + runc 文档](docs/docker-linux-runc.md)，设计取舍见 [ADR-0001](docs/adr-0001-docker-linux-runc.md)。最小前置条件：Linux Docker Engine（本地 Unix socket；Windows 开发机可使用 Linux Docker Desktop named pipe）、已存在的 Linux 镜像、镜像内 Python 3，以及 cgroup v2 用于运行集成测试。镜像不会自动拉取；声明 `VOLUME` 的镜像不支持。

集成测试默认被忽略且不会连接 Docker。显式运行真实后端：

```bash
CIRCUS_DOCKER_SANDBOX=1 CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
  cargo test --locked --test docker_sandbox -- --ignored
```

测试使用 `python:3.12-slim`，可用 `CIRCUS_DOCKER_IMAGE` 指定另一个包含 Python 3 的本地 Linux 镜像。实际运行环境、命令、结果和限制应记录在对应 PR；不要把编译、忽略测试或 CI 通过写成隔离验证。

## 命令

同样在 `circus-sandbox/` 下执行：

| 目的                | 命令                                                                                            |
| ------------------- | ----------------------------------------------------------------------------------------------- |
| 格式检查 / 格式化   | `cargo fmt --check` / `cargo fmt`                                                               |
| Clippy              | `cargo clippy --locked --all-targets -- -D warnings`                                            |
| 构建                | `cargo build --locked`                                                                          |
| 测试                | `cargo test --locked`                                                                           |
| 只跑本 API 集成测试 | `cargo test --locked --test workspace`                                                          |
| 文档                | `RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps`                                         |
| 运行示例            | `cargo run --locked --example workspace`                                                        |
| 资源请求示例        | `cargo run --locked --example resource_policy`                                                  |
| 依赖策略            | 先 `cargo install --locked --version 0.20.2 cargo-deny`，再在本目录 `cargo deny --locked check` |

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

### 未发布：Docker 工作区文件快照

- 新增 `capture_workspace_snapshot` 和 `restore_workspace_snapshot`：使用 Docker archive API 捕获运行中 tmpfs 的普通文件/目录，并替换新建 sandbox 的启动暂存区；复用 `Arc<CodeSnapshot>`，不复制队列中的快照载荷。
- 运行态捕获临时暂停后恢复，已暂停的 sandbox 保持暂停；独立限制 archive 传输、文件载荷与条目数量，恢复使用目标配置限制。清理失败保留所有权，完整语义见[后端文档](docs/docker-linux-runc.md#workspace-file-snapshots)。
- 不代表进程、内存、VM checkpoint 或 Docker commit，不声明本次改动已经通过真实 Docker 验证。

### 未发布：当前能力与完整交付边界

- 同步 workspace、受字节/条目预算约束的进程内代码快照、资源请求值校验，以及显式配置的异步 Linux Docker + runc 后端是当前实现；Docker API 的生命周期、命令/文件操作与前提见[后端文档](docs/docker-linux-runc.md)。
- 工作区文件快照与尚未实现的进程/内存/VM checkpoint 明确区分；Kubernetes、containerd、gVisor、Kata、Firecracker、OCI 环境准备、Git、预启动与池仍保留在完整交付范围，没有 HTTP/RPC 或 Paseo 接入。
- 本次修订只纠正文档事实，不声明发布、合并、检查或真实运行成功。

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
