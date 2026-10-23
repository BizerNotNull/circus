# 工程说明

本文只记录当前事实和检查入口。本机工具已安装、Docker 引擎可达，或本文已写入仓库，都不等于沙箱已实现，也不等于 PR 已合并。

## 当前工程

当前检出已有真实 Rust library crate：`Cargo.toml`、Cargo 生成的 `Cargo.lock`、`rust-toolchain.toml`、Apache-2.0 `LICENSE`、`deny.toml`、`src/lib.rs` 与 `src/workspace/`、真实 example 和测试。Rust 1.99.1 / edition 2024；公开入口只有同步 `Workspace` 文件 API，不是进程沙箱、容器或服务。

本文按当前检出描述工程范围，不以文件存在代替合并、发布或最新提交的检查。交付记录入口见 [交付计划](delivery-plan.md)，安全扫描约定见 [安全说明](../../.github/sandbox-security.md)。

## 文档与 QA

文档与手写源码规模检查固定在 `circus-sandbox/qa`，要求 Node.js 24.18.0 与 npm。这是精确运行时，不是 `>=22`。Prettier 3.9.10 按 Unicode 列宽对齐表格；Node.js 22 与 24.18.0 算出的列宽不同，同一 Markdown 会被格式化成不同结果。因此 `package.json` 的 `engines.node`、`circus-sandbox/qa/.node-version` 和质量工作流的 `node-version-file` 都固定 24.18.0。Paseo 自己声明的 Node 版本仍以 `circus-paseo/` 为准，不修改 `circus-paseo/`。

在仓库根目录执行：

```text
npm --prefix circus-sandbox/qa ci
npm --prefix circus-sandbox/qa audit --audit-level=low
npm --prefix circus-sandbox/qa run check
node --test circus-sandbox/qa/check.test.mjs
```

`ci` 是 npm 安装锁文件的内置命令，不是 package script，不要写成 `npm run ci`。`audit` 也是 npm 内置命令。package script 有 `check`、`format` 和 `test`。`check` 由 `check.mjs` 先枚举 Markdown 白名单，再用 `markdownlint/promise` 的 `readConfig` 读取 `circus-sandbox/qa/.markdownlint.json`，然后调用 `lint({ files, config })`，接着运行 `prettier --check`、本地链接检查和手写 `.rs` 行数检查。该配置是 JSON，不是 JSONC；`readConfig` 使用 JSON.parse，注释会使读取失败。不执行 markdownlint-cli2，也不再使用它的 glob 配置。`format` 只对同一 Markdown 白名单执行 `prettier --write`，不修复链接，也不运行 Cargo。验证入口是 `check` 加上上面的 `audit`，不是 `format`。`test` 执行 `node --test check.test.mjs`。仓库根目录的单文件命令是 `node --test circus-sandbox/qa/check.test.mjs`，需要先完成上面的 `ci`。它在临时目录运行当前 `check.mjs`，不改检出，也不访问外网。创建不了文件符号链接时失败，不能把权限不足当成通过。它不代替 `check`。

`package.json` 的直接依赖是 markdownlint 0.42.0、prettier 3.9.10、mdast-util-from-markdown 2.1.0 和 github-slugger 2.0.0，不再声明 markdownlint-cli2。markdownlint 0.42.0 不带 braces 或 fast-glob。CLI2 的文件 glob 链会带入 micromatch 和 braces 3.0.3；braces 3.0.3 及更低版本对应 CVE-2026-93687。`check.mjs` 已经枚举文件，不需要这套 glob，所以移除了 CLI2，而不是把 advisory 写成忽略，也不是降级。`overrides` 仍把 katex 精确固定为 0.18.2。保留质量基线的 checker、manifest 与生成锁文件，不重新引入 CLI2/braces。具体审计结果只记在对应 PR。

Markdown 与链接白名单是 `README.md`、`AGENTS.md` 和 `circus-sandbox/**/*.md`，并跳过 `node_modules/` 与 Cargo `target/` 生成目录；`cargo doc` 生成的第三方许可证不被检查或格式化。已存在的 `.github/sandbox-security.md` 进入同一白名单。链接检查在这些文件之外还包含 `.github/pull_request_template.md`，并检查模板里的本地链接。指向 Paseo 的本地链接会被解析，但不会扫描 `circus-paseo/` 或 `.omp/`。markdownlint 不限制行宽。围栏必须有语言标签。Prettier 3.9.10 不重排正文，也不改围栏内示例；它会按自身宽度对齐表格。

