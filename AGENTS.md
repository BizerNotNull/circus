# Repository Guidelines

## Project Overview

Circus 是面向 AI Agents 的沙箱项目：`circus-sandbox` 承担 Rust 核心，`circus-paseo` 作为前端调用方。沙箱目前只有 workspace 文件库，没有 Paseo—沙箱集成，也没有进程或容器隔离；不要把规划写成现有能力。

- **当前开发阶段**：现在处于沙箱开发阶段，工作聚焦 `circus-sandbox`，不需要改动 `circus-paseo`；除非用户明确要求，不主动修改 Paseo。
- **Paseo 暂不开发**：说明与开发规则见 [circus-paseo/CLAUDE.md](circus-paseo/CLAUDE.md)。除非任务明确要求，不修改其源码、依赖或配置。
- **沙箱开发入口**：[circus-sandbox/AGENTS.md](circus-sandbox/AGENTS.md)，用户说明见 [circus-sandbox/README.md](circus-sandbox/README.md)。七个领域的职责边界仍由沙箱指南维护；其中只有 workspace 文件 API 已实现。完整交付目标见 [交付计划](circus-sandbox/docs/delivery-plan.md)，当前门禁与命令见 [工程说明](circus-sandbox/docs/engineering.md)，安全扫描约定见 [安全说明](.github/sandbox-security.md)。按当前检出描述实现，不以规划或工作流存在代替交付与运行证据。

## Architecture & Data Flow

- 现有 Paseo：移动端/Web/桌面/CLI → 客户端协议层 → Node.js daemon，负责 Agent 管理及实时输出；这不是已接入 Rust 沙箱的链路。
- 当前沙箱：库调用方 → `Workspace` → cap-std `Dir`。只有临时目录内的相对文件操作，没有统一执行 API、生命周期或后端适配器。
- 目标沙箱：统一 API → 生命周期管理 → 基础设施调度 → 运行时隔离；镜像环境、代码工作区提供执行资源，预启动/快照/沙箱池优化启动。
- 目标约束：对外 API 屏蔽后端差异；领域层与部署、隔离适配器分离。生命周期统一拥有状态与回收逻辑，调用方不得绕过它直接操作后端。在这些模块实现之前，不能按这条链路调用。

## Key Directories

| 路径                    | 用途                                                                                                                                                                                                       |
| ----------------------- | ---------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `circus-sandbox/`       | Rust library crate。已实现 workspace 文件 API、`examples/workspace.rs`、`tests/workspace.rs`、Apache-2.0 `LICENSE` 与 `deny.toml`。                                                                        |
| `circus-sandbox/src/`   | 已导出 `workspace`。`api`、`lifecycle`、`infrastructure`、`runtime`、`environment`、`pool` 仍是占位目录，不是已实现模块。                                                                                  |
| `circus-sandbox/tests/` | `workspace.rs` 用真实临时目录验证公开文件 API。                                                                                                                                                            |
| `circus-paseo/`         | 已有 npm workspace 应用；内部结构与开发流程交由其 `CLAUDE.md` 说明。                                                                                                                                       |
| `.github/workflows/`    | Paseo CI 仍按路径触发。沙箱包含 `sandbox-quality.yml`、`sandbox-security.yml` 和 `sandbox-rust.yml`。质量、密钥/依赖、actions/QA/Rust CodeQL 与 Cargo 检查各自的范围见工程说明；它们不证明进程或容器隔离。 |

## Development Commands

根目录没有统一构建入口。沙箱命令在 `circus-sandbox/` 执行；rustup 读取该目录的 `rust-toolchain.toml`（Rust 1.99.1）。`Cargo.lock` 由 Cargo 生成后提交，检查使用 `--locked`。

| 目的              | 命令                                                                                                             |
| ----------------- | ---------------------------------------------------------------------------------------------------------------- |
| 格式检查 / 格式化 | `cargo fmt --check` / `cargo fmt`                                                                                |
| Clippy            | `cargo clippy --locked --all-targets -- -D warnings`                                                             |
| 构建              | `cargo build --locked`                                                                                           |
| 测试              | `cargo test --locked`                                                                                            |
| 文档              | `RUSTDOCFLAGS=-D warnings cargo doc --locked --no-deps`                                                          |
| 运行示例          | `cargo run --locked --example workspace`                                                                         |
| 依赖策略          | 先 `cargo install --locked --version 0.20.2 cargo-deny`，再在 `circus-sandbox/` 执行 `cargo deny --locked check` |

上表的 fmt、clippy、build、test、doc 与 example 也是 `Sandbox Rust` 在 Windows 与 Linux 上的源码检查。cargo-deny 与 CodeQL 的范围、许可证 allow，以及 `io-lifetimes` / `windows-sys` 传递重复见[沙箱指南](circus-sandbox/AGENTS.md)。

