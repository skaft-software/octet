"""Private echo suppression must not rewrite MCP result control fields."""
from __future__ import annotations

from contextlib import ExitStack
import copy
from dataclasses import replace
import json
from pathlib import Path
import tempfile
import time
import unittest
from unittest import mock

from octet_mcp.catalog import lower_tool_result, normalize_catalog_tool, published_tool_name, ToolResultError
from octet_mcp.config import BridgeConfig
from octet_mcp.http_2026 import McpHttp2026Client
from octet_mcp.interactions import run_operation
from octet_mcp.manager import BridgeManager
from octet_mcp.protocol import McpError
from octet_mcp.resources import call_resource, resource_bindings
from octet_mcp.streamable_http import McpStreamableHttpClient

from .helpers import FakeExtension, limits
from .test_interactions import PrivateUI, handler_for, needs_input
from .test_owner_lifecycle import context
from .test_protocol_2026 import DISCOVERY
from .test_resource_journey import PrivateOwnerExtension
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _initialize_result, _json_result, _remote_config, _sse_event,
)

TOOL = {"name": "fixture", "inputSchema": {"type": "object"},
        "annotations": {"readOnlyHint": True}}
URI = "fixture:entry"


def private_form(answer):
    kind = "boolean" if type(answer) is bool else "string"
    return {"message": "Choose a preference", "requestedSchema": {
        "type": "object", "properties": {"choice": {"type": kind}}, "required": ["choice"],
    }}


class PrivateResultRedactionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.scratch = Path(temporary.name)
        self.extension = FakeExtension(self.scratch)

    def lower(self, result, *, schema=None):
        tool = {**TOOL, **({"outputSchema": schema} if schema is not None else {})}
        binding = normalize_catalog_tool("fixture", "Fixture", tool, server_catalog_revision=1)
        return lower_tool_result(self.extension, binding, result, scratch_directory=self.scratch)

    def drive(self, answer, terminal, *, method="tools/call"):
        original = copy.deepcopy(terminal)
        handler = handler_for(PrivateUI(json.dumps({"choice": answer}), "accept"))
        responses = iter([needs_input(private_form(answer)), terminal])
        calls = []
        def send(method, params, **kwargs):
            calls.append(params)
            return next(responses)
        params = {"name": "fixture", "arguments": {}} if method == "tools/call" else {"uri": URI}
        result = run_operation(send, method, params, handler=handler, deadline=time.monotonic() + 1)
        self.assertEqual(len(calls), 2)
        self.assertEqual(calls[1]["inputResponses"]["server-input"], {
            "action": "accept", "content": {"choice": answer},
        })
        self.assertEqual(terminal, original)
        return result

    def test_loopback_both_eras_preserve_status_and_content_after_private_collisions(self):
        for modern in (False, True):
            for answer in ("isError", "Error", "resultType", "complete", "content", "text", "type", False, True):
                with self.subTest(modern=modern, answer=answer), ExitStack() as cleanup:
                    is_error = answer is not False
                    terminal = {"resultType": "complete", "isError": is_error,
                                "content": [{"type": "text", "text": "Operation failed" if is_error else "Operation succeeded"}],
                                "structuredContent": {"echo": answer}, "_meta": {"echo": answer}}
                    replies = []
                    tool_calls = []
                    def responder(request):
                        message = request.message()
                        method = message.get("method")
                        if method == "initialize":
                            return _json_result(request, {**_initialize_result(), "protocolVersion": "2025-11-25"})
                        if method == "server/discover":
                            return _json_result(request, {**DISCOVERY, "capabilities": {"tools": {}}})
                        if method == "tools/list":
                            return _json_result(request, {"tools": [TOOL]})
                        if method == "tools/call":
                            tool_calls.append(message)
                            params = message.get("params", {})
                            progress = {"jsonrpc": "2.0", "method": "notifications/progress", "params": {
                                "progressToken": params["_meta"]["progressToken"], "progress": 1, "total": 2,
                                "message": answer if isinstance(answer, str) else json.dumps(answer),
                                "resultType": "complete", "isError": answer, "_meta": {"echo": answer},
                            }}
                            completed = (_sse_event(progress)
                                         + _sse_event({"jsonrpc": "2.0", "id": message["id"], "result": terminal}))
                            if modern:
                                if "inputResponses" not in params:
                                    return _json_result(request, needs_input(private_form(answer), requestState="state"))
                                replies.append(params["inputResponses"]["server-input"])
                                return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=completed)
                            return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=(
                                _sse_event({"jsonrpc": "2.0", "id": "private-form", "method": "elicitation/create",
                                            "params": private_form(answer)}) + completed))
                        if method is None:
                            replies.append(message.get("result", message.get("error")))
                        return _HttpReply(status=202)
                    fixture = _LoopbackFixture(responder)
                    cleanup.callback(fixture.close)
                    config = replace(_remote_config(fixture.url), protocol_version="2026-07-28" if modern else None)
                    client_type = McpHttp2026Client if modern else McpStreamableHttpClient
                    client = client_type(config, limits(), enable_elicitation=True)
                    cleanup.callback(client.close)
                    client.start()
                    client.list_tools()
                    handler = handler_for(PrivateUI(json.dumps({"choice": answer}), "accept"))
                    events = []
                    result = client.call_tool("fixture", {}, interaction_handler=handler, progress=events.append)
                    self.assertEqual(len(events), 1)
                    self.assertEqual(events[0]["message"], "[private input]")
                    self.assertEqual(events[0]["_meta"], {"echo": "[private input]"})
                    if answer == "complete":
                        self.assertEqual(events[0]["resultType"], "[private input]")
                    if answer is False or answer is True:
                        self.assertEqual(events[0]["isError"], "[private input]")
                    self.assertEqual(replies, [{"action": "accept", "content": {"choice": answer}}])
                    self.assertEqual(len(tool_calls), 2 if modern else 1)
                    self.assertIsNone(client._fatal)
                    self.assertEqual(result["content"], terminal["content"])
                    self.assertEqual(result["resultType"], "complete")
                    self.assertIs(result["isError"], is_error)
                    self.assertEqual(result["structuredContent"], {"echo": "[private input]"})
                    self.assertEqual(result["_meta"], {"echo": "[private input]"})
                    self.assertIs(self.lower(result)["is_error"], is_error)

    def test_manager_both_eras_validate_raw_and_redacted_payloads(self):
        cases = [
            ("text", "tools/call", {"content": [{"type": "text", "text": False}]}, None, True),
            ("embedded", "tools/call", {"content": [
                {"type": "resource", "resource": {"uri": URI, "text": False}},
            ]}, None, True),
            ("link", "tools/call", {"content": [
                {"type": "resource_link", "uri": URI, "name": False},
            ]}, None, True),
            ("read", "resources/read", {"contents": [{"uri": URI, "text": False}]}, None, True),
        ]
        for field in ("name", "title", "description"):
            cases.append(("embedded-" + field, "tools/call", {"content": [
                {"type": "resource", "resource": {"uri": URI, "text": "done", field: False}},
            ]}, None, True))
        for schema_type in ("string", "boolean", ["boolean", "string"]):
            # A string schema is invalid before masking, a boolean schema after;
            # the union remains valid on both sides and must still succeed.
            cases.append((schema_type, "tools/call", {
                "content": [{"type": "text", "text": "done"}], "structuredContent": {"echo": False},
            }, {"type": "object", "properties": {"echo": {"type": schema_type}}},
                isinstance(schema_type, str)))
        for modern in (False, True):
            for label, operation, payload, schema, expected_error in cases:
                with self.subTest(modern=modern, case=label), ExitStack() as cleanup:
                    terminal = {"isError": False, **payload} if operation == "tools/call" else payload
                    tool = {**TOOL, **({"outputSchema": schema} if schema is not None else {})}
                    replies, calls = [], []
                    def responder(request):
                        message = request.message()
                        method = message.get("method")
                        if method in ("initialize", "server/discover"):
                            result = copy.deepcopy(DISCOVERY) if modern else {
                                **_initialize_result(), "protocolVersion": "2025-11-25",
                            }
                            return _json_result(request, {**result, "capabilities": {"tools": {}, "resources": {}}})
                        if method == "tools/list":
                            return _json_result(request, {"tools": [tool]})
                        if method == operation:
                            calls.append(message)
                            params = message["params"]
                            if modern:
                                if "inputResponses" not in params:
                                    return _json_result(request, needs_input(private_form(False)))
                                replies.append(params["inputResponses"]["server-input"])
                                return _json_result(request, {"resultType": "complete", **terminal})
                            return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=(
                                _sse_event({"jsonrpc": "2.0", "id": "private-form", "method": "elicitation/create",
                                            "params": private_form(False)})
                                + _sse_event({"jsonrpc": "2.0", "id": message["id"], "result": terminal})))
                        if method is None:
                            replies.append(message.get("result", message.get("error")))
                        return _HttpReply(status=202)
                    fixture = _LoopbackFixture(responder)
                    cleanup.callback(fixture.close)
                    extension = PrivateOwnerExtension(self.scratch, ['{"choice":false}', "accept"])
                    server = replace(_remote_config(fixture.url), protocol_version="2026-07-28" if modern else None)
                    manager = BridgeManager(
                        extension, BridgeConfig(servers=(server,), limits=limits()), scratch_directory=self.scratch,
                        experimental_streamable_http_mcp=True, private_ui=True,
                    )
                    cleanup.callback(manager.shutdown)
                    manager.start()
                    owner = context()
                    manager.observe_session("session/started", {"session_id": "host-session"})
                    self.assertTrue(manager.request_action("restart", "remote", context=owner).result(3))
                    name = (published_tool_name("remote", "fixture") if operation == "tools/call"
                            else "mcp_resources_remote_read")
                    arguments = {} if operation == "tools/call" else {"uri": URI}
                    result = extension._tools[name]["handler"](arguments, owner)
                    self.assertIs(result["is_error"], expected_error, result)
                    if not expected_error:
                        self.assertEqual(result["structured_content"], {"echo": "[private input]"})
                    self.assertEqual(replies, [{"action": "accept", "content": {"choice": False}}])
                    self.assertEqual(len(calls), 2 if modern else 1)
                    self.assertEqual(extension.artifacts, {})
                    self.assertIsNone(manager._servers["remote"].client._fatal)

    def test_protocol_shaped_payloads_do_not_gain_redaction_exemptions(self):
        for answer in ("isError", "resultType", "complete", "type", "text", "mimeType", "image/png", False):
            with self.subTest(answer=answer):
                payload = {"isError": answer, "resultType": answer,
                           "content": [{"type": answer, "text": answer, "mimeType": answer}]}
                terminal = {"content": [{"type": "text", "text": "done"}],
                            "structuredContent": payload, "_meta": payload, "extra": payload}
                result = self.drive(answer, terminal)
                for field in ("structuredContent", "_meta", "extra"):
                    encoded = json.dumps(result[field])
                    if isinstance(answer, str):
                        self.assertNotIn(answer, encoded.replace("[private input]", ""))
                    else:
                        self.assertNotIn("false", encoded)
                self.assertEqual(result["content"], terminal["content"])

    def test_resource_read_and_embedded_content_keep_only_protocol_structure(self):
        for answer in ("resource", "resource_link", "uri", "text", "contents", "mimeType"):
            with self.subTest(answer=answer):
                resource = {"uri": URI, "text": "echo: " + answer, "mimeType": "application/octet-stream",
                            "_meta": {"echo": answer}}
                result = self.drive(answer, {"resultType": "complete", "contents": [resource]}, method="resources/read")
                self.assertEqual(result["contents"][0]["uri"], URI)
                self.assertEqual(result["contents"][0]["text"], "echo: [private input]")
                client = mock.Mock(server_capabilities={"resources": {}})
                client.request.return_value = result
                binding = next(b for b in resource_bindings("fixture", "Fixture", server_catalog_revision=1)
                               if b.operation == "resources/read")
                lowered = call_resource(binding, client, {"uri": URI}, limits=limits(), timeout_ms=1000, cancellation=None)
                self.assertFalse(lowered["is_error"])
                client.request.assert_called_once()

                terminal = {"content": [
                    {"type": "resource", "resource": resource},
                    {"type": "resource_link", "uri": URI, "name": "entry", "description": "echo: " + answer},
                ]}
                result = self.drive(answer, terminal)
                self.assertEqual(result["content"][0]["type"], "resource")
                self.assertEqual(result["content"][0]["resource"]["text"], "echo: [private input]")
                self.assertEqual(result["content"][1]["type"], "resource_link")
                self.assertFalse(self.lower(result)["is_error"])

    def test_embedded_extension_fields_validate_before_masking_without_key_exemptions(self):
        for field in ("name", "title", "description", "size"):
            with self.subTest(field=field):
                resource = {"uri": URI, "text": "done", field: False}
                # Matching the key must not hide a field the lowerer validates.
                with self.assertRaises(McpError):
                    self.drive(field, {"content": [{"type": "resource", "resource": resource}]})
                resource[field] = 1 if field == "size" else "detail"
                result = self.drive(field, {"content": [{"type": "resource", "resource": resource}]})
                self.assertNotIn(field, result["content"][0]["resource"])
                self.assertEqual(result["content"][0]["resource"]["[private input]"], resource[field])
                self.assertFalse(self.lower(result)["is_error"])

    def test_media_protocol_literals_survive_without_exempting_payload_data(self):
        for kind, mime in (("image", "image/png"), ("audio", "audio/wav")):
            for answer in (kind, "type", "data", "mimeType", mime):
                with self.subTest(kind=kind, answer=answer):
                    terminal = {"content": [{"type": kind, "mimeType": mime, "data": "eHl6",
                                             "_meta": {"echo": answer}}]}
                    result = self.drive(answer, terminal)
                    part = result["content"][0]
                    self.assertEqual((part["type"], part["mimeType"], part["data"]), (kind, mime, "eHl6"))
                    self.assertEqual(part["_meta"], {"echo": "[private input]"})
                    lowered = self.lower(result)
                    artifact = next(part for part in lowered["content"] if part["type"] == kind)
                    self.assertEqual(self.extension.artifacts[artifact["artifact_id"]], (mime, b"xyz"))

    def test_invalid_control_values_and_redacted_output_schema_still_fail_closed(self):
        for answer, terminal in (
            ("true", {"isError": "true", "content": [{"type": "text", "text": "done"}]}),
            ("private-kind", {"content": [{"type": "private-kind", "text": "done"}]}),
            ("private/mime", {"content": [{"type": "image", "mimeType": "private/mime", "data": "eHl6"}]}),
        ):
            with self.subTest(answer=answer):
                result = self.drive(answer, terminal)
                self.assertNotIn(answer, json.dumps(result))
                with self.assertRaises(ToolResultError):
                    self.lower(result)
        result = self.drive(False, {"isError": False, "content": [{"type": "text", "text": "done"}],
                                    "structuredContent": {"echo": False}})
        self.assertEqual(result["structuredContent"], {"echo": "[private input]"})
        with self.assertRaises(ToolResultError):
            self.lower(result, schema={"type": "object", "properties": {"echo": {"type": "boolean"}}})
