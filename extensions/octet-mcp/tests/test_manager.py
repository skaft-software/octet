from __future__ import annotations

import json
from dataclasses import replace
from pathlib import Path
import random
import tempfile
import threading
import unittest
from unittest import mock

from octet_mcp.config import BridgeConfig, HttpAuthConfig, ServerConfig
from octet_mcp.manager import BridgeManager
from octet_mcp.protocol import McpTransportError

from .helpers import (
    FakeExtension,
    limits,
    real_server_config,
    server_config,
    wait_for,
)


def root_node(snapshot, server_id):
    for node in snapshot["collection"]["nodes"]:
        if node["id"] == f"server:{server_id}":
            return node
    raise AssertionError(f"missing server node {server_id}")


class ManagerTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.scratch = Path(self.temporary.name)
        self.managers = []

    def tearDown(self):
        for manager in reversed(self.managers):
            manager.shutdown()
        self.temporary.cleanup()

    def manager(self, server, bridge_limits=None, *, policy="deny", confirm=None):
        selected_limits = bridge_limits or limits(
            backoff_initial_ms=10,
            backoff_max_ms=20,
            shutdown_timeout_ms=500,
        )
        extension = FakeExtension(self.scratch, policy=policy, confirm=confirm)
        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=selected_limits),
            scratch_directory=self.scratch,
            random_source=random.Random(1),
        )
        self.managers.append(manager)
        manager.start()
        return extension, manager

    def test_queued_restart_targets_retired_state_and_new_restart_still_starts_disabled(self):
        server = server_config("stable")
        extension = FakeExtension(self.scratch)
        clients = []

        def factory(*_args):
            client = mock.Mock(alive=False)
            client.start.side_effect = McpTransportError(
                "fixture_stop", "controlled test", permanent=True
            )
            clients.append(client)
            return client

        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=limits()),
            scratch_directory=self.scratch,
            client_factory=factory,
        )
        self.managers.append(manager)
        queued = []
        with mock.patch.object(manager, "_submit", side_effect=lambda fn, *args: queued.append((fn, args))):
            manager.request_action("restart", "fixture")
            manager.apply_config(BridgeConfig(servers=(replace(server, enabled=False),), limits=limits()))
            self.assertEqual(len(queued), 1)
            stale_callback, stale_args = queued.pop()
            self.assertFalse(stale_callback(*stale_args))
            self.assertEqual(clients, [], "queued old restart must not start replacement config")

            manager.request_action("restart", "fixture")
            new_callback, new_args = queued.pop()
            self.assertFalse(new_callback(*new_args))
            self.assertEqual(len(clients), 1, "a newly admitted explicit restart may start disabled server")
            clients[0].start.assert_called_once_with()

    def test_retirement_at_start_effect_boundary_prevents_client_start(self):
        entered = threading.Event()
        release = threading.Event()
        extension = FakeExtension(self.scratch)
        client = mock.Mock(alive=True)
        server = server_config("stable")
        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=limits()),
            scratch_directory=self.scratch,
            client_factory=lambda *_args: client,
        )
        self.managers.append(manager)
        begin_effect = manager._begin_lifecycle_effect

        def paused_effect(state):
            entered.set()
            self.assertTrue(release.wait(2), "test did not release pre-start boundary")
            return begin_effect(state)

        starting = threading.Thread(target=manager._start_server, args=("fixture", False))
        with mock.patch.object(manager, "_begin_lifecycle_effect", side_effect=paused_effect):
            starting.start()
            self.assertTrue(entered.wait(1), "startup did not reach its effect boundary")
            applied = threading.Event()
            reconfigured = threading.Thread(
                target=lambda: (manager.apply_config(BridgeConfig(servers=(), limits=limits())), applied.set())
            )
            reconfigured.start()
            wait_for(lambda: manager._servers["fixture"].retirement_pending, message="retirement effect fence")
            release.set()
            starting.join(2)
            reconfigured.join(2)
        self.assertFalse(starting.is_alive())
        self.assertTrue(applied.is_set())
        client.start.assert_not_called()
        client.close.assert_called_once_with()

    def test_retirement_wins_before_catalog_registration_admission(self):
        entered = threading.Event()
        release = threading.Event()
        extension = FakeExtension(self.scratch)
        register = mock.Mock(wraps=extension.register_tools)
        extension.register_tools = register
        client = mock.Mock(alive=True)
        client.start.return_value = None

        def list_tools():
            entered.set()
            self.assertTrue(release.wait(2), "test did not release catalog probe")
            return [{"name": "probe", "inputSchema": {"type": "object", "properties": {}}}]

        client.list_tools.side_effect = list_tools
        server = server_config("stable")
        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=limits()),
            scratch_directory=self.scratch,
            client_factory=lambda *_args: client,
        )
        self.managers.append(manager)
        starting = threading.Thread(target=manager._start_server, args=("fixture", False))
        starting.start()
        self.assertTrue(entered.wait(1), "catalog probe did not start")
        applied = threading.Event()
        reconfigured = threading.Thread(
            target=lambda: (manager.apply_config(BridgeConfig(servers=(), limits=limits())), applied.set())
        )
        reconfigured.start()
        wait_for(lambda: manager._servers["fixture"].retirement_pending, message="retirement admission fence")
        release.set()
        starting.join(2)
        reconfigured.join(2)
        self.assertFalse(starting.is_alive())
        self.assertTrue(applied.is_set())
        register.assert_not_called()
        self.assertEqual(extension._tools, {})
        client.close.assert_called_once_with()

    def test_retirement_waits_for_host_registration_then_unpublishes_catalog(self):
        entered = threading.Event()
        release = threading.Event()
        extension = FakeExtension(self.scratch)
        original_register = extension.register_tools

        def blocking_register(definitions):
            entered.set()
            self.assertTrue(release.wait(2), "test did not release host registration")
            return original_register(definitions)

        extension.register_tools = blocking_register
        client = mock.Mock(alive=True)
        client.start.return_value = None
        client.list_tools.return_value = [
            {"name": "probe", "inputSchema": {"type": "object", "properties": {}}}
        ]
        server = server_config("stable")
        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=limits()),
            scratch_directory=self.scratch,
            client_factory=lambda *_args: client,
        )
        self.managers.append(manager)
        starting = threading.Thread(target=manager._start_server, args=("fixture", False))
        starting.start()
        self.assertTrue(entered.wait(1), "catalog registration did not start")
        applied = threading.Event()
        reconfigured = threading.Thread(
            target=lambda: (manager.apply_config(BridgeConfig(servers=(), limits=limits())), applied.set())
        )
        reconfigured.start()
        wait_for(lambda: manager._servers["fixture"].retirement_pending, message="retirement admission fence")
        self.assertFalse(manager._servers["fixture"].retired, "retirement linearizes after admitted registration")
        self.assertFalse(applied.is_set(), "retirement must wait for admitted host callback")
        release.set()
        starting.join(2)
        reconfigured.join(2)
        self.assertFalse(starting.is_alive())
        self.assertTrue(applied.is_set())
        self.assertEqual(extension._tools, {})
        client.start.assert_called_once_with()
        client.close.assert_called_once_with()

    def two_server_manager(self):
        extension = FakeExtension(self.scratch)
        server_a = server_config("stable", server_id="a")
        server_b = server_config("stable", server_id="b")
        clients = {}

        def factory(config, *_args):
            client = mock.Mock(alive=True)
            client.start.return_value = None
            client.list_tools.return_value = [
                {"name": "probe", "inputSchema": {"type": "object", "properties": {}}}
            ]
            clients[config.id] = client
            return client

        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server_a, server_b), limits=limits()),
            scratch_directory=self.scratch,
            client_factory=factory,
        )
        self.managers.append(manager)
        return extension, manager, server_a, server_b, clients

    def test_register_callback_cannot_reentrantly_remove_other_server(self):
        extension, manager, server_a, server_b, clients = self.two_server_manager()
        self.assertTrue(manager._start_server("b", False))
        original_register = extension.register_tools
        rejection = []
        attempted = False

        def reentrant_register(definitions):
            nonlocal attempted
            if not attempted and any(item["name"].startswith("mcp_a_") for item in definitions):
                attempted = True
                try:
                    manager.apply_config(BridgeConfig(servers=(server_a,), limits=limits()))
                except RuntimeError as error:
                    rejection.append(str(error))
            return original_register(definitions)

        extension.register_tools = reentrant_register
        self.assertTrue(manager._start_server("a", False))
        self.assertEqual(len(rejection), 1)
        self.assertEqual(set(manager._servers), {"a", "b"})
        self.assertFalse(manager._servers["b"].retired)
        self.assertEqual(len(extension._tools), 2)
        clients["b"].close.assert_not_called()
        manager.apply_config(BridgeConfig(servers=(server_a,), limits=limits()))
        self.assertNotIn("b", manager._servers)
        clients["b"].close.assert_called_once_with()

    def test_register_callback_shutdown_rejection_does_not_latch_and_later_shutdown_works(self):
        extension, manager, _server_a, _server_b, clients = self.two_server_manager()
        self.assertTrue(manager._start_server("b", False))
        original_register = extension.register_tools
        rejection = []
        attempted = False

        def reentrant_register(definitions):
            nonlocal attempted
            if not attempted and any(item["name"].startswith("mcp_a_") for item in definitions):
                attempted = True
                try:
                    manager.shutdown()
                except RuntimeError as error:
                    rejection.append(str(error))
            return original_register(definitions)

        extension.register_tools = reentrant_register
        self.assertTrue(manager._start_server("a", False))
        self.assertEqual(len(rejection), 1)
        self.assertFalse(manager._shutting_down)
        self.assertFalse(manager._servers["a"].retired)
        self.assertFalse(manager._servers["b"].retired)
        clients["b"].close.assert_not_called()
        manager.shutdown()
        clients["b"].close.assert_called_once_with()

    def test_register_callback_remove_all_rejection_preflights_before_sorted_retirement(self):
        extension, manager, _server_a, _server_b, clients = self.two_server_manager()
        self.assertTrue(manager._start_server("a", False))
        original_register = extension.register_tools
        rejection = []
        attempted = False

        def reentrant_register(definitions):
            nonlocal attempted
            if not attempted and any(item["name"].startswith("mcp_b_") for item in definitions):
                attempted = True
                try:
                    manager.apply_config(BridgeConfig(servers=(), limits=limits()))
                except RuntimeError as error:
                    rejection.append(str(error))
            return original_register(definitions)

        extension.register_tools = reentrant_register
        self.assertTrue(manager._start_server("b", False))
        self.assertEqual(len(rejection), 1)
        self.assertEqual(set(manager._servers), {"a", "b"})
        self.assertFalse(manager._servers["a"].retired)
        self.assertFalse(manager._servers["b"].retired)
        clients["a"].close.assert_not_called()
        manager.apply_config(BridgeConfig(servers=(), limits=limits()))
        self.assertEqual(manager._servers, {})
        clients["a"].close.assert_called_once_with()
        clients["b"].close.assert_called_once_with()

    def test_real_fixture_end_to_end_preserves_structured_and_media_results(self):
        extension, manager = self.manager(real_server_config())
        wait_for(
            lambda: root_node(manager.snapshot(), "real-fixture")["state"] == "active",
            message="real fixture ready",
        )
        names = sorted(extension._tools)
        self.assertEqual(len(names), 3)
        echo_name = next(name for name in names if "fixture_echo" in name)
        media_name = next(name for name in names if "fixture_media" in name)
        unknown_name = next(name for name in names if "fixture_unknown_effect" in name)

        echo = extension._tools[echo_name]["handler"]({"value": "hello"}, {})
        self.assertFalse(echo["is_error"])
        self.assertEqual(echo["structured_content"], {"echo": "hello"})
        self.assertEqual(echo["content"][0]["text"], "fixture echo: hello")

        media = extension._tools[media_name]["handler"]({}, {})
        self.assertFalse(media["is_error"])
        self.assertEqual([part["type"] for part in media["content"]], ["text", "image", "audio"])
        self.assertEqual(len(extension.artifacts), 2)

        denied = extension._tools[unknown_name]["handler"]({}, {})
        self.assertTrue(denied["is_error"])
        self.assertIn("denied", denied["content"][0]["text"].lower())
        self.assertTrue(extension.presentations)

    def test_removed_server_fences_a_client_constructed_by_queued_start(self):
        entered = threading.Event()
        release = threading.Event()
        client = mock.Mock(alive=True)

        def factory(*_args):
            entered.set()
            self.assertTrue(release.wait(2), "test did not release client factory")
            return client

        server = server_config("stable")
        extension = FakeExtension(self.scratch)
        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=limits()),
            scratch_directory=self.scratch,
            client_factory=factory,
        )
        self.managers.append(manager)
        starting = threading.Thread(target=manager._start_server, args=("fixture", False))
        starting.start()
        self.assertTrue(entered.wait(1), "client construction did not start")

        applied = threading.Event()
        reconfigured = threading.Thread(
            target=lambda: (manager.apply_config(BridgeConfig(servers=(), limits=limits())), applied.set())
        )
        reconfigured.start()
        # apply_config marks the old state retired before it waits for its
        # per-server operation lock. Release construction only after that fence.
        wait_for(lambda: manager._servers["fixture"].retired, message="retirement fence")
        release.set()
        starting.join(2)
        reconfigured.join(2)
        self.assertFalse(starting.is_alive())
        self.assertTrue(applied.is_set())
        client.start.assert_not_called()
        client.close.assert_called_once_with()
        self.assertEqual(extension._tools, {})
        self.assertNotIn("fixture", manager._servers)

    def test_catalog_add_replace_remove_and_epoch_pinned_schema_handlers(self):
        extension, manager = self.manager(server_config("catalog"))
        wait_for(
            lambda: root_node(manager.snapshot(), "fixture")["state"] == "active",
            message="initial catalog",
        )
        revision_one = extension.catalogs[1]
        versioned_name = next(name for name in revision_one if "versioned" in name)
        removed_name = next(name for name in revision_one if "removed" in name)
        old_handler = revision_one[versioned_name]["handler"]

        self.assertTrue(manager.refresh_server("fixture"))
        current = extension.catalogs[extension._revision]
        added_name = next(name for name in current if "added" in name)
        self.assertNotIn(removed_name, current)
        new_handler = current[versioned_name]["handler"]

        old_result = old_handler({"value": "x", "extra": 1}, {})
        new_result = new_handler({"value": "x", "extra": 1}, {})
        self.assertTrue(old_result["is_error"], "old epoch must enforce its old schema")
        self.assertFalse(new_result["is_error"], "replacement epoch must use its new schema")
        self.assertEqual(new_result["structured_content"]["version"], "v2")

        self.assertTrue(manager.refresh_server("fixture"))
        final = extension.catalogs[extension._revision]
        self.assertNotIn(added_name, final)
        self.assertIn(versioned_name, final)

    def test_crash_restarts_after_each_success_without_replaying_calls(self):
        bridge_limits = limits(
            backoff_initial_ms=10,
            backoff_max_ms=20,
            shutdown_timeout_ms=300,
        )
        extension, manager = self.manager(
            server_config("crash", max_restarts=1), bridge_limits
        )
        wait_for(
            lambda: root_node(manager.snapshot(), "fixture")["state"] == "active",
            message="crash fixture ready",
        )
        initial_revision = extension._revision
        first_name = next(iter(extension._tools))
        first = extension._tools[first_name]["handler"]({"value": "first"}, {})
        self.assertTrue(first["is_error"])
        self.assertIn("not replayed", first["content"][0]["text"])
        wait_for(
            lambda: extension._revision >= initial_revision + 2
            and root_node(manager.snapshot(), "fixture")["state"] == "active",
            message="one automatic restart",
        )

        second_name = next(iter(extension._tools))
        second = extension._tools[second_name]["handler"]({"value": "second"}, {})
        self.assertTrue(second["is_error"])
        wait_for(
            lambda: extension._revision >= initial_revision + 4
            and root_node(manager.snapshot(), "fixture")["state"] == "active",
            message="restart counter reset after a successful startup",
        )
        self.assertEqual(manager._servers["fixture"].restart_attempt, 0)

    def test_consecutive_failed_starts_back_off_then_park(self):
        server = server_config("stable", max_restarts=1)
        extension = FakeExtension(self.scratch)
        clients = []

        def factory(*_args):
            client = mock.Mock(alive=False)
            client.start.side_effect = McpTransportError("transport_lost", "MCP fixture transport lost")
            clients.append(client)
            return client

        manager = BridgeManager(
            extension,
            BridgeConfig(servers=(server,), limits=limits(backoff_initial_ms=10, backoff_max_ms=20)),
            scratch_directory=self.scratch,
            client_factory=factory,
            random_source=random.Random(1),
        )
        self.managers.append(manager)
        with mock.patch("octet_mcp.manager.threading.Timer") as timer:
            self.assertFalse(manager._start_server("fixture", False))
            self.assertEqual(manager._servers["fixture"].state, "backoff")
            self.assertEqual(manager._servers["fixture"].restart_attempt, 1)
            self.assertGreaterEqual(timer.call_args.args[0], 0.001)
            self.assertLessEqual(timer.call_args.args[0], 0.01)
            self.assertFalse(manager._start_server("fixture", False))
            self.assertEqual(manager._servers["fixture"].state, "parked")
            self.assertEqual(len(clients), 2)
            self.assertEqual(timer.call_count, 1)

    def test_permanent_protocol_failure_parks_and_publishes_no_tool(self):
        extension, manager = self.manager(server_config("malformed", max_restarts=8))
        wait_for(
            lambda: root_node(manager.snapshot(), "fixture")["state"] == "unavailable",
            message="permanent malformed frame parking",
        )
        self.assertEqual(extension._tools, {})
        detail = manager.execute_command(["show", "fixture"])["text"]
        self.assertIn("parked", detail)
        self.assertIn("malformed_frame", detail)

    def test_compact_and_generic_state_never_expose_launch_or_environment_values(self):
        secret = "TOP_SECRET_CONFIG_VALUE"
        configured = server_config(
            "stable",
            environment={"FIXTURE_TOKEN": secret},
            extra_args=("--shutdown-marker", "/sensitive/path/marker"),
        )
        extension, manager = self.manager(configured)
        snapshot = manager.snapshot()
        encoded = json.dumps(snapshot)
        status = manager.status_contribution()["text"]
        self.assertNotIn(secret, encoded)
        self.assertNotIn("shutdown-marker", encoded)
        self.assertNotIn("/sensitive", encoded)
        self.assertNotIn(secret, status)
        self.assertRegex(status, r"^mcp \d+/1 · \d+ tools")
        self.assertEqual(snapshot["revision"], extension.presentations[-1]["revision"])

    def test_confirm_unknown_tools_asks_once_and_denial_does_not_dispatch(self):
        approved = real_server_config(confirm_unknown_tools=True)
        extension, manager = self.manager(approved, policy="allow", confirm=True)
        wait_for(
            lambda: root_node(manager.snapshot(), "real-fixture")["state"] == "active",
            message="confirming fixture ready",
        )
        name = next(name for name in extension._tools if "fixture_unknown_effect" in name)
        result = extension._tools[name]["handler"]({}, {})
        self.assertFalse(result["is_error"])
        self.assertEqual(len(extension.confirm_calls), 1)
        call = extension.confirm_calls[0]
        self.assertIn("Real local fixture", call["detail"])
        self.assertIn(name, call["detail"])
        self.assertFalse(call["default"])
        # An explicitly read-only tool must never raise a confirmation.
        read_only = next(name for name in extension._tools if "fixture_echo" in name)
        self.assertFalse(extension._tools[read_only]["handler"]({"value": "x"}, {})["is_error"])
        self.assertEqual(len(extension.confirm_calls), 1)

        denied = real_server_config(confirm_unknown_tools=True)
        declined_extension, declined_manager = self.manager(
            denied, policy="allow", confirm=False
        )
        wait_for(
            lambda: root_node(declined_manager.snapshot(), "real-fixture")["state"] == "active",
            message="declining fixture ready",
        )
        declined_name = next(
            name for name in declined_extension._tools if "fixture_unknown_effect" in name
        )
        declined = declined_extension._tools[declined_name]["handler"]({}, {})
        self.assertTrue(declined["is_error"])
        self.assertIn("did not confirm", declined["content"][0]["text"])

    def test_confirm_unknown_tools_without_a_surface_fails_closed(self):
        server = real_server_config(confirm_unknown_tools=True)
        # `confirm=None` leaves the fake surface raising, like a headless frontend.
        extension, manager = self.manager(server, policy="allow", confirm=None)
        wait_for(
            lambda: root_node(manager.snapshot(), "real-fixture")["state"] == "active",
            message="headless fixture ready",
        )
        name = next(name for name in extension._tools if "fixture_unknown_effect" in name)
        result = extension._tools[name]["handler"]({}, {})
        self.assertTrue(result["is_error"])
        self.assertIn("confirmation request failed", result["content"][0]["text"])

    def test_remote_gate_rejects_before_credentials_dns_or_workers(self):
        remote = ServerConfig(
            id="remote",
            label="Remote fixture",
            command="",
            args=(),
            cwd=self.scratch,
            environment={},
            transport="streamable-http",
            url="https://mcp.example.invalid/mcp",
            auth=HttpAuthConfig(credential="remote_fixture"),
        )
        credential_provider = mock.Mock()
        client_factory = mock.Mock(side_effect=AssertionError("remote client must not be built"))
        extension = FakeExtension(self.scratch)
        with mock.patch("socket.getaddrinfo", side_effect=AssertionError("DNS must not run")), mock.patch(
            "octet_mcp.manager.ThreadPoolExecutor",
            side_effect=AssertionError("manager worker must not be constructed"),
        ):
            manager = BridgeManager(
                extension,
                BridgeConfig(servers=(remote,), limits=limits()),
                scratch_directory=self.scratch,
                credential_provider=credential_provider,
                client_factory=client_factory,
            )
            self.managers.append(manager)
            manager.start()
            self.assertTrue(manager.request_action("refresh").result())
            with self.assertRaisesRegex(ValueError, "process-owner experimental CLI opt-in"):
                manager.request_action("restart", "remote")

        self.assertIsNone(manager._executor)
        self.assertEqual(manager._servers["remote"].state, "parked")
        client_factory.assert_not_called()
        credential_provider.bearer_token.assert_not_called()

    def test_safe_user_actions_route_through_declared_mcp_command(self):
        extension, manager = self.manager(real_server_config())
        wait_for(
            lambda: root_node(manager.snapshot(), "real-fixture")["state"] == "active"
        )
        snapshot = manager.snapshot()
        actions = {action["id"]: action for action in snapshot["actions"]}
        self.assertEqual(actions["refresh:real-fixture"]["command"], "mcp")
        self.assertEqual(
            actions["restart:real-fixture"]["arguments"],
            ["restart", "real-fixture"],
        )
        response = manager.execute_command(["stop", "real-fixture"])
        self.assertIn("requested", response["text"])
        wait_for(
            lambda: root_node(manager.snapshot(), "real-fixture")["state"] == "stopped"
        )


if __name__ == "__main__":
    unittest.main()