文档与 QA 命令在仓库根目录执行：

```text
npm --prefix circus-sandbox/qa ci
npm --prefix circus-sandbox/qa audit --audit-level=low
npm --prefix circus-sandbox/qa run check
node --test circus-sandbox/qa/check.test.mjs
```

QA 固定 Node.js 24.18.0 与 npm，不继承 Paseo 的 Node 版本。`ci` 和 `audit` 是 npm 内置命令，不是 package script。检查使用现有 `check.mjs`、markdownlint promise API 与锁文件，不重新引入 markdownlint-cli2/braces。白名单、`format` 和安全扫描入口统一见工程说明。

以下仅是已有 Paseo 命令，**在 `circus-paseo/` 下执行，且仅用于明确要求的 Paseo 任务**：

| 目的               | 命令                                          |
| ------------------ | --------------------------------------------- |
| 安装依赖 / 构建    | `npm ci` / `npm run build`                    |
| 启动 daemon / 前端 | `npm run dev` / `npm run dev:app`（分别启动） |
| 类型检查 / lint    | `npm run typecheck` / `npm run lint`          |
| 格式检查           | `npm run format:check`                        |
| 单文件测试         | `npx vitest run <file> --bail=1`              |

## Code Conventions & Common Patterns

- 沙箱采用 Rust 职责模块、显式 `Result` 错误及构造参数依赖注入。当前文件 API 是同步阻塞的，异步调用必须 `spawn_blocking`，且不引入第二套异步运行时。工具链与依赖原则见[沙箱指南](circus-sandbox/AGENTS.md)：已引入的直接依赖只有 cap-std 4.0.3、cap-fs-ext 4.0.3、tempfile 3.x、thiserror 2.x；表中其余库仍未加入。持久化方案尚未选定。
- 外部后端适配与领域逻辑分离；失败、取消和销毁均需明确资源回收责任。安全与状态不变量见沙箱指南。
- 不复制 Paseo 的 TypeScript 状态模型或框架到 Rust 核心；涉及 Paseo 时遵循其本地指南及 `docs/coding-standards.md`。
- 文档区分“当前”与“规划”；子项目细节只维护一份，根指南保留边界与链接。

## Important Files

- [circus-sandbox/AGENTS.md](circus-sandbox/AGENTS.md)：沙箱架构约定、1.99.1 基线与命令。可执行入口是库示例，不是服务或进程沙箱。
- [circus-paseo/CLAUDE.md](circus-paseo/CLAUDE.md)：Paseo 权威入口；进一步指向架构、开发、测试与 QA 文档。
- `circus-paseo/package.json`、`circus-paseo/package-lock.json`：现有 npm workspace 命令及依赖锁。
- `circus-paseo/.tool-versions`：Paseo 工具版本声明，不作为沙箱 Rust 版本依据。
- `lefthook.yml`：根提交钩子目前仅覆盖 Paseo 的格式、lint 与类型检查，不覆盖沙箱。

## Runtime/Tooling Preferences

- Windows 下开发时，如遇环境兼容性、工具链或运行调试问题，可以在 WSL 中开发、调试或运行相关工具。
- 沙箱使用 Rust 1.99.1 / Cargo；版本基线及依赖约束统一维护在[沙箱指南](circus-sandbox/AGENTS.md)。已实现目录不保留 `.gitkeep`；其余占位目录的 `.gitkeep` 仍只用于跟踪空目录。
- Paseo 使用 Node.js（当前声明 `22.20.0`）与 npm workspaces；保留 `package-lock.json`，不用 Bun/Yarn/pnpm 替换。
- Paseo lint/格式化统一走 npm scripts；工具实现以 `package.json` 为准，避免文档中的旧工具名称误导。
- 未获许可不得重启端口 `6767` 的 Paseo 主 daemon，避免中断已有 Agent。

## Testing & QA

- 沙箱用 `cargo test --locked` 验证已实现行为，没有覆盖率百分比门槛。`tests/workspace.rs` 覆盖写上限、恰好等于上限的读取、非法路径、嵌套目录与删除、重复 `cleanup` 和 `Closed`。大于上限的已有文件读取与符号链接逃逸在 crate 私有测试中，因为公开 API 不暴露工作区根。未实现的生命周期、隔离和后端路径不能用 mock 声称通过。
- 手写 `.rs` 单文件不超过 1200 行（含 1200）。文档、链接、格式、行数与 QA 回归测试由质量门禁覆盖；这些检查不代替 Cargo 行为测试或真实后端证据。检查结果记在对应 PR。
- Paseo 使用 Vitest、浏览器测试与 Playwright E2E；完整规则见 [测试指南](circus-paseo/docs/testing.md) 和 [QA 指南](circus-paseo/docs/qa.md)。
- 本地仅运行受影响测试文件；禁止默认执行 `npm test` 或整个工作区测试，全量验证交给 CI。Paseo 改动按其指南执行类型、lint 与格式检查。

