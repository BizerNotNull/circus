# Sandbox Guidelines

## 状态与边界

`circus-sandbox` 是计划用 Rust 实现的 Agent 沙箱核心。本检出只有职责目录与测试目录，没有 `Cargo.toml`、可运行入口或后端实现。并行的 `feat/capability-workspace` 才会加入首个真实 crate，且不依赖本质量基线合并；未合并前不要把它写成已有能力。开发约定不是已交付功能。Paseo 接入另行开发，本次不修改 `../circus-paseo/`。交付顺序见 [docs/delivery-plan.md](docs/delivery-plan.md)。

## Architecture & Data Flow

规划调用链：调用方 → `api` → `lifecycle` → `infrastructure` → `runtime`。`environment` 与 `workspace` 准备执行环境，`pool` 复用同一生命周期入口优化启动。后端通过适配器隔离；领域层不依赖 Docker/Kubernetes SDK。

| 目录                  | 职责与边界                                                                                |
| --------------------- | ----------------------------------------------------------------------------------------- |
| `src/api/`            | 统一创建、执行命令、文件操作、销毁入口；不泄露后端类型，不预设 HTTP、RPC 或进程内传输。   |
| `src/lifecycle/`      | 创建、启动、运行、暂停、恢复、销毁的状态转换与资源回收；状态只能由此修改。                |
| `src/infrastructure/` | Docker、Kubernetes、containerd 等部署与调度适配；负责资源分配，不承担隔离机制实现。       |
| `src/runtime/`        | runc、gVisor、Kata Containers、Firecracker 等运行时能力与隔离策略；不支持的能力明确报错。 |
| `src/environment/`    | OCI 镜像、基础环境、依赖准备与可复现环境标识。                                            |
| `src/workspace/`      | Git clone、worktree、代码副本的创建与回收；限定文件操作边界，防止路径逃逸。               |
| `src/pool/`           | 预启动、快照、沙箱池；复用生命周期与回收规则，禁止跨租用泄露代码或凭据。                  |
| `tests/`              | 未来的跨模块契约与真实后端集成测试。                                                      |

这些后端是候选目标，不代表全部同时支持；实现时按需求选择，禁止用空适配器声称支持。运行时必须先消费 workspace 文件契约再开发。池与快照仍保留，支持策略只写在交付计划。

## Code Conventions & Common Patterns

- Rust 模块、函数用 `snake_case`，类型与 trait 用 `PascalCase`；按职责内聚，不预先拆成七个 crate。
- API 使用领域请求、结果和错误；预期失败返回 `Result`，禁止用 panic 处理外部输入或吞掉清理失败。
- 生命周期显式建模合法转换；失败与取消必须有资源归属及回收路径，销毁应可重复调用。
- 通过构造参数注入后端、配置及外部服务；trait 只用于真实替换边界，不引入全局可变状态。
- 异步运行时统一使用 Tokio；阻塞 Git/文件/进程操作不得阻塞异步执行线程。取消与超时不得遗留孤儿资源。
- 隔离级别、资源限制和权限由显式配置决定；禁止静默降级隔离，禁止默认把宿主目录或凭据暴露给沙箱。

## Rust 工具链基线

选型核对日期：2026-10-24。本检出尚无 `rust-toolchain.toml` 或 Cargo 工程；以下基线在首个真实 crate 中落地。

