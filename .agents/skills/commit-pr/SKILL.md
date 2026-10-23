---
name: commit-pr
description: >
  自动整理当前改动、生成准确的 commit 并创建 Pull Request。用于用户要求提交代码、commit 后提 PR、向上游贡献或向自己的远程仓库提 PR，即使只说“提交并提个 PR”。支持 upstream 与 remote 两种目标，使用项目 PR 模板，正文简洁自然。不用于只审查代码、只拟写提交信息、发布版本或未要求提交的普通开发任务；只要求 commit 时不要擅自 push 或创建 PR。
---

# Commit 与 PR

默认完成用户授权的提交和 PR，不停在生成草稿。只有 commit 请求就只提交；创建本技能本身不构成执行提交、推送或发 PR 的授权。缺少目标信息或权限时，完成安全的本地步骤，说明具体阻塞，不改投其他仓库。

## 1. 确定范围与目标

在仓库根目录执行只读检查：

```sh
git status --short --branch
git diff --stat
git diff --cached --stat
git diff
git diff --cached
git remote -v
git branch -vv
git log -8 --format=%s
```

- 阅读未跟踪且拟提交的文件；diff 不会显示其内容。二进制文件单独核对用途与大小。
- 已有暂存内容就是默认提交边界，不擅自混入未暂存改动；若与用户任务冲突，先确认范围。暂存区为空时，仅选择本次任务的明确路径，不使用 `git add .` 或 `git add -A` 收进无关工作。
- 检查密钥、个人信息、生成物和无关文件。发现疑似敏感内容，停止对外操作并告知用户，不在回复或 PR 中复述秘密。
- 读取受影响目录的开发约定、提交钩子配置和实际验证入口。沿用项目约定，不修改无关代码、依赖或配置来凑检查通过。
- 检查当前分支；不要直接在目标 base 分支提交或推送。必要时用 `git switch -c <type>/<short-topic>` 保留当前改动并新建主题分支。不强行切换、stash 或丢弃用户工作。

### 两种 PR 路由

`remote` 是用户选定的远程仓库，不是“向 upstream 推送”的同义词。Git 分支的 tracking upstream 也不等于上游项目仓库。

| 模式 | PR 目标 target | 分支推送位置 push_remote | PR head |
| --- | --- | --- | --- |
| `upstream`：向上游提交 | 用户指定的上游；否则解析 `upstream` remote；无该 remote 时核实当前 fork 的 GitHub parent | 自己可写的 fork，通常为 `origin`，必须核实 | `<fork-owner>:<branch>` |
| `remote`：向远程提交 | 用户指定 remote 对应的仓库；未指定时使用已核实的 `origin` | 同一个目标 remote | `<branch>` |

1. 从用户意图识别模式；“向上游贡献”选 `upstream`，“向 origin / 自己仓库提 PR”选 `remote`。只说“提 PR”且无法唯一确定时，只问一次目标模式，不根据 remote 名称猜测意图。
2. 用 `git remote get-url <remote>` 解析真实仓库，再用 `gh repo view <owner/repo> --json nameWithOwner,isFork,parent,defaultBranchRef` 核实身份。不要写死仓库 owner 或 `main`。
3. 记录 `mode`、`target`、`push_remote`、`head_owner`、`branch`、`base`。`base` 优先用用户指定分支，否则查询 **target** 的 `defaultBranchRef.name`，不用本地 tracking 分支代替。
4. 上游模式必须核实 push 仓库与 target 属于同一 fork 网络。不能从自己 fork 发跨仓 PR 时，解释缺少的 fork / 权限，不自动创建 fork 或改推上游。远程模式若没有目标仓库写权限，同样停止，不悄悄改成 fork 模式。
5. 发布前明确展示 target、base、head 和推送 remote。用户已明确授权这些目标的 push / PR 时直接执行；目标或范围尚未授权时，确认后再对外发布。

## 2. 生成并执行 commit

参考 oh-my-pi 的 `omp commit` 提示词，而非照搬其自动暂存或失败回退行为：

