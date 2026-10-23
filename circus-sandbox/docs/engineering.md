# 工程说明

本文只记录当前事实和检查入口。本机工具已安装、Docker 引擎可达，或本文已写入仓库，都不等于沙箱已实现，也不等于 PR 已合并。

## 本 PR：无 Cargo

`ci/sandbox-quality-baseline` 不创建 `Cargo.toml`、空 crate、可运行入口或后端适配器。本检出仍然不能执行 Cargo 构建、测试或沙箱隔离验证。

文档与手写源码规模检查固定在 `circus-sandbox/qa`，要求 Node.js 24.18.0 与 npm。这是精确运行时，不是 `>=22`。Prettier 3.9.10 按 Unicode 列宽对齐表格；Node.js 22 与 24.18.0 算出的列宽不同，同一 Markdown 会被格式化成不同结果。因此 `package.json` 的 `engines.node`、`circus-sandbox/qa/.node-version` 和质量工作流的 `node-version-file` 都固定 24.18.0。Paseo 自己声明的 Node 版本仍以 `circus-paseo/` 为准，不修改 `circus-paseo/`。

在仓库根目录执行：

```text
npm --prefix circus-sandbox/qa ci
npm --prefix circus-sandbox/qa audit --audit-level=low
npm --prefix circus-sandbox/qa run check
node --test circus-sandbox/qa/check.test.mjs
```

`ci` 是 npm 安装锁文件的内置命令，不是 package script，不要写成 `npm run ci`。`audit` 也是 npm 内置命令。package script 有 `check`、`format` 和 `test`。`check` 由 `check.mjs` 先枚举 Markdown 白名单，再用 `markdownlint/promise` 的 `readConfig` 读取 `circus-sandbox/qa/.markdownlint.json`，然后调用 `lint({ files, config })`，接着运行 `prettier --check`、本地链接检查和手写 `.rs` 行数检查。该配置是 JSON，不是 JSONC；`readConfig` 使用 JSON.parse，注释会使读取失败。不执行 markdownlint-cli2，也不再使用它的 glob 配置。`format` 只对同一 Markdown 白名单执行 `prettier --write`，不修复链接，也不运行 Cargo。验证入口是 `check` 加上上面的 `audit`，不是 `format`。`test` 执行 `node --test check.test.mjs`。仓库根目录的单文件命令是 `node --test circus-sandbox/qa/check.test.mjs`，需要先完成上面的 `ci`。它在临时目录运行当前 `check.mjs`，不改检出，也不访问外网。创建不了文件符号链接时失败，不能把权限不足当成通过。它不代替 `check`。

`package.json` 的直接依赖是 markdownlint 0.42.0、prettier 3.9.10、mdast-util-from-markdown 2.1.0 和 github-slugger 2.0.0，不再声明 markdownlint-cli2。markdownlint 0.42.0 不带 braces 或 fast-glob。CLI2 的文件 glob 链会带入 micromatch 和 braces 3.0.3；braces 3.0.3 及更低版本对应 CVE-2026-93687。`check.mjs` 已经枚举文件，不需要这套 glob，所以从 manifest 去掉 CLI2，而不是把 advisory 写成忽略，也不是降级。`overrides` 仍把 katex 精确固定为 0.18.2。npm audit 当时有没有列出该 CVE，不改变这个选择。锁文件要按该 manifest 重新生成后才不再解析这条 glob 链；重新生成和审计的结果只记在对应 PR，不写进本文。

Markdown 与链接白名单是 `README.md`、`AGENTS.md` 和 `circus-sandbox/**/*.md`，并跳过 `circus-sandbox/qa/node_modules`。`.github/sandbox-security.md` 只在文件存在时进入同一白名单；本检出没有它，缺失不算失败，本文也不链接该路径。链接检查在这些文件之外还包含 `.github/pull_request_template.md`，并检查模板里的本地链接。指向 Paseo 的本地链接会被解析，但不会扫描 `circus-paseo/` 或 `.omp/`。markdownlint 不限制行宽。围栏必须有语言标签。Prettier 3.9.10 不重排正文，也不改围栏内示例；它会按自身宽度对齐表格。

手写 `.rs` 上限是 1200 行，含 1200 行。计数忽略 `target/`、`node_modules/` 和带生成文件头的文件。本检出没有手写 Rust 文件。

命令的通过与否记在对应 PR。本文只定义入口和支持边界，不记录某次运行结果，也不把文档写入当成已合并。

## 文件规模

规则见上一节。超过 1200 行时应拆分职责，不用空模块凑数。本检查不把生成目录或 Paseo 源码算进手写 Rust。

## 主机边界

- 隔离证据必须来自 Linux 上可达的 Docker Engine，并且内存、CPU 与 pids 由显式配置限制。Windows 编译成功不是隔离通过。引擎可达也不是适配器已实现。
- 验证只能操作自己创建的容器和卷，禁止全局 prune 或清理无关资源。
- Rust 1.99.1 是项目基线，不是本机清单。工具链文件、组件和第三方库约束只维护在 [沙箱指南](../AGENTS.md)。rust-analyzer 不是构建依赖。

## 检查与安全门禁

Paseo CI 与沙箱门禁互不代替。Action 的完整 SHA 以工作流文件为准，不在本文复制第二份清单。本检出有质量工作流和 QA 的 CodeQL 配置。安全工作流、gitleaks 脚本、actions 的 CodeQL 配置、Dependabot 和 `.gitleaksignore` 不在本检出，本文不链接这些本地路径，也不把它们的命令写成可在本检出执行。

