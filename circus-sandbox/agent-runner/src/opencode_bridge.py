"""OpenCode CLI adapter over the existing host-only Responses and MCP channels."""


class OpenCodeHostController(HostController):
    def __init__(self, provider, model, token, prompt, resume=None):
        super().__init__(provider, model, token, prompt, resume)
        self.tools = {}
        self.started = False
        self.steps = 0

    def on_event(self, message):
        if not isinstance(message, dict) or self.finished:
            raise BridgeError("unexpected OpenCode event")
        session = message.get("sessionID")
        if not isinstance(session, str):
            raise BridgeError("missing OpenCode session id")
        session = thread_id(session)
        if self.thread_id is not None and session != self.thread_id:
            raise BridgeError("mismatched OpenCode session")
        if self.resume is not None and session != self.resume:
            raise BridgeError("mismatched resumed OpenCode session")
        if self.thread_id is None:
            self.thread_id = session
            event("agent_session", id=session)
        kind = message.get("type")
        if kind in ("step_start", "text", "reasoning", "tool_use") and not self.started:
            self.started = True
            event("running")
        if kind in ("text", "reasoning"):
            part = message.get("part")
            text = part.get("text") if isinstance(part, dict) else None
            if not isinstance(text, str):
                raise BridgeError("invalid OpenCode text event")
            if kind == "text":
                text_events("text", text)
            return
        if kind == "step_start":
            return
        if kind == "step_finish":
            part = message.get("part")
            reason = part.get("reason") if isinstance(part, dict) else None
            if not isinstance(reason, str) or not reason or len(reason) > 128:
                raise BridgeError("invalid OpenCode step result")
            self.steps += 1
            return
        if kind == "tool_use":
            part = message.get("part")
            if not isinstance(part, dict) or part.get("type") != "tool":
                raise BridgeError("invalid OpenCode tool event")
            tool_id, name = part.get("id"), part.get("tool")
            state = part.get("state")
            if (not isinstance(tool_id, str) or not 0 < len(tool_id) <= 256
                    or any(ord(c) < 32 for c in tool_id)
                    or not isinstance(name, str) or not 0 < len(name) <= 256
                    or any(ord(c) < 32 for c in name)
                    or not isinstance(state, dict)
                    or state.get("status") not in ("completed", "error")
                    or tool_id in self.tools or len(self.tools) >= 256):
                raise BridgeError("invalid OpenCode tool result")
            self.tools[tool_id] = name
            event("tool", id=tool_id, tool=name, phase="started", exit_code=None)
            output = state.get("output")
            if isinstance(output, str) and output:
                text_events("tool_output", output, id=tool_id)
            error = state.get("error")
            if state["status"] == "error" and isinstance(error, str) and error:
                text_events("tool_output", error, id=tool_id)
            self.tools.pop(tool_id)
            event("tool", id=tool_id, tool=name, phase="completed", exit_code=None)
            return
        if kind == "error":
            raise BridgeError("OpenCode task failed", "agent_failed")
        raise BridgeError("unsupported OpenCode event")

    def finish(self, code):
        if code != 0 or self.thread_id is None or self.steps == 0 or self.tools:
            raise BridgeError("OpenCode exited before a successful turn", "agent_failed")
        self.finished = True
        self.revoked.set()
        event("turn_completed")
        send({"channel": "shutdown"})

    def run(self):
        send({"channel": "start", "prompt": self.prompt, "session_id": self.resume})
        for message in frames(sys.stdin.buffer):
            channel = message.get("channel")
            if channel == "opencode_event":
                self.on_event(message.get("message"))
            elif channel == "http_request":
                self.model_request(message)
            elif channel == "mcp_request" and self.mcp is not None:
                self.mcp.request(message, self.token)
            elif channel in ("error", "rpc_closed"):
                if channel == "error":
                    raise BridgeError("OpenCode output stream failed")
            elif channel == "exit":
                self.finish(message.get("code"))
                return
            else:
                raise BridgeError("unexpected OpenCode guest channel")
        raise BridgeError("OpenCode guest output closed before completion")


def main_opencode_host(provider=None):
    controller = None
    try:
        model = required("CIRCUS_MODEL", 256)
        token = required("CIRCUS_TASK_TOKEN", 256)
        prompt = os.environ.get("CIRCUS_AGENT_PROMPT", "")
        if not prompt or len(prompt.encode()) > 64 * 1024:
            raise BridgeError("invalid prompt size")
        resume = os.environ.get("CIRCUS_RESUME_THREAD") or None
        if provider is None:
            provider = ModelEndpoint(required("CIRCUS_MODEL_BASE_URL", 2048),
                                    required("CIRCUS_MODEL_API_KEY", 4096),
                                    os.environ.get("CIRCUS_MODEL_CA_FILE"))
        controller = OpenCodeHostController(provider, model, token, prompt, resume)
        connections = decode_json(os.environ.get("CIRCUS_MCP_CONFIG", "[]"))
        if connections:
            controller.mcp = McpBroker(connections, controller.revoked)
        controller.run()
    except Exception as error:
        failure(error)
        raise SystemExit(1) from None
    finally:
        if controller:
            controller.revoked.set()
            if controller.mcp is not None:
                controller.mcp.close()