## Commit & Pull Request Workflow

采用短生命周期分支、小范围 PR；CI 验证可自动检查的契约，Sourcery 辅助代码审查，作者负责确认正确性与风险，不以机器人通过代替实际验证。

### 1. 实现与本地验证

- 从目标分支建立任务分支，一个 PR 只解决一个明确问题；避免混入无关重构、格式化或依赖升级。
- 修改前确认作用域与子项目约定；提交前检查 diff，只暂存本任务文件或片段，不覆盖、提交或清理用户的无关改动。
- 按 `Testing & QA` 执行受影响的检查，并实际运行改变的行为；文档改动只检查事实、路径与引用。未运行或受环境限制的项目必须如实记录。

### 2. 提交与创建 PR

- 提交按逻辑拆分，每个提交保持可理解、可验证；推荐 `type(scope): 简述`，如 `fix(lifecycle): 修复销毁时的资源回收`、`docs(workflow): 补充 PR 流程`。提交说明交代必要的原因与约束，不只复述文件列表。
- 推送前完成本地自审：检查遗漏的调用方、失败路径、测试与文档，以及密钥、临时文件和调试代码。不得用跳过 hooks 的方式掩盖失败。
- PR 标题描述结果；正文包含：问题与改动、作用域/非目标、验证命令与结果、风险及必要的回滚方式；有关联 issue 时链接。未完成实现或验证时保留 Draft，不声明可合并。
- 只有任务明确授权时才执行 commit、push、创建 PR 或合并；要求修改文件不等于授权发布。不得直接推送目标分支。

### 3. CI 与 Sourcery 审查闭环

- PR 提交后并行关注 GitHub Actions CI 和 Sourcery review；两者职责不同，不能互相替代。准备合并前，必须确认审查覆盖最新改动；Sourcery 未触发或未覆盖新增改动时，按其实际支持的方式请求审查，不假定它已经完成。
- Paseo CI 按路径触发，规则见 `.github/workflows/paseo-ci.yml` 与 `.github/paseo-ci-paths.yml`。根文档或沙箱改动可能不触发它；未触发或跳过不等于 Paseo 通过，也不能代替沙箱检查。
- 沙箱三套工作流均在本检出，范围见 [工程说明](circus-sandbox/docs/engineering.md)：质量门禁含 npm audit、文档/行数检查、QA 测试、actionlint 和 `CodeQL QA`；安全门禁含全历史 Gitleaks、PR-only `Dependency review` 和仅分析安全工作流的 `CodeQL (actions)`；`Sandbox Rust` 在 Windows/Linux 上运行 fmt、clippy、build、test、严格 rustdoc 和 example，并在 Ubuntu 运行 cargo-deny 与 Rust CodeQL。结果以对应提交的 Actions 为准，本文不记录某次检查通过，也不以旧提交结果为新提交背书。不为“变绿”修改无关文件。
- CI 失败先读失败 job 的日志，区分代码问题与基础设施故障。代码问题修复并补做针对性验证；明确的临时基础设施故障可重跑并记录原因，不靠反复重跑或放宽检查掩盖失败。
- 逐条判断 Sourcery 和人工审查意见：有效问题修复并说明验证；不采纳时回复技术理由、证据或取舍。安全、隔离、生命周期及资源回收问题必须明确处理，不盲从建议，也不无说明地关闭讨论。
- 审查修复默认追加提交，便于查看增量；只有明确需要整理历史时才重写已推送提交，且不得覆盖他人提交。每轮修复完成本地验证后集中推送，再检查新一轮 CI 与审查；旧提交的绿灯不能为新提交背书。

### 4. 合并与收尾

- 合并前确认：PR 不再是 Draft；所声称的检查有该 PR 中的证据；审查问题已修复或有明确的不采纳说明；无未解决的阻断项或冲突。机器人评论不是批准审查，也不是合并授权。
- `main` 当前不要求非作者批准；任务明确授权时，可在最新 CI 通过、审查问题闭环且无冲突后自动 squash merge。仍须使用 PR，不使用管理员绕过或强推。工作流存在不等于 required status check；缺失或被跳过的必需检查不是实际覆盖。
- 不绕过分支保护或必需检查。若必需检查因路径过滤一直 pending，应处理检查规则或交由维护者处理，不把缺失检查视为成功。
- 推荐 squash merge，使目标分支保持“一项任务一个提交”；若仓库已有合并策略，以仓库设置为准。合并说明保留关键背景与验证证据。
- 合并后查看适用的目标分支 CI；出现回归优先回滚或提交最小修复，不把 PR 阶段通过当作最终保证。确认不再使用后清理任务分支，交付时报告 PR、验证结果与已知限制。
