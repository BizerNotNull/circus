# Sandbox Guidelines

## 状态与边界

`circus-sandbox` 是 Rust library crate（包名 `circus-sandbox`，edition 2024）。当前提供 workspace 文件能力、`runtime::ResourceLimits` 资源请求校验，以及两个明确限定的本地 Linux 异步容器后端：Docker Engine + `runc`，和 containerd + `runc`（`io.containerd.runc.v2`、`overlayfs`）。workspace 文件操作在 tempfile 新建的临时目录上，用 cap-std 目录句柄做相对路径读写；资源模型只校验请求值，后端负责显式配置和生命周期，不代表通用进程隔离或 Paseo 接入。

已实现：

- `Workspace` 同步 API：`new`、`write_file`、`read_file`、`create_dir_all`、`remove_file`、`cleanup`。方法会阻塞调用线程。
- `CodeSource::open` 只接受可信库调用方显式授权的源目录读取 grant；路径不是不可信租户请求的授权凭证。根打开遵循宿主路径解析，之后子项访问通过只读 cap-std 目录能力约束，不暴露源根路径。`copy_to_workspace(max_file_bytes, SnapshotBudget)` 逐文件复制到新建的自有 `Workspace`，源不被修改；不是接管或挂载宿主目录，也不改变 `Workspace::new` 的边界。
- 复制预算仅约束本次内容字节和文件/目录条目（根不计、空目录和零字节文件计入），精确等于上限允许；不是后续写入配额。拒绝源子项链接、非普通节点和不可移植名称，不按不可信 metadata 分配整个源树。`CodeCopyError` 清理失败分支保留部分工作区所有权以便重试。同步复制不保证源被外部修改时的一致快照或崩溃原子性；没有 Git metadata 特殊策略或隐式凭据共享。
- 既有 `CodeSnapshot` 是 `Workspace` 到进程内的内容捕获与新工作区恢复；不是外部源复制、进程 checkpoint、持久化快照或 Docker/pool 能力。不得重复实现或将其改名为外部代码副本。
- 临时工作区由 tempfile 新建，不接受宿主目录，公开 API 不暴露工作区根路径。
- 跨平台拒绝空路径、绝对路径、`..`、Windows 盘符/UNC 前缀和反斜杠；通过后的访问走 cap-std，防止符号链接逃逸和基于路径字符串的 TOCTOU。
- 读写使用同一个 `max_file_bytes`。它是单个文件的上限：`new` 拒绝 `0` 和 `usize::MAX`，以便读取 `limit + 1` 时不溢出，也不按该上限预分配。它不是工作区总字节配额、文件数配额或宿主磁盘配额，当前也没有总配额 API。写满临时目录是宿主磁盘耗尽，不是 `LimitExceeded`。不要把这个参数写成沙箱配额，也不要在本 crate 增加总配额 API。后续真实进程或容器后端必须显式限制资源。
- `write_file` 不是原子替换。已关闭、非法路径、超限、缺失父目录和非普通文件在截断前返回，这些失败不修改已有文件。打开普通文件后先把长度设为 0 再写入；此后的 I/O 失败可能截断或留下不完整内容。不要写成原子写入，也不要为此增加原子 API。
- Unix 上 `read_file` / `write_file` 通过 cap-std `OpenOptions` 设置 `nonblock(true)` 再打开，因此 `open` 本身不等待 FIFO 对端。句柄级 `is_file` 在任何读取或 `set_len`/写入之前执行。路径 metadata 预检不是这个修复：工作区内指向 FIFO 的符号链接，以及打开前普通文件被换成 FIFO，都不得阻塞，并返回 `NotRegularFile`（保留调用方路径）。指向普通文件的工作区符号链接仍跟随。逃出工作区的符号链接仍是 cap-std 的 I/O 错误，不是新错误变体。Windows 不设置该标志；CreateFile 保持 write+create 且不预先 truncate，读取是只读打开。目录、FIFO、设备及其他非普通节点，包括指向它们的工作区符号链接，都是 `NotRegularFile`。
- `cleanup` 可重复调用；成功后文件操作返回 `Closed`。清理失败保留工作区归属，可重试，且不会标成已关闭。`Drop` 只做尽力清理，错误只有显式 `cleanup` 能观察到。
- `remove_file` 是 cap-std `Dir::remove_file`：对最终组件 unlink，不跟随该符号链接。非目录项都会删，包括普通文件、符号链接、FIFO 和 socket。目录返回 `Io` 且保留。文档若写成“只删除普通文件”，纠正文档；不要加删除前的类型预检，那会和 unlink 竞态。`NotRegularFile` 只属于读写。
- 同一 `Workspace` 的并发读写由内部 `RwLock` 保护。这只序列化本进程内对这一个值的调用，不是外部进程隔离，也不是 crash-safe 原子写。不要把这把锁写成原子替换或磁盘配额，也不要为此增加原子写入 API。
- `runtime::ResourceLimits` / `ResourceLimitsError` 是后端中立的资源值模型，无 `Default`。内存/工作区为正字节数且不超过 `i64::MAX`，CPU 为正 milli-CPU（`1000` 等于一个 CPU），进程数为正；工作区不超过内存，单文件上限不超过工作区。单文件上限为 `1..usize::MAX`，拒绝 `0` 和 `usize::MAX`，与 `Workspace::new` 一致。标量错误先于关联错误。校验不提供聚合准入/预留、内核强制或隔离，不附加宿主容量或 Docker 专用约束。
- `Workspace::capture_code_snapshot` 使用字节/条目预算捕获普通文件和目录，拒绝符号链接与特殊节点；`CodeSnapshot::restore` 创建独立工作区。这是进程内存中的有界代码快照，进程退出后不持久存在，不是容器/VM 文件系统、进程或客户机内存检查点。
- `Sandbox::spawn_process(ProcessRequest)` 是每沙箱一个的交互进程会话：显式 argv、相对 workspace 工作目录、显式环境覆盖、生命周期超时和每流输出上限。`ProcessSession` 的 stdin、有序 stdout/stderr 块、退出通知和取消彼此独立；文件、status、pause/resume 和 destroy 在进程运行期间仍可用。请求在入队前校验（`SandboxError::InvalidProcessRequest`），已有进程时再次 spawn 或一次性 `exec` 返回 `SandboxError::ProcessActive`。正常退出保留容器与 workspace；超时、输出超限、传输失败、会话 Drop 或 `cancel` 请求整体容器清理并通过 `ProcessExit::Terminated` 报告观测到的 `CleanupReport`。I/O 任务不删除容器、不修改 lifecycle 状态；它不是 TTY、进程组信号 API、分离式恢复或持久化机制。

