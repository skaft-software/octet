"""Hermetic storefront-shaped journeys; these are not live Shopify qualification."""

from __future__ import annotations

from dataclasses import replace
import json
from pathlib import Path
import tempfile
import threading
import unittest

from octet_mcp.config import BridgeConfig
from octet_mcp.manager import BridgeManager

from .helpers import FakeExtension, limits, wait_for
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _initialize_result, _json_result, _remote_config,
)


PROFILE = "https://agent.example.invalid/profile.json"
OWNER_CONTEXT = {
    "host": {"session_id": "storefront-session"},
    "resource_owner": {
        "session_id": "storefront-durable-owner",
        "extension_instance_id": "storefront-host-instance",
        "process_generation": 1,
    },
}


def _tool(name, properties, required, *, read_only=False):
    result = {
        "name": name,
        "description": "Untrusted storefront-shaped fixture, not a Shopify server.",
        "inputSchema": {
            "type": "object", "properties": properties, "required": required,
            "additionalProperties": False,
        },
    }
    if read_only:
        result["annotations"] = {"readOnlyHint": True}
    return result


class StorefrontFixture:
    """Two explicitly configured endpoints, with observable fixture mutations."""

    def __init__(self):
        self.lock = threading.Lock()
        self.calls = []
        self.mutations = 0
        self.server = _LoopbackFixture(self.respond)

    def close(self):
        self.server.close()

    def config(self, name, path):
        origin = self.server.url.rsplit("/", 1)[0]
        return replace(_remote_config(origin + path), id=name, label=name)

    def respond(self, request):
        if request.method == "DELETE":
            return _HttpReply()
        message = request.message()
        method = message["method"]
        if method == "initialize":
            return _json_result(request, _initialize_result("storefront-shaped-fixture"))
        if method.startswith("notifications/"):
            return _HttpReply(status=202)
        if method == "tools/list":
            if request.target == "/api/ucp/mcp":
                tools = [_tool("search_catalog", {
                    "meta": {
                        "type": "object",
                        "properties": {"ucp-agent": {
                            "type": "object", "properties": {"profile": {"type": "string"}},
                            "required": ["profile"], "additionalProperties": False,
                        }},
                        "required": ["ucp-agent"], "additionalProperties": False,
                    },
                    "catalog": {
                        "type": "object", "properties": {"query": {"type": "string"}},
                        "required": ["query"], "additionalProperties": False,
                    },
                }, ["meta", "catalog"], read_only=True)]
            elif request.target == "/api/mcp":
                tools = [
                    _tool("search_shop_policies_and_faqs", {"query": {"type": "string"}},
                          ["query"], read_only=True),
                    _tool("get_cart", {"cart_id": {"type": "string"}},
                          ["cart_id"], read_only=True),
                    _tool("update_cart", {"quantity": {"type": "integer", "minimum": 0}},
                          ["quantity"]),
                ]
            else:
                return _HttpReply(status=404)
            return _json_result(request, {"tools": tools})
        if method != "tools/call":
            return _HttpReply(status=400)
        params = message["params"]
        name, arguments = params["name"], params["arguments"]
        with self.lock:
            self.calls.append((request.target, name, arguments))
            if name == "update_cart":
                self.mutations += 1
        if name == "search_catalog":
            if arguments["meta"]["ucp-agent"]["profile"] != PROFILE:
                return _json_result(request, {"content": [{"type": "text", "text": "invalid profile"}], "isError": True})
            payload = {"products": [{"id": "fixture-product", "title": "Fixture coffee"}]}
        elif name == "search_shop_policies_and_faqs":
            payload = {"answer": "Fixture returns are accepted within 30 days."}
        else:
            payload = {"cart_id": "fixture-cart", "checkout_url": "https://checkout.example.invalid/fixture"}
        return _json_result(request, {"content": [{"type": "text", "text": json.dumps(payload)}]})