手写 `.rs` 上限是 1200 行，含 1200 行。计数忽略 `target/`、`node_modules/` 和带生成文件头的文件。本检出的 crate 源码、example 和测试在此范围内，不再是无 Rust 的目录基线。

命令的通过与否记在对应 PR。本文只定义入口和支持边界，不记录某次运行结果，也不把文档写入当成已合并。

## 文件规模

规则见上一节。超过 1200 行时应拆分职责，不用空模块凑数。本检查不把生成目录或 Paseo 源码算进手写 Rust。

## 主机边界

- 隔离证据必须来自 Linux 上可达的 Docker Engine，并且内存、CPU 与 pids 由显式配置限制。Windows 编译成功不是隔离通过。引擎可达也不是适配器已实现。
- 验证只能操作自己创建的容器和卷，禁止全局 prune 或清理无关资源。
- Rust 1.99.1 是项目基线，不是本机清单。工具链文件、组件和第三方库约束只维护在 [沙箱指南](../AGENTS.md)。rust-analyzer 不是构建依赖。

## 检查与安全门禁

Paseo CI 与沙箱门禁互不代替。Action 的完整 SHA 以工作流文件为准，不在本文复制第二份清单。本检出包含质量、安全和 Rust 工作流；三个 CodeQL 配置各自限制分析范围，不宣称覆盖全部仓库。

| 路径                                                     | 本检出                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |
| -------------------------------------------------------- | ------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------ |
| `.github/workflows/paseo-ci.yml`                         | 只覆盖 Paseo，不验证沙箱。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                           |
| `.github/workflows/sandbox-quality.yml`                  | `pull_request`、push `main`、`merge_group`、`workflow_dispatch`，无路径过滤。两个 job：`Sandbox quality` 与 `CodeQL QA`。质量 job 是 `contents: read`，超时 15 分钟；除 `merge_group` 外，并发会取消进行中的运行。步骤为 checkout、Node 24.18.0、`npm --prefix circus-sandbox/qa ci`、`npm --prefix circus-sandbox/qa audit --audit-level=low`、`npm --prefix circus-sandbox/qa run check`、`npm --prefix circus-sandbox/qa test`，然后 `bash .github/scripts/sandbox-actionlint.sh`。low 及以上 advisory 使该 job 失败；audit 不是省略项，也不是 warn-only。它不运行 Cargo 或沙箱。 |
| `.github/codeql/sandbox-qa.yml`                          | 路径只含 `circus-sandbox/qa`。忽略 `circus-sandbox/qa/node_modules`、`circus-paseo/` 和 `.omp/`。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                    |
| `.github/scripts/sandbox-actionlint.sh`                  | actionlint 1.7.12 按官方发布资产的 SHA-256 固定。下载前校验和必须是 64 位十六进制；下载后比对，不一致即失败。用 `mktemp -d` 建立私有临时目录，并在 `EXIT` 时清理。清理保留原退出码，不把校验和失败变成成功。shellcheck 只检查这个脚本。actionlint 只检查 `sandbox-quality.yml`。                                                                                                                                                                                                                                                                                                     |
| `.github/workflows/sandbox-security.yml`                 | 与质量工作流同类触发、无路径过滤。`Gitleaks` 扫描 checkout 中所有 ref 的完整历史（含 merge diff）；`Dependency review` 只在 `pull_request` 比较依赖变化，low 及以上、runtime/development 范围失败，非 warn-only；不得将它当成 merge queue 实际审查覆盖。`CodeQL (actions)` 只分析安全工作流。                                                                                                                                                                                                                                                                                        |
| `.github/scripts/sandbox-gitleaks.sh`、`.gitleaksignore` | 脚本固定 gitleaks 8.30.1，按内置 SHA-256 校验官方发布归档，要求非浅检出，失败关闭。例外只有两个历史公开 X25519 低阶点测试向量的精确 fingerprint，不是路径或目录豁免。                                                                                                                                                                                                                                                                                                                                                                                                                |
| `.github/codeql/sandbox.yml`                             | `actions`、`build-mode: none`、`security-extended`，只含 `.github/workflows/sandbox-security.yml`，不分析 Paseo、QA JavaScript 或 Rust。                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `.github/dependabot.yml`                                 | 每周只更新 `/` 的 `github-actions`。可能为所有工作流中的 action 开 PR，但不批准或合并；当前未配置 QA npm 或 Cargo 更新。                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `.github/workflows/sandbox-rust.yml`                     | `Sandbox Rust`，所有 PR、push `main`、`merge_group` 和手动运行，无路径过滤。`rust (ubuntu-latest)` 与 `rust (windows-latest)` 跑 fmt、clippy、build、test、严格 rustdoc 和 example；Ubuntu 的 `dependencies` 跑 cargo-deny 0.20.2；`codeql` 是 Rust `build-mode: none`。命令与策略见沙箱指南。                                                                                                                                                                                                                                                                                       |
| `.github/codeql/sandbox-rust.yml`                        | 仅分析 `circus-sandbox` Rust，忽略 Paseo 与 `target`，不编译 crate，不能代替行为测试。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                               |

