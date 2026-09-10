"""Bounded resource operations and their actual dynamic manager catalog surface."""
from __future__ import annotations

import base64
import copy
from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
import json
from pathlib import Path
import tempfile
import threading
import unittest
from unittest import mock

from octet_extension import CancelledError
from octet_mcp.catalog import ToolInputError, ToolResultError, published_tool_name
from octet_mcp.config import BridgeConfig
from octet_mcp.manager import BridgeManager
from octet_mcp.protocol import McpTimeout
from octet_mcp.resources import (
    MAX_RESOURCE_BLOB_BYTES, MAX_RESOURCE_CONTENTS, MAX_RESOURCE_CURSOR_BYTES,
    MAX_RESOURCE_DEPTH, MAX_RESOURCE_ENTRIES, MAX_RESOURCE_NODES,
    MAX_RESOURCE_RESULT_BYTES, MAX_RESOURCE_TEXT_BYTES, MAX_RESOURCE_URI_BYTES,
    ResourceBinding, ResourceUnsupportedError, call_resource, resource_bindings,
    supports_resources,
)

from .helpers import FakeCancellation, FakeExtension, limits, server_config, wait_for
from .test_owner_lifecycle import OwnerExtension, context


def resource_payload(result):
    return json.loads(result["content"][0]["text"].split("\n", 1)[1])


class ResourceClient:
    """No resource implementation in tools/call, and no modern-only keywords."""

    def __init__(self, config=None, _limits=None, on_failure=None, on_tools_changed=None,
                 *, credential_provider=None):
        self.server_capabilities = {"resources": {}, "tools": {}}
        self.alive = False
        self.calls = []
        self.list_count = 0
        self.responses = []
        self.on_failure = on_failure
        self.on_tools_changed = on_tools_changed
        self.tools = [{
            "name": "resources/read", "inputSchema": {"type": "object"},
            "annotations": {"readOnlyHint": False},
        }]

    def start(self):
        self.alive = True

    def close(self):
        self.alive = False

    def list_tools(self):
        self.list_count += 1
        return copy.deepcopy(self.tools)

    def call_tool(self, name, arguments, *, cancellation, progress):
        self.calls.append(("tools/call", {"name": name, "arguments": arguments}))
        return {"content": [{"type": "text", "text": "ordinary tool"}]}

    def request(self, operation, params, *, timeout_ms, cancellation=None,
                progress=None, include_progress_token=False):
        if cancellation is not None:
            cancellation.raise_if_cancelled()
        self.calls.append((operation, dict(params), timeout_ms, include_progress_token))
        if self.responses:
            response = self.responses.pop(0)
            return response() if callable(response) else response
        if operation == "resources/list":
            return {"resources": []}
        if operation == "resources/templates/list":
            return {"resourceTemplates": []}
        return {"contents": [{"uri": params["uri"], "text": "resource text"}]}


