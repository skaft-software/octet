"""Hermetic resource journeys through real stdio and both HTTP-era clients."""
from __future__ import annotations

import base64
from concurrent.futures import ThreadPoolExecutor
import copy
from dataclasses import replace
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest import mock

from octet_extension import CancelledError
from octet_mcp.catalog import published_tool_name
from octet_mcp.config import BridgeConfig, HttpAuthConfig
from octet_mcp.http_2026 import McpHttp2026Client
from octet_mcp.manager import BridgeManager
from octet_mcp.interactions import make_interaction_handler
from octet_mcp.protocol import McpError
from octet_mcp.protocol import McpStdioClient
from octet_mcp.resources import MAX_RESOURCE_TEXT_BYTES
from octet_mcp.streamable_http import McpStreamableHttpClient

from .helpers import FakeCancellation, FakeExtension, limits, server_config, wait_for
from .test_owner_lifecycle import OwnerBroker, OwnerExtension, context
from .test_protocol_2026 import DISCOVERY, METADATA, annotated_tool
from .test_resources import resource_payload
from .test_interactions import FORM, URL, needs_input
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _initialize_result, _json_result, _remote_config, _sse_event,
)


# Intentionally in this owned test file, not a second runtime or shared fixture.
# The reviewed fixture responds over the resident manager's existing stdio pipes.
_STDIO_FIXTURE = r'''
import base64
import json
import sys

journal = sys.argv[1]
for line in sys.stdin:
    message = json.loads(line)
    with open(journal, "a", encoding="utf-8") as log:
        log.write(json.dumps(message, separators=(",", ":")) + "\n")
    method = message.get("method")
    params = message.get("params", {})
    if "id" not in message:
        continue
    if method == "initialize":
        result = {"protocolVersion": "2025-06-18", "capabilities": {"tools": {}, "resources": {}},
                  "serverInfo": {"name": "resource-fixture", "version": "1"}}
    elif method == "tools/list":
        result = {"tools": [
            {"name": "linked", "inputSchema": {"type": "object"}, "annotations": {"readOnlyHint": True}},
            {"name": "resources/read", "inputSchema": {"type": "object"}},
        ]}
    elif method == "tools/call":
        result = {"content": [
            {"type": "resource_link", "uri": "file:///not-a-host-read", "name": "untrusted link"},
            {"type": "resource", "resource": {"uri": "embedded:data", "text": "embedded text"}},
        ]}
    elif method in ("resources/list", "resources/templates/list"):
        templates = method == "resources/templates/list"
        field = "resourceTemplates" if templates else "resources"
        identity = "uriTemplate" if templates else "uri"
        if "cursor" not in params:
            result = {field: [{identity: "custom:first/{+path}" if templates else "custom:first", "name": "first"}],
                      "nextCursor": " opaque/%2F+cursor\u0000 "}
        else:
            assert params["cursor"] == " opaque/%2F+cursor\u0000 "
            result = {field: [{identity: "file:///opaque/{name}" if templates else "file:///opaque", "name": "second"}]}
    elif method == "resources/read":
        uri = params["uri"]
        if uri == "fixture:slow":
            continue  # Keep reading stdin so cancellation can be observed.
        if uri == "fixture:interaction":
            result = {"resultType": "input_required", "requestState": "PRIVATE continuation"}
        else:
            result = {"contents": [
                {"uri": uri, "text": "UNTRUSTED resource text\n\u001b[31m", "mimeType": "text/plain"},
                {"uri": "binary:opaque", "blob": base64.b64encode(b"\x00\xffbinary").decode(),
                 "mimeType": "application/octet-stream"},
            ]}
    else:
        print(json.dumps({"jsonrpc": "2.0", "id": message["id"],
                          "error": {"code": -32601, "message": "unsupported"}}), flush=True)
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": message["id"], "result": result}), flush=True)
'''


class ResourceStdioJourneyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.script = self.directory / "resource_fixture.py"
        self.script.write_text(_STDIO_FIXTURE)
        self.journal = self.directory / "requests.jsonl"
        self.extension = FakeExtension(self.directory)
        self.server = replace(
            server_config(request_timeout_ms=2000), command=sys.executable,
            args=(str(self.script), str(self.journal)), cwd=self.directory,
        )
        self.manager = BridgeManager(
            self.extension, BridgeConfig(servers=(self.server,), limits=limits(shutdown_timeout_ms=200)),
            scratch_directory=self.directory,
        )
        self.manager.start()
        wait_for(lambda: self.manager._servers["fixture"].state == "ready")

    def tearDown(self):
        self.manager.shutdown()
        self.temporary.cleanup()

    def requests(self):
        return [json.loads(line) for line in self.journal.read_text().splitlines()]

    def handler(self, suffix):
        return self.extension._tools["mcp_resources_fixture_" + suffix]["handler"]

    def test_live_lists_templates_reads_and_resource_content_share_one_stdio_connection(self):
        client = self.manager._servers["fixture"].client
        self.assertIsInstance(client, McpStdioClient)
        self.assertEqual(len(self.extension._tools), 5)
        for suffix, field in (("list", "resources"), ("templates", "resourceTemplates")):
            result = self.handler(suffix)({}, {})
            self.assertFalse(result["is_error"])
            self.assertEqual(len(resource_payload(result)[field]), 2)
        linked = published_tool_name("fixture", "linked")
        result = self.extension._tools[linked]["handler"]({}, {})
        self.assertFalse(result["is_error"])
        self.assertEqual(len(result["content"]), 2)
        self.assertIn("resource link", result["content"][0]["text"])
        self.assertEqual([m for m in self.requests() if m["method"] == "resources/read"], [])
        uri = "file:///not-a-host-read?x=%2F+雪#opaque"
        read = self.handler("read")({"uri": uri}, {})
        self.assertFalse(read["is_error"])
        contents = resource_payload(read)["contents"]
        self.assertEqual(contents[0]["uri"], uri)
        self.assertEqual(contents[0]["text"], "UNTRUSTED resource text\n\u001b[31m")
        self.assertEqual(base64.b64decode(contents[1]["blob"]), b"\x00\xffbinary")
        self.assertEqual(self.extension.artifacts, {})
        self.assertIs(self.manager._servers["fixture"].client, client)
        self.assertEqual(sum(m["method"] == "initialize" for m in self.requests()), 1)
        denied_name = published_tool_name("fixture", "resources/read")
        before = len(self.requests())
        self.assertTrue(self.extension._tools[denied_name]["handler"]({}, {})["is_error"])
        self.assertEqual(len(self.requests()), before, "synthetic read authority cannot bless a similarly named tool")

    def test_stdio_cancellation_does_not_replay_or_replace_connection_and_old_epoch_stays_retired(self):
        handler = self.handler("read")
        client = self.manager._servers["fixture"].client
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(handler, {"uri": "fixture:slow"}, {})
            wait_for(lambda: any(m["method"] == "resources/read" for m in self.requests()))
            self.extension.cancellation.cancel()
            with self.assertRaises(CancelledError):
                future.result(timeout=2)
        wait_for(lambda: any(m["method"] == "notifications/cancelled" for m in self.requests()))
        requests = self.requests()
        self.assertEqual(sum(m["method"] == "resources/read" for m in requests), 1)
        self.assertEqual(sum(m["method"] == "notifications/cancelled" for m in requests), 1)
        self.assertIs(self.manager._servers["fixture"].client, client)
        self.extension.cancellation = FakeCancellation()
        self.assertFalse(handler({"uri": "fixture:fresh"}, {})["is_error"])
        self.assertTrue(self.manager.restart_server("fixture"))
        before = len(self.requests())
        self.assertTrue(handler({"uri": "fixture:stale"}, {})["is_error"])
        self.assertEqual(len(self.requests()), before)
        self.assertFalse(self.handler("read")({"uri": "fixture:fresh"}, {})["is_error"])

    def test_stdio_input_required_is_not_empty_success_or_automatic_continuation(self):
        result = self.handler("read")({"uri": "fixture:interaction"}, {})
        self.assertTrue(result["is_error"])
        self.assertIn("unsupported", result["content"][0]["text"])
        self.assertNotIn("PRIVATE", json.dumps(result))
        self.assertEqual(sum(m["method"] == "resources/read" for m in self.requests()), 1)


class ResourceHttpJourneyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.directory = Path(self.temporary.name)
        self.managers = []
        self.fixtures = []
        self.release = threading.Event()
        self.entered = threading.Event()
        self.read_mode = "complete"
        self.schema_header = "Old"

    def tearDown(self):
        self.release.set()
        for manager in self.managers:
            manager.shutdown()
        for fixture in self.fixtures:
            fixture.close()
            self.assertEqual(fixture.errors, ())
        self.temporary.cleanup()

    def manager(self, modern, *, resources=True, tools=True, auth=True):
        sessions = []
        capabilities = {}
        if resources:
            capabilities["resources"] = {}
        if tools:
            capabilities["tools"] = {}

        def responder(request):
            if request.method == "DELETE":
                self.assertFalse(modern)
                return _HttpReply()
            message = request.message()
            method = message["method"]
            params = message.get("params", {})
            if auth:
                self.assertEqual(request.header("Authorization"), "Bearer token-owner-a")
            else:
                self.assertIsNone(request.header("Authorization"))
            if modern:
                self.assertEqual(request.method, "POST")
                self.assertEqual(request.header("MCP-Protocol-Version"), "2026-07-28")
                self.assertEqual(request.header("Mcp-Method"), method)
                self.assertIsNone(request.header("Mcp-Session-Id"))
                self.assertIsNone(request.header("Last-Event-ID"))
                for key, value in METADATA.items():
                    self.assertEqual(params["_meta"][key], value)
            if method in {"server/discover", "initialize"}:
                self.assertEqual(method, "server/discover" if modern else "initialize")
                result = copy.deepcopy(DISCOVERY) if modern else _initialize_result()
                result["capabilities"] = capabilities
                sessions.append("resource-session-" + str(len(sessions) + 1))
                return _json_result(request, result, headers={} if modern else {"Mcp-Session-Id": sessions[-1]})
            if not modern:
                self.assertEqual(request.header("Mcp-Session-Id"), sessions[-1])
            if method in {"notifications/initialized", "notifications/cancelled"}:
                self.assertFalse(modern)
                return _HttpReply(status=202)
            if method == "tools/list":
                self.assertTrue(tools)
                tool = annotated_tool()
                tool["inputSchema"]["properties"]["value"]["x-mcp-header"] = self.schema_header
                return _json_result(request, {"tools": [tool]})
            if method == "tools/call":
                value = params["arguments"]["value"]
                return _json_result(request, {"content": [{"type": "text", "text": value}], "structuredContent": {"echo": value}})
            self.assertTrue(resources)
            if method in {"resources/list", "resources/templates/list"}:
                field, identity = (("resources", "uri") if method == "resources/list"
                                   else ("resourceTemplates", "uriTemplate"))
                if "cursor" in params:
                    self.assertEqual(params["cursor"], " cursor/%2f+雪 ")
                    result = {field: [{identity: "opaque:second/{id}", "name": "second"}]}
                else:
                    result = {field: [{identity: "file:///private/{id}", "name": "first"}],
                              "nextCursor": " cursor/%2f+雪 "}
                return _json_result(request, result)
            self.assertEqual(method, "resources/read")
            if self.read_mode == "blocked":
                self.entered.set()
                if not self.release.wait(3):
                    raise AssertionError("fixture read was not released")
            if self.read_mode == "input_required":
                result = needs_input(requestState="PRIVATE continuation")
            elif self.read_mode == "oversize":
                result = {"contents": [{"uri": params["uri"], "text": "x" * (MAX_RESOURCE_TEXT_BYTES + 1)}]}
            else:
                result = {"contents": [{"uri": params["uri"], "text": "PRIVATE resource body"},
                                       {"uri": "opaque:blob", "blob": "AP8=", "mimeType": "application/octet-stream"}]}
            return _json_result(request, result)

        fixture = _LoopbackFixture(responder)
        self.fixtures.append(fixture)
        owner = context()
        extension = OwnerExtension(self.directory)
        broker = OwnerBroker()
        server = replace(
            _remote_config(fixture.url, auth=HttpAuthConfig(credential="same-reference") if auth else None,
                           request_timeout_ms=2000),
            protocol_version="2026-07-28" if modern else None,
        )
        manager = BridgeManager(
            extension, BridgeConfig(servers=(server,), limits=limits(shutdown_timeout_ms=100)),
            credential_provider=broker, scratch_directory=self.directory,
            experimental_streamable_http_mcp=True,
        )
        self.managers.append(manager)
        manager.start()
        self.assertEqual(fixture.requests, ())
        manager.observe_session("session/started", {"session_id": "host-session"})
        self.assertTrue(manager.request_action("restart", "remote", context=owner).result(3))
        return extension, manager, fixture, broker, owner

    @staticmethod
    def handler(extension, suffix="read"):
        return extension._tools["mcp_resources_remote_" + suffix]["handler"]

    def test_legacy_and_modern_http_list_template_read_use_one_owner_bound_client(self):
        for modern in (False, True):
            with self.subTest(modern=modern):
                extension, manager, fixture, broker, owner = self.manager(modern)
                client = manager._servers["remote"].client
                self.assertIs(type(client), McpHttp2026Client if modern else McpStreamableHttpClient)
                for suffix, field in (("list", "resources"), ("templates", "resourceTemplates")):
                    result = self.handler(extension, suffix)({}, owner)
                    self.assertFalse(result["is_error"])
                    self.assertEqual(len(resource_payload(result)[field]), 2)
                uri = "https://user:password@127.0.0.1/host-is-not-fetching?x=%2f+雪#f"
                read = self.handler(extension)({"uri": uri}, owner)
                self.assertFalse(read["is_error"])
                self.assertEqual(resource_payload(read)["requestedUri"], uri)
                read_request = fixture.requests[-1]
                self.assertEqual(read_request.message()["params"]["uri"], uri)
                self.assertEqual(read_request.target, "/mcp")
                if modern:
                    self.assertEqual(read_request.header("Mcp-Name"), "=?base64?aHR0cHM6Ly91c2VyOnBhc3N3b3JkQDEyNy4wLjAuMS9ob3N0LWlzLW5vdC1mZXRjaGluZz94PSUyZivpm6ojZg==?=")
                self.assertIs(manager._servers["remote"].client, client)
                self.assertTrue(all(call[2] == owner["resource_owner"] for call in broker.calls))
                self.assertEqual(extension.artifacts, {})
                self.assertNotIn("PRIVATE resource body", json.dumps(extension.presentations))
                self.assertNotIn("mcp_resources_remote_read", json.dumps(manager.snapshot()))
                self.assertIn("mcp_resources_remote_read", json.dumps(manager.snapshot(owner)))
                before = len(fixture.requests)
                for foreign in ({}, context("other"), context(instance="other"), context(generation=2), context(host_session="other")):
                    self.assertTrue(self.handler(extension)({"uri": uri}, foreign)["is_error"])
                self.assertEqual(len(fixture.requests), before)

    def test_resource_only_and_unsupported_resources_are_capability_gated_on_both_eras(self):
        for modern in (False, True):
            for resources, tools in ((True, False), (False, True)):
                with self.subTest(modern=modern, resources=resources):
                    extension, manager, fixture, _, owner = self.manager(modern, resources=resources, tools=tools, auth=False)
                    names = [name for name in extension._tools if name.startswith("mcp_resources_")]
                    self.assertEqual(len(names), 3 if resources else 0)
                    methods = [r.message()["method"] for r in fixture.requests]
                    self.assertEqual("tools/list" in methods, tools)
                    self.assertFalse(any(method.startswith("resources/") for method in methods))
                    if resources:
                        self.assertFalse(self.handler(extension)({"uri": "opaque:explicit"}, owner)["is_error"])

    def test_modern_manager_passes_the_epoch_pinned_header_schema_and_restarts_retire_reads(self):
        extension, manager, fixture, _, owner = self.manager(True)
        name = published_tool_name("remote", "echo")
        old = extension._tools[name]["handler"]
        old_read = self.handler(extension)
        self.schema_header = "New"
        self.assertTrue(manager.request_action("refresh", "remote", context=owner).result(3))
        self.assertFalse(old({"value": "old epoch"}, owner)["is_error"])
        self.assertEqual(fixture.requests[-1].header("Mcp-Param-Old"), "old epoch")
        self.assertIsNone(fixture.requests[-1].header("Mcp-Param-New"))
        self.assertFalse(extension._tools[name]["handler"]({"value": "new epoch"}, owner)["is_error"])
        self.assertEqual(fixture.requests[-1].header("Mcp-Param-New"), "new epoch")
        self.assertTrue(manager.request_action("restart", "remote", context=owner).result(3))
        before = len(fixture.requests)
        self.assertTrue(old_read({"uri": "opaque:stale"}, owner)["is_error"])
        self.assertEqual(len(fixture.requests), before)
        self.assertFalse(self.handler(extension)({"uri": "opaque:current"}, owner)["is_error"])
        self.assertEqual(sum(r.message()["method"] == "server/discover" for r in fixture.requests), 2)

    def test_resource_result_bounds_and_input_required_fail_closed_without_continuation(self):
        for modern in (False, True):
            with self.subTest(modern=modern):
                extension, _, fixture, _, owner = self.manager(modern)
                for mode in ("input_required", "oversize"):
                    self.read_mode = mode
                    before = len(fixture.requests)
                    result = self.handler(extension)({"uri": "opaque:read"}, owner)
                    self.assertTrue(result["is_error"])
                    self.assertNotIn("PRIVATE", json.dumps(result))
                    self.assertEqual(len(fixture.requests), before + 1)
                self.read_mode = "complete"
                self.assertFalse(self.handler(extension)({"uri": "opaque:fresh"}, owner)["is_error"])

    def test_cancellation_uses_the_existing_transport_without_replaying_resource_read(self):
        for modern in (False, True):
            with self.subTest(modern=modern):
                self.entered.clear()
                self.release.clear()
                self.read_mode = "blocked"
                extension, manager, fixture, _, owner = self.manager(modern)
                client = manager._servers["remote"].client
                with ThreadPoolExecutor(max_workers=1) as pool:
                    future = pool.submit(self.handler(extension), {"uri": "opaque:slow"}, owner)
                    self.assertTrue(self.entered.wait(2))
                    extension.cancellation.cancel()
                    with self.assertRaises(CancelledError):
                        future.result(timeout=2)
                self.release.set()
                if not modern:
                    wait_for(lambda: any(r.message()["method"] == "notifications/cancelled" for r in fixture.requests))
                methods = [r.message()["method"] for r in fixture.requests]
                self.assertEqual(methods.count("resources/read"), 1)
                self.assertEqual(methods.count("notifications/cancelled"), 0 if modern else 1)
                self.assertIs(manager._servers["remote"].client, client)
                extension.cancellation = FakeCancellation()
                self.read_mode = "complete"
                self.assertFalse(self.handler(extension)({"uri": "opaque:fresh"}, owner)["is_error"])

    def test_owner_settlement_aborts_and_revokes_resources_without_fresh_owner_fallback(self):
        extension, manager, fixture, broker, owner = self.manager(False)
        handler = self.handler(extension)
        self.read_mode = "blocked"
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(handler, {"uri": "opaque:private"}, owner)
            self.assertTrue(self.entered.wait(2))
            manager.observe_session("session/settled", {"session_id": "host-session"})
            self.release.set()
            result = future.result(timeout=2)
        self.assertTrue(result["is_error"])
        self.assertNotIn("PRIVATE resource body", json.dumps(result))
        wait_for(lambda: not extension._tools)
        before = len(broker.calls)
        self.assertTrue(handler({"uri": "opaque:private"}, owner)["is_error"])
        self.assertEqual(len(broker.calls), before)
        manager.observe_session("session/started", {"session_id": "new-host"})
        with self.assertRaises(ValueError):
            manager.request_action("restart", "remote", context=context("new-owner", host_session="new-host"))
        self.assertEqual(sum(r.message()["method"] == "resources/read" for r in fixture.requests if r.method == "POST"), 1)


