"""Transient overlays exercise the authoritative manager and its real stdio client."""
from dataclasses import replace
import json
from pathlib import Path
import sys
import tempfile
import unittest

from octet_mcp.config import BridgeConfig
from octet_mcp.manager import BridgeManager
from octet_mcp.ownership import ResourceOwner
from octet_mcp.pi_registration import PiMcpRegistrations
from .helpers import FakeExtension, limits, wait_for


class PiRegistrationTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.root = Path(self.temp.name)
        self.extension = FakeExtension(self.root, policy="allow")
        self.manager = BridgeManager(self.extension, BridgeConfig(servers=(), limits=limits()),
                                     scratch_directory=self.root)
        self.registry = PiMcpRegistrations(self.manager)
        self.origin = ResourceOwner("session", "pi-instance", 1)
        self.active = ResourceOwner("session", "bridge-instance", 1)
        self.context = {"workspace": str(self.root), "resource_owner": self.active.wire(),
                        "mcp_registration_owner": self.origin.wire()}
        self.manager.start()

    def tearDown(self):
        self.manager.shutdown()
        self.temp.cleanup()

    def records(self, **changes):
        config = {"command": sys.executable,
                  "args": [str(Path(__file__).resolve().parents[1] / "fixtures/real_mcp_server.py")],
                  "exposure": "direct"}
        config.update(changes)
        return [{"name": "native-proof", "extensionPath": "/reviewed/factory.mjs", "config": config}]

    def apply(self, records):
        return json.loads(self.registry.command(["__pi_replace", json.dumps(records)], self.context)["text"])

    def test_real_stdio_overlay_replacement_owner_and_removal(self):
        self.assertEqual(len(self.apply(self.records())["changes"]["added"]), 1)
        wait_for(lambda: len(self.extension._tools) == 3)
        state = next(iter(self.manager._servers.values()))
        binding = next(item for item in state.tools.values() if item.upstream_name == "fixture_echo")
        ownerless = self.manager._call_tool(binding, state.client, {"value": "hello"}, {})
        self.assertTrue(ownerless["is_error"])
        allowed = self.manager._call_tool(binding, state.client, {"value": "hello"}, self.context)
        self.assertFalse(allowed["is_error"])
        self.assertEqual(allowed["structured_content"], {"echo": "hello"})
        old_client = state.client
        changed = self.apply(self.records(env={"PROOF": "literal"}))
        self.assertEqual(len(changed["changes"]["changed"]), 1)
        wait_for(lambda: len(self.extension._tools) == 3)
        self.assertFalse(old_client.alive)
        stale = dict(self.context, mcp_registration_owner=ResourceOwner("other", "pi-instance", 1).wire())
        self.registry.command(["__pi_release"], stale)
        self.assertEqual(len(self.manager.config.servers), 1, "foreign cleanup cannot remove a live overlay")
        self.registry.command(["__pi_release"], self.context)
        self.assertEqual(self.manager.config.servers, ())
        self.assertEqual(self.extension._tools, {})
        self.assertEqual(list(self.root.glob("*.json")), [])

    def test_marker_and_session_gate_and_redacted_profile_refusals(self):
        with self.assertRaises(ValueError):
            self.registry.command(["__pi_replace", json.dumps(self.records())], {"workspace": str(self.root)})
        with self.assertRaises(ValueError):
            self.registry.command(["__pi_replace", json.dumps(self.records())],
                                  dict(self.context, resource_owner=ResourceOwner("other", "bridge-instance", 1).wire()))
        for config, code in [({"exposure": "codemode"}, "unsupported_exposure"),
                             ({"env": {"TOKEN": "${SECRET}"}}, "unsupported_env_expansion"),
                             ({"url": "https://example.invalid"}, "unsupported_transport")]:
            result = self.apply(self.records(**config))
            self.assertEqual(result["errors"], [{"name": "native-proof", "code": code}])
            self.assertNotIn("SECRET", json.dumps(result))
        self.assertEqual(self.manager.config.servers, ())

    def test_file_configured_namespace_wins_and_user_edits_preserve_overlay(self):
        self.apply(self.records(enabled=False))
        transient = self.manager.config.servers[0]
        base = replace(self.manager.config, servers=(replace(transient, id="native-proof", registration_owner=None),))
        self.registry.apply_config(base)
        result = self.apply(self.records(enabled=False))
        self.assertEqual(result["shadowed"], ["native-proof"])
        self.assertEqual([server.id for server in self.manager.config.servers], ["native-proof"])
        self.registry.command(["__pi_release"], self.context)
        self.assertEqual([server.id for server in self.manager.config.servers], ["native-proof"])
