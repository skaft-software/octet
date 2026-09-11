"""Stock SDK/runtime/manager/HTTP composition; private UI and servers are fixtures."""
from __future__ import annotations

from dataclasses import replace
import json
from pathlib import Path
import tempfile
import unittest

from octet_mcp.catalog import published_tool_name
from octet_mcp.http_2026 import McpHttp2026Client
from octet_mcp.streamable_http import McpStreamableHttpClient

from .helpers import wait_for
from .test_auth_runtime import AuthHost
from .test_interactions import FORM, needs_input
from .test_owner_lifecycle import context
from .test_protocol_2026 import DISCOVERY
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _initialize_result, _json_result, _remote_config, _sse_event,
)


class RuntimeHttpTests(unittest.TestCase):
    def setUp(self):
        directory = tempfile.TemporaryDirectory()
        self.addCleanup(directory.cleanup)
        self.root = Path(directory.name)

    def host(self, modern):
        replies = []
        tool = {"name": "private_echo", "inputSchema": {"type": "object", "additionalProperties": False},
                "annotations": {"readOnlyHint": True}}

        def responder(request):
            if request.method == "DELETE":
                return _HttpReply(status=202)
            message = request.message()
            method = message.get("method")
            params = message.get("params", {})
            if method in {"server/discover", "initialize"}:
                result = dict(DISCOVERY) if modern else _initialize_result()
                if not modern:
                    result["protocolVersion"] = "2025-11-25"
                    self.assertIn("elicitation", params["capabilities"])
                result["capabilities"] = {"tools": {}, "resources": {}}
                return _json_result(request, result)
            if method == "tools/list":
                return _json_result(request, {"tools": [tool]})
            if method in {"tools/call", "resources/read"}:
                if method == "tools/call":
                    result = {"content": [], "structuredContent": {"answer": "hidden-answer", "value": "visible-data"}}
                else:
                    result = {"contents": [{"uri": params["uri"], "text": "hidden-answer visible-data"}]}
                if modern:
                    if "inputResponses" not in params:
                        return _json_result(request, needs_input(FORM, requestState="opaque-state"))
                    self.assertEqual(params["requestState"], "opaque-state")
                    replies.append(params["inputResponses"]["server-input"])
                    return _json_result(request, {"resultType": "complete", **result})
                # The separate live-SSE tests require a callback before the terminal
                # result exists. Here the real SDK/manager correlation is the target.
                return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=(
                    _sse_event({"jsonrpc": "2.0", "id": message["id"], "method": "elicitation/create", "params": FORM})
                    + _sse_event({"jsonrpc": "2.0", "id": message["id"], "result": result})))
            if method is None:
                replies.append(message.get("result", message.get("error")))
            return _HttpReply(status=202)

        fixture = _LoopbackFixture(responder)
        self.addCleanup(fixture.close)
        config = replace(_remote_config(fixture.url, request_timeout_ms=3000),
                         protocol_version="2026-07-28" if modern else None)
        host = AuthHost(self.root / ("modern" if modern else "legacy"), config, real_http=True)
        self.addCleanup(host.close)
        self.assertTrue(host.manager._private_ui)
        host.manager.start()
        self.assertFalse(fixture.requests)
        host.command(["restart", "remote"])
        wait_for(lambda: host.manager._servers["remote"].state == "ready")
        self.assertIsInstance(host.manager._servers["remote"].client,
                              McpHttp2026Client if modern else McpStreamableHttpClient)
        return host, fixture, replies

    def call(self, host, name, arguments, *, owner=None):
        return host.invoke("tool/call", {"name": name, "arguments": arguments,
                           "catalog_revision": host.extension.tool_catalog_revision,
                           "context": owner or host.owner})

    def test_stock_runtime_private_forms_and_modern_continuations_are_really_wired(self):
        for modern in (False, True):
            with self.subTest(modern=modern):
                host, fixture, replies = self.host(modern)
                for name, arguments in ((published_tool_name("remote", "private_echo"), {}),
                                        ("mcp_resources_remote_read", {"uri": "file:///opaque-not-a-host-read"})):
                    answers = iter(['{"name":"hidden-answer"}', "accept"])
                    host.answers = lambda params: next(answers)
                    start = len(host.private)
                    response = self.call(host, name, arguments)
                    self.assertFalse(response["result"]["is_error"], response)
                    self.assertIn("visible-data", json.dumps(response))
                    self.assertNotIn("hidden-answer", json.dumps(response))
                    self.assertNotIn("opaque-state", json.dumps(response))
                    private = host.private[start:]
                    self.assertEqual(len(private), 3)
                    self.assertTrue(all(item["params"]["parent_request_id"] == response["id"] for item in private))
                    wait_for(lambda: bool(replies))
                    self.assertEqual(replies.pop(), {"action": "accept", "content": {"name": "hidden-answer"}})
                self.assertNotIn("hidden-answer", json.dumps(host.public) + host.logs.getvalue())
                methods = [r.message().get("method") for r in fixture.requests if r.method == "POST"]
                self.assertEqual(methods.count("tools/call"), 2 if modern else 1)
                self.assertEqual(methods.count("resources/read"), 2 if modern else 1)
                self.assertEqual(fixture.errors, ())

    def test_stock_runtime_foreign_or_settled_owner_cannot_prompt_or_send(self):
        for modern in (False, True):
            with self.subTest(modern=modern):
                host, fixture, _ = self.host(modern)
                name = published_tool_name("remote", "private_echo")
                before = len(fixture.requests)
                response = self.call(host, name, {}, owner=context("foreign-owner"))
                self.assertTrue(response["result"]["is_error"])
                self.assertFalse(host.private)
                self.assertEqual(len(fixture.requests), before)
                host.event("session/settled", host.owner["host"]["session_id"])
                wait_for(lambda: name not in host.extension._tools)
                response = self.call(host, name, {})
                self.assertEqual(response["error"]["code"], -32601)
                self.assertFalse(host.private)
                self.assertEqual(len(fixture.requests), before)


if __name__ == "__main__":
    unittest.main()
