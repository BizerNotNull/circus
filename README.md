# circus

Circus 是面向 AI Agents 的沙箱仓库。`circus-sandbox` 计划用 Rust 实现核心；`circus-paseo` 是已有前端，当前未接入沙箱。

## 当前状态

下列是仓库边界，不是已交付能力，也不是合并完成。

- 本检出没有 `Cargo.toml`、可运行入口或后端适配器，不能安装或调用沙箱 API。Rust 基线是 1.99.1，见沙箱指南。首个真实 crate 在并行分支 `feat/capability-workspace`，不依赖本基线合并；未合并前不是 `main` 的能力。
- 文档与手写 Rust 规模检查的入口在 `circus-sandbox/qa`，QA 运行时固定 Node.js 24.18.0。本检出可执行的命令和支持边界见 [工程说明](circus-sandbox/docs/engineering.md)。
- 合并通过 PR 完成，不要求非作者批准；最新 CI 和适用审查问题仍需处理，不绕过保护。某次检查是否通过只记在对应 PR，不记在本文。
- Paseo CI 只覆盖 Paseo。本检出的沙箱质量工作流覆盖文档、QA 和 `circus-sandbox/qa` 的 CodeQL。本检出没有 `sandbox-rust.yml`。密钥扫描、dependency review 和 actions CodeQL 在并行安全基线，不在质量工作流里。它们互不代替，也不互相等待。Paseo 说明见 [circus-paseo/CLAUDE.md](circus-paseo/CLAUDE.md)；沙箱任务不修改该树。

## 导航

| 文档                                                                         | 内容                                         |
| ---------------------------------------------------------------------------- | -------------------------------------------- |
| [AGENTS.md](AGENTS.md)                                                       | 仓库边界、命令与提交流程                     |
| [circus-sandbox/AGENTS.md](circus-sandbox/AGENTS.md)                         | 沙箱架构约定、Rust 1.99.1 基线与第三方库约束 |
| [circus-sandbox/docs/delivery-plan.md](circus-sandbox/docs/delivery-plan.md) | 功能验收与小 PR 依赖顺序                     |
| [circus-sandbox/docs/engineering.md](circus-sandbox/docs/engineering.md)     | 当前工程事实、QA 命令与尚未具备的能力        |