class _OwnerAwareExtension(FakeExtension):
    def __init__(self, scratch):
        super().__init__(scratch)
        self.negotiated_features |= {"lifecycle_events"}

    def publish_presentation(self, snapshot, *, resource_owner=None):
        del resource_owner
        super().publish_presentation(snapshot)


class StorefrontJourneyTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.addCleanup(self.temporary.cleanup)
        self.fixture = StorefrontFixture()
        self.addCleanup(self.fixture.close)
        self.extension = _OwnerAwareExtension(Path(self.temporary.name))
        self.manager = BridgeManager(
            self.extension,
            BridgeConfig(
                servers=(self.fixture.config("catalog", "/api/ucp/mcp"),
                         self.fixture.config("store", "/api/mcp")),
                limits=limits(shutdown_timeout_ms=250),
            ),
            scratch_directory=Path(self.temporary.name),
            experimental_streamable_http_mcp=True,
        )
        self.addCleanup(self.manager.shutdown)
        self.manager.start()
        self.manager.observe_session("session/started", {"session_id": "storefront-session"})
        for name in ("catalog", "store"):
            self.manager.execute_command(["restart", name], OWNER_CONTEXT)
        wait_for(lambda: len(self.extension._tools) == 4, message="both storefront fixture catalogs")

    def handler(self, name):
        matches = [tool["handler"] for published, tool in self.extension._tools.items()
                   if "_" + name + "_" in published]
        self.assertEqual(len(matches), 1)
        return matches[0]

    def test_search_policy_cart_and_no_implicit_purchase(self):
        search = self.handler("search_catalog")({
            "meta": {"ucp-agent": {"profile": PROFILE}}, "catalog": {"query": "coffee"},
        }, OWNER_CONTEXT)
        self.assertFalse(search["is_error"])
        self.assertIn("Fixture coffee", search["content"][0]["text"])
        policy = self.handler("search_shop_policies_and_faqs")({"query": "Returns?"}, OWNER_CONTEXT)
        self.assertFalse(policy["is_error"])
        self.assertIn("30 days", policy["content"][0]["text"])
        denied = self.handler("update_cart")({"quantity": 1}, OWNER_CONTEXT)
        self.assertTrue(denied["is_error"])
        self.assertEqual(self.fixture.mutations, 0)
        # This fake host decision tests bridge dispatch only. Real trusted
        # host confirmation/token behavior has separate Rust integration tests.
        self.extension.policy = "allow"
        updated = self.handler("update_cart")({"quantity": 1}, OWNER_CONTEXT)
        self.assertFalse(updated["is_error"])
        self.assertEqual(self.fixture.mutations, 1)
        cart = self.handler("get_cart")({"cart_id": "fixture-cart"}, OWNER_CONTEXT)
        self.assertFalse(cart["is_error"])
        self.assertIn("checkout.example.invalid", cart["content"][0]["text"])
        self.assertEqual(self.fixture.mutations, 1, "reading checkout URL is not checkout execution")
        self.assertFalse(self.fixture.server.errors)

    def test_catalog_schema_owner_and_retired_handlers_fail_before_egress(self):
        search = self.handler("search_catalog")
        before = len(self.fixture.calls)
        invalid = search({"catalog": {"query": "coffee"}}, OWNER_CONTEXT)
        self.assertTrue(invalid["is_error"])
        foreign = {**OWNER_CONTEXT, "resource_owner": {
            **OWNER_CONTEXT["resource_owner"], "session_id": "another-durable-owner",
        }}
        rejected = search({"meta": {"ucp-agent": {"profile": PROFILE}},
                           "catalog": {"query": "coffee"}}, foreign)
        self.assertTrue(rejected["is_error"])
        self.manager.observe_session("session/settled", {"session_id": "storefront-session"})
        retired = search({"meta": {"ucp-agent": {"profile": PROFILE}},
                          "catalog": {"query": "coffee"}}, OWNER_CONTEXT)
        self.assertTrue(retired["is_error"])
        self.assertEqual(len(self.fixture.calls), before)