仍未实现，不得写成现有能力：Kubernetes、gVisor、Kata Containers、Firecracker、RPC、registry 拉取与认证、容器/VM 运行时快照（进程、客户机内存检查点）、工作区总配额执行、宿主磁盘配额、Paseo 接入。已实现且可写为现有能力：`Workspace`、`CodeSource` 独立代码副本、进程内 `CodeSnapshot`、Docker/containerd 工作区文件快照与执行、固定提交 Git clone 与受控 worktree、本地 OCI 镜像解析、有界预启动租约池、交互进程会话、后端中立统一 API，以及默认启用 `http` feature 的认证 JSON HTTP 传输（端点见 [HTTP API](docs/http-api.md)）。它们都不是隔离、持久化或聚合资源预留的证据。当前 Docker / containerd slice 的范围和限制见 [Linux Docker + runc](docs/docker-linux-runc.md)、[containerd + runc](docs/containerd-runc.md) 及 [ADR-0001](docs/adr-0001-docker-linux-runc.md)。不修改 `../circus-paseo/`。

用户说明见 [README.md](README.md)，工程命令与门禁边界见 [docs/engineering.md](docs/engineering.md)，完整目标及交付记录入口见 [docs/delivery-plan.md](docs/delivery-plan.md)，安全扫描约定见 [安全说明](../.github/sandbox-security.md)。当前检出已有 Workspace crate、许可证策略、元数据/质量/安全/Rust 工作流，但开发约定和工作流存在仍然不是“全部领域已交付”或检查已通过。

## Architecture & Data Flow

当前调用路径包括：workspace 调用方 → `circus_sandbox::Workspace` → cap-std `Dir`，资源请求调用方 → `runtime::ResourceLimits`，以及容器调用方 → `api` → `lifecycle` → 私有 `infrastructure::Backend` 边界 → Docker Engine 或 containerd。交互进程调用方 → `Sandbox::spawn_process` → `lifecycle::process` → 后端 attach 传输；I/O 任务只搬运 stdin/stdout/stderr 和退出状态。后端不泄露 bollard、containerd-client 或 tonic 类型，所有状态修改与容器删除由 lifecycle worker 串行拥有。

