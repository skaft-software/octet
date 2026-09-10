from __future__ import annotations

from dataclasses import replace
import json
import os
from pathlib import Path
import stat
import tempfile
import time
import unittest
from unittest import mock

from octet_mcp.auth import AuthBinding, AuthError, AuthOwner, TokenRecord
from octet_mcp.auth_service import AuthService
from octet_mcp.auth_store import PrivateTokenStore
from octet_mcp.config import HttpAuthConfig, ServerConfig


def owner_context(session="owner-a", generation=1, instance="instance-a"):
    return {"resource_owner": {"session_id": session, "extension_instance_id": instance,
                               "process_generation": generation}}


def server(kind="bearer"):
    auth = HttpAuthConfig("mcp_key") if kind == "bearer" else HttpAuthConfig(
        "oauth_key", type="oauth", issuer="https://auth.example.test/tenant",
        client_id="registered-public-client")
    return ServerConfig("remote", "Reviewed server", "", (), Path("/"), {},
                        transport="streamable-http", url="https://mcp.example.test/mcp", auth=auth)


def binding(config=None, context=None):
    return AuthBinding.from_server(AuthOwner.from_context(context or owner_context()), config or server())


class AuthStoreTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()
        self.path = self.root / "private"
        self.store = PrivateTokenStore(self.path)
        self.binding = binding()

    def transaction(self, bind=None, **kwargs):
        return self.store.transaction(bind or self.binding, deadline=time.monotonic() + 5,
                                      cancel=kwargs.get("cancel", lambda: False))

    def save(self):
        with self.transaction() as tx:
            tx.save(TokenRecord("bearer.secret"))
        return self.path / (self.binding.key + ".json")

    def test_inert_constructor_and_plaintext_private_roundtrip(self):
        self.assertFalse(self.path.exists())
        path = self.save()
        self.assertEqual(stat.S_IMODE(path.stat().st_mode), 0o600)
        self.assertEqual(stat.S_IMODE(self.path.stat().st_mode), 0o700)
        self.assertIn("bearer.secret", path.read_text())  # Not encryption.
        with self.transaction() as tx:
            record = tx.load()
            self.assertEqual(record.access_token, "bearer.secret")
            self.assertNotIn("bearer.secret", repr(record))
            self.assertTrue(tx.delete())
            self.assertIsNone(tx.load())
            self.assertFalse(tx.delete())

    def test_disk_binding_is_durable_but_live_lease_uses_complete_triple(self):
        self.save()
        fresh = binding(context=owner_context(generation=2, instance="replacement"))
        self.assertEqual(fresh.key, self.binding.key)
        self.assertNotEqual(fresh.lease_key, self.binding.lease_key)
        with self.transaction(fresh) as tx:
            self.assertEqual(tx.load().access_token, "bearer.secret")
        variants = [binding(context=owner_context(session="owner-b")),
                    binding(replace(server(), id="another-server")),
                    binding(replace(server(), url="https://mcp.example.test/another")),
                    binding(replace(server(), scope="project")),
                    binding(replace(server(), auth=HttpAuthConfig("another_reference")))]
        for other in variants:
            with self.subTest(key=other.key), self.transaction(other) as tx:
                self.assertIsNone(tx.load())

    def test_copied_token_record_cannot_cross_owner(self):
        original = self.save()
        other = binding(context=owner_context(session="owner-b"))
        target = self.path / (other.key + ".json")
        target.write_bytes(original.read_bytes())
        target.chmod(0o600)
        with self.assertRaisesRegex(AuthError, "safety check"), self.transaction(other) as tx:
            tx.load()

    def test_rejects_symlink_hardlink_fifo_and_insecure_file_mode(self):
        path = self.save()
        path.chmod(0o644)
        with self.assertRaises(AuthError), self.transaction() as tx:
            tx.load()
        path.chmod(0o600)
        hardlink = self.root / "hardlink"
        os.link(path, hardlink)
        with self.assertRaises(AuthError), self.transaction() as tx:
            tx.save(TokenRecord("new"))
        hardlink.unlink()
        path.unlink()
        outside = self.root / "outside"
        outside.write_text("do not touch")
        outside.chmod(0o600)
        path.symlink_to(outside)
        for operation in ("load", "delete", "save"):
            with self.subTest(operation=operation), self.assertRaises(AuthError), self.transaction() as tx:
                getattr(tx, operation)(TokenRecord("new")) if operation == "save" else getattr(tx, operation)()
        self.assertEqual(outside.read_text(), "do not touch")
        path.unlink()
        os.mkfifo(path, 0o600)
        started = time.monotonic()
        with self.assertRaises(AuthError), self.transaction() as tx:
            tx.load()
        self.assertLess(time.monotonic() - started, 0.5)

    def test_symlink_swap_at_open_cannot_read_a_secret_elsewhere(self):
        path = self.save()
        outside = self.root / "outside"
        outside.write_text("stolen-secret")
        outside.chmod(0o600)
        original_open = os.open
        def racing_open(name, flags, *args, **kwargs):
            if name == path.name:
                path.unlink()
                path.symlink_to(outside)
            return original_open(name, flags, *args, **kwargs)
        with mock.patch("octet_mcp.auth_store.os.open", side_effect=racing_open):
            with self.assertRaises(AuthError), self.transaction() as tx:
                tx.load()
        self.assertEqual(outside.read_text(), "stolen-secret")

    def test_rejects_linked_ancestors_permissions_and_traversal(self):
        real = self.root / "real"
        real.mkdir(mode=0o700)
        self.path.symlink_to(real, target_is_directory=True)
        with self.assertRaises(AuthError), self.transaction():
            pass
        self.path.unlink()
        self.path.mkdir(mode=0o755)
        with self.assertRaises(AuthError), self.transaction():
            pass
        self.path.chmod(0o700)
        self.store = PrivateTokenStore(self.path / ".." / "escape")
        with self.assertRaises(AuthError), self.transaction():
            pass
        parent = self.root / "writable"
        parent.mkdir(mode=0o777)
        parent.chmod(0o777)
        self.store = PrivateTokenStore(parent / "private")
        with self.assertRaises(AuthError), self.transaction():
            pass

    def test_wrong_uid_lock_links_bounds_duplicates_and_cancellation(self):
        path = self.save()
        with mock.patch("octet_mcp.auth_store.os.getuid", return_value=os.getuid() + 1):
            with self.assertRaises(AuthError), self.transaction():
                pass
        for data in (b" " * (65536 + 1), b'{"version":1,"version":1}',
                     b'{"token":{"access_token":"secret"}}'):
            path.write_bytes(data)
            with self.assertRaises(AuthError), self.transaction() as tx:
                tx.load()
        lock = self.path / (self.binding.key + ".lock")
        lock.unlink()
        lock.symlink_to(path)
        with self.assertRaises(AuthError), self.transaction():
            pass
        with self.assertRaisesRegex(AuthError, "cancelled"), self.transaction(cancel=lambda: True):
            pass

    def test_refresh_transaction_lock_is_nonblocking_across_store_instances(self):
        other = PrivateTokenStore(self.path)
        with self.transaction():
            with self.assertRaisesRegex(AuthError, "already in progress"), other.transaction(
                    self.binding, deadline=time.monotonic() + 5, cancel=lambda: False):
                pass

    def test_write_failure_leaves_previous_record_and_no_temporary_secret(self):
        path = self.save()
        with mock.patch("octet_mcp.auth_store.os.replace", side_effect=OSError("RAW SECRET")):
            with self.assertRaises(AuthError) as caught:
                with self.transaction() as tx:
                    tx.save(TokenRecord("replacement-secret"))
        self.assertNotIn("RAW SECRET", str(caught.exception))
        self.assertEqual(json.loads(path.read_text())["token"]["access_token"], "bearer.secret")
        self.assertFalse(list(self.path.glob("*.tmp")))


class BearerServiceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name).resolve() / "private"
        self.service = AuthService(PrivateTokenStore(self.path), experimental_streamable_http_mcp=True)
        self.addCleanup(self.service.shutdown)
        self.config = server()
        self.current = True

    def command(self, action, **kwargs):
        return self.service.execute_command(action, self.config, owner_context(),
                                            is_current=lambda: self.current,
                                            trusted_user_command=True, **kwargs)

    def provider(self, context=None, config=None):
        return self.service.scoped_provider(config or self.config, context or owner_context(),
                                             is_current=lambda: self.current)

    def test_private_bearer_setup_lookup_logout_and_no_ambient_fallback(self):
        private = mock.Mock(return_value="bearer.secret")
        snapshots = []
        with mock.patch.dict(os.environ, {"mcp_key": "ambient-token", "ACCESS_TOKEN": "ambient-token"}):
            self.assertIsNone(self.provider().bearer_token("mcp_key", server_id="remote"))
            private.assert_not_called()
            result = self.command("login", request_input=private, present_status=snapshots.append)
        self.assertTrue(private.call_args.kwargs["secret"])
        self.assertEqual(result["auth"]["state"], "active")
        self.assertNotIn("bearer.secret", json.dumps([result, snapshots]))
        provider = self.provider()
        self.assertEqual(provider.bearer_token("mcp_key", server_id="remote"), "bearer.secret")
        self.assertIsNone(provider.bearer_token("wrong", server_id="remote"))
        self.assertIsNone(provider.bearer_token("mcp_key", server_id="wrong"))
        self.assertEqual(self.command("logout")["auth"]["state"], "stopped")
        self.assertIsNone(provider.bearer_token("mcp_key", server_id="remote"))

    def test_model_server_data_gate_owner_and_cancellation_cannot_initiate_setup(self):
        private = mock.Mock(return_value="never-read")
        for trusted, ctx in ((False, owner_context()), (True, {}), (1, owner_context())):
            response = self.service.execute_command("login", self.config, ctx,
                        is_current=lambda: True, trusted_user_command=trusted, request_input=private)
            self.assertEqual(response["auth"]["code"], "authentication_denied")
        blocked = AuthService(PrivateTokenStore(self.path))
        result = blocked.execute_command("login", self.config, owner_context(),
                     is_current=lambda: True, trusted_user_command=True, request_input=private)
        self.assertEqual(result["auth"]["code"], "authentication_gate")
        private.assert_not_called()
        self.assertFalse(self.path.exists())
        self.assertEqual(self.command("login", request_input=private, cancel=lambda: True)["auth"]["state"],
                         "cancelled")
        self.assertEqual(self.command("login", request_input=lambda *a, **k: None)["auth"]["state"], "cancelled")

    def test_bad_private_values_and_exceptions_are_never_diagnostics(self):
        for secret in ("Authorization: raw-secret", "raw-secret\nInjected: yes", "☃", "x" * 16385):
            result = self.command("login", request_input=lambda *a, **k: secret)
            self.assertEqual(result["auth"]["state"], "unavailable")
            self.assertNotIn(secret, json.dumps(result))
        result = self.command("login", request_input=mock.Mock(side_effect=RuntimeError("RAW TOKEN")))
        self.assertNotIn("RAW TOKEN", json.dumps(result))

    def test_stale_live_lease_fails_fresh_same_durable_owner_resumes(self):
        self.command("login", request_input=lambda *a, **k: "bearer.secret")
        provider = self.provider()
        self.current = False
        self.assertIsNone(provider.bearer_token("mcp_key", server_id="remote"))
        fresh = self.service.scoped_provider(self.config, owner_context(generation=2, instance="new-instance"),
                                             is_current=lambda: True)
        self.assertEqual(fresh.bearer_token("mcp_key", server_id="remote"), "bearer.secret")
        self.current = True
        other = self.provider(owner_context(session="different-owner"))
        self.assertIsNone(other.bearer_token("mcp_key", server_id="remote"))
        changed = self.provider(config=replace(self.config, url="https://mcp.example.test/new"))
        self.assertIsNone(changed.bearer_token("mcp_key", server_id="remote"))


if __name__ == "__main__":
    unittest.main()
