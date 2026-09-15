from __future__ import annotations

import json
import unittest

try:
    from .helpers import BridgeProcess, FIXTURES, NODE
except ImportError:
    from helpers import BridgeProcess, FIXTURES, NODE


@unittest.skipUnless(NODE, "node is required for bridge UI protocol tests")
class BridgeUiLifetimeTests(unittest.TestCase):
    def open_bridge(self, *features: str, api_version: str = "0.2") -> BridgeProcess:
        bridge = BridgeProcess(
            extension=FIXTURES / "ui-lifecycle-extension.mjs",
            fixture_mode="ui-bridge",
            api_version=api_version,
        )
        self.addCleanup(bridge.close)
        self.host_requests: list[dict] = []
        self.editor = {"text": "", "revision": 0, "focused": True}

        def editor(message: dict) -> dict:
            self.host_requests.append(message)
            params = message["params"]
            if params["operation"] == "set":
                self.editor["text"] = params["text"]
                self.editor["revision"] += 1
            elif params["operation"] == "paste":
                self.editor["text"] += params["text"]
                self.editor["revision"] += 1
            return dict(self.editor)

        bridge.handlers["ui/editor"] = editor
        bridge.handlers["ui/autocomplete/register"] = lambda message: self.host_requests.append(message) or {"accepted": True}
        params = bridge.initialization_params(*features)
        if api_version == "0.2":
            params["contributes"] = {"ui": ["status", "header"]}
        response = bridge.request("initialize", params)
        self.assertNotIn("error", response)
        bridge.initialized = True
        return bridge

    def command(self, bridge: BridgeProcess, name: str) -> dict:
        response = bridge.request("command/execute", {"name": name, "arguments": []})
        self.assertNotIn("error", response)
        return response

    def complete(self, bridge: BridgeProcess, text: str = "seed", revision: int = 1) -> dict:
        response = bridge.request("ui/autocomplete/complete", {
            "text": text, "cursor": len(text.encode()), "revision": revision,
        })
        self.assertNotIn("error", response)
        return response["result"]

    def test_editor_autocomplete_and_resize_cross_the_bridge(self) -> None:
        bridge = self.open_bridge("runtime_commands", "semantic_ui", "editor_handoff", "autocomplete", "terminal_input")
        self.command(bridge, "ui-install")
        self.assertEqual({"prefix": "seed", "items": [{"value": "seed-complete", "label": "fixture choice"}]}, self.complete(bridge))
        self.assertTrue(any(m["method"] == "ui/editor" and m["params"]["operation"] == "set" for m in self.host_requests))
        bridge.send({"jsonrpc": "2.0", "method": "ui/resize", "params": {"columns": 40, "rows": 20}})
        bridge.wait_for(lambda messages: any(
            m.get("method") == "status/contribution" and m.get("params", {}).get("text") == "header-40"
            for m in messages
        ), description="resized header contribution")
        for message in bridge.messages:
            if message.get("method", "").startswith("ui/") or message.get("method") == "status/contribution":
                # Local adapter fences are not fabricated host resource ownership.
                self.assertFalse({"owner", "resource_owner", "session_id", "process_generation"} & message.get("params", {}).keys())
        self.assertEqual([], bridge.protocol_errors)

    def test_autocomplete_registration_outlives_cancelled_installing_request(self) -> None:
        bridge = self.open_bridge("runtime_commands", "semantic_ui", "editor_handoff", "autocomplete")
        request_id = bridge.send_request("command/execute", {"name": "ui-install-and-wait", "arguments": []})
        bridge.wait_for(lambda messages: any(m.get("method") == "input/request" for m in messages), description="held installing command")
        bridge.wait_for(lambda messages: any(m.get("method") == "ui/autocomplete/register" for m in messages), description="autocomplete admission")
        bridge.send({"jsonrpc": "2.0", "method": "$/cancelRequest", "params": {"id": request_id, "reason": "fixture cancellation"}})
        self.assertEqual(-32800, bridge.wait_response(request_id)["error"]["code"])
        self.assertEqual("choice", self.complete(bridge, "choice", 0)["prefix"])
        self.command(bridge, "ui-remove-provider")
        self.assertEqual({"prefix": "", "items": []}, self.complete(bridge, "choice", 0))
        self.assertEqual(2, len([m for m in self.host_requests if m["method"] == "ui/autocomplete/register"]))

    def test_settlement_disposes_once_and_old_callbacks_cannot_reenter_new_owner(self) -> None:
        bridge = self.open_bridge("runtime_commands", "semantic_ui", "editor_handoff", "autocomplete", "lifecycle_events")
        self.command(bridge, "ui-install")
        self.assertNotIn("error", bridge.request("session/settled", {}))
        self.assertNotIn("error", bridge.request("session/settled", {}))
        self.assertNotIn("error", bridge.request("session/started", {}))
        self.command(bridge, "ui-report")
        report = json.loads(bridge.notifications()[-1])
        self.assertEqual({"componentDisposals": 1, "providerDisposals": 1, "statusRejected": True, "renderRejected": True}, report)
        self.assertFalse(any(m.get("params", {}).get("key") == "late-status" for m in bridge.messages))
        self.assertEqual(1, len([m for m in bridge.messages if m.get("method") == "ui/contribution"
                                and m.get("params", {}).get("key") == "fixture-status"
                                and m.get("params", {}).get("text") is None]))
        self.assertEqual({"prefix": "", "items": []}, self.complete(bridge))

    def test_settlement_cancels_noncooperative_completion_without_late_response(self) -> None:
        bridge = self.open_bridge("runtime_commands", "semantic_ui", "editor_handoff", "autocomplete", "lifecycle_events")
        self.command(bridge, "ui-install")
        query_id = bridge.send_request("ui/autocomplete/complete", {
            "text": "hold", "cursor": 4, "revision": 1,
        })
        bridge.wait_for(lambda _messages: "suggestions-held" in bridge.notifications(),
                        description="noncooperative completion started")
        settle_id = bridge.send_request("session/settled", {})
        self.assertIn("error", bridge.wait_response(query_id))
        self.assertNotIn("error", bridge.wait_response(settle_id))
        self.assertNotIn("error", bridge.request("session/started", {}))
        self.command(bridge, "ui-release-suggestions")
        self.assertEqual({"prefix": "", "items": []}, self.complete(bridge))
        self.assertEqual(1, len([message for message in bridge.messages
                                 if message.get("id") == query_id and "method" not in message]))
        self.assertEqual([], bridge.protocol_errors)

    def test_shutdown_cancels_editor_wait_before_joining_ordered_lane(self) -> None:
        bridge = self.open_bridge("runtime_commands", "editor_handoff")
        bridge.wait_for(lambda _messages: any(m["method"] == "ui/editor" and m["params"]["operation"] == "get"
                                             for m in self.host_requests), description="initial editor refresh handled")
        bridge.handlers.pop("ui/editor")
        command_id = bridge.send_request("command/execute", {"name": "ui-editor-wait", "arguments": []})
        pending = bridge.wait_for(lambda messages: next((m for m in messages if m.get("method") == "ui/editor"
                                     and m.get("params", {}).get("text") == "held-write"), None), description="blocked editor mutation")
        shutdown_id = bridge.send_request("shutdown", {})
        self.assertIn("error", bridge.wait_response(command_id))
        self.assertEqual({}, bridge.wait_response(shutdown_id)["result"])
        self.assertTrue(any(m.get("method") == "$/cancelRequest" and m.get("params", {}).get("id") == pending["id"] for m in bridge.messages))

    def test_editor_observations_and_utf8_cursor_validation_are_authoritative(self) -> None:
        bridge = self.open_bridge("runtime_commands", "semantic_ui", "editor_handoff", "autocomplete")
        self.command(bridge, "ui-install")
        bridge.send({"jsonrpc": "2.0", "method": "ui/editor-state", "params": {"text": "é", "revision": 2, "focused": True}})
        self.command(bridge, "ui-editor-read")
        self.assertEqual("é", bridge.notifications()[-1])
        self.assertEqual("é", self.complete(bridge, "é", 2)["prefix"])
        self.assertEqual({"prefix": "", "items": []}, self.complete(bridge, "seed", 1))
        invalid = bridge.request("ui/autocomplete/complete", {"text": "é", "cursor": 1, "revision": 2})
        self.assertIn("UTF-8 boundary", invalid["error"]["message"])
        bridge.send({"jsonrpc": "2.0", "method": "ui/editor-state", "params": {"text": "stale", "revision": 1, "focused": True}})
        self.command(bridge, "ui-editor-read")
        self.assertEqual("é", bridge.notifications()[-1])

    def test_absent_ui_feature_and_api03_never_grant_legacy_ui_authority(self) -> None:
        bridge = self.open_bridge("runtime_commands")
        response = bridge.request("command/execute", {"name": "ui-widget", "arguments": []})
        self.assertIn("semantic_ui not negotiated", response["error"]["message"])
        self.assertFalse(any(m.get("method") == "ui/contribution" for m in bridge.messages))
        self.assertEqual([], self.host_requests)
        provider = self.open_bridge(api_version="0.3")
        response = provider.request("ui/autocomplete/complete", {"text": "x", "cursor": 1, "revision": 0})
        self.assertIn("error", response)
        self.assertFalse(any(m.get("method", "").startswith("ui/") for m in provider.messages))