尚未实现的目标链路（Kubernetes、容器/VM 运行时快照、RPC 等）仍然只是规划，不得按目录存在推断能力；Docker/containerd 后端、预启动池、工作区文件快照、Git 准备、交互进程会话与 `src/http` 传输已实现。`src/http` 只做认证、路由、DTO 转换和租户注册表，所有状态修改仍进入 `lifecycle`。

| 目录                  | 职责与边界                                                                                                                                                                                                                                                                                                                                                                                                                                              |
| --------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `src/api/`            | 统一创建、执行命令、文件操作、销毁入口；不泄露后端类型，不预设 HTTP、RPC 或进程内传输。                                                                                                                                                                                                                                                                                                                                                                 |
| `src/lifecycle/`      | 创建、启动、运行、暂停、恢复、销毁的状态转换与资源回收；状态只能由此修改。                                                                                                                                                                                                                                                                                                                                                                              |
| `src/infrastructure/` | Docker 与 containerd 适配器（均已实现），以及 Kubernetes 等未来部署适配；负责资源分配，不承担隔离机制实现。                                                                                                                                                                                                                                                                                                                                             |
| `src/runtime/`        | 已实现后端中立的资源请求值校验，不执行资源限制。当前 Docker / containerd + runc 隔离策略由 `infrastructure`/`lifecycle` 显式配置并验证；Kubernetes、gVisor、Kata Containers、Firecracker 等仍未实现。                                                                                                                                                                                                                                                   |
| `src/environment/`    | OCI 镜像、基础环境、依赖准备与可复现环境标识。                                                                                                                                                                                                                                                                                                                                                                                                          |
| `src/workspace/`      | 已实现：能力目录内的相对文件读写、建目录、删除与回收，进程内 `CodeSnapshot`，显式授权源的 `CodeSource` 独立代码副本，固定提交 `GitCheckout` 与自有 `GitWorktree`。Git 准备是受控可信宿主进程，不是进程或文件系统隔离。                                                                                                                                                                                                                                  |
| `src/pool/`           | 后端中立的 `SandboxPool`（Docker 与 containerd 通用）：预启动、快照、沙箱池；复用生命周期与回收规则，禁止跨租用泄露代码或凭据。                                                                                                                                                                                                                                                                                                                         |
| `tests/`              | `tests/workspace.rs` 验证公开 workspace API；`tests/docker_sandbox.rs`、`docker_process.rs`、`docker_hardening.rs`、`docker_snapshot.rs`、`docker_pool.rs`、`docker_environment*.rs`、`git_*` 与 `agent_image.rs` 都是默认忽略、仅连接显式真实 Docker Engine（或真实宿主 Git）的集成测试；`containerd_sandbox.rs`、`containerd_snapshot.rs`、`containerd_process.rs`、`containerd_pool.rs` 默认忽略，仅在 `CIRCUS_CONTAINERD=1` 时连接真实 containerd。 |

Kubernetes、池与容器/VM 运行时快照仍保留为规划目标；已有内存代码快照不等于这些目标已完成。当前 Docker 与 containerd + runc 适配器支持真实容器路径，禁止用空适配器声称支持。运行时必须先消费 workspace 文件契约再开发。

## Code Conventions & Common Patterns

- Rust 模块、函数用 `snake_case`，类型与 trait 用 `PascalCase`；按职责内聚，不预先拆成七个 crate。
- API 使用领域请求、结果和错误；预期失败返回 `Result`，禁止用 panic 处理外部输入或吞掉清理失败。
- 生命周期显式建模合法转换；失败与取消必须有资源归属及回收路径，销毁应可重复调用。
- 通过构造参数注入后端、配置及外部服务；trait 只用于真实替换边界，不引入全局可变状态。
- 当前 workspace API 是同步且阻塞的。将来从异步上下文调用时必须 `spawn_blocking`；计划中的异步运行时仍只有 Tokio，不引入 async-std/smol 或第二套运行时。取消与超时不得遗留孤儿资源。阻塞任务开始后不能靠 abort 终止。
- 隔离级别、资源限制和权限由显式配置决定；禁止静默降级隔离，禁止默认把宿主目录或凭据暴露给沙箱。