class PrivateOwnerExtension(OwnerExtension):
    api_version = "0.2"

    def __init__(self, scratch, answers):
        super().__init__(scratch)
        self.answers = iter(answers)
        self.inputs = []
        self.confirms = []
        self.input_entered = threading.Event()
        self.input_release = threading.Event()
        self.input_release.set()

    def request_input(self, prompt, **kwargs):
        self.inputs.append((prompt, kwargs, threading.get_ident()))
        self.input_entered.set()
        if not self.input_release.wait(3):
            raise AssertionError("private input was not released")
        return next(self.answers)

    def confirm(self, prompt, **kwargs):
        self.confirms.append((prompt, kwargs, threading.get_ident()))
        return True


class ManagerInteractionJourneyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.scratch = Path(self.temporary.name)
        self.managers = []
        self.fixtures = []
        self.extensions = []
        self.schema_header = "Initial"

    def tearDown(self):
        for extension in self.extensions:
            extension.input_release.set()
        for manager in self.managers:
            manager.shutdown()
        for fixture in self.fixtures:
            fixture.close()
            self.assertEqual(fixture.errors, ())
        self.temporary.cleanup()

    def manager(self, modern, *, private_ui=True, read_only=True, url=False):
        extension = PrivateOwnerExtension(self.scratch, ["accept"] if url else ['{"name":"private-name"}', "accept"])
        self.extensions.append(extension)
        owner = context()
        replies = []
        interaction = URL if url else FORM
        state = " opaque-state/雪\u0000 "

        def responder(request):
            if request.method == "DELETE":
                return _HttpReply()
            message = request.message()
            if "method" not in message:
                replies.append(message)
                return _HttpReply(status=202)
            method = message["method"]
            params = message.get("params", {})
            if method in {"initialize", "server/discover"}:
                result = copy.deepcopy(DISCOVERY) if modern else _initialize_result()
                result["capabilities"] = {"tools": {}, "resources": {}}
                if not modern:
                    self.assertEqual(params["capabilities"], {"elicitation": {"form": {}, "url": {}}} if private_ui else {})
                    result["protocolVersion"] = "2025-11-25" if private_ui else "2025-06-18"
                return _json_result(request, result, headers={} if modern else {"Mcp-Session-Id": "interactive-session"})
            if method in {"notifications/initialized", "notifications/cancelled"}:
                return _HttpReply(status=202)
            if method == "tools/list":
                tool = annotated_tool()
                tool["annotations"] = {"readOnlyHint": read_only}
                tool["inputSchema"]["properties"]["value"]["x-mcp-header"] = self.schema_header
                return _json_result(request, {"tools": [tool]})
            if method == "resources/list":
                return _json_result(request, {"resources": []})
            self.assertIn(method, {"tools/call", "resources/read"})
            terminal = ({"contents": [{"uri": params["uri"], "text": "private-name"}]}
                        if method == "resources/read" else {
                            "content": [{"type": "text", "text": "private-name"}],
                            "structuredContent": {"echo": "private-name"},
                        })
            if modern:
                self.assertEqual(params["_meta"]["io.modelcontextprotocol/clientCapabilities"],
                                 {"elicitation": {"form": {}, "url": {}}} if private_ui else {})
                if "inputResponses" not in params:
                    return _json_result(request, needs_input(interaction, requestState=state))
                self.assertEqual(params["requestState"], state)
                replies.append(params["inputResponses"]["server-input"])
                return _json_result(request, {"resultType": "complete", **terminal})
            peer = {"jsonrpc": "2.0", "id": "private-form", "method": "elicitation/create", "params": interaction}
            final = {"jsonrpc": "2.0", "id": message["id"], "result": terminal}
            return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=_sse_event(peer) + _sse_event(final))

        fixture = _LoopbackFixture(responder)
        self.fixtures.append(fixture)
        server = replace(_remote_config(fixture.url, request_timeout_ms=3000),
                         protocol_version="2026-07-28" if modern else None)
        manager = BridgeManager(
            extension, BridgeConfig(servers=(server,), limits=limits(shutdown_timeout_ms=100)),
            experimental_streamable_http_mcp=True, private_ui=private_ui, scratch_directory=self.scratch,
        )
        self.managers.append(manager)
        manager.start()
        manager.observe_session("session/started", {"session_id": "host-session"})
        self.assertTrue(manager.request_action("restart", "remote", context=owner).result(3))
        return extension, manager, fixture, owner, replies

    @staticmethod
    def call(extension, method, owner):
        name = "mcp_resources_remote_read" if method == "resources/read" else published_tool_name("remote", "echo")
        arguments = {"uri": "opaque:private"} if method == "resources/read" else {"value": "original"}
        return extension._tools[name]["handler"](arguments, owner)

    def test_actual_tool_and_resource_interactions_are_private_parent_bound_and_retired(self):
        for modern in (False, True):
            for method in ("tools/call", "resources/read"):
                with self.subTest(modern=modern, method=method):
                    extension, manager, fixture, owner, replies = self.manager(modern)
                    retained = []
                    def make(*args, **kwargs):
                        handler = make_interaction_handler(*args, **kwargs)
                        retained.append(handler)
                        return handler
                    with mock.patch("octet_mcp.manager.make_interaction_handler", side_effect=make):
                        result = self.call(extension, method, owner)
                    self.assertFalse(result["is_error"], result)
                    self.assertNotIn("private-name", json.dumps(result))
                    self.assertEqual(len(extension.inputs), 2)
                    for _, kwargs, thread in extension.inputs:
                        self.assertEqual(kwargs, {"secret": True, "parent_request_id": 77})
                        self.assertNotEqual(thread, threading.get_ident())
                    self.assertEqual(extension.confirms[0][1]["parent_request_id"], 77)
                    reply = replies[0] if modern else replies[0]["result"]
                    self.assertEqual(reply, {"action": "accept", "content": {"name": "private-name"}})
                    with self.assertRaises(McpError):
                        retained[0].check_active()
                    self.assertNotIn("private-name", json.dumps(extension.presentations))

    def test_private_surface_is_explicit_and_denied_tools_never_create_handlers(self):
        for modern in (False, True):
            extension, manager, fixture, owner, replies = self.manager(modern, private_ui=False)
            self.call(extension, "resources/read", owner)
            self.assertEqual(extension.inputs, [])
            self.assertEqual(extension.confirms, [])
            if not modern:
                self.assertEqual(replies[0]["error"]["code"], -32601)
            extension, manager, fixture, owner, _ = self.manager(modern, read_only=False)
            before = len(fixture.requests)
            with mock.patch("octet_mcp.manager.make_interaction_handler") as make:
                self.assertTrue(self.call(extension, "tools/call", owner)["is_error"])
            make.assert_not_called()
            self.assertEqual(len(fixture.requests), before)
            self.assertEqual(extension.inputs, [])

    def test_url_interaction_is_manual_and_does_not_open_or_fetch_server_url(self):
        for modern in (False, True):
            extension, _, fixture, owner, replies = self.manager(modern, url=True)
            with mock.patch("socket.getaddrinfo", side_effect=AssertionError("manual URL must not resolve")):
                result = self.call(extension, "resources/read", owner)
            self.assertFalse(result["is_error"])
            self.assertEqual(replies[0] if modern else replies[0]["result"], {"action": "accept"})
            self.assertEqual(len(extension.inputs), 1)
            self.assertTrue(all(request.target == "/mcp" for request in fixture.requests))

    def test_private_callback_uses_captured_parent_and_cannot_send_after_owner_retirement(self):
        for retire in (False, True):
            extension, manager, _, owner, replies = self.manager(True)
            extension.input_release.clear()
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(self.call, extension, "resources/read", owner)
                try:
                    self.assertTrue(extension.input_entered.wait(2))
                    extension.request_id = 999  # A different ambient host call cannot steal attribution.
                    if retire:
                        manager.observe_session("session/settled", {"session_id": "host-session"})
                    else:
                        owner["resource_owner"]["session_id"] = "mutated-context-must-not-rebind"
                finally:
                    extension.input_release.set()
                result = future.result(2)
            self.assertEqual(result["is_error"], retire)
            self.assertTrue(all(kwargs["parent_request_id"] == 77 for _, kwargs, _ in extension.inputs))
            if retire:
                self.assertEqual(replies, [])

    def test_catalog_change_during_private_confirmation_prevents_continuation(self):
        for modern in (False, True):
            extension, manager, fixture, owner, replies = self.manager(modern, read_only=False)
            extension.policy = "allow"  # Explicit host action policy, separate from private UI consent.
            extension.input_release.clear()
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(self.call, extension, "tools/call", owner)
                try:
                    self.assertTrue(extension.input_entered.wait(2))
                    self.schema_header += "Changed"
                    self.assertTrue(manager.request_action("refresh", "remote", context=owner).result(2))
                finally:
                    extension.input_release.set()
                self.assertTrue(future.result(2)["is_error"])
            self.assertEqual(replies, [])
            self.assertEqual(sum(request.message().get("method") == "tools/call" for request in fixture.requests if request.method == "POST"), 1)

    def test_transport_preparation_rechecks_redeemed_approval_before_first_mcp_bytes(self):
        for modern in (False, True):
            extension, manager, fixture, owner, _ = self.manager(modern, read_only=False)
            extension.policy = "allow"
            client = manager._servers["remote"].client
            connected, release = threading.Event(), threading.Event()
            original = client._connection
            first = [True]
            def prepare(operation, deadline):
                connection = original(operation, deadline)
                if first[0]:
                    first[0] = False
                    connect = connection.connect
                    def delayed():
                        connect()
                        connected.set()
                        if not release.wait(3):
                            raise AssertionError("transport preparation was not released")
                    connection.connect = delayed
                return connection
            with mock.patch.object(client, "_connection", side_effect=prepare), ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(self.call, extension, "tools/call", owner)
                try:
                    self.assertTrue(connected.wait(2))
                    self.schema_header += "Changed"
                    self.assertTrue(manager.request_action("refresh", "remote", context=owner).result(2))
                finally:
                    release.set()
                self.assertTrue(future.result(2)["is_error"])
            self.assertEqual(sum(request.message().get("method") == "tools/call" for request in fixture.requests if request.method == "POST"), 0)
            self.assertEqual(extension.inputs, [])


if __name__ == "__main__":
    unittest.main()
