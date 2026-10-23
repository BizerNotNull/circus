# circus

Circus 是面向 AI Agents 的沙箱仓库。当前 Rust 实现在 [circus-sandbox](circus-sandbox/README.md)：临时目录上的能力文件 API，不是进程沙箱或容器。`circus-paseo` 是已有前端，当前未接入沙箱。

## 当前状态

以下按当前检出描述实现与后续目标，具体合并和检查记录见对应 PR：

- `circus-sandbox` 已有真实 Rust library crate、`Cargo.toml`、锁文件、源码、example 和测试；使用 Rust 1.99.1 / edition 2024。`Workspace` 提供受限相对文件操作、单文件读写上限和显式可重试清理；没有命令执行、进程隔离、容器、统一领域 API 或 HTTP/RPC 服务。
- 文档与手写 Rust 规模检查在 `circus-sandbox/qa`，QA 运行时固定 Node.js 24.18.0。实际命令、扫描范围和检查边界见 [工程说明](circus-sandbox/docs/engineering.md)，完整交付目标见 [交付计划](circus-sandbox/docs/delivery-plan.md)。
- 当前检出同时包含 `Sandbox quality`、`CodeQL QA`、`Gitleaks`、`Dependency review`、`CodeQL (actions)` 和 `Sandbox Rust`（Windows/Linux 源码检查、cargo-deny 和 Rust CodeQL）。这些检查互不代替，也不证明进程或容器隔离。某次运行结果只记在对应 PR；安全扫描约定见 [安全说明](.github/sandbox-security.md)。
- 合并通过 PR 完成，不要求非作者批准；获得任务授权后，可在最新 CI 通过、适用审查问题闭环且无冲突时自动 squash merge，不绕过保护。工作流存在不等于已配置为 required status check。
- Paseo CI 只覆盖 Paseo；沙箱任务不修改该树，Paseo 规则见 [circus-paseo/CLAUDE.md](circus-paseo/CLAUDE.md)。

## 导航

| 文档                                                                         | 内容                                            |
| ---------------------------------------------------------------------------- | ----------------------------------------------- |
| [AGENTS.md](AGENTS.md)                                                       | 仓库边界、命令与提交流程                        |
| [circus-sandbox/README.md](circus-sandbox/README.md)                         | 当前 Workspace 文件 API、错误与限制             |
| [circus-sandbox/AGENTS.md](circus-sandbox/AGENTS.md)                         | 沙箱架构约定、Rust 基线与第三方库约束           |
| [circus-sandbox/docs/delivery-plan.md](circus-sandbox/docs/delivery-plan.md) | 完整目标、功能验收与小 PR 依赖顺序              |
| [circus-sandbox/docs/engineering.md](circus-sandbox/docs/engineering.md)     | 当前工程事实、QA/安全/Rust 门禁与尚未具备的能力 |