## Rust 工具链基线

选型核对日期：2026-10-24。开发基线仍是 Rust 1.99.1 stable、edition 2024。本目录已有 `rust-toolchain.toml` 与 `Cargo.toml`；这不表示每台开发机已经安装该工具链。

- 本目录的 `rust-toolchain.toml` 必须保持下列内容。Cargo、rustc、标准库、rustfmt、Clippy 使用该发布配套组件，不单独升级或混用版本。管理器采用 rustup 稳定版；其版本不替代项目编译器锁定。

  ```toml
  [toolchain]
  channel = "1.99.1"
  profile = "minimal"
  components = ["rustfmt", "clippy"]
  ```

- `Cargo.toml` 显式设置 `edition = "2024"`、`rust-version = "1.99.1"`、`license = "Apache-2.0"`。crate 禁止 unsafe。提交由 Cargo 生成的 `Cargo.lock`，不手写、不手改锁文件来绕过解析。构建使用 `--locked`，依赖 feature 按需开启，避免默认启用 `full`。多 crate 出现后再统一 workspace 依赖，不预建 workspace。
- Windows 本机开发使用 `x86_64-pc-windows-msvc`，需要 Visual Studio Build Tools 的 C++ 工具与 Windows SDK；Linux 隔离后端在 Linux 主机/虚拟机验证，不能把 Windows 编译成功视为隔离通过。仅为实际支持的平台安装 target；编辑器按需安装 rust-analyzer，不把它作为构建依赖。
- 工具链升级通过显式修改版本、核对依赖 MSRV、运行受影响检查及真实后端 smoke 验证完成；不自动追踪最新版本。[rustup 配置依据](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)。

## 第三方库选型与去重