def opencode_model_id(value):
    return (isinstance(value, str) and 1 <= len(value) <= 256
            and value[0].isascii() and value[0].isalnum()
            and all(c.isascii() and (c.isalnum() or c in "._:-") for c in value))


def opencode_config(port, model, token):
    if type(port) is not int or not 1 <= port <= 65535:
        raise BridgeError("invalid loopback port")
    if not opencode_model_id(model):
        raise BridgeError("invalid OpenCode model id")
    if not isinstance(token, str) or not token:
        raise BridgeError("invalid task credential")
    result = {
        "model": f"circus/{model}",
        "provider": {
            "circus": {
                "npm": "@ai-sdk/openai",
                "name": "Circus task channel",
                "options": {
                    "baseURL": f"http://127.0.0.1:{port}/v1",
                    "apiKey": token,
                },
                "models": {model: {"name": model}},
            }
        },
    }
    if os.environ.get("CIRCUS_MCP_ENABLED") == "1":
        result["mcp"] = {
            "circus": {
                "type": "local",
                "command": ["python3", "-I", "-B", "/workspace/mcp_guest.py", str(port)],
                "enabled": True,
                "environment": {"CIRCUS_TASK_TOKEN": token},
                "timeout": 60000,
            }
        }
    return result


class OpenCodeGuestHandler(GuestHandler):
    auth_header = "Authorization"
    auth_prefix = "Bearer "


def opencode_command(model, session_id=None):
    if not opencode_model_id(model):
        raise BridgeError("invalid OpenCode model id")
    # Awaiting a fixed title prevents OpenCode's automatic title request from
    # racing the first turn through the single-request host controller.
    command = ["opencode", "run", "--format", "json", "--title", "Circus task",
               "--model", f"circus/{model}"]
    if session_id is not None:
        command.extend(["--session", thread_id(session_id)])
    return command


def main_opencode_guest():
    token = required("CIRCUS_TASK_TOKEN", 256)
    model = required("CIRCUS_MODEL", 256)
    server = GuestServer(token, OpenCodeGuestHandler)
    incoming = frames(sys.stdin.buffer)
    start = next(incoming, None)
    if not isinstance(start, dict) or start.get("channel") != "start":
        raise BridgeError("missing task start")
    prompt = start.get("prompt")
    if not isinstance(prompt, str) or not 0 < len(prompt.encode()) <= 64 * 1024:
        raise BridgeError("invalid prompt")
    session_id = start.get("session_id")
    if session_id is not None:
        session_id = thread_id(session_id)
    environment = os.environ.copy()
    environment.update({
        "OPENCODE_CONFIG_CONTENT": json.dumps(
            opencode_config(server.server_port, model, token), separators=(",", ":"), allow_nan=False
        ),
        "OPENCODE_CONFIG_DIR": "/workspace/.circus/home/.config/opencode",
        "XDG_CONFIG_HOME": "/workspace/.circus/home/.config",
        "XDG_DATA_HOME": "/workspace/.circus/home/.local/share",
        "XDG_CACHE_HOME": "/workspace/.circus/home/.cache",
        "XDG_STATE_HOME": "/workspace/.circus/home/.local/state",
    })
    command = opencode_command(model, session_id)
    with subprocess.Popen(command, shell=False, env=environment, cwd="/workspace/project",
                          stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL) as process:
        try:
            process.stdin.write(prompt.encode())
            process.stdin.close()
        except (BrokenPipeError, OSError):
            raise BridgeError("OpenCode did not accept the task prompt") from None
        def output():
            try:
                for message in frames(process.stdout):
                    send({"channel": "opencode_event", "message": message})
                send({"channel": "rpc_closed"})
            except Exception:
                send({"channel": "error"})

        reader = threading.Thread(target=output, daemon=True)
        web = threading.Thread(target=server.serve_forever, daemon=True)

        def wait():
            code = process.wait()
            reader.join(timeout=5)
            send({"channel": "exit", "code": code})

        waiter = threading.Thread(target=wait, daemon=True)
        reader.start()
        web.start()
        waiter.start()
        try:
            for message in incoming:
                channel = message.get("channel")
                if channel in ("http_start", "http_chunk", "http_end", "http_error"):
                    pending = server.pending.get(message.get("id"))
                    if pending is None:
                        raise BridgeError("unknown model response id")
                    pending.put(message, timeout=5)
                elif channel == "mcp_response":
                    pending = server.pending_mcp.get(message.get("id"))
                    if pending is None:
                        raise BridgeError("unknown MCP response id")
                    pending.put(message, timeout=5)
                elif channel == "shutdown":
                    break
                else:
                    raise BridgeError("unexpected host channel")
        finally:
            server.shutdown()
            server.server_close()
            if process.poll() is None:
                process.kill()
            process.wait(timeout=5)
            reader.join(timeout=5)
            waiter.join(timeout=5)
