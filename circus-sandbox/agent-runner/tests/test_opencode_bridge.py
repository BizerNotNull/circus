"""OpenCode adapter policy and event tests; no model calls or sandbox claims."""

import base64
import json
import threading
import time
from pathlib import Path
import types
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "src"
bridge = types.ModuleType("circus_opencode_test")
exec((SOURCE / "codex_bridge.py").read_text(), bridge.__dict__)
exec((SOURCE / "opencode_bridge.py").read_text(), bridge.__dict__)


class OpenCodeTests(unittest.TestCase):
    def setUp(self):
        self.provider = bridge.ModelEndpoint("https://example.invalid/approved/v1", "host-secret")
        self.controller = bridge.OpenCodeHostController(
            self.provider, "gpt-4.1", "task-token", "test prompt"
        )

    def test_responses_policy_is_fixed_to_host_model_and_task_token(self):
        body = json.dumps({"model": "gpt-4.1", "stream": True, "store": True}).encode()
        request = {
            "id": 1,
            "method": "POST",
            "path": "/v1/responses",
            "token": "task-token",
            "body": base64.b64encode(body).decode(),
        }
        _, forwarded = self.controller.validate_request(request)
        self.assertFalse(json.loads(forwarded)["store"])
        for change in ({"path": "/v1/chat/completions"}, {"token": "wrong"}):
            with self.subTest(change=change), self.assertRaises(bridge.BridgeError):
                self.controller.validate_request({**request, **change, "id": 2})
        self.assertEqual(self.provider.path, "/approved/v1/responses")
        self.assertEqual(self.provider.key, "host-secret")


    def test_expired_model_response_does_not_emit_late_http_start(self):
        sent = []

        class DelayedProvider:
            def forward(_self, _body, emit):
                time.sleep(0.02)
                emit("http_start", None)
                emit("http_end", None)

        self.controller.provider = DelayedProvider()
        self.controller.response_deadline = time.monotonic() - 1
        request = {
            "id": 1, "method": "POST", "path": "/v1/responses", "token": "task-token",
            "body": base64.b64encode(json.dumps({
                "model": "gpt-4.1", "stream": True,
            }).encode()).decode(),
        }
        with patch.object(bridge, "send", side_effect=lambda message: sent.append(message)), \
                patch.object(bridge.time, "monotonic", side_effect=[0, 100, 100]):
            self.controller.model_request(request)
            self.controller.worker.join(2)
        self.assertEqual(self.controller.response_deadline, 34)
        self.assertEqual(sent, [])

    def test_simultaneous_summary_and_user_requests_are_serialized_and_correlated(self):
        requests = []
        entered = threading.Event()
        release = threading.Event()

        class BlockingProvider:
            def forward(_self, body, emit):
                request = json.loads(body)
                requests.append(request["input"])
                if request["input"] == "summary":
                    entered.set()
                    self.assertTrue(release.wait(5))
                emit("http_start", None)
                emit("http_end", None)

        self.controller.provider = BlockingProvider()
        sent = []
        first = {
            "id": 1, "method": "POST", "path": "/v1/responses", "token": "task-token",
            "body": base64.b64encode(json.dumps({
                "model": "gpt-4.1", "stream": True, "input": "summary",
            }).encode()).decode(),
        }
        second = {**first, "id": 2, "body": base64.b64encode(json.dumps({
            "model": "gpt-4.1", "stream": True, "input": "user",
        }).encode()).decode()}
        with patch.object(bridge, "send", side_effect=lambda message: sent.append(message)):
            self.controller.model_request(first)
            self.assertTrue(entered.wait(2))
            pending = threading.Thread(target=self.controller.model_request, args=(second,))
            pending.start()
            time.sleep(0.02)
            self.assertTrue(pending.is_alive())
            release.set()
            pending.join(2)
            self.assertFalse(pending.is_alive())
            self.controller.worker.join(2)
        self.assertEqual(requests, ["summary", "user"])
        self.assertEqual([(item["channel"], item["id"]) for item in sent],
                         [("http_start", 1), ("http_end", 1),
                          ("http_start", 2), ("http_end", 2)])
    def test_json_events_map_text_tools_session_and_turn_completion(self):
        events = []
        messages = [
            {"type": "step_start", "sessionID": "ses_abc123"},
            {"type": "text", "sessionID": "ses_abc123", "part": {"text": "hello"}},
            {"type": "tool_use", "sessionID": "ses_abc123", "part": {
                "type": "tool", "id": "tool-1", "tool": "bash", "state": {
                    "status": "completed", "output": "ok"}}},
            {"type": "step_finish", "sessionID": "ses_abc123", "part": {"reason": "stop"}},
        ]
        with patch.object(bridge, "event", side_effect=lambda kind, **data: events.append((kind, data))), \
                patch.object(bridge, "text_events", side_effect=lambda kind, text, **data: events.append((kind, text, data))), \
                patch.object(bridge, "send"):
            for message in messages:
                self.controller.on_event(message)
            self.controller.finish(0)
        self.assertIn(("agent_session", {"id": "ses_abc123"}), events)
        self.assertIn(("text", "hello", {}), events)
        self.assertIn(("tool", {"id": "tool-1", "tool": "bash", "phase": "completed", "exit_code": None}), events)
        self.assertEqual(events[-1], ("turn_completed", {}))
        self.assertTrue(self.controller.revoked.is_set())

    def test_mismatched_session_and_failed_turn_never_succeed(self):
        self.controller.resume = "ses_expected"
        with patch.object(bridge, "event"), patch.object(bridge, "send"):
            with self.assertRaises(bridge.BridgeError):
                self.controller.on_event({"type": "text", "sessionID": "ses_other", "part": {"text": "x"}})
            self.controller.resume = None
            self.controller.on_event({"type": "step_start", "sessionID": "ses_expected"})
            with self.assertRaises(bridge.BridgeError):
                self.controller.finish(1)
            with self.assertRaises(bridge.BridgeError):
                self.controller.on_event({"type": "error", "sessionID": "ses_expected"})

    def test_config_and_command_pin_provider_resume_and_fixed_title(self):
        config = bridge.opencode_config(1234, "gpt-4.1", "task-token")
        self.assertEqual(config["model"], "circus/gpt-4.1")
        self.assertEqual(config["provider"]["circus"]["options"]["baseURL"], "http://127.0.0.1:1234/v1")
        self.assertEqual(config["provider"]["circus"]["options"]["apiKey"], "task-token")
        self.assertEqual(bridge.opencode_command("gpt-4.1", "ses_abc123"), [
            "opencode", "run", "--format", "json", "--title", "Circus task",
            "--model", "circus/gpt-4.1", "--session", "ses_abc123",
        ])
        for model in ["", "gpt/model", "bad model", "é"]:
            with self.subTest(model=model), self.assertRaises(bridge.BridgeError):
                bridge.opencode_config(1234, model, "task-token")


if __name__ == "__main__":
    unittest.main()
