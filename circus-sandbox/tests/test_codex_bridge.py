"""Host/guest protocol policy tests; no Docker, model traffic or real credentials."""

import base64
import importlib.util
import io
import json
from pathlib import Path
import unittest
from unittest.mock import patch

SOURCE = Path(__file__).resolve().parents[1] / "examples/codex_agent/bridge.py"
SPEC = importlib.util.spec_from_file_location("circus_codex_bridge", SOURCE)
bridge = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(bridge)


class ModelPolicyTests(unittest.TestCase):
    def setUp(self):
        self.controller = bridge.HostController(None, "allowed-model", "task-token", "task")

    def request(self, body=None, **overrides):
        raw = body if body is not None else b'{"model":"allowed-model","stream":true,"store":true}'
        return {"id": 1, "method": "POST", "path": "/v1/responses", "token": "task-token",
                "body": base64.b64encode(raw).decode(), **overrides}

    def test_guest_cannot_select_destination_or_forward_authorization(self):
        for override in [{"path": "https://other.invalid/responses"}, {"path": "/v1/models"},
                         {"path": "/v1/responses?redirect=1"}, {"method": "CONNECT"},
                         {"token": "wrong"}]:
            with self.subTest(override=override), self.assertRaises(bridge.BridgeError):
                self.controller.validate_request(self.request(**override))

    def test_model_and_background_are_fixed_and_storage_is_disabled(self):
        for raw in [b'{"model":"other","stream":true}',
                    b'{"model":"allowed-model","stream":false}',
                    b'{"model":"allowed-model","stream":true,"background":true}',
                    b'{"model":"other","model":"allowed-model","stream":true}']:
            with self.subTest(raw=raw), self.assertRaises(bridge.BridgeError):
                self.controller.validate_request(self.request(raw))
        _, body = self.controller.validate_request(self.request())
        self.assertIs(json.loads(body)["store"], False)

    def test_no_replay_and_bounded_requests(self):
        self.controller.validate_request(self.request())
        with self.assertRaises(bridge.BridgeError):
            self.controller.validate_request(self.request())
        self.controller.seen = set(range(bridge.MAX_REQUESTS))
        with self.assertRaises(bridge.BridgeError):
            self.controller.validate_request(self.request(id=100))

    def test_bounded_untrusted_json_and_encodings(self):
        for data in [b'{"a":NaN}', b'{"a":1,"a":2}', b'\xff', b'[' * 2000]:
            with self.subTest(data=data[:20]), self.assertRaises(bridge.BridgeError):
                bridge.decode_json(data)
        for encoded in ["!", "x" * 100, None]:
            with self.assertRaises(bridge.BridgeError):
                bridge.unpack(encoded, 16)
        with self.assertRaises(bridge.BridgeError):
            list(bridge.frames(io.BytesIO(b'{"channel":"rpc"}')))
        with patch.object(bridge, "MAX_FRAME", 10), self.assertRaises(bridge.BridgeError):
            list(bridge.frames(io.BytesIO(b'{"channel":"rpc"}\n')))

    def test_endpoint_requires_https_and_has_no_guest_url(self):
        for url in ["http://example.com/v1", "https://user:pass@example.com/v1",
                    "https://example.com/v1?api_key=x", "https://example.com/v1#x",
                    "https://example.com/../v1", "https://example.com:bad/v1"]:
            with self.subTest(url=url), self.assertRaises(bridge.BridgeError):
                bridge.ModelEndpoint(url, "host-key")
        endpoint = bridge.ModelEndpoint("https://gateway.example/api/v1/", "host-key")
        self.assertEqual((endpoint.host, endpoint.port, endpoint.path),
                         ("gateway.example", 443, "/api/v1/responses"))

    def test_upstream_errors_do_not_echo_credentials(self):
        class FailingProvider:
            def forward(self, *_args):
                raise RuntimeError("upstream echoed host-key and task prompt")
        self.controller.provider = FailingProvider()
        sent = []
        with patch.object(bridge, "send", sent.append):
            self.controller.model_request(self.request())
            self.controller.worker.join(timeout=2)
        self.assertEqual(sent, [{"channel": "http_error", "id": 1}])

    def test_revocation_prevents_new_model_requests(self):
        self.controller.revoked.set()
        with self.assertRaises(bridge.BridgeError):
            self.controller.model_request(self.request())

    def test_completions_are_correlated_and_failures_are_not_success(self):
        self.controller.thread_id, self.controller.turn_id = "thread", "turn"
        for thread, turn, status in [("other", "turn", "completed"),
                                     ("thread", "other", "completed"),
                                     ("thread", "turn", "failed")]:
            with self.assertRaises(bridge.BridgeError):
                self.controller.on_rpc({"method": "turn/completed", "params": {
                    "threadId": thread, "turn": {"id": turn, "status": status}}})
        sent = []
        with patch.object(bridge, "send", sent.append):
            self.controller.on_rpc({"method": "turn/completed", "params": {
                "threadId": "thread", "turn": {"id": "turn", "status": "completed"}}})
        self.assertTrue(self.controller.finished)
        self.assertTrue(self.controller.revoked.is_set())
        self.assertEqual(sent, [{"channel": "shutdown"}])

    def test_unexpected_approval_requests_are_not_granted(self):
        with self.assertRaises(bridge.BridgeError):
            self.controller.on_rpc({"id": 10, "method": "item/commandExecution/requestApproval"})


if __name__ == "__main__":
    unittest.main()
