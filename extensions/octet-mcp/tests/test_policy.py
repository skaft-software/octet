from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

from octet_extension import CancelledError
from octet_mcp.config import BridgeConfig
from octet_mcp.manager import BridgeManager

from .helpers import FakeCancellation, FakeExtension, limits, server_config, wait_for


class PolicyCallTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.scratch = Path(self.temporary.name)
        self.journal = self.scratch / "upstream.jsonl"
        self.extension = FakeExtension(self.scratch, policy="allow")
        self.extension.evaluate_policy = mock.Mock(wraps=self.extension.evaluate_policy)
        server = replace(
            server_config(request_timeout_ms=1000),
            args=(str(Path(__file__).with_name("policy_mcp_server.py")), str(self.journal)),
        )
        self.manager = BridgeManager(
            self.extension,
            BridgeConfig(servers=(server,), limits=limits(shutdown_timeout_ms=300)),
            scratch_directory=self.scratch,
        )
        self.addCleanup(self.manager.shutdown)
        self.manager.start()
        wait_for(lambda: self.manager._servers["fixture"].state == "ready")

    def published_name(self, upstream_name):
        return next(
            binding.published_name
            for binding in self.manager._servers["fixture"].tools.values()
            if binding.upstream_name == upstream_name
        )

    def call(self, upstream_name, arguments=None):
        definition = self.extension._tools[self.published_name(upstream_name)]
        return definition["handler"](arguments or {}, {})

    def messages(self):
        if not self.journal.exists():
            return []
        # A final line may still be being written while a cancellation test polls.
        lines = self.journal.read_text(encoding="utf-8").splitlines(keepends=True)
        return [json.loads(line) for line in lines if line.endswith("\n")]

    def upstream_calls(self):
        return [message for message in self.messages() if message["method"] == "tools/call"]

    def test_authorized_unknown_missing_and_mutation_each_reach_upstream_once(self):
        names = ("unknown", "missing", "mutation", "contradictory")
        for name in names:
            with self.subTest(name=name):
                self.extension.evaluate_policy.reset_mock()
                result = self.call(name, {"hold": False})
                self.assertFalse(result["is_error"])
                self.assertEqual(result["content"][0]["text"], "executed")
                self.extension.evaluate_policy.assert_called_once_with({
                    "kind": "external_side_effect",
                    "operation": "mcp.tool.call",
                    "target": {
                        "server": "fixture",
                        "tool": self.published_name(name),
                        "server_catalog_revision": 1,
                        "arguments": {"hold": False},
                    },
                    "data_classes": ["tool_arguments"],
                    "adapter_hints": {
                        "read_only": False,
                        "destructive": name in {"mutation", "contradictory"},
                    },
                })
        self.manager.shutdown()
        self.assertEqual(
            [(message["params"]["name"], message["params"]["arguments"])
             for message in self.upstream_calls()],
            [(name, {"hold": False}) for name in names],
        )

    def test_non_read_only_calls_fail_closed_without_upstream_dispatch(self):
        features = self.extension.negotiated_features
        decisions = (
            ("denied", {"decision": "deny"}),
            ("approval_not_negotiated", {"decision": "ask", "approval_token": "unused"}),
            ("unknown_decision", {"decision": "unexpected"}),
            ("missing_decision", {}),
            ("evaluation_failed", None),
            ("policy_unavailable", {"decision": "allow"}),
        )
        for scenario, decision in decisions:
            for name in ("unknown", "missing", "mutation", "contradictory"):
                with self.subTest(scenario=scenario, name=name):
                    self.extension.negotiated_features = (
                        features - {"policy_intents"} if scenario == "policy_unavailable" else features
                    )
                    policy = self.extension.evaluate_policy
                    policy.reset_mock()
                    policy.return_value = decision
                    policy.side_effect = RuntimeError("unavailable") if decision is None else None
                    result = self.call(name)
                    self.assertTrue(result["is_error"])
                    self.assertIn("denied", result["content"][0]["text"].lower())
                    self.assertEqual(policy.call_count, 0 if scenario == "policy_unavailable" else 1)
        self.manager.shutdown()
        self.assertEqual(self.upstream_calls(), [])

    def test_read_only_bypasses_denied_or_unavailable_policy(self):
        self.extension.evaluate_policy.return_value = {"decision": "deny"}
        for available in (True, False):
            with self.subTest(policy_available=available):
                if not available:
                    self.extension.negotiated_features -= {"policy_intents"}
                result = self.call("read_only")
                self.assertFalse(result["is_error"])
                self.assertEqual(result["content"][0]["text"], "executed")
        self.extension.evaluate_policy.assert_not_called()
        self.manager.shutdown()
        self.assertEqual(
            [message["params"]["name"] for message in self.upstream_calls()],
            ["read_only", "read_only"],
        )

    def assert_mutation_not_replayed(self):
        # A succeeding call fences earlier stdio traffic; reconnect and catalog
        # refresh must not restore an ambiguous mutation as pending work either.
        self.assertFalse(self.call("read_only")["is_error"])
        self.assertTrue(self.manager.restart_server("fixture"))
        self.assertTrue(self.manager.refresh_server("fixture"))
        self.assertFalse(self.call("read_only")["is_error"])
        self.manager.shutdown()
        self.assertEqual(
            [(message["params"]["name"], message["params"]["arguments"])
             for message in self.upstream_calls()],
            [("mutation", {"hold": True}), ("read_only", {}), ("read_only", {})],
        )
        self.extension.evaluate_policy.assert_called_once()
        calls = self.upstream_calls()
        cancellations = [
            message for message in self.messages()
            if message["method"] == "notifications/cancelled"
        ]
        self.assertEqual(len(cancellations), 1)
        self.assertEqual(cancellations[0]["params"]["requestId"], calls[0]["id"])

    def test_authorized_mutation_timeout_never_replays(self):
        result = self.call("mutation", {"hold": True})
        self.assertTrue(result["is_error"])
        self.assertIn("timed out", result["content"][0]["text"])
        self.assertIn("not retried", result["content"][0]["text"])
        self.assert_mutation_not_replayed()

    def test_authorized_mutation_cancellation_never_replays(self):
        with ThreadPoolExecutor(max_workers=1) as executor:
            future = executor.submit(self.call, "mutation", {"hold": True})
            try:
                wait_for(lambda: len(self.upstream_calls()) == 1, message="upstream mutation")
                self.extension.cancellation.cancel("cancel mutation")
                with self.assertRaises(CancelledError):
                    future.result(timeout=2)
            finally:
                self.extension.cancellation.cancel()
        self.extension.cancellation = FakeCancellation()
        self.assert_mutation_not_replayed()


if __name__ == "__main__":
    unittest.main()