- 原始完整提示词：[analysis.md](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/commit/conventional/prompts/analysis.md)。
- 快速提示词：[fast.md](https://github.com/can1357/oh-my-pi/blob/main/packages/coding-agent/src/commit/conventional/prompts/fast.md)。
- 命令行为：[Creating commits](https://omp.sh/docs/commit)。

只根据 diff、文件内容、验证结果及用户提供的上下文分类，不猜测动机或未实现的行为。

- 一项可独立审查、验证的变更一个 commit；无关事项拆分，依赖项先提交。每处改动只提交一次，清单外的工作保持原状。混在同一文件且无法安全分离时先澄清，不能为了拆分覆盖用户改动。
- 使用 Conventional Commits：`type(scope): summary`。类型选 `feat`、`fix`、`refactor`、`perf`、`docs`、`test`、`build`、`ci`、`chore`、`style` 或 `revert`，按主要语义选，不按文件数量选。
- scope 仅在单一组件明显主导语义或约占 60% 以上改动时填写；跨组件或含糊时省略。优先沿用近期准确的 scope，最多两个短词，不用 `src`、`all`、`misc` 或项目名充数。
- summary 用小写英文过去时动词开头；含前缀通常不超过 72 字符，无句点、Markdown 或重复类型前缀。项目有明确提交规范时优先遵循项目规范。
- 正文通常省略；必要时写 1–3 条、最多 6 条高信息量要点，说明实际影响或非显然取舍。英文要点用过去时并以句点结束；不列导入、空格或机械重命名。
- issue 引用必须有依据。仅完整解决且目标仓库匹配时使用关闭关键字；否则用 `Refs`，跨仓库引用写完整 `owner/repo#number`。

提交信息形状（括号 scope 和正文均可省略）：

```text
fix(parser): handled empty token streams

- Preserved the caller's existing error handling.
```

执行顺序：

1. 按项目要求运行受影响的检查；行为改动还需实际运行受影响路径。纯文档 / 技能改动只检查结构、路径、引用和命令来源，不启动服务或跑全工作区测试。
2. 使用 `git add -- <明确路径>` 暂存，并核对 `git diff --cached` 与 `git diff --cached --check`。不要用暂存整文件的方式夹带同文件中的无关修改。
3. 用 `git commit -m '<header>'` 提交；有正文时添加独立的 `-m '<body>'`。复杂引号改用临时 UTF-8 消息文件和 `git commit -F <file>`，成功后删除临时文件。
4. 保留正常 hooks；禁止 `--no-verify`、擅自 amend、force push、`reset --hard`。hook 失败先读报错并修复任务范围内的问题，不能把不通过写成通过。
5. 失败后查看 `git status --short` 和 `git log -3 --oneline`，确认哪些提交已成功，再处理剩余改动。不盲目重跑原计划。不要直接调用可能自动全量暂存或生成兜底提交的 `omp commit` 来绕过这些边界。

无新改动但分支有待发出的提交时可继续 PR 流程，不创建空 commit；没有可供 PR 的差异就结束并说明。

## 3. 用项目模板撰写 PR

以下文件路径均相对于 **Git 仓库根目录**，不是技能目录。

- 本项目根仓库的 PR 读取 `.github/pull_request_template.md`，无论改动是否位于 `circus-paseo`，不要误用嵌套仓库模板。
- 只有 target 确实是独立 Paseo 仓库时，才使用 `circus-paseo/.github/PULL_REQUEST_TEMPLATE.md`，并遵循其中要求的贡献与 QA 文档。
- 上游模式读取目标 base 分支的实际 PR 模板与贡献要求。可用 `gh api 'repos/<target>/contents/.github/PULL_REQUEST_TEMPLATE.md?ref=<base>' --jq .content` 取回并解码；路径或分支含特殊字符时正确编码。找不到时检查 target 的根目录、`docs` 和 `.github/PULL_REQUEST_TEMPLATE/`，不能把网络或鉴权失败当成无模板。上游确无模板才回退到本项目根模板，并说明回退。
- 模板有多个候选且无明确默认时让用户选，不随意拼接。保留选定模板的标题、必填字段和检查清单；删去填写提示与空占位，不复制一份容易过期的模板到技能目录。

先查看 target base 到 head 的全部提交和差异，而不只概括最后一次 commit。需要时 fetch 精确的目标分支到明确的 remote-tracking ref；不要 merge / rebase 或覆盖本地分支。核实 head 没有夹带与本次 PR 无关的旧提交。

### 简洁、自然的文字

- 沿用模板及仓库语言。本项目默认中文正文，代码名和命令保留原文。
- 标题直接写变化，例如“修复空输入时的解析失败”，不要“全面优化解析体验”。
- 每节一两句或少量短条目即可；写“什么问题、改了什么、怎么验证”，不逐文件复述 diff。
- 不用“本 PR 旨在”“值得注意的是”“显著提升”“赋能”“优雅地”“全面增强”等套话；不加表情、宣传语、自我评价或未经要求的 AI 署名。不删除项目规定的归属声明。
- 不虚构性能、安全性、兼容性或测试结果；不要为了显得完整填满所有小节。不适用项写原因，不空喊“无风险”。
- 验证只写实际命令、工作目录和观察结果。未运行写原因及剩余风险；未满足的清单保持未勾选。CI 未运行或仍在执行就如实写，不以“CI 会检查”代替证据。
- 只给有事实支撑的回退方式；区分已实现与规划。审阅正文，删掉任何不增加事实的句子。

## 4. 推送并创建 PR

1. 将填写好的模板写入临时 UTF-8 正文文件，不留在提交内容中。
2. 明确推送 ref，防止 Git 默认配置推错远程：

```sh
git push --set-upstream <push_remote> HEAD:refs/heads/<branch>
```

3. 查询相同 target / head / base 是否已有 PR，核实返回记录的 head 仓库，避免同名分支误判。有则返回 URL；只有用户要求时更新其正文，不重复创建或覆盖人工修改。
4. 用显式 `--repo`、`--base`、`--head`、`--body-file` 创建，不用 `--fill` 绕过模板，也不依赖当前目录的默认 GitHub 仓库。

上游模式：

```sh
gh pr create --repo <upstream-owner/repo> --base <base> --head <fork-owner>:<branch> --title '<title>' --body-file <body-file>
```

远程模式：

```sh
gh pr create --repo <remote-owner/repo> --base <base> --head <branch> --title '<title>' --body-file <body-file>
```

未授权时不附带 reviewer、assignee、label、合并、发布或其他额外操作。必要验证未通过或未完成时先报告，不擅自发成 ready PR；用户允许继续发布时可用 `--draft` 并写明限制。

5. 用 `gh pr view <url> --json url,baseRefName,headRefName,headRepositoryOwner,body` 核对实际目标分支、head 和模板正文；用返回 URL 核对目标仓库。创建调用报错时先查询有无已创建 PR，避免重试产生重复项。推送失败保留本地提交，PR 失败保留已推送分支，报告具体状态；不强推、不改目标、不声称完成。
6. 删除本次临时文件。最终只给 commit 短 SHA 与标题、PR URL、`mode / target / base ← head`、实际验证结果及未完成项。不要粘贴整份 PR 正文。