- **Rust 1.99.1 stable、edition 2024**。采用较新的稳定补丁版，不使用浮动 `stable`、beta 或 nightly 作为构建基线。[官方发布说明](https://blog.rust-lang.org/2026/10/15/Rust-1.99.1/)与[stable 分发清单](https://static.rust-lang.org/dist/channel-rust-stable.toml)为版本依据。
- 首次创建真实 crate 时，在本目录提交以下 `rust-toolchain.toml`；Cargo、rustc、标准库、rustfmt、Clippy 使用该发布配套组件，不单独升级或混用版本。管理器采用 rustup 稳定版；其版本不替代项目编译器锁定。

  ```toml
  [toolchain]
  channel = "1.99.1"
  profile = "minimal"
  components = ["rustfmt", "clippy"]
  ```

- `Cargo.toml` 显式设置 `edition = "2024"`、`rust-version = "1.99.1"`；提交 `Cargo.lock`。构建使用 `--locked`，依赖 feature 按需开启，避免默认启用 `full`。多 crate 出现后再统一 workspace 依赖，不预建 workspace。
- Windows 本机开发使用 `x86_64-pc-windows-msvc`，需要 Visual Studio Build Tools 的 C++ 工具与 Windows SDK；Linux 隔离后端在 Linux 主机/虚拟机验证，不能把 Windows 编译成功视为隔离通过。仅为实际支持的平台安装 target；编辑器按需安装 rust-analyzer，不把它作为构建依赖。
- 工具链升级通过显式修改版本、核对依赖 MSRV、运行受影响检查及真实后端 smoke 验证完成；不自动追踪最新版本。[rustup 配置依据](https://rust-lang.github.io/rustup/overrides.html#the-toolchain-file)。

## 第三方库选型与去重

以下是按职责确定的默认选择，不是已安装的依赖清单；只有实现需要该能力时才加入 `Cargo.toml`。优先标准库，其次选择有持续维护、公开源码和实际采用的库；不为降低流行度放弃成熟生态，也不以 stars 或下载量代替性能证据。crate 版本在首次引入时选择兼容上述工具链的较新非预发布版，核对 MSRV、许可证与安全公告，并由 `Cargo.lock` 固定实际解析版本。

| 类别                    | 默认选择                                                                                                                 | 选择依据与使用边界                                                                                                                                                                                  |
| ----------------------- | ------------------------------------------------------------------------------------------------------------------------ | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| 异步 I/O、任务与同步    | [`tokio` 1.x](https://tokio.rs/tokio)                                                                                    | 统一运行时、计时器、进程和异步同步原语，避免跨运行时适配开销；不同时引入 async-std/smol。阻塞任务开始后不能靠 abort 终止，生命周期必须负责回收。                                                    |
| 协作取消                | [`tokio-util::sync::CancellationToken`](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html) | 需要跨任务取消时引入；与 Tokio 互补，不另建取消框架。取消通知不等于资源已经回收。                                                                                                                   |
| 序列化模型              | [`serde` 1.x](https://serde.rs/)                                                                                         | 统一结构化数据的 Serialize/Deserialize；优先具体类型与借用，避免无必要的中间树和复制。                                                                                                              |
| JSON 编解码             | [`sonic-rs`](https://github.com/cloudwego/sonic-rs)                                                                      | 性能优先的默认 JSON 实现，复用 Serde 模型；SIMD 优化主要面向 x86_64/aarch64。采用安全解析 API，不为吞吐绕过 UTF-8 校验；兼容边界见下文。                                                            |
| 领域错误                | [`thiserror` 2.x](https://github.com/dtolnay/thiserror)                                                                  | 为显式错误枚举生成标准 Error 实现，不额外引入动态错误封装；公开 API 保留可匹配的错误类型及来源链。                                                                                                  |
| 结构化日志与追踪        | [`tracing`](https://github.com/tokio-rs/tracing)                                                                         | 事件与 span 统一入口；实际可执行入口按需配套 tracing-subscriber，库模块不设置全局 subscriber。过滤未启用日志，避免预先格式化字段；不再并用独立日志框架。                                            |
| 出站 HTTP               | [`reqwest`](https://github.com/seanmonstar/reqwest)                                                                      | 复用连接池 Client，采用异步接口；只为独立出站请求引入，不重写后端 SDK 的 HTTP 层。裁剪 TLS/features，并限制不可信 URL、重定向与代理的权限边界。                                                     |
| HTTP 服务（确认传输后） | [`axum`](https://github.com/tokio-rs/axum)                                                                               | 确需 HTTP API 时作为唯一默认框架，复用 Tokio/Hyper 生态；当前不引入，不据此改变传输未定的架构边界。                                                                                                 |
| CLI（需要时）           | [`clap` 4.x](https://docs.rs/clap/latest/clap/)                                                                          | 有命令行入口时才引入，统一解析与帮助信息；纯库不依赖 CLI 框架。性能侧关注启动时间与二进制体积，而非请求吞吐。                                                                                       |
| 临时目录                | [`tempfile` 3.x](https://docs.rs/tempfile/latest/tempfile/)                                                              | 管理临时工作区；检查权限与清理失败，不能只依靠 Drop 处理崩溃后的残留，也不能把随机路径当租户隔离。                                                                                                  |
| 受限文件访问            | [`cap-std`](https://github.com/bytecodealliance/cap-std)                                                                 | 从已授权目录句柄做相对访问；避免“字符串检查后再打开”的路径竞态。它不替代进程隔离、挂载策略或租户权限控制。                                                                                          |
| 底层 Linux 系统接口     | [`rustix` 1.x](https://docs.rs/rustix/latest/rustix/)                                                                    | 仅当标准库/能力式文件 API 缺少所需 syscall 时使用；隔离在 runtime 适配器中，不再并用 nix 提供同一接口，检查内核能力且不静默降级。                                                                   |
| Docker Engine 适配      | [`bollard`](https://github.com/fussybeaver/bollard)                                                                      | 仅实现 Docker 后端时引入异步 SDK，不重复手写 Docker REST 客户端；daemon socket 属高权限管理面，禁止暴露给沙箱。                                                                                     |
| Kubernetes 适配         | [`kube`（kube-rs）](https://github.com/kube-rs/kube)                                                                     | 仅实现 Kubernetes 后端时引入；按需开启 client/runtime，限定凭据与 RBAC，不因 SDK 存在而引入控制器或第二套调度状态。                                                                                 |
| Git、普通集合与配置     | 标准库与宿主 Git；暂不加专用 crate                                                                                       | Git clone/worktree 使用受控 Git 进程，参数分开传递，不拼接 shell；异步路径使用 Tokio 进程 API 并明确终止/等待责任。集合与同步先用 std/Tokio；配置格式及持久化未定，不预引入配置框架、数据库或 ORM。 |

### 性能依据与兼容边界

- “性能最好”限定为目标平台、真实数据与相同语义下的测量结果；当前没有可运行沙箱，**没有本项目最快库的实测结论**。表中选型综合性能机制、维护与生态适配；网络/后端 SDK 不存在可脱离负载比较的统一性能排名。
- JSON 的具体取舍：sonic-rs 上游 README 提供 Xeon Platinum 8260 上 twitter/citm_catalog/canada 数据的基准，安全 `from_slice` 在这些结构体解析用例中快于 serde_json/simd-json；这是**上游结果，非本项目测量**。调研时仓库约 937 stars，是有实际采用但非最大众的性能型选择；stars 仅作采用度信号。[基准与运行条件](https://github.com/cloudwego/sonic-rs#benchmark)。
- sonic-rs 并非 serde_json 的无条件替代：非类型化 Value 的重复键行为存在差异，SDK 暴露的 serde_json 类型不能直接互换。[兼容说明](https://github.com/cloudwego/sonic-rs/blob/main/docs/serdejson_compatibility.md)。业务 JSON 默认只选 sonic-rs；遇到硬性兼容要求时先评估全局改选，而非任意新增第二套解析路径。
- 不把上游建议的 `-C target-cpu=native` 无条件用于分发产物；构建机与部署 CPU 可能不同，须明确目标指令集并验证可移植性。首次实现 JSON 路径时测量真实负载的吞吐、分配与尾延迟，同时验证重复键、数值精度、非法 UTF-8 和资源上限；不靠 unchecked API 或放松语义获得“更快”。

### 同类第三方库约束

- **尽可能避免使用两个同一类的第三方库：同一职责默认只保留一个直接依赖与一套业务使用方式。** 新增前检查现有依赖、标准库和已选生态能否满足；替换时迁移所有调用点并删除旧直接依赖，不保留双实现或兼容别名。
- 配套且职责互补的 crate 不算重复，例如 serde + sonic-rs、tracing + tracing-subscriber、tokio + tokio-util；后端 SDK 内部的 HTTP/JSON 实现也不等于允许业务层任意并用同类库。
- 无法消除的 SDK 类型约束、平台差异或传递依赖例外，必须在引入说明中记录原因、使用边界及去重不可行的证据；不能为去重破坏第三方兼容性，也不能以“传递依赖已存在”为由扩大直接使用。Cargo 工程建立后用 `cargo tree --duplicates` 与 `cargo tree -e features` 检查重复版本和 feature 来源；不同名称的同类库还需人工检查，命令输出不自动证明无重复。

## Runtime, Commands & QA

- Rust 核心使用上述 Cargo 工具链，不继承 Paseo 的工具链版本。服务入口尚未配置。本检出没有 `sandbox-rust.yml`。质量工作流范围见 [docs/engineering.md](docs/engineering.md)，独立安全门禁见 [安全工程说明](../.github/sandbox-security.md)。它们不能代替后端行为与隔离测试。
- 本检出没有 `Cargo.toml`，不能在这里执行 Cargo。首个真实 crate 属于并行的 `feat/capability-workspace`：cap-std 受限临时工作区、有界读写、可重试幂等清理、真实 example 与测试、Apache-2.0，无容器或服务。不创建空壳入口。未合并不算本检出已有实现。
- Cargo 工程建立后的检查以该 crate 分支为准。本检出不把未运行的 Cargo 写成已通过。
- 单元测试与模块共置；`tests/` 验证 API 行为、非法状态转换、失败回收、路径边界及隔离能力。需要容器或虚拟化的测试应显式标注环境要求并隔离资源。
- 手写 `.rs` 单文件不超过 1200 行，含 1200 行。文档 QA 的命令、白名单和 `format` 脚本只写在 [docs/engineering.md](docs/engineering.md)。检查结果记在 PR，不记在指南里。
- 启动优化必须测量冷启动与复用路径，并验证复用不泄露状态。没有覆盖率百分比门槛；实际后端行为必须有运行证据，不能仅靠 mock。