| 路径                                    | 本检出                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                        |
| --------------------------------------- | --------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------------- |
| `.github/workflows/paseo-ci.yml`        | 只覆盖 Paseo。本 PR 不修改它。它不验证沙箱。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                                  |
| `.github/workflows/sandbox-quality.yml` | `pull_request`、push `main`、`merge_group`、`workflow_dispatch`，无路径过滤。两个 job：`Sandbox quality` 与 `CodeQL QA`。质量 job 是 `contents: read`，超时 15 分钟；除 `merge_group` 外，并发会取消进行中的运行。步骤为 checkout、Node 24.18.0、`npm --prefix circus-sandbox/qa ci`、`npm --prefix circus-sandbox/qa audit --audit-level=low`、`npm --prefix circus-sandbox/qa run check`、`npm --prefix circus-sandbox/qa test`，然后 `bash .github/scripts/sandbox-actionlint.sh`。low 及以上 advisory 使该 job 失败；audit 不是省略项，也不是 warn-only。不构建、不运行沙箱，没有 Cargo。 |
| `.github/codeql/sandbox-qa.yml`         | 路径只含 `circus-sandbox/qa`。忽略 `circus-sandbox/qa/node_modules`、`circus-paseo/` 和 `.omp/`。                                                                                                                                                                                                                                                                                                                                                                                                                                                                                             |
| `.github/scripts/sandbox-actionlint.sh` | actionlint 1.7.12 按官方发布资产的 SHA-256 固定。下载前校验和必须是 64 位十六进制；下载后比对，不一致即失败。用 `mktemp -d` 建立私有临时目录，并在 `EXIT` 时清理。清理保留原退出码，不把校验和失败变成成功。shellcheck 只检查这个脚本。actionlint 只检查 `sandbox-quality.yml`。                                                                                                                                                                                                                                                                                                              |

`CodeQL QA` 超时 30 分钟。语言是 `javascript-typescript`，`build-mode` 为 none，查询集为 `security-extended`，配置文件是 `.github/codeql/sandbox-qa.yml`。权限是 contents、actions 的 read，以及 `security-events: write`。它分析本 PR 已有的 QA 源码，不分析 actions 或 Rust，也不扫描 Paseo。分析或上传失败则该 job 失败。告警是否阻断合并属于 ruleset，不在本工作流配置。它不依赖安全基线合并。

并行分支 `ci/sandbox-security-baseline` 从 `main` 单独交付安全门禁，不依赖本 PR 合并，本 PR 的 CodeQL 也不依赖它。本检出不能执行它的命令。该分支的实际范围以该分支为准，本文不复制未合并清单。按已拆开的计划，它不包含 QA 源码，CodeQL 只覆盖 actions，不扫描 Paseo，也不再分析本 PR 已覆盖的 JavaScript。Dependabot 只覆盖 `github-actions`。`circus-sandbox/qa` 的 npm 依赖更新等该 PR 合并后再追加到已有配置，本 PR 不新建 Dependabot，避免两份配置冲突。这是配置文件冲突，不是分析前置。

Rust 的 fmt、clippy、构建、测试、文档、example、cargo-deny 和 Rust CodeQL 随 `feat/capability-workspace` 的 `Sandbox Rust` 工作流交付，不在本检出。本检出没有 `sandbox-rust.yml`，也不以该工作流合并为前置。没有该提交上的运行记录，不能写成已构建或已分析。

dependency review 与密钥扫描的通过记录属于安全基线 PR 及其后的提交。本 PR 不要求、也不附这些扫描结果。许可证允许列表仍是后续独立 PR。dependency review 不是许可证策略。

`npm --prefix circus-sandbox/qa audit` 只审计 `circus-sandbox/qa` 的锁文件。它不审计 `circus-paseo/`，也不审计仓库里其它 npm 树。这次审计通过、CodeQL QA 通过，或安全基线合并，都不等于整个仓库的漏洞告警已经解决。告警数量只记在对应 PR 或报告，不写进本文。

secret scanning 与 push protection 是 GitHub 平台设置，不是本质量工作流的替代品。工作流存在也不等于它已被设为 `main` 的 required status check。合并仍需 1 个批准审查，作者不能批准自己的 PR，不得绕过保护。

质量工作流不覆盖 Rust CodeQL、Cargo 构建或沙箱隔离。本检出不链接未出现的 `sandbox-rust.yml`。它仍不覆盖沙箱隔离、`paseo-ci.yml` 的修改、Paseo 依赖审计和分支保护变更。质量门禁与安全基线都不把继承自仓库的 Paseo 告警写成已清除。

## 随后的 Rust 与真实后端

本分支没有 Cargo 工程。首个真实 crate 在并行分支 `feat/capability-workspace`，不依赖本基线合并。它只提供 cap-std 受限临时工作区的有界读写和可重试幂等清理，没有容器或服务。未合并前不是 `main` 的实现。

crate 所在分支上的 Cargo 检查以该分支的 `Sandbox Rust` 工作流为准。本检出没有 `Cargo.toml`，不能在这里执行这些命令，也不把未运行的 Cargo 写成已通过。

运行时必须消费上述 workspace 契约之后再开发。建议的首个适配仍是 Linux 上的 Docker + runc，且必须有真实运行证据。主机引擎可达不能提前声明该适配已支持。顺序见 [交付计划](delivery-plan.md)。