class ResourceBoundsTests(unittest.TestCase):
    def setUp(self):
        self.bindings = {binding.operation: binding for binding in resource_bindings(
            "fixture", "Reviewed fixture", server_catalog_revision=7,
        )}
        self.client = ResourceClient()
        self.token = FakeCancellation()

    def call(self, operation="resources/read", arguments=None, bridge_limits=None):
        return call_resource(
            self.bindings[operation], self.client,
            arguments if arguments is not None else {"uri": "opaque:test"},
            limits=bridge_limits or limits(), timeout_ms=1000, cancellation=self.token,
        )

    def test_synthetic_names_are_stable_bounded_and_disjoint_from_upstream_tools(self):
        names = set()
        for server in ("a", "a-list", "resources", "s" * 32):
            for binding in resource_bindings(server, "Fixture", server_catalog_revision=1):
                self.assertIsInstance(binding, ResourceBinding)
                self.assertRegex(binding.published_name, r"^[a-z][a-z0-9_]{0,63}$")
                self.assertNotIn(binding.published_name, names)
                names.add(binding.published_name)
                for name in (binding.published_name, binding.operation, "read", "templates"):
                    self.assertNotEqual(binding.published_name, published_tool_name(server, name))
        newer = resource_bindings("fixture", "Reviewed fixture", server_catalog_revision=8)
        self.assertEqual([b.fingerprint for b in self.bindings.values()], [b.fingerprint for b in newer])

    def test_capability_requires_an_object_not_a_hint_and_sends_nothing_when_absent(self):
        for capabilities in (None, [], {}, {"resources": True}, {"resources": False},
                             {"resources": "yes"}, {"resources": []}):
            with self.subTest(capabilities=capabilities):
                self.client.server_capabilities = capabilities
                self.assertFalse(supports_resources(self.client))
                with self.assertRaises(ResourceUnsupportedError):
                    self.call()
        self.assertEqual(self.client.calls, [])

    def test_lists_paginate_exact_opaque_cursors_and_preserve_untrusted_descriptors(self):
        cursor = " cursor:\u0000/雪?not=a+url "
        for operation, field, identity in (
            ("resources/list", "resources", "uri"),
            ("resources/templates/list", "resourceTemplates", "uriTemplate"),
        ):
            with self.subTest(operation=operation):
                first = {identity: "file:///private/{name}?a=%2F#f", "name": "first",
                         "description": "UNTRUSTED\u001b[31m", "icons": [{"src": "http://127.0.0.1/private"}]}
                second = {identity: "custom:雪/{+path}", "name": "second", "_meta": {"literal": True}}
                self.client.responses = [{field: [first], "nextCursor": cursor}, {field: [second]}]
                with mock.patch("socket.getaddrinfo", side_effect=AssertionError("no URI fetch")):
                    result = self.call(operation, {})
                self.assertEqual(resource_payload(result)[field], [first, second])
                self.assertEqual(self.client.calls[-2][1], {})
                self.assertEqual(self.client.calls[-1][1], {"cursor": cursor})
                self.assertFalse(self.client.calls[-1][3])
                self.assertNotIn("\u001b", result["content"][0]["text"])
                self.assertNotIn(cursor, json.dumps(result))

    def test_read_preserves_order_text_blob_and_exact_uri_without_host_fetching(self):
        uri = "https://user:password@127.0.0.1/private?x=%2f+1#opaque"
        text = "  original\n\u0000\u001b[31m 雪  "
        blob = base64.b64encode(b"\x00\x01\xffbinary").decode("ascii")
        contents = [
            {"uri": uri, "mimeType": "text/plain", "text": text},
            {"uri": "file:///host-is-not-authority", "mimeType": "application/octet-stream", "blob": blob},
        ]
        self.client.responses = [{"contents": contents, "resultType": "complete"}]
        with mock.patch("socket.getaddrinfo", side_effect=AssertionError("no URI fetch")):
            result = self.call(arguments={"uri": uri})
        self.assertFalse(result["is_error"])
        self.assertEqual(resource_payload(result), {
            "serverId": "fixture", "operation": "resources/read", "requestedUri": uri,
            "contents": contents,
        })
        self.assertEqual(self.client.calls[0][1], {"uri": uri})
        self.assertTrue(self.client.calls[0][3])
        self.assertEqual(result["metadata"]["mcp"]["serverCatalogRevision"], 7)
        self.assertEqual([part["type"] for part in result["content"]], ["text"])
        self.assertNotIn("structured_content", result)

    def test_uri_byte_bounds_reject_before_dispatch_without_truncation(self):
        for uri in (None, 1, "", "x" * (MAX_RESOURCE_URI_BYTES + 1),
                    "雪" * (MAX_RESOURCE_URI_BYTES // 3 + 1), "\ud800"):
            with self.subTest(uri_type=type(uri)), self.assertRaises(ToolInputError):
                self.call(arguments={"uri": uri})
        self.assertEqual(self.client.calls, [])
        uri = "x" * MAX_RESOURCE_URI_BYTES
        self.assertEqual(resource_payload(self.call(arguments={"uri": uri}))["requestedUri"], uri)

    def test_malformed_read_results_fail_closed_not_empty_success(self):
        malformed = [
            [], {}, {"contents": None}, {"contents": {}}, {"contents": [None]},
            {"contents": [{"uri": "r"}]}, {"contents": [{"uri": "r", "text": "", "blob": ""}]},
            {"contents": [{"uri": "", "text": "x"}]}, {"contents": [{"uri": "r", "text": 4}]},
            {"contents": [{"uri": "r", "text": "x", "mimeType": None}]},
            {"contents": [{"uri": "r", "blob": "not-base64!?"}]},
            {"contents": [{"uri": "r", "blob": "AA==\n"}]},
            {"contents": [], "resultType": "unknown"}, {"contents": [], "resultType": None},
            {"contents": [], "resultType": "complete", "requestState": "opaque"},
            {"contents": [], "inputRequests": {}},
            {"contents": [{"uri": "r", "text": "\ud800"}]},
            {"contents": [], "_meta": {"bad": float("nan")}},
            {"contents": [], "_meta": {"bad": 2**53}},
        ]
        for response in malformed:
            with self.subTest(response=response):
                self.client.responses = [response]
                with self.assertRaises(ToolResultError):
                    self.call()

    def test_input_required_is_explicitly_unsupported_and_never_continued(self):
        for operation in self.bindings:
            self.client.responses = [{"resultType": "input_required", "requestState": "SECRET opaque state"}]
            before = len(self.client.calls)
            with self.assertRaisesRegex(ResourceUnsupportedError, "no continuation was sent") as error:
                self.call(operation)
            self.assertEqual(len(self.client.calls), before + 1)
            self.assertNotIn("SECRET", str(error.exception))

    def test_individual_and_aggregate_text_blob_and_content_count_bounds(self):
        for content in (
            {"text": "x" * MAX_RESOURCE_TEXT_BYTES},
            {"blob": base64.b64encode(b"x" * MAX_RESOURCE_BLOB_BYTES).decode("ascii")},
        ):
            self.client.responses = [{"contents": [{"uri": "r", **content}]}]
            self.assertFalse(self.call()["is_error"])
        over = [
            [{"uri": "r", "text": "x" * (MAX_RESOURCE_TEXT_BYTES + 1)}],
            [{"uri": "r", "blob": base64.b64encode(b"x" * (MAX_RESOURCE_BLOB_BYTES + 1)).decode("ascii")}],
            [{"uri": "r", "text": "x" * (MAX_RESOURCE_TEXT_BYTES // 2 + 1)}] * 2,
            [{"uri": "r", "text": ""}] * (MAX_RESOURCE_CONTENTS + 1),
            [{"uri": "r", "blob": base64.b64encode(b"x" * 100_000).decode("ascii")}] * 3,
        ]
        for contents in over:
            with self.subTest(count=len(contents)):
                self.client.responses = [{"contents": contents}]
                with self.assertRaises(ToolResultError):
                    self.call()

    def test_pagination_cycles_entry_bounds_duplicates_and_malformed_descriptors(self):
        for operation, field, identity in (
            ("resources/list", "resources", "uri"),
            ("resources/templates/list", "resourceTemplates", "uriTemplate"),
        ):
            item = {identity: "r", "name": "r"}
            pages = [
                [{field: {}, "nextCursor": "next"}],
                [{field: [None]}], [{field: [{identity: "r"}]}],
                [{field: [{**item, "size": True}]}], [{field: [{**item, "size": -1}]}],
                [{field: [{**item, "description": "x" * 4097}]}],
                [{field: [item, item]}],
                [{field: [item], "nextCursor": "n"}, {field: [item]}],
                [{field: [item] * (MAX_RESOURCE_ENTRIES + 1)}],
                [{field: [], "nextCursor": "n"}, {field: [], "nextCursor": "n"}],
                [{field: [], "nextCursor": ""}], [{field: [], "nextCursor": True}],
                [{field: [], "nextCursor": "x" * (MAX_RESOURCE_CURSOR_BYTES + 1)}],
                [{field: [], "nextCursor": "a"}, {field: [], "nextCursor": "b"}],
            ]
            for responses in pages:
                with self.subTest(operation=operation, pages=len(responses)):
                    self.client.responses = responses
                    with self.assertRaises(ToolResultError):
                        self.call(operation, {}, limits(max_catalog_pages=2))

    def test_aggregate_envelope_rendered_and_structural_bounds_include_ignored_fields(self):
        self.client.responses = [
            {"resources": [], "_meta": {"padding": "x" * 300}, "nextCursor": "next"},
            {"resources": [], "_meta": {"padding": "x" * 300}},
        ]
        with self.assertRaisesRegex(ToolResultError, "aggregate"):
            self.call("resources/list", {}, limits(max_frame_bytes=500))
        nested = {}
        for _ in range(MAX_RESOURCE_DEPTH + 1):
            nested = {"nested": nested}
        for extra in (nested, [None] * MAX_RESOURCE_NODES, "x" * (MAX_RESOURCE_RESULT_BYTES + 1)):
            self.client.responses = [{"contents": [], "_meta": extra}]
            with self.assertRaises(ToolResultError):
                self.call()
        # Final bridge-authored labels/provenance also count, not just wire data.
        self.client.responses = [{"contents": []}]
        with self.assertRaises(ToolResultError):
            self.call(bridge_limits=limits(max_frame_bytes=100))

    def test_pages_share_one_deadline_and_cancellation_budget(self):
        now = [10.0]
        def page():
            now[0] += 0.4
            return {"resources": [], "nextCursor": str(now[0])}
        self.client.responses = [page, page, page]
        with mock.patch("octet_mcp.resources.time.monotonic", side_effect=lambda: now[0]):
            with self.assertRaises(McpTimeout):
                self.call("resources/list", {})
        deadlines = [call[2] for call in self.client.calls]
        self.assertEqual(len(deadlines), 3)
        self.assertGreater(deadlines[0], deadlines[1])
        self.assertGreater(deadlines[1], deadlines[2])
        self.client.calls.clear()
        def cancelled_page():
            self.token.cancel()
            return {"resources": [], "nextCursor": "never-follow"}
        self.client.responses = [cancelled_page]
        with self.assertRaises(CancelledError):
            self.call("resources/list", {})
        self.assertEqual(len(self.client.calls), 1)


class ResourceCatalogTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.scratch = Path(self.temporary.name)
        self.managers = []

    def tearDown(self):
        for manager in self.managers:
            manager.shutdown()
        self.temporary.cleanup()

    def manager(self, client=None, *, bridge_limits=None, remote=False):
        client = client or ResourceClient()
        extension = OwnerExtension(self.scratch) if remote else FakeExtension(self.scratch)
        server = server_config()
        if remote:
            server = replace(server, transport="streamable-http", command="", args=(),
                             url="http://127.0.0.1:9/mcp")
        manager = BridgeManager(
            extension, BridgeConfig(servers=(server,), limits=bridge_limits or limits()),
            scratch_directory=self.scratch, client_factory=lambda *a, **k: client,
            experimental_streamable_http_mcp=remote,
        )
        self.managers.append(manager)
        manager.start()
        if not remote:
            wait_for(lambda: manager._servers["fixture"].state in {"ready", "parked"})
        return extension, manager, client

    def test_real_published_handlers_use_resource_methods_not_upstream_tools_or_policy_hints(self):
        extension, manager, client = self.manager()
        self.assertEqual(len(extension._tools), 4)
        for operation, arguments in (("list", {}), ("templates", {}), ("read", {"uri": "file:///x"})):
            name = "mcp_resources_fixture_" + operation
            result = extension._tools[name]["handler"](arguments, {})
            self.assertFalse(result["is_error"])
        self.assertEqual([call[0] for call in client.calls], [
            "resources/list", "resources/templates/list", "resources/read",
        ])
        upstream = published_tool_name("fixture", "resources/read")
        denied = extension._tools[upstream]["handler"]({}, {})
        self.assertTrue(denied["is_error"])
        self.assertEqual(len(client.calls), 3, "name/readOnly metadata cannot redirect a tool or grant policy")
        before = len(client.calls)
        invalid = extension._tools["mcp_resources_fixture_read"]["handler"]({"uri": "r", "server": "other"}, {})
        self.assertTrue(invalid["is_error"])
        self.assertEqual(len(client.calls), before)
        self.assertEqual(extension.artifacts, {})
        self.assertEqual(list(self.scratch.iterdir()), [])

    def test_resource_only_server_publishes_without_probing_tools_list(self):
        client = ResourceClient()
        client.server_capabilities = {"resources": {}}
        extension, manager, client = self.manager(client)
        self.assertEqual(manager._servers["fixture"].state, "ready")
        self.assertEqual(len(extension._tools), 3)
        self.assertEqual(client.list_count, 0)
        self.assertFalse(extension._tools["mcp_resources_fixture_list"]["handler"]({}, {})["is_error"])

    def test_capability_refresh_adds_removes_and_fences_historical_resource_bindings(self):
        client = ResourceClient()
        client.server_capabilities = {"tools": {}}
        extension, manager, client = self.manager(client)
        self.assertEqual(len(extension._tools), 1)
        client.server_capabilities["resources"] = {}
        self.assertTrue(manager.refresh_server("fixture"))
        self.assertEqual(len(extension._tools), 4)
        handler = extension._tools["mcp_resources_fixture_read"]["handler"]
        epoch = extension._revision
        self.assertTrue(manager.refresh_server("fixture"))
        self.assertEqual(extension._revision, epoch, "unchanged resource definitions must not churn epochs")
        del client.server_capabilities["resources"]
        self.assertTrue(manager.refresh_server("fixture"))
        self.assertEqual(len(extension._tools), 1)
        before = len(client.calls)
        self.assertTrue(handler({"uri": "r"}, {})["is_error"])
        self.assertEqual(len(client.calls), before)

    def test_stopped_connection_never_dispatches_historical_read_even_if_client_claims_alive(self):
        extension, manager, client = self.manager()
        handler = extension._tools["mcp_resources_fixture_read"]["handler"]
        self.assertTrue(manager.stop_server("fixture"))
        client.alive = True  # A client cannot authorize replacement of its captured epoch.
        before = len(client.calls)
        self.assertTrue(handler({"uri": "r"}, {})["is_error"])
        self.assertEqual(len(client.calls), before)
        self.assertEqual(extension._tools, {})

    def test_synthetic_tools_count_toward_per_server_and_global_catalog_limits(self):
        for bridge_limits in (limits(max_tools_per_server=3), limits(max_total_tools=3)):
            with self.subTest(bridge_limits=bridge_limits):
                extension, manager, _ = self.manager(bridge_limits=bridge_limits)
                self.assertEqual(manager._servers["fixture"].state, "parked")
                self.assertEqual(extension._tools, {})

    def test_public_owner_predicate_is_inert_complete_and_bound_to_lifecycle_and_generation(self):
        extension, manager, client = self.manager(remote=True)
        owner = context()
        self.assertFalse(manager.is_current_owner(owner))
        manager.observe_session("session/started", {"session_id": "host-session"})
        self.assertFalse(manager.is_current_owner(owner), "observation cannot bind a fresh owner")
        self.assertIsNone(manager._remote_scope)
        self.assertIsNone(manager._executor)
        self.assertFalse(client.alive)
        self.assertTrue(manager.request_action("restart", "fixture", context=owner).result(2))
        self.assertTrue(manager.is_current_owner(owner))
        handler = extension._tools["mcp_resources_fixture_read"]["handler"]
        before = len(client.calls)
        for foreign in (None, {}, {"resource_owner": owner["resource_owner"]},
                        context("other"), context(instance="other"), context(generation=2),
                        context(host_session="other"), context(generation=True)):
            with self.subTest(foreign=foreign):
                self.assertFalse(manager.is_current_owner(foreign))
                self.assertTrue(handler({"uri": "private:r"}, foreign or {})["is_error"])
        self.assertEqual(len(client.calls), before)
        self.assertFalse(handler({"uri": "private:r"}, owner)["is_error"])
        manager.observe_session("session/settled", {"session_id": "host-session"})
        self.assertFalse(manager.is_current_owner(owner))
        wait_for(lambda: not extension._tools)
        before = len(client.calls)
        self.assertTrue(handler({"uri": "private:r"}, owner)["is_error"])
        self.assertEqual(len(client.calls), before)
        manager.observe_session("session/started", {"session_id": "host-session"})
        self.assertFalse(manager.is_current_owner(owner))

    def test_queued_unsafe_call_never_approves_or_dispatches_a_replaced_catalog(self):
        extension, manager, client = self.manager(bridge_limits=limits(max_concurrent_calls=1))
        name = published_tool_name("fixture", "resources/read")
        handler = extension._tools[name]["handler"]
        extension.evaluate_policy = mock.Mock(return_value={"decision": "allow"})
        waiting = threading.Event()
        original = manager._acquire_call_slot
        def wait_slot(*args):
            waiting.set()
            return original(*args)
        self.assertTrue(manager._calls.acquire(blocking=False))
        with ThreadPoolExecutor(max_workers=1) as pool, mock.patch.object(manager, "_acquire_call_slot", side_effect=wait_slot):
            future = pool.submit(handler, {}, {})
            try:
                self.assertTrue(waiting.wait(1))
                extension.evaluate_policy.assert_not_called()
                client.tools[0]["description"] = "changed while admission was saturated"
                self.assertTrue(manager.refresh_server("fixture"))
            finally:
                manager._calls.release()
            self.assertTrue(future.result(2)["is_error"])
        extension.evaluate_policy.assert_not_called()
        self.assertEqual(client.calls, [])

    def test_catalog_change_during_approval_issuance_or_redemption_revokes_dispatch(self):
        for blocked_phase in ("issue", "redeem"):
            with self.subTest(phase=blocked_phase):
                extension, manager, client = self.manager()
                extension.negotiated_features |= {"approvals"}
                handler = extension._tools[published_tool_name("fixture", "resources/read")]["handler"]
                entered, release = threading.Event(), threading.Event()
                calls = []
                def approve(intent, *, approval_token=None):
                    phase = "issue" if approval_token is None else "redeem"
                    calls.append(phase)
                    if phase == blocked_phase:
                        entered.set()
                        if not release.wait(2):
                            raise AssertionError("approval was not released")
                    return ({"decision": "ask", "approval_token": "a" * 64} if phase == "issue"
                            else {"decision": "allow"})
                extension.evaluate_policy = approve
                with ThreadPoolExecutor(max_workers=1) as pool:
                    future = pool.submit(handler, {}, {})
                    try:
                        self.assertTrue(entered.wait(1))
                        client.tools[0]["description"] = "catalog changed during confirmation"
                        self.assertTrue(manager.refresh_server("fixture"))
                    finally:
                        release.set()
                    self.assertTrue(future.result(2)["is_error"])
                self.assertEqual(calls, ["issue", "redeem"])
                self.assertEqual(client.calls, [])

    def test_pending_host_publication_invalidates_approval_before_manager_ack(self):
        extension, manager, client = self.manager()
        handler = extension._tools[published_tool_name("fixture", "resources/read")]["handler"]
        approving, release_approval = threading.Event(), threading.Event()
        published, release_ack = threading.Event(), threading.Event()
        def approve(intent):
            approving.set()
            if not release_approval.wait(2):
                raise AssertionError("approval was not released")
            return {"decision": "allow"}
        extension.evaluate_policy = approve
        register = extension.register_tools
        def blocked_ack(definitions):
            reply = register(definitions)  # Host publication happened; manager ack is still pending.
            published.set()
            if not release_ack.wait(2):
                raise AssertionError("publication acknowledgement was not released")
            return reply
        with ThreadPoolExecutor(max_workers=2) as pool, mock.patch.object(extension, "register_tools", side_effect=blocked_ack):
            future = pool.submit(handler, {}, {})
            try:
                self.assertTrue(approving.wait(1))
                client.tools[0]["description"] = "published but not acknowledged"
                refresh = pool.submit(manager.refresh_server, "fixture")
                self.assertTrue(published.wait(1))
                release_approval.set()
                self.assertTrue(future.result(1)["is_error"])
                self.assertEqual(client.calls, [])
            finally:
                release_ack.set()
                release_approval.set()
            self.assertTrue(refresh.result(2))

    def test_another_servers_catalog_change_revokes_an_approved_call_global_epoch(self):
        extension = FakeExtension(self.scratch)
        clients = {name: ResourceClient() for name in ("first", "second")}
        manager = BridgeManager(
            extension, BridgeConfig(servers=tuple(server_config(server_id=name) for name in clients), limits=limits()),
            client_factory=lambda config, *args, **kwargs: clients[config.id], scratch_directory=self.scratch,
        )
        self.managers.append(manager)
        manager.start()
        wait_for(lambda: all(state.state == "ready" for state in manager._servers.values()))
        name = published_tool_name("first", "resources/read")
        binding = manager._servers["first"].tools[name]
        entered, release = threading.Event(), threading.Event()
        def approve(intent):
            entered.set()
            if not release.wait(2):
                raise AssertionError("approval was not released")
            return {"decision": "allow"}
        extension.evaluate_policy = approve
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(extension._tools[name]["handler"], {}, {})
            try:
                self.assertTrue(entered.wait(1))
                clients["second"].tools[0]["description"] = "other server changed"
                self.assertTrue(manager.refresh_server("second"))
                self.assertIs(manager._servers["first"].tools[name], binding)
            finally:
                release.set()
            self.assertTrue(future.result(2)["is_error"])
        self.assertEqual(clients["first"].calls, [])


if __name__ == "__main__":
    unittest.main()
