"""Real loopback HTTP evidence for manager-level credential/session ownership."""
from __future__ import annotations

from pathlib import Path
import tempfile
import unittest

from octet_mcp.config import BridgeConfig, HttpAuthConfig
from octet_mcp.manager import BridgeManager

from .helpers import limits, wait_for
from .test_owner_lifecycle import OwnerBroker, OwnerExtension, context
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _initialize_result, _json_result, _remote_config, _tool,
)


class OwnerHttpTests(unittest.TestCase):
    def test_distinct_owner_bridges_use_distinct_bearer_tokens_and_http_sessions(self):
        sessions = {}

        def responder(request):
            token = request.header("authorization")
            if request.method == "DELETE":
                return _HttpReply()
            message = request.message()
            method = message["method"]
            if method == "initialize":
                self.assertIsNone(request.header("mcp-session-id"))
                session = "http-session-" + str(len(sessions) + 1)
                sessions[token] = session
                return _json_result(request, _initialize_result(), headers={"Mcp-Session-Id": session})
            self.assertEqual(request.header("mcp-session-id"), sessions[token])
            if method == "notifications/initialized":
                return _HttpReply(status=202)
            if method == "tools/list":
                return _json_result(request, {"tools": [_tool()]})
            if method == "tools/call":
                value = message["params"]["arguments"]["value"]
                return _json_result(request, {
                    "content": [{"type": "text", "text": "echo: " + value}],
                    "structuredContent": {"echo": value}, "isError": False,
                })
            return _HttpReply(status=400)

        fixture = _LoopbackFixture(responder)
        self.addCleanup(fixture.close)
        broker = OwnerBroker()
        with tempfile.TemporaryDirectory() as directory:
            managers = []
            try:
                for name in ("owner-a", "owner-b"):
                    owner = context(name, host_session="host-" + name)
                    extension = OwnerExtension(Path(directory))
                    manager = BridgeManager(
                        extension,
                        BridgeConfig(
                            servers=(_remote_config(fixture.url, auth=HttpAuthConfig(credential="same-reference")),),
                            limits=limits(shutdown_timeout_ms=250),
                        ),
                        scratch_directory=Path(directory), credential_provider=broker,
                        experimental_streamable_http_mcp=True,
                    )
                    managers.append(manager)
                    before = len(fixture.requests)
                    manager.start()
                    self.assertEqual(len(fixture.requests), before)
                    manager.observe_session("session/started", {"session_id": owner["host"]["session_id"]})
                    self.assertTrue(manager.request_action("restart", "remote", context=owner).result(3))
                    handler = next(iter(extension._tools.values()))["handler"]
                    self.assertEqual(handler({"value": name}, owner)["structured_content"], {"echo": name})
                    before = len(fixture.requests)
                    self.assertTrue(handler({"value": "foreign"}, context("foreign"))["is_error"])
                    self.assertEqual(len(fixture.requests), before)
                    manager.observe_session("session/settled", {"session_id": owner["host"]["session_id"]})
                    wait_for(lambda: not extension._tools)
                    before = len(broker.calls)
                    self.assertTrue(handler({"value": "settled"}, owner)["is_error"])
                    self.assertEqual(len(broker.calls), before)
            finally:
                for manager in managers:
                    manager.shutdown()
        self.assertEqual(set(sessions), {"Bearer token-owner-a", "Bearer token-owner-b"})
        self.assertEqual(len(set(sessions.values())), 2)
        self.assertEqual(fixture.errors, ())

    def test_authless_remote_still_requires_active_complete_owner(self):
        def responder(request):
            self.assertIsNone(request.header("authorization"))
            if request.method == "DELETE":
                return _HttpReply()
            method = request.message()["method"]
            if method == "initialize":
                return _json_result(request, _initialize_result(), headers={"Mcp-Session-Id": "anonymous-session"})
            if method == "notifications/initialized":
                return _HttpReply(status=202)
            return _json_result(request, {"tools": [_tool()]})

        fixture = _LoopbackFixture(responder)
        self.addCleanup(fixture.close)
        with tempfile.TemporaryDirectory() as directory:
            extension = OwnerExtension(Path(directory))
            manager = BridgeManager(
                extension, BridgeConfig(servers=(_remote_config(fixture.url),), limits=limits(shutdown_timeout_ms=100)),
                experimental_streamable_http_mcp=True,
            )
            try:
                manager.start()
                self.assertEqual(fixture.requests, ())
                with self.assertRaises(ValueError):
                    manager.request_action("restart", "remote")
                owner = context()
                manager.observe_session("session/started", {"session_id": "host-session"})
                self.assertTrue(manager.request_action("restart", "remote", context=owner).result(3))
                handler = next(iter(extension._tools.values()))["handler"]
                before = len(fixture.requests)
                self.assertTrue(handler({"value": "foreign"}, context("owner-b"))["is_error"])
                self.assertEqual(len(fixture.requests), before)
                manager.observe_session("session/settled", {"session_id": "host-session"})
                wait_for(lambda: not extension._tools)
                self.assertTrue(handler({"value": "late"}, owner)["is_error"])
            finally:
                manager.shutdown()
        self.assertEqual(fixture.errors, ())


if __name__ == "__main__":
    unittest.main()
