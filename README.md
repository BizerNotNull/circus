# circus

Circus 是面向 AI Agents 的沙箱仓库。当前 Rust 实现在 [circus-sandbox](circus-sandbox/README.md)：能力文件 API、受预算约束的进程内代码快照与工作区文件快照、资源与显式配置校验、Git 准备与有界预启动池，以及本地 Linux Docker Engine + `runc` 异步容器后端（含每沙箱一个交互进程会话）。`circus-paseo` 是已有前端，当前未接入沙箱。

## 当前状态

以下按当前检出描述实现与后续目标，具体合并和检查记录见对应 PR：

- `circus-sandbox` 是 Rust 1.99.1 / edition 2024 library crate。`Workspace` 提供受限相对文件操作、单文件读写上限和显式可重试清理；后端中立的 `Sandbox` 异步 API 通过 `BackendConfig`/`SandboxConfig` 提供创建、启动、暂停/恢复、命令与交互进程执行、文件操作、工作区快照、状态和销毁。配置要求本地 Docker 端点、已存在且包含 Python 3 的 Linux 镜像及显式资源/隔离参数；不会自动拉取镜像。默认启用的 `http` feature 在领域 API 之上提供认证 JSON HTTP 服务；端点、错误码、租户隔离与清理归属见 [HTTP API](circus-sandbox/docs/http-api.md)。
- `runtime::ResourceLimits` 校验资源请求的数值与关联边界；Docker 后端将配置映射到容器设置。值校验本身不是内核强制、资源预留或聚合准入，独立 `Workspace` 也没有总字节或宿主磁盘配额。
- `Workspace::capture_code_snapshot` 在显式字节与条目预算内复制普通文件和空目录，恢复总是创建新工作区；拒绝符号链接及特殊节点。这是进程内文件副本，不是进程、内存或容器快照。
- Kubernetes、containerd、gVisor、Kata、Firecracker 以及容器/VM 运行时快照（进程或客户机内存检查点）仍未实现；OCI 环境准备、Git clone/worktree、工作区文件快照、预启动池与认证 HTTP API 已实现；没有 RPC 或 Paseo 接入。这些仍是完整七领域交付范围，不是因当前 Docker slice 而取消的目标。支持边界与真实后端运行前提见[沙箱说明](circus-sandbox/README.md)。
- 文档与手写 Rust 规模检查在 `circus-sandbox/qa`，QA 运行时固定 Node.js 24.18.0。实际命令、扫描范围和检查边界见 [工程说明](circus-sandbox/docs/engineering.md)，完整交付目标见 [交付计划](circus-sandbox/docs/delivery-plan.md)。
- 当前检出同时包含 `PR title`、`Sandbox quality`、`CodeQL QA`、`Gitleaks`、`Dependency review`、`CodeQL (actions)`、`Sandbox Rust`（Windows/Linux 源码检查、cargo-deny 和 Rust CodeQL）、`Sandbox Docker`（GitHub 托管 Linux 运行器上的真实 Engine 忽略套件）和 `Sandbox architecture`（Windows/Linux 语义架构域门禁）。标题元数据检查不安装依赖，也不触发其他工作流的重型检查。这些检查互不代替，也不证明进程或容器隔离。某次运行结果只记在对应 PR；安全扫描约定见 [安全说明](.github/sandbox-security.md)。
- 合并通过 PR 完成，不要求非作者批准；获得任务授权后，可在最新 CI 通过、适用审查问题闭环且无冲突时自动 squash merge，不绕过保护。工作流存在不等于已配置为 required status check。
- Paseo CI 只覆盖 Paseo；沙箱任务不修改该树，Paseo 规则见 [circus-paseo/CLAUDE.md](circus-paseo/CLAUDE.md)。

## 导航

| 文档                                                                         | 内容                                            |
| ---------------------------------------------------------------------------- | ----------------------------------------------- |
| [AGENTS.md](AGENTS.md)                                                       | 仓库边界、命令与提交流程                        |
| [circus-sandbox/README.md](circus-sandbox/README.md)                         | 当前文件/代码快照、Docker API、配置与限制       |
| [circus-sandbox/AGENTS.md](circus-sandbox/AGENTS.md)                         | 沙箱架构约定、Rust 基线与第三方库约束           |
| [circus-sandbox/docs/delivery-plan.md](circus-sandbox/docs/delivery-plan.md) | 完整目标、功能验收与小 PR 依赖顺序              |
| [circus-sandbox/docs/engineering.md](circus-sandbox/docs/engineering.md)     | 当前工程事实、QA/安全/Rust 门禁与尚未具备的能力 |
