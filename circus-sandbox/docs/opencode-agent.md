# 在沙箱中运行 OpenCode

`AgentKind::OpenCode` 是 `circus-agent-runner` 的固定 Agent 适配器。它复用现有任务、事件、产物、取消、MCP 与清理契约；不引入第二套 Session 管理器。此变更提供运行器和 stdio 配置选择，尚未提供维护的 OpenCode 镜像；调用方镜像必须预装 `opencode` CLI。

## 运行与凭据边界

```text
宿主 AgentRunner / stdio bridge
  ├─ 持有真实模型 key、HTTPS 目标、模型名、CA 配置
  ├─ 只转发指定模型的有界 OpenAI Responses SSE 请求
  └─ Docker + runc 一次性沙箱（禁网、non-root、只读根、无宿主 bind）
       ├─ 127.0.0.1 模型入口，只接受本任务临时 token
       ├─ opencode run --format json
       └─ /workspace/project 与 /workspace/.circus/home：项目及可选归档状态
```

沙箱内 OpenCode 使用 `@ai-sdk/openai` 自定义 provider，目标固定到本地回环转发器；真实 API key 只由宿主写入 HTTPS 请求。宿主强制匹配配置的模型、流式请求与 `store=false`，拒绝 guest 选择上游地址或携带任意认证头。容器禁网，MCP（如授权）沿用已配置的宿主 broker。模型网关必须兼容 OpenAI Responses API；Anthropic Messages 与仅兼容 Chat Completions 的端点不适用。

`ModelConfig.model` 对 OpenCode 必须是 1–256 个 ASCII 字符，首字符为字母或数字，其余字符只能是字母、数字、`.`、`_`、`:`、`-`。该检查发生在 sandbox 分配前。

## Rust 调用

```rust,ignore
let runner = AgentRunner::new(config)?.with_agent(AgentKind::OpenCode);
let task = runner.start(request)?;
```

OpenCode JSONL 文本、工具、阶段和 Session 事件映射到既有 `TaskEvent`；reasoning 不作为用户可见文本转发。适配器以固定 `Circus task` 标题启动 CLI，并通过标准输入传递原始 prompt，避免自动标题请求与首个模型请求并发，也避免 CLI 参数对引号和空白重写。成功还要求 CLI 以退出码 0 结束，并报告有效 step 完成和原生 Session ID。错误事件、Session 不匹配、CLI 非零退出和输出协议异常均使任务失败，不会被包装成成功。

`TaskRequest.agent_session` 可传回 OpenCode 原生 Session ID（长度及字符规则与 runner 现有边界相同）。需要跨任务续接时，还必须复用同一 `TaskWorkspace` 归档：每个任务仍创建新 sandbox，OpenCode 的 XDG 数据目录位于 `/workspace/.circus/home` 并随工作区归档保存。没有归档时，Session 文件不会跨销毁保留。归档是调用方持有的明文数据，需自行管理保留与保护。

stdio bridge 配置支持 `"agent": "open-code"`；省略该字段仍默认 Codex。镜像选择仍由调用方 `agentImage` 决定。任何 Agent CLI 都不是 shell 命令配置，不能通过配置传入任意可执行文件或参数。

## 验证

以下检查不需要 Docker、OpenCode CLI、模型凭据或真实模型服务：

```bash
cargo test --locked -p circus-agent-runner --lib
cargo test --locked -p circus-stdio-bridge
python3 -B -m unittest discover -s agent-runner/tests -p 'test_opencode_*.py' -v
```

这些测试覆盖 Responses host-policy、事件映射、Session 校验、固定 provider 配置、命令参数和 bridge agent 选择；它们不证明真实 OpenCode 镜像或 CLI 运行。含 CLI 的实际镜像与 Docker 端到端验收为后续独立工作。
