# Codex 单任务调用示例

`examples/codex_agent/` 在现有 `DockerSandbox` 和 `ProcessSession` 上演示一个 Codex turn：初始化 app-server、创建会话、发送任务、等待完成，通过沙箱文件 API 取回一个指定文件，再销毁沙箱。它是调用方示例，尚未接入 Paseo，也不是新的沙箱核心 API。

## 模型通道

容器始终使用 `network=none`、只读根、非 root、无宿主 bind 和显式资源限额。容器内 Python 转发器只监听自己的 `127.0.0.1`，将 Codex 的模型请求封装到已有进程标准流；宿主 Python 控制器验证请求后访问操作者指定的 HTTPS 模型服务，并将 Responses SSE 流送回。Docker socket 只由 Rust 管理端使用。

```text
宿主控制器 ── HTTPS /responses ── 模型服务
     │
     │ ProcessSession stdin/stdout，带长度和总量限制
     │
容器转发器 ── 127.0.0.1 ── Codex app-server ── 项目文件与工具
```

- 上游地址、模型名和真实 API key 由宿主显式配置。真实 key 不写入镜像、工作区或沙箱 `ProcessRequest`，只传给宿主控制器。每次任务生成独立临时 token；任务完成、失败或控制器退出后不再接受其请求。
- 宿主只接受 `POST /v1/responses`，模型必须匹配配置，要求流式响应，强制 `store=false` 并拒绝后台任务。容器不能指定上游 URL、转发 Authorization 或任意 HTTP 头；不跟随重定向，不继承系统 HTTP 代理。
- 临时 token 的权限覆盖整个任务沙箱，不区分同一沙箱里的 Codex 与其工具进程；沙箱中的代码可以使用获准的模型通道。该通道不提供内容过滤、费用计量或 token 计费上限，也不提供 Git、软件包下载或任意互联网访问。
- 单次模型请求正文最多 2 MiB、响应最多 8 MiB；每个任务最多 16 次请求、累计响应最多 16 MiB，只允许一个在途模型请求。模型连接与读取空闲超时为 30 秒。每条进程输出流最多 32 MiB，进程与宿主控制器期限为 180 秒；这些是示例预算。
- 使用 Codex 的 `externalSandbox` turn 策略：文件、进程、网络与资源边界由外层 Docker 沙箱执行。自动化示例遇到额外审批或用户输入请求会失败，不代替用户批准。

Rust 示例使用 Tokio 管理宿主 Python 子进程与沙箱会话；失败或超时会终止并等待宿主控制器、显式调用沙箱销毁。正常任务先通过 `read_file` 取回指定结果，再销毁沙箱。若销毁失败，命令返回清理错误；不会宣称资源已回收。无需另外暴露监听端口、挂载 socket 或开启容器网络。

## 运行真实模型任务

前提：已构建 [Codex Agent 镜像](../images/codex/README.md)、显式可用的本地 Linux Docker Engine、宿主 Python 3.11+（含可信 CA）和支持 Codex 工具调用的 Responses SSE 服务。仅兼容 Chat Completions 的网关不能用于本示例。`CIRCUS_HOST_PYTHON` 可指定宿主 Python 路径，默认 `python3`；示例不会读取 Codex 个人登录文件。

在 `circus-sandbox/` 运行，以下为 Bash 示例。地址和模型名必须替换为已确认的服务配置；模型请求会发送任务内容并可能产生费用。

```bash
export CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock
export CIRCUS_AGENT_IMAGE=circus-agent-codex:0.164.0
export CIRCUS_MODEL_BASE_URL=https://your-approved-gateway.example/v1
export CIRCUS_MODEL=your-approved-model
read -r -s -p 'Model API key: ' CIRCUS_MODEL_API_KEY
export CIRCUS_MODEL_API_KEY
printf '\n'
export CIRCUS_AGENT_PROMPT='Create answer.py with add(a, b). Run a Python assertion that add(20, 22) equals 42. Write 42 followed by a newline to result.txt after the test passes.'
export CIRCUS_AGENT_RESULT=project/result.txt
cargo run --locked --example codex_agent
unset CIRCUS_MODEL_API_KEY
```

成功时打印完成提示与取回文件内容；失败时返回非零状态，不回显上游响应、模型内容或凭证。示例创建空项目，输入代码可由调用方使用已有 `write_file` 在启动前写入。`CIRCUS_AGENT_RESULT` 是沙箱工作区内的相对路径；指定文件不存在、超限或不安全时，取回失败并回收沙箱。

## 验证与边界

宿主策略与本地 HTTPS 测试不请求模型、不需要 Docker；HTTPS 测试需要 OpenSSL CLI，只生成临时测试证书，不修改系统信任设置：

```bash
python3 -B -m unittest discover -s tests -p 'test_codex_*.py' -v
```

真实 Docker / Codex 测试使用固定脚本生成的 Responses 事件作为对端，**不进行模型推理，也不证明任何实际网关兼容**：

```bash
CIRCUS_DOCKER_SANDBOX=1 \
CIRCUS_DOCKER_ENDPOINT=unix:///var/run/docker.sock \
CIRCUS_AGENT_IMAGE=circus-agent-codex:0.164.0 \
  cargo test --locked --test codex_agent -- --ignored --nocapture
```

这个测试会让真实 Codex 执行工具、生成 Python 文件并运行断言，检查外部连接被拒绝、工具环境没有上游 key，然后通过沙箱文件 API 取回结果；还覆盖上游失败和超时后的容器回收。测试对端仅由测试代码注入，正常示例没有切换到假模型的运行选项。验收使用 linux/amd64；其他平台、实际网关、长任务和高并发需单独验证。

协议依据：[Codex app-server](https://developers.openai.com/codex/app-server/) 的 `initialize`、`thread/start`、`turn/start`、`turn/completed`，以及 [配置参考](https://developers.openai.com/codex/config-reference/) 的自定义 Responses provider。运行验证针对镜像固定的 Codex CLI 0.164.0；升级 CLI 后需重跑实际验收。