以下是按职责确定的默认选择。只有实现需要该能力时才加入 `Cargo.toml`。当前直接依赖包括 [`cap-std` 4.0.3](https://github.com/bytecodealliance/cap-std)、[`cap-fs-ext` 4.0.3](https://github.com/bytecodealliance/cap-std)（Unix 上 `OpenOptionsSyncExt::nonblock`，不开 default features）、[`tempfile` 3.x](https://docs.rs/tempfile/latest/tempfile/)、[`thiserror` 2.x](https://github.com/dtolnay/thiserror)、[`axum` 0.8](https://github.com/tokio-rs/axum)、`serde`/`serde_json` 1.x 与 [`base64` 0.22](https://docs.rs/base64/latest/base64/)（均为非预发布）；axum、serde、serde_json、base64 由默认启用的 `http` feature 控制，关闭时这些可选依赖不编译。测试使用 `reqwest` 0.12 的 HTTP 客户端，仅启用 JSON、不启用 TLS。表中其余库仍未引入。优先标准库，其次选择有持续维护、公开源码和实际采用的库；不为降低流行度放弃成熟生态，也不以 stars 或下载量代替性能证据。crate 版本在首次引入时选择兼容上述工具链的较新非预发布版，核对 MSRV、许可证与安全公告，并由 `Cargo.lock` 固定实际解析版本。

| 类别                 | 默认选择                                                                                                                 | 选择依据与使用边界                                                                                                                                                                                  |
| -------------------- | ------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 异步 I/O、任务与同步 | [`tokio` 1.x](https://tokio.rs/tokio)                                                                                    | 统一运行时、计时器、进程和异步同步原语，避免跨运行时适配开销；不同时引入 async-std/smol。阻塞任务开始后不能靠 abort 终止，生命周期必须负责回收。                                                    |
| 协作取消             | [`tokio-util::sync::CancellationToken`](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html) | 需要跨任务取消时引入；与 Tokio 互补，不另建取消框架。取消通知不等于资源已经回收。                                                                                                                   |
| 序列化模型           | [`serde` 1.x](https://serde.rs/)                                                                                         | 统一结构化数据的 Serialize/Deserialize；优先具体类型与借用，避免无必要的中间树和复制。                                                                                                              |
| JSON 编解码          | [`sonic-rs`](https://github.com/cloudwego/sonic-rs)                                                                      | 性能优先的默认 JSON 实现，复用 Serde 模型；SIMD 优化主要面向 x86_64/aarch64。采用安全解析 API，不为吞吐绕过 UTF-8 校验；兼容边界见下文。                                                            |
| 领域错误             | [`thiserror` 2.x](https://github.com/dtolnay/thiserror)                                                                  | 为显式错误枚举生成标准 Error 实现，不额外引入动态错误封装；公开 API 保留可匹配的错误类型及来源链。                                                                                                  |
| 结构化日志与追踪     | [`tracing`](https://github.com/tokio-rs/tracing)                                                                         | 事件与 span 统一入口；实际可执行入口按需配套 tracing-subscriber，库模块不设置全局 subscriber。过滤未启用日志，避免预先格式化字段；不再并用独立日志框架。                                            |
| 出站 HTTP            | [`reqwest`](https://github.com/seanmonstar/reqwest)                                                                      | 复用连接池 Client，采用异步接口；只为独立出站请求引入，不重写后端 SDK 的 HTTP 层。裁剪 TLS/features，并限制不可信 URL、重定向与代理的权限边界。                                                     |
| CLI（需要时）        | [`clap` 4.x](https://docs.rs/clap/latest/clap/)                                                                          | 有命令行入口时才引入，统一解析与帮助信息；纯库不依赖 CLI 框架。性能侧关注启动时间与二进制体积，而非请求吞吐。                                                                                       |
| 临时目录             | [`tempfile` 3.x](https://docs.rs/tempfile/latest/tempfile/)                                                              | 管理临时工作区；检查权限与清理失败，不能只依靠 Drop 处理崩溃后的残留，也不能把随机路径当租户隔离。                                                                                                  |
| 受限文件访问         | [`cap-std`](https://github.com/bytecodealliance/cap-std) 与 [`cap-fs-ext`](https://github.com/bytecodealliance/cap-std)  | 从已授权目录句柄做相对访问；避免“字符串检查后再打开”的路径竞态。`cap-fs-ext` 只提供 cap-std 未放进 `std` 形状的打开标志（当前是 Unix nonblocking open）。它不替代进程隔离、挂载策略或租户权限控制。 |
| 底层 Linux 系统接口  | [`rustix` 1.x](https://docs.rs/rustix/latest/rustix/)                                                                    | 仅当标准库/能力式文件 API 缺少所需 syscall 时使用；隔离在 runtime 适配器中，不再并用 nix 提供同一接口，检查内核能力且不静默降级。                                                                   |
| Docker Engine 适配   | [`bollard`](https://github.com/fussybeaver/bollard)                                                                      | 仅实现 Docker 后端时引入异步 SDK，不重复手写 Docker REST 客户端；daemon socket 属高权限管理面，禁止暴露给沙箱。                                                                                     |
| Kubernetes 适配      | [`kube`（kube-rs）](https://github.com/kube-rs/kube)                                                                     | 仅实现 Kubernetes 后端时引入；按需开启 client/runtime，限定凭据与 RBAC，不因 SDK 存在而引入控制器或第二套调度状态。                                                                                 |
| Git、普通集合与配置  | 标准库与宿主 Git；暂不加专用 crate                                                                                       | Git clone/worktree 使用受控 Git 进程，参数分开传递，不拼接 shell；异步路径使用 Tokio 进程 API 并明确终止/等待责任。集合与同步先用 std/Tokio；配置格式及持久化未定，不预引入配置框架、数据库或 ORM。 |

`http` feature 的 JSON 传输使用 axum 0.8 的 `json` feature 与 serde_json；axum 的 `Json` 提取/响应与本 crate 的 DTO 解析共用 serde 模型，bollard 的依赖树中也已有 serde_json，因此不引入 sonic-rs 作为第二条业务 JSON 路径。axum 只做认证、路由与 DTO 转换，默认拒绝匿名，不把后端类型带入传输模型；关闭 `http` feature 时 axum、serde、serde_json 与 base64 不编译。

### 性能依据与兼容边界

- “性能最好”限定为目标平台、真实数据与相同语义下的测量结果。workspace 文件 API 可以运行，但尚无进程或容器沙箱，**没有本项目最快库的实测结论**。表中选型综合性能机制、维护与生态适配；网络/后端 SDK 不存在可脱离负载比较的统一性能排名。
- JSON 的具体取舍：sonic-rs 上游 README 提供 Xeon Platinum 8260 上 twitter/citm_catalog/canada 数据的基准，安全 `from_slice` 在这些结构体解析用例中快于 serde_json/simd-json；这是**上游结果，非本项目测量**。调研时仓库约 937 stars，是有实际采用但非最大众的性能型选择；stars 仅作采用度信号。[基准与运行条件](https://github.com/cloudwego/sonic-rs#benchmark)。
- sonic-rs 并非 serde_json 的无条件替代：非类型化 Value 的重复键行为存在差异，SDK 暴露的 serde_json 类型不能直接互换。[兼容说明](https://github.com/cloudwego/sonic-rs/blob/main/docs/serdejson_compatibility.md)。业务 JSON 当前经 serde_json 使用（axum `json` feature 与 DTO 解析必需）；sonic-rs 未引入，若未来引入必须整体迁移而非并存。
- 不把上游建议的 `-C target-cpu=native` 无条件用于分发产物；构建机与部署 CPU 可能不同，须明确目标指令集并验证可移植性。首次实现 JSON 路径时测量真实负载的吞吐、分配与尾延迟，同时验证重复键、数值精度、非法 UTF-8 和资源上限；不靠 unchecked API 或放松语义获得“更快”。

### 同类第三方库约束

- **尽可能避免使用两个同一类的第三方库：同一职责默认只保留一个直接依赖与一套业务使用方式。** 新增前检查现有依赖、标准库和已选生态能否满足；替换时迁移所有调用点并删除旧直接依赖，不保留双实现或兼容别名。
- 配套且职责互补的 crate 不算重复，例如 serde + sonic-rs、tracing + tracing-subscriber、tokio + tokio-util；后端 SDK 内部的 HTTP/JSON 实现也不等于允许业务层任意并用同类库。
- 无法消除的 SDK 类型约束、平台差异或传递依赖例外，必须在引入说明中记录原因、使用边界及去重不可行的证据；不能为去重破坏第三方兼容性，也不能以“传递依赖已存在”为由扩大直接使用。用 `cargo tree --duplicates` 与 `cargo tree -e features` 检查重复版本和 feature 来源；不同名称的同类库还需人工检查，命令输出不自动证明无重复。

## Runtime, Commands & QA

- Rust 核心使用上述 Cargo 工具链，不继承 Paseo 的工具链版本。服务入口尚未配置。`.github/workflows/sandbox-rust.yml` 显示名为 `Sandbox Rust`，检查名为 `rust (ubuntu-latest)`、`rust (windows-latest)`、`dependencies` 和 `codeql`。没有路径过滤，不调用 Paseo CI；`main` push、PR 的 `opened`/`reopened`/`synchronize`、merge queue 和 `workflow_dispatch` 都会启动。结果只记在对应 PR，不在本文声称当前提交已通过。命令在 `circus-sandbox/` 执行，rustup 读取本目录 `rust-toolchain.toml`。工作流存在不是服务可运行、隔离通过或分支保护已配置。`.github/workflows/sandbox-architecture.yml` 使用同一组触发条件，检查名为 `architecture (ubuntu-latest)` 与 `architecture (windows-latest)`。`.github/workflows/sandbox-docker.yml`（`Sandbox Docker`）只在 `pull_request` 与 `workflow_dispatch` 触发，在 GitHub 的 `ubuntu-latest` 上运行八个真实 Engine 忽略套件；它不要求本地 Docker，也不运行 Paseo。
- 本 crate 不允许 unsafe。库只导出已实现的 `Workspace` 文件 API 和 `runtime` 资源值模型，不预建其他空模块，也不创建空壳二进制来假装后端可用。
- 格式检查：`cargo fmt --check`（写回用 `cargo fmt`）。
- Clippy：`cargo clippy --locked --all-targets -- -D warnings`。
- 构建：`cargo build --locked`。
- 测试：`cargo test --locked`。公开文件 API 集成测试是 `cargo test --locked --test workspace`。连示例一起编译用 `cargo test --locked --all-targets`。
- 代码副本公开 API 测试：`cargo test --locked --test code_copy`。使用真实源临时目录，覆盖独立副本/源不变、嵌套内容、精确单文件/总字节/条目预算、空目录/零字节文件、符号链接逃逸、Unix 非普通节点与不可移植名称；Windows 链接权限不足必须失败，不能静默跳过。
- 文档：`RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps`。与 CI 的文档检查一致。
- 文件示例：`cargo run --locked --example workspace`，必须实际写、读、删除并 `cleanup`，打印读到的字节，不能打印与 `Result` 无关的成功字样。资源示例：`cargo run --locked --example resource_policy`，构造合法请求、标明单位，再观察冲突请求的类型化拒绝；它不是后端 smoke。
- 代码副本示例：`cargo run --locked --example code_copy`，创建显式授权的临时源，读取目的工作区实际字节，显式清理目的工作区及源。`CodeSource` 打开与复制同样阻塞，异步调用使用 `spawn_blocking`；任务开始后不能靠 abort 终止。
- 单元测试与模块共置，可覆盖工作区内符号链接逃逸、大于上限的已有文件读取，以及清理失败后的重试。读取超限时 `LimitExceeded.actual` 是探测到的 `limit + 1`，不是未知总长。`tests/workspace.rs` 验证写上限、恰好等于上限的读取、非法路径、嵌套目录/删除、重复 `cleanup` 与 `Closed`；`tests/code_copy.rs` 则通过公开授权源 API 建立超限文件和特殊节点。符号链接用例的必要前提是进程能创建符号链接。Windows 需要开发者模式，或持有 `SeCreateSymbolicLinkPrivilege`。缺少该权限时对应用例必须失败，不能把 `PermissionDenied` 当成跳过并静默返回成功。不要修改宿主安全设置、组策略或开发者模式来让测试通过。容器或虚拟化测试须显式标注环境要求并隔离资源。
- 架构域门禁：先安装 `cargo-modules` 0.28.0，再在仓库根目录执行 `npm --prefix circus-sandbox/qa ci` 和 `npm --prefix circus-sandbox/qa run architecture`。脚本必须用真实的 `cargo modules dependencies --lib --all-features --no-externs --no-owns` DOT，验证工具精确为 0.28.0，按语义 ID 和声明 kind 投影到顶层域，接受同域 getter/impl 递归，拒绝固定矩阵之外的跨域 `uses` 边。不得用 `--acyclic`、`--no-fns`/`--no-types`、手写 Rust 源码解析或忽略 analyzer 失败来绕过。矩阵、根门面规则和限制只在[工程说明](docs/engineering.md#架构域门禁)维护，不在本文件复制第二份。
- 架构门禁只证明当前 crate 的跨域 `uses` 边满足 allowlist DAG；它不证明外部 SDK 暴露面，也不检查域内循环。`cargo-modules` 是 MPL-2.0 开发期 CLI 工具，`dotparser` 1.1.1 是 MIT 的 QA 依赖，都不是 crate 的运行时依赖。
- 启动优化必须测量冷启动与复用路径，并验证复用不泄露状态。没有覆盖率百分比门槛。实际后端行为必须有运行证据，不能仅靠 mock；workspace 文件测试使用真实临时目录。Windows 编译成功不能当成隔离或 Linux 后端通过，也不能把缺少符号链接权限时的静默跳过当成符号链接约束已验证。
- 质量门禁继续使用 `circus-sandbox/qa` 的现有 checker 与锁文件，Node.js 固定 24.18.0。手写 `.rs` 单文件不超过 1200 行（含 1200）。文档白名单、QA 回归测试、npm audit、actionlint、`CodeQL QA` 和安全门禁的精确范围统一见工程说明；不重新引入 markdownlint-cli2/braces，不把 QA 当作 Rust 或隔离测试。
- `sandbox-metadata.yml` 的 `PR title` 独立检查标题格式；只监听 PR 的 `opened`、`reopened`、`synchronize`、`edited`，不安装依赖，标题经环境变量传给 CLI 且不回显。仅编辑标题不启动其他沙箱工作流的重型检查。标题语法与提交流程见根指南；QA 测试入口同时覆盖 checker、标题验证器和架构域夹具。
- 合并使用 PR，不要求非作者批准；任务明确授权后可在最新 CI 通过、适用审查问题闭环且无冲突时自动 squash merge，不使用管理员绕过或强推。提交流程见根指南，安全扫描范围见安全说明。

### CI 检查内容

- `rust`：两个平台都在 `circus-sandbox/` 安装 Rust 1.99.1（minimal，含 rustfmt 与 clippy），然后跑 `cargo fmt --check`、`cargo clippy --locked --all-targets -- -D warnings`、`cargo build --locked`、`cargo test --locked`、`RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps`、`cargo run --locked --example workspace` 和 `cargo run --locked --example resource_policy`。`fail-fast` 为 false。这是源码级检查，不是容器或进程隔离验证。
- `dependencies`：只在 `ubuntu-latest`。`cargo install --locked --version 0.20.2 cargo-deny`，然后 `cargo deny --locked check`。本地复现使用同一安装版本，并在 `circus-sandbox/` 执行检查。
- `codeql`：只在 `ubuntu-latest`。语言 `rust`，`build-mode: none`，配置 `.github/codeql/sandbox-rust.yml`。只分析 `circus-sandbox`，忽略 `circus-paseo` 与 `circus-sandbox/target`。它不编译 crate，不能代替 `cargo test`。
- `architecture (ubuntu-latest)` / `architecture (windows-latest)`：在 `circus-sandbox/` 安装 Rust 1.99.1、在 QA 安装 Node.js 24.18.0，然后 `cargo install --locked --version 0.28.0 cargo-modules`、`npm --prefix circus-sandbox/qa ci`、`npm --prefix circus-sandbox/qa run architecture`。作业超时 20 分钟，`fail-fast` 为 false，无 Docker 要求。实际运行器先验证 `rustc` 与 `cargo-modules` 版本，再由脚本解析真实 DOT；失败、空图、错误 crate、未知顶层命名空间或违禁跨域边都使检查失败。
- `docker (<suite>)`：`.github/workflows/sandbox-docker.yml` 只在 `pull_request` 与 `workflow_dispatch` 触发，无路径过滤；权限仅 `contents: read`。八项矩阵是 `docker_sandbox`、`docker_snapshot`、`docker_pool`、`docker_hardening`、`docker_staging`、`docker_environment`、`docker_environment_prep`、`docker_process`；`fail-fast` 为 false，作业超时 60 分钟。每个 job 在 `ubuntu-latest` 安装 Rust 1.99.1（minimal）、`docker pull python:3.12-slim`、打印 Engine 版本与镜像 ID/RepoDigests，然后以 `CIRCUS_DOCKER_SANDBOX=1`、`CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock`、`CIRCUS_DOCKER_IMAGE=python:3.12-slim`、`CIRCUS_DOCKER_ARCH=amd64` 执行 `cargo test --locked --test <suite> -- --ignored`。它只在 GitHub 托管运行器上提供真实 Engine 证据，不要求本地 Docker，也不运行 Paseo；未运行、取消或失败都不是通过声明。`agent-image` 未加入：Codex 镜像构建需要网络，不能离线构建，仍按显式 `CIRCUS_AGENT_IMAGE` 手动验收。

`deny.toml` 的许可证 allow 只有 `Apache-2.0`、`MIT`、`Apache-2.0 WITH LLVM-exception`（winx 0.36.4 只声明这一表达式，不是 Apache-2.0 的别名）、`Unicode-3.0`（unicode-ident 1.0.27 是 `(MIT OR Apache-2.0) AND Unicode-3.0`）与 `BSD-3-Clause`（matchit 0.8.4 经 axum 引入，`MIT AND BSD-3-Clause`）与 `Zlib`（foldhash 0.1.5 经 containerd gRPC 依赖树的 hashbrown 引入）。`exceptions` 为空。未列出的许可证失败。漏洞公告失败，且不得写入 ignore。yanked 为 deny，unsound 为 all，unmaintained 不忽略，unused-ignored-advisory 为 deny。来源只允许 crates.io；未知 registry 与 git 失败。通配版本 deny。`multiple-versions` 是 warn，不是 deny。

当前 `Cargo.lock` 中，不 fork 上游就去不掉的传递重复：`io-lifetimes` 2.0.4（`fs-set-times` 0.20.3）与 3.0.1（cap-std、cap-primitives、cap-fs-ext、io-extras）；`windows-sys`（Windows SDK 绑定）0.59.0（`fs-set-times`、winx 0.36.4）、0.60.2（io-extras 0.19.0）、0.61.2（cap-primitives、cap-fs-ext、rustix 1.1.5、errno）。它们来自 cap-std 4.0.3 / cap-fs-ext 4.0.3 的依赖闭包，不是本 crate 的第二套直接依赖。不要为此再引入同名 crate，也不要把这条 warn 写成检查已通过或已失败。