`CodeQL QA` 超时 30 分钟。语言是 `javascript-typescript`，`build-mode` 为 none，查询集为 `security-extended`，配置文件是 `.github/codeql/sandbox-qa.yml`。权限是 contents、actions 的 read，以及 `security-events: write`。只分析 QA 源码，不分析 actions 或 Rust，也不扫描 Paseo。分析或上传失败则 job 失败；告警是否阻断合并属于 ruleset，不由分析工作流本身保证。

完整历史密钥扫描的本地入口（仓库根目录）：

```bash
bash .github/scripts/sandbox-gitleaks.sh
```

需要 Bash、Git、curl、tar（Windows 归档需要 unzip）与 GNU coreutils 的 `sha256sum`；macOS 需要另装 GNU coreutils。脚本拒绝浅检出，不取运行时远端 checksums 作为信任依据，下载、校验和扫描失败都失败关闭。dependency review 与 CodeQL 在 GitHub 工作流执行，不提供虚构的本地替代命令。

Rust fmt、clippy、构建、测试、文档、example、cargo-deny 和 Rust CodeQL 均已配置。Cargo 命令在 `circus-sandbox/` 执行，完整命令表见 [README](../README.md#命令)。`deny.toml` 已配置显式许可证 allow、漏洞、来源与依赖策略，细节见沙箱指南；dependency review 不替代许可证策略。某次实际运行结果只记在对应提交的 PR。

`npm --prefix circus-sandbox/qa audit` 只审计 `circus-sandbox/qa` 的锁文件。它不审计 `circus-paseo/`，也不审计仓库里其它 npm 树。这次审计通过、CodeQL QA 通过，或安全基线合并，都不等于整个仓库的漏洞告警已经解决。告警数量只记在对应 PR 或报告，不写进本文。

secret scanning 与 push protection 是 GitHub 平台设置，不是工作流的替代品。工作流存在也不等于它已设为 `main` 的 required status check。当前不要求非作者批准；任务授权后的自动 squash merge 仍须经过 PR、最新 CI、适用审查问题闭环且无冲突，不绕过保护或强推。提交流程见根指南，安全扫描范围见安全说明。

质量门禁不覆盖 Cargo、Rust CodeQL 或隔离；安全门禁的 actions CodeQL 只覆盖安全工作流，actionlint 只覆盖质量工作流及其脚本，不代表所有工作流均被分析。三套门禁都不证明沙箱进程隔离、Paseo 依赖审计、分支保护配置或全仓库漏洞清零。

## 当前 Workspace 与后续真实后端

`Workspace` 提供临时目录内的受限相对读写、建目录、unlink 和显式可重试幂等清理。`max_file_bytes` 是单文件上限，不是工作区总配额。内部 `RwLock` 保护同一值的并发读写，不是外部进程隔离或 crash-safe 原子替换。当前检出有真实 example、公开 API 集成测试和 crate 私有测试；某次运行的环境、命令与结果只记在对应 PR，不以旧提交的绿灯为新提交背书。

尚未实现命令执行、统一领域 API、生命周期状态机、进程/网络/系统调用隔离、容器与虚拟化适配器、OCI 环境准备、Git clone/worktree、HTTP/RPC、池、预启动和快照，以及工作区总配额。占位目录不是可调用模块。

后续运行时必须先消费 workspace 文件契约。建议的首个适配仍是 Linux 上的 Docker + runc，必须有真实运行证据；主机引擎可达或 Windows 编译成功不能提前声明支持。完整目标、依赖及池/快照验收仍见 [交付计划](delivery-plan.md)。
