"""Hermetic owner/generation fences for the explicitly single-owner remote bridge."""
from __future__ import annotations

from concurrent.futures import ThreadPoolExecutor
from dataclasses import replace
from pathlib import Path
import tempfile
import threading
import time
import unittest
from unittest import mock

from octet_extension import CancelledError
from octet_mcp.config import BridgeConfig, HttpAuthConfig
from octet_mcp.manager import BridgeManager
from octet_mcp.protocol import McpCancelled, McpTransportError

from .helpers import FakeCancellation, FakeExtension, limits, server_config, wait_for


def context(session="owner-a", *, instance="instance", generation=1, host_session="host-session"):
    return {
        "resource_owner": {
            "session_id": session,
            "extension_instance_id": instance,
            "process_generation": generation,
        },
        "host": {"session_id": host_session},
    }


class OwnerExtension(FakeExtension):
    def __init__(self, scratch):
        super().__init__(scratch)
        self.negotiated_features |= {"lifecycle_events"}
        self.presentation_owners = []

    def publish_presentation(self, snapshot, *, resource_owner=None):
        super().publish_presentation(snapshot)
        self.presentation_owners.append(resource_owner)


class OwnerBroker:
    def __init__(self):
        self.calls = []

    def bearer_token(self, credential, *, server_id, resource_owner, deadline=None, cancel=lambda: False):
        self.calls.append((credential, server_id, dict(resource_owner)))
        return None if cancel() else "token-" + resource_owner["session_id"]


class RemoteClient:
    def __init__(self, config, _limits, on_failure, on_tools_changed, *, credential_provider):
        self.config = config
        self.provider = credential_provider
        self.on_failure = on_failure
        self.on_tools_changed = on_tools_changed
        self.alive = False
        self.closed = threading.Event()
        self.list_entered = threading.Event()
        self.call_entered = threading.Event()
        self.release_call = threading.Event()
        self.block_catalog = False
        self.block_call = False
        self.version = 1
        self.list_count = 0
        self.calls = []
        self.tokens = []

    def credential(self):
        token = self.provider.bearer_token("shared-reference", server_id=self.config.id)
        self.tokens.append(token)
        return token

    def start(self):
        self.credential()
        self.alive = not self.closed.is_set()

    def list_tools(self):
        self.list_count += 1
        self.credential()
        self.list_entered.set()
        if self.block_catalog:
            if not self.closed.wait(3):
                raise AssertionError("catalog was not aborted")
            raise McpTransportError("closed", "Closed fixture")
        properties = {"value": {"type": "string"}}
        if self.version > 1:
            properties["extra"] = {"type": "integer"}
        return [{
            "name": "owner_tool",
            "description": "Owner-private catalog description",
            "inputSchema": {
                "type": "object", "properties": properties,
                "required": ["value"], "additionalProperties": False,
            },
            "annotations": {"readOnlyHint": True},
        }]

    def call_tool(self, name, arguments, *, cancellation, progress):
        cancellation.raise_if_cancelled()
        self.calls.append((name, dict(arguments)))
        self.credential()
        self.call_entered.set()
        while self.block_call and not self.release_call.wait(0.01):
            if cancellation.cancelled:
                raise McpCancelled("cancelled", "Fixture cancelled")
        return {"content": [{"type": "text", "text": arguments["value"]}], "isError": False}

    def close(self):
        self.alive = False
        self.closed.set()


class OwnerLifecycleTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory()
        self.scratch = Path(self.temporary.name)
        self.managers = []
        self.clients = []
        self.broker = OwnerBroker()
        self.owner = context()

    def tearDown(self):
        for manager in self.managers:
            manager.shutdown()
        self.temporary.cleanup()

    def manager(self, *, factory=None, broker=None, bridge_limits=None, start=True, enabled=True):
        extension = OwnerExtension(self.scratch)
        remote = replace(
            server_config(), id="remote", transport="streamable-http", command="", args=(),
            url="http://127.0.0.1:9/mcp", auth=HttpAuthConfig(credential="shared-reference"), enabled=enabled,
        )

        def make_client(*args, **kwargs):
            client = (factory or RemoteClient)(*args, **kwargs)
            self.clients.append(client)
            return client

        manager = BridgeManager(
            extension, BridgeConfig(servers=(remote,), limits=bridge_limits or limits()),
            scratch_directory=self.scratch, credential_provider=broker or self.broker,
            client_factory=make_client, experimental_streamable_http_mcp=True,
        )
        self.managers.append(manager)
        if start:
            manager.start()
        return extension, manager

    def bind(self, manager, owner=None):
        owner = owner or self.owner
        manager.observe_session("session/started", {"session_id": owner["host"]["session_id"]})
        return manager.request_action("restart", "remote", context=owner).result(timeout=3)

    def handler(self, extension):
        return next(iter(extension._tools.values()))["handler"]

    def settle(self, manager):
        manager.observe_session("session/settled", {
            "session_id": self.owner["host"]["session_id"], "outcome": "cancelled",
        })

    def test_owner_activation_is_idempotent_before_and_after_bootstrap_and_cannot_revive(self):
        extension, manager = self.manager(start=False)
        manager.observe_session("session/started", {"session_id": "host-session"})
        self.assertTrue(manager.activate_owner(self.owner))
        self.assertEqual(self.clients, [])
        self.assertIsNone(manager._executor)
        manager.start()
        wait_for(lambda: manager._servers["remote"].state == "ready")
        for _ in range(3):
            self.assertTrue(manager.activate_owner(self.owner))
        self.assertEqual(len(self.clients), 1)
        with self.assertRaises(ValueError):
            manager.activate_owner(context("owner-b"))
        self.assertTrue(manager.request_action("stop", "remote", context=self.owner).result(2))
        self.assertTrue(manager.activate_owner(self.owner))
        # Even an already-queued bootstrap must not undo an explicit stop.
        self.assertFalse(manager._start_server("remote", False, manager._remote_scope))
        self.assertEqual(len(self.clients), 1)
        self.settle(manager)
        with self.assertRaises(ValueError):
            manager.activate_owner(self.owner)
        self.assertEqual(extension._tools, {})

    def test_activation_leaves_disabled_remote_templates_inert(self):
        extension, manager = self.manager(enabled=False)
        manager.observe_session("session/started", {"session_id": "host-session"})
        self.assertFalse(manager.activate_owner(self.owner))
        self.assertEqual(self.clients, [])
        self.assertIsNone(manager._executor)
        self.assertEqual(extension._tools, {})

    def test_remote_startup_and_observation_are_inert_without_owner_and_lifecycle(self):
        extension, manager = self.manager()
        self.assertIsNone(manager._executor)
        manager.execute_command(["status"], self.owner)
        manager.execute_command(["snapshot"], self.owner)
        self.assertEqual(self.clients, [])
        self.assertEqual(self.broker.calls, [])
        with self.assertRaisesRegex(ValueError, "lifecycle"):
            manager.request_action("restart", "remote", context=self.owner)
        manager.observe_session("session/started", {"session_id": "host-session"})
        for invalid in ({}, {"resource_owner": {}}, context(generation=True), context(generation=0)):
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                manager.request_action("restart", "remote", context=invalid)
        extension.negotiated_features -= {"lifecycle_events"}
        with self.assertRaisesRegex(ValueError, "lifecycle"):
            manager.request_action("restart", "remote", context=self.owner)
        self.assertEqual(self.clients, [])

    def test_distinct_owner_instance_and_generation_never_borrow_credentials_or_session(self):
        extension, manager = self.manager()
        self.assertTrue(self.bind(manager))
        handler = self.handler(extension)
        client = self.clients[0]
        self.assertFalse(handler({"value": "allowed"}, self.owner)["is_error"])
        count = len(self.broker.calls)
        for foreign in ({}, context("owner-b"), context(instance="other"), context(generation=2)):
            with self.subTest(foreign=foreign):
                self.assertTrue(handler({"value": "forbidden"}, foreign)["is_error"])
                for action in ("restart", "stop", "refresh"):
                    with self.assertRaises(ValueError):
                        manager.request_action(action, "remote", context=foreign)
        with self.assertRaises(ValueError):
            manager.request_action("refresh")
        for action in (manager.refresh_server, manager.restart_server, manager.stop_server):
            with self.assertRaises(ValueError):
                action("remote")
        self.assertEqual(len(client.calls), 1)
        self.assertEqual(len(self.broker.calls), count)
        self.assertTrue(all(call[2] == self.owner["resource_owner"] for call in self.broker.calls))
        self.assertEqual(set(client.tokens), {"token-owner-a"})

    def test_shared_broker_requires_explicit_independent_owner_bound_bridges(self):
        first_extension, first = self.manager()
        second_extension, second = self.manager()
        second_owner = context("owner-b", host_session="other-host-session")
        self.assertTrue(self.bind(first))
        self.assertTrue(self.bind(second, second_owner))
        self.handler(first_extension)({"value": "a"}, self.owner)
        self.handler(second_extension)({"value": "b"}, second_owner)
        self.assertEqual(set(self.clients[0].tokens), {"token-owner-a"})
        self.assertEqual(set(self.clients[1].tokens), {"token-owner-b"})
        self.assertIsNot(first._servers["remote"].client, second._servers["remote"].client)

    def test_catalog_epochs_and_queued_notifications_keep_their_connection_and_owner(self):
        extension, manager = self.manager()
        self.assertTrue(self.bind(manager))
        client = self.clients[0]
        old_handler = self.handler(extension)
        client.version = 2
        client.on_tools_changed(client)
        wait_for(lambda: extension._revision == 2)
        new_handler = self.handler(extension)
        self.assertTrue(old_handler({"value": "x", "extra": 1}, self.owner)["is_error"])
        self.assertFalse(new_handler({"value": "x", "extra": 1}, self.owner)["is_error"])
        self.assertTrue(new_handler({"value": "x"}, context("owner-b"))["is_error"])
        old_count = len(client.calls)
        self.assertTrue(manager.request_action("restart", "remote", context=self.owner).result(3))
        replacement = self.clients[-1]
        self.assertTrue(new_handler({"value": "x"}, self.owner)["is_error"])
        self.assertEqual(len(client.calls), old_count)
        client.on_tools_changed(client)
        manager._refresh_changed_client(manager._servers["remote"], client, None)
        self.assertEqual(replacement.list_count, 1)
        broker_count = len(self.broker.calls)
        self.assertIsNone(client.provider.bearer_token("shared-reference", server_id="remote"))
        self.assertEqual(len(self.broker.calls), broker_count)

    def test_settled_owner_aborts_inflight_call_and_cannot_reactivate_or_publish_results(self):
        extension, manager = self.manager()
        self.assertTrue(self.bind(manager))
        handler = self.handler(extension)
        client = self.clients[0]
        client.block_call = True
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(handler, {"value": "private"}, self.owner)
            self.assertTrue(client.call_entered.wait(1))
            manager.observe_session("session/settled", {"session_id": "unrelated-host-session"})
            self.assertFalse(client.closed.is_set())
            self.settle(manager)
            result = future.result(2)
        self.assertTrue(result["is_error"])
        self.assertNotIn("private", str(result))
        wait_for(lambda: client.closed.is_set() and not extension._tools)
        self.assertTrue(handler({"value": "late"}, self.owner)["is_error"])
        broker_count = len(self.broker.calls)
        client.on_tools_changed(client)
        manager.observe_session("session/started", {"session_id": "host-session"})
        with self.assertRaises(ValueError):
            manager.request_action("restart", "remote", context=self.owner)
        self.assertIsNone(client.provider.bearer_token("shared-reference", server_id="remote"))
        self.assertEqual(len(self.broker.calls), broker_count)
        self.assertEqual(len(self.clients), 1)
        self.assertIsNone(manager._servers["remote"].timer)

    def test_late_success_after_owner_settlement_is_not_lowered_or_returned(self):
        extension, manager = self.manager()
        self.assertTrue(self.bind(manager))
        client = self.clients[0]
        handler = self.handler(extension)
        entered, release = threading.Event(), threading.Event()

        def ignores_cancel(*args, **kwargs):
            entered.set()
            self.assertTrue(release.wait(2))
            return {"content": [{"type": "text", "text": "late private response"}], "isError": False}

        with mock.patch.object(client, "call_tool", side_effect=ignores_cancel):
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(handler, {"value": "pending"}, self.owner)
                self.assertTrue(entered.wait(1))
                self.settle(manager)
                release.set()
                result = future.result(2)
        self.assertTrue(result["is_error"])
        self.assertNotIn("late private response", str(result))
        self.assertEqual(extension.artifacts, {})

    def test_cancelled_call_is_not_replayed_and_does_not_retire_other_calls(self):
        extension, manager = self.manager()
        self.assertTrue(self.bind(manager))
        client = self.clients[0]
        client.block_call = True
        handler = self.handler(extension)
        with ThreadPoolExecutor(max_workers=1) as pool:
            future = pool.submit(handler, {"value": "cancel"}, self.owner)
            self.assertTrue(client.call_entered.wait(1))
            extension.cancellation.cancel()
            with self.assertRaises(CancelledError):
                future.result(2)
        self.assertEqual(len(client.calls), 1)
        extension.cancellation = FakeCancellation()
        client.block_call = False
        self.assertFalse(handler({"value": "later"}, self.owner)["is_error"])
        self.assertEqual(len(self.clients), 1)
        self.assertEqual(len(client.calls), 2)

    def test_owner_settlement_during_broker_lookup_discards_token(self):
        entered, release = threading.Event(), threading.Event()
        broker = OwnerBroker()
        extension, manager = self.manager(broker=broker)
        self.assertTrue(self.bind(manager))
        provider = self.clients[0].provider
        original = broker.bearer_token

        def blocked(*args, **kwargs):
            entered.set()
            self.assertTrue(release.wait(2))
            return original(*args, **kwargs)

        with mock.patch.object(broker, "bearer_token", side_effect=blocked):
            with ThreadPoolExecutor(max_workers=1) as pool:
                future = pool.submit(provider.bearer_token, "shared-reference", server_id="remote")
                self.assertTrue(entered.wait(1))
                self.settle(manager)
                release.set()
                self.assertIsNone(future.result(2))
        wait_for(lambda: not extension._tools)
        count = len(broker.calls)
        self.assertIsNone(provider.bearer_token("shared-reference", server_id="remote"))
        self.assertEqual(len(broker.calls), count)

    def test_credential_lookup_preserves_request_deadline_and_owner_cancellation(self):
        broker = OwnerBroker()
        _, manager = self.manager(broker=broker)
        self.assertTrue(self.bind(manager))
        provider = self.clients[0].provider
        cancelled = threading.Event()
        captured = {}

        def lookup(credential, *, server_id, resource_owner, deadline, cancel):
            captured.update(deadline=deadline, cancel=cancel, owner=resource_owner)
            self.assertFalse(cancel())
            return "private-token"

        deadline = time.monotonic() + 1
        with mock.patch.object(broker, "bearer_token", side_effect=lookup):
            self.assertEqual(provider.bearer_token(
                "shared-reference", server_id="remote", deadline=deadline,
                cancel=cancelled.is_set), "private-token")
        self.assertEqual(captured["deadline"], deadline)
        self.assertEqual(captured["owner"], self.owner["resource_owner"])
        cancelled.set()
        self.assertTrue(captured["cancel"]())
        cancelled.clear()
        self.assertFalse(captured["cancel"]())
        self.settle(manager)
        self.assertTrue(captured["cancel"]())

    def test_settlement_and_shutdown_abort_inflight_catalog_discovery(self):
        for operation in (self.settle, lambda manager: manager.shutdown()):
            with self.subTest(operation=operation):
                def factory(*args, **kwargs):
                    client = RemoteClient(*args, **kwargs)
                    client.block_catalog = True
                    return client

                extension, manager = self.manager(factory=factory)
                manager.observe_session("session/started", {"session_id": "host-session"})
                future = manager.request_action("restart", "remote", context=self.owner)
                wait_for(lambda: bool(self.clients) and self.clients[-1].list_entered.is_set())
                client = self.clients[-1]
                operation(manager)
                self.assertFalse(future.result(2))
                self.assertTrue(client.closed.is_set())
                self.assertEqual(extension._tools, {})
                self.assertIsNone(manager._servers["remote"].timer)

    def test_reconnect_retains_owner_and_settlement_invalidates_backoff(self):
        extension, manager = self.manager(bridge_limits=limits(backoff_initial_ms=1, backoff_max_ms=2))
        self.assertTrue(self.bind(manager))
        first = self.clients[0]
        first.on_failure(first, McpTransportError("lost", "Fixture lost"))
        wait_for(lambda: len(self.clients) == 2 and manager._servers["remote"].state == "ready")
        self.assertFalse(self.handler(extension)({"value": "new"}, self.owner)["is_error"])
        self.assertEqual(set(self.clients[-1].tokens), {"token-owner-a"})
        scope = manager._remote_scope
        self.settle(manager)
        wait_for(lambda: not extension._tools)
        manager._submit_restart_after_backoff("remote", scope)
        self.assertEqual(len(self.clients), 2)
        self.assertIsNone(manager._servers["remote"].timer)

    def test_remote_presentation_and_command_snapshots_are_owner_fenced(self):
        extension, manager = self.manager()
        self.assertTrue(self.bind(manager))
        handler = self.handler(extension)
        handler({"value": "private"}, self.owner)
        name = next(iter(extension._tools))
        self.assertIn(name, str(manager.snapshot(self.owner)))
        self.assertNotIn(name, str(manager.snapshot()))
        self.assertNotIn(name, str(manager.snapshot(context("owner-b"))))
        self.assertNotIn(name, manager.execute_command(["snapshot"], context("owner-b"))["text"])
        self.assertEqual(extension.presentation_owners[-1], self.owner["resource_owner"])
        self.settle(manager)
        wait_for(lambda: not extension._tools)
        self.assertNotIn(name, str(manager.snapshot(self.owner)))


if __name__ == "__main__":
    unittest.main()
