"""Example-only Codex/model bridge. Loaded with an explicit host or guest entrypoint.

The guest has no external network. The host accepts only bounded Responses requests
to its operator-configured HTTPS endpoint; the upstream key never crosses the pipe.
"""

import base64
import hmac
import http.client
from http.server import BaseHTTPRequestHandler, HTTPServer
import json
import os
import queue
import ssl
import subprocess
import sys
import threading
from urllib.parse import urlsplit

MAX_BODY = 2 * 1024 * 1024
MAX_FRAME = 3 * 1024 * 1024
MAX_RESPONSE = 8 * 1024 * 1024
MAX_TOTAL_RESPONSE = 16 * 1024 * 1024
MAX_REQUESTS = 16
CHUNK = 8192
WRITE_LOCK = threading.Lock()


class BridgeError(Exception):
    """Only fixed, non-sensitive diagnostics may be exposed to the operator."""


def unique_object(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise BridgeError("duplicate JSON key")
        result[key] = value
    return result


def decode_json(data):
    try:
        return json.loads(data, object_pairs_hook=unique_object,
                          parse_constant=lambda _: (_ for _ in ()).throw(BridgeError("non-finite JSON number")))
    except (ValueError, UnicodeError, RecursionError) as error:
        raise BridgeError("invalid JSON") from error


def send(message):
    wire = json.dumps(message, separators=(",", ":"), allow_nan=False).encode() + b"\n"
    if len(wire) > MAX_FRAME:
        raise BridgeError("outbound frame too large")
    with WRITE_LOCK:
        sys.stdout.buffer.write(wire)
        sys.stdout.buffer.flush()


def frames(stream):
    while True:
        line = stream.readline(MAX_FRAME + 1)
        if not line:
            return
        if len(line) > MAX_FRAME or not line.endswith(b"\n"):
            raise BridgeError("invalid frame boundary or size")
        message = decode_json(line)
        if not isinstance(message, dict):
            raise BridgeError("frame must be an object")
        yield message


def unpack(value, limit):
    if not isinstance(value, str) or len(value) > (limit + 2) // 3 * 4:
        raise BridgeError("invalid encoded body size")
    try:
        body = base64.b64decode(value, validate=True)
    except (ValueError, UnicodeError) as error:
        raise BridgeError("invalid body encoding") from error
    if len(body) > limit:
        raise BridgeError("body too large")
    return body


def required(name, limit):
    value = os.environ.get(name, "")
    if not value or len(value.encode()) > limit or any(ord(c) < 32 for c in value):
        raise BridgeError(f"invalid {name}")
    return value


class ModelEndpoint:
    def __init__(self, base_url, key):
        try:
            url = urlsplit(base_url)
            port = url.port or 443
        except ValueError as error:
            raise BridgeError("invalid model endpoint") from error
        if (url.scheme != "https" or not url.hostname or url.username or url.password
                or url.query or url.fragment or any(ord(c) <= 32 for c in base_url)
                or "\\" in base_url or any(part in (".", "..") for part in url.path.split("/"))):
            raise BridgeError("model endpoint must be an explicit HTTPS base URL")
        if not key or any(ord(c) < 32 or ord(c) > 126 for c in key):
            raise BridgeError("invalid upstream API key")
        self.host, self.port = url.hostname, port
        self.path = url.path.rstrip("/") + "/responses"
        self.key = key

    def forward(self, body, emit):
        # http.client does not follow redirects or read environment proxy settings.
        # Destination and Authorization come exclusively from host configuration.
        connection = http.client.HTTPSConnection(
            self.host, self.port, timeout=30, context=ssl.create_default_context())
        try:
            connection.request("POST", self.path, body=body, headers={
                "Authorization": "Bearer " + self.key,
                "Content-Type": "application/json", "Accept": "text/event-stream",
            })
            response = connection.getresponse()
            if response.status != 200:
                # Never echo upstream errors, headers or redirects into guest logs.
                raise BridgeError("model service rejected the request")
            if response.getheader("Content-Type", "").split(";", 1)[0].strip() != "text/event-stream":
                raise BridgeError("model service must return a Responses event stream")
            emit("http_start", None)
            total = 0
            while chunk := response.read1(CHUNK):
                total += len(chunk)
                if total > MAX_RESPONSE:
                    raise BridgeError("model response exceeded its budget")
                emit("http_chunk", chunk)
            emit("http_end", None)
        finally:
            connection.close()


class HostController:
    def __init__(self, provider, model, token, prompt):
        self.provider, self.model, self.token, self.prompt = provider, model, token, prompt
        self.thread_id = self.turn_id = None
        self.finished = False
        self.seen = set()
        self.total = 0
        self.worker = None
        self.revoked = threading.Event()

    def rpc(self, request_id, method, params):
        message = {"method": method, "params": params}
        if request_id is not None:
            message["id"] = request_id
        send({"channel": "rpc", "message": message})

    def validate_request(self, message):
        request_id = message.get("id")
        if (type(request_id) is not int or request_id <= 0 or request_id in self.seen
                or len(self.seen) >= MAX_REQUESTS):
            raise BridgeError("invalid model request id or request budget exhausted")
        if message.get("method") != "POST" or message.get("path") != "/v1/responses":
            raise BridgeError("model route is not allowed")
        token = message.get("token")
        if not isinstance(token, str) or not hmac.compare_digest(token.encode(), self.token.encode()):
            raise BridgeError("invalid task credential")
        raw = unpack(message.get("body"), MAX_BODY)
        body = decode_json(raw)
        if (not isinstance(body, dict) or body.get("model") != self.model
                or body.get("stream") is not True or body.get("background", False) is not False):
            raise BridgeError("model request violates task policy")
        # Persisting provider-side responses is never enabled by guest input.
        body["store"] = False
        encoded = json.dumps(body, separators=(",", ":"), allow_nan=False).encode()
        if len(encoded) > MAX_BODY:
            raise BridgeError("model body exceeded its budget")
        self.seen.add(request_id)
        return request_id, encoded

    def model_request(self, message):
        # A new HTTP request can arrive just after the prior worker emitted EOF.
        # Give that worker a bounded chance to release the single-request slot.
        if self.worker and self.worker.is_alive():
            self.worker.join(timeout=0.1)
        if self.finished or self.revoked.is_set() or (self.worker and self.worker.is_alive()):
            raise BridgeError("model channel is unavailable or busy")
        request_id, body = self.validate_request(message)

        def emit(kind, chunk):
            if self.revoked.is_set():
                raise BridgeError("task credential revoked")
            response = {"channel": kind, "id": request_id}
            if chunk is not None:
                self.total += len(chunk)
                if self.total > MAX_TOTAL_RESPONSE:
                    raise BridgeError("task response budget exhausted")
                response["body"] = base64.b64encode(chunk).decode("ascii")
            send(response)

        def forward():
            try:
                self.provider.forward(body, emit)
            except Exception:
                if not self.revoked.is_set():
                    send({"channel": "http_error", "id": request_id})

        self.worker = threading.Thread(target=forward, daemon=True)
        self.worker.start()

    def on_rpc(self, message):
        if not isinstance(message, dict):
            raise BridgeError("invalid app-server message")
        if "id" in message:
            if "method" in message:
                # This unattended example never grants an unexpected server request.
                raise BridgeError("app-server requested unsupported client interaction")
            if "error" in message:
                raise BridgeError("app-server rejected a request")
            result = message.get("result", {})
            if message["id"] == 1:
                if not result.get("userAgent"):
                    raise BridgeError("invalid initialize response")
                self.rpc(None, "initialized", {})
                self.rpc(2, "thread/start", {
                    "model": self.model, "modelProvider": "circus", "cwd": "/workspace/project",
                    "approvalPolicy": "never", "ephemeral": True,
                })
            elif message["id"] == 2:
                self.thread_id = result.get("thread", {}).get("id")
                if not isinstance(self.thread_id, str) or not self.thread_id:
                    raise BridgeError("missing thread id")
                self.rpc(3, "turn/start", {
                    "threadId": self.thread_id, "input": [{"type": "text", "text": self.prompt}],
                    "approvalPolicy": "never",
                    # The existing Docker policy is the enforcement boundary.
                    "sandboxPolicy": {"type": "externalSandbox", "networkAccess": "restricted"},
                })
            elif message["id"] == 3:
                turn_id = result.get("turn", {}).get("id")
                if self.turn_id is not None and turn_id != self.turn_id:
                    raise BridgeError("mismatched turn response")
                self.turn_id = turn_id
            else:
                raise BridgeError("unexpected response id")
        elif message.get("method") in ("turn/started", "turn/completed"):
            params = message.get("params", {})
            turn = params.get("turn", {})
            turn_id = turn.get("id")
            if (params.get("threadId") != self.thread_id or not isinstance(turn_id, str)
                    or (self.turn_id is not None and self.turn_id != turn_id)):
                raise BridgeError("mismatched turn notification")
            self.turn_id = turn_id
            if message["method"] == "turn/completed":
                if turn.get("status") != "completed":
                    raise BridgeError("agent turn did not complete successfully")
                self.finished = True
                self.revoked.set()
                send({"channel": "shutdown"})

    def run(self):
        self.rpc(1, "initialize", {"clientInfo": {"name": "circus_agent_example", "version": "0.1.0"}})
        for message in frames(sys.stdin.buffer):
            channel = message.get("channel")
            if channel == "rpc":
                self.on_rpc(message.get("message"))
            elif channel == "http_request":
                self.model_request(message)
            elif channel == "rpc_closed":
                if not self.finished:
                    raise BridgeError("app-server output closed before turn completion")
            elif channel == "exit":
                if not self.finished or message.get("code") != 0:
                    raise BridgeError("agent bridge exited before successful completion")
                return
            else:
                raise BridgeError("unexpected guest channel")
        raise BridgeError("guest output closed before completion")


def main_host(provider=None):
    controller = None
    try:
        model = required("CIRCUS_MODEL", 256)
        token = required("CIRCUS_TASK_TOKEN", 256)
        # Prompts may contain newlines; never log their content.
        prompt = os.environ.get("CIRCUS_AGENT_PROMPT", "")
        if not prompt or len(prompt.encode()) > 64 * 1024:
            raise BridgeError("invalid prompt size")
        if provider is None:
            provider = ModelEndpoint(required("CIRCUS_MODEL_BASE_URL", 2048),
                                     required("CIRCUS_MODEL_API_KEY", 4096))
        controller = HostController(provider, model, token, prompt)
        controller.run()
        print("Codex turn completed; model channel revoked", file=sys.stderr)
    except Exception as error:
        # Upstream/client/library errors may include credentials or response content.
        print(str(error) if isinstance(error, BridgeError) else "agent controller failed", file=sys.stderr)
        raise SystemExit(1) from None
    finally:
        if controller:
            controller.revoked.set()


class GuestServer(HTTPServer):
    def __init__(self, token):
        super().__init__(("127.0.0.1", 0), GuestHandler)
        self.token, self.next_id = token, 0
        self.pending = {}


class GuestHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        self.close_connection = True
        self.connection.settimeout(30)
        if (self.path != "/v1/responses" or self.headers.get("Transfer-Encoding")
                or self.headers.get("Content-Encoding")
                or len(self.headers.get_all("Content-Length", [])) != 1
                or not hmac.compare_digest(self.headers.get("Authorization", "").encode(),
                                           ("Bearer " + self.server.token).encode())):
            self.send_error(403)
            return
        try:
            length = int(self.headers["Content-Length"])
        except ValueError:
            self.send_error(400)
            return
        if not 0 < length <= MAX_BODY or self.server.next_id >= MAX_REQUESTS:
            self.send_error(413)
            return
        request_id = self.server.next_id = self.server.next_id + 1
        responses = queue.Queue(maxsize=32)
        self.server.pending[request_id] = responses
        started = False
        try:
            body = self.rfile.read(length)
            if len(body) != length:
                raise BridgeError("incomplete model request")
            send({"channel": "http_request", "id": request_id, "method": "POST", "path": self.path,
                  "token": self.server.token, "body": base64.b64encode(body).decode("ascii")})
            while True:
                message = responses.get(timeout=35)
                kind = message["channel"]
                if kind == "http_start" and not started:
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.send_header("Connection", "close")
                    self.end_headers()
                    started = True
                elif kind == "http_chunk" and started:
                    self.wfile.write(unpack(message.get("body"), CHUNK))
                    self.wfile.flush()
                elif kind == "http_end" and started:
                    return
                else:
                    raise BridgeError("model stream failed")
        except Exception:
            if not started:
                self.send_error(502, "model channel unavailable")
        finally:
            self.server.pending.pop(request_id, None)


def main_guest():
    token = required("CIRCUS_TASK_TOKEN", 256)
    server = GuestServer(token)
    port = server.server_address[1]
    settings = [
        'model_provider="circus"', 'web_search="disabled"',
        'model_providers.circus={name="Circus task channel",'
        f'base_url="http://127.0.0.1:{port}/v1",'
        'env_key="CIRCUS_TASK_TOKEN",wire_api="responses",requires_openai_auth=false,'
        'supports_websockets=false,request_max_retries=0,stream_max_retries=0}',
    ]
    argv = ["codex"]
    for setting in settings:
        argv.extend(["-c", setting])
    argv.extend(["app-server", "--listen", "stdio://"])
    # argv is built only from fixed settings and the loopback port allocated above;
    # no shell or caller-supplied command string is involved.
    with subprocess.Popen(argv, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                          stderr=subprocess.DEVNULL, cwd="/workspace/project") as process:
        def output():
            try:
                for message in frames(process.stdout):
                    send({"channel": "rpc", "message": message})
                send({"channel": "rpc_closed"})
            except Exception:
                send({"channel": "error"})

        reader = threading.Thread(target=output, daemon=True)
        web = threading.Thread(target=server.serve_forever, daemon=True)
        reader.start()
        web.start()
        try:
            for message in frames(sys.stdin.buffer):
                channel = message.get("channel")
                if channel == "rpc":
                    wire = json.dumps(message["message"], allow_nan=False).encode() + b"\n"
                    process.stdin.write(wire)
                    process.stdin.flush()
                elif channel in ("http_start", "http_chunk", "http_end", "http_error"):
                    pending = server.pending.get(message.get("id"))
                    if pending is None:
                        raise BridgeError("unknown model response id")
                    pending.put(message, timeout=5)
                elif channel == "shutdown":
                    break
                else:
                    raise BridgeError("unexpected host channel")
        finally:
            server.shutdown()
            server.server_close()
            process.stdin.close()
            try:
                code = process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                code = process.wait(timeout=5)
            reader.join(timeout=5)
        send({"channel": "exit", "code": code})
