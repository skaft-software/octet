from __future__ import annotations

from dataclasses import replace
import json
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from unittest import mock
from urllib.parse import parse_qs, urlencode, urlsplit

from octet_mcp.auth_service import AuthService, OwnerCredentialProvider
from octet_mcp.auth_store import PrivateTokenStore
from octet_mcp.oauth_http import OAuthHttp, OAuthHttpResponse

from .test_auth import binding, owner_context
from .test_oauth import FakeAuthExchange, callback_for


class OAuthServiceTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name).resolve() / "private"
        self.fake = FakeAuthExchange()
        self.store = PrivateTokenStore(self.path)
        self.now = 1000
        self.current = True
        self.service = AuthService(self.store, OAuthHttp(self.fake),
                                   experimental_streamable_http_mcp=True, clock=lambda: self.now)
        self.addCleanup(self.service.shutdown)
        self.config = self.fake.config
        self.urls = []
        self.snapshots = []

    def command(self, action, context=None, **kwargs):
        return self.service.execute_command(action, self.config, context or owner_context(),
                   is_current=lambda: self.current, trusted_user_command=True,
                   present_status=self.snapshots.append, **kwargs)

    def present(self, url, **kwargs):
        self.assertEqual(kwargs, {"issuer": self.config.auth.issuer, "resource": self.config.url})
        self.urls.append(url)
        return True

    def flow(self, context=None):
        return self.service._flows[binding(self.config, context).lease_key]

    def login_manual(self):
        result = self.command("login-manual", present_authorization=self.present)
        self.assertEqual(result["auth"]["state"], "pending", result)
        flow = self.flow()
        callback = callback_for(flow.attempt)
        private = mock.Mock(return_value=callback)
        result = self.command("complete-manual", request_input=private)
        self.assertTrue(private.call_args.kwargs["secret"])
        self.assertEqual(result["auth"]["state"], "active", result)
        return result

    def token(self, context=None, cancel=lambda: False):
        provider = self.service.scoped_provider(self.config, context or owner_context(),
                   is_current=lambda: self.current, cancel=cancel)
        return provider.bearer_token(self.config.auth.credential, server_id=self.config.id)

    def read_record(self):
        with self.store.transaction(binding(self.config), deadline=time.monotonic() + 5,
                                    cancel=lambda: False) as transaction:
            return transaction.load()

    def test_browser_begin_returns_promptly_poll_finishes_once_no_auth_data_in_status(self):
        started = time.monotonic()
        result = self.command("login", present_authorization=self.present)
        self.assertEqual(result["auth"]["state"], "pending", result)
        self.assertLess(time.monotonic() - started, 1)
        flow = self.flow()
        verifier, state = flow.attempt._verifier, flow.attempt._state
        posts = lambda: [r for r in self.fake.calls if r.method == "POST"]
        self.assertFalse(posts())
        self.assertEqual(self.command("status")["auth"]["state"], "pending")
        self.assertEqual(self.command("poll")["auth"]["state"], "pending")
        callback = urlsplit(callback_for(flow.attempt))
        with socket.create_connection(("127.0.0.1", callback.port), timeout=1) as sock:
            sock.sendall((f"GET {callback.path}?{callback.query} HTTP/1.1\r\nHost: {callback.netloc}\r\n\r\n").encode())
            self.assertIn(b"200 OK", sock.recv(4096))
        self.assertTrue(flow.listener.done.wait(1))
        self.assertFalse(posts())  # Callback is not a token worker or replay mechanism.
        result = self.command("poll")
        self.assertEqual(result["auth"]["state"], "active", result)
        self.assertEqual(self.token(), "access.secret")
        self.assertEqual(len(posts()), 1)
        self.assertEqual(self.command("poll")["auth"]["code"], "authentication_required")
        self.assertEqual(len(posts()), 1)
        for secret in (state, verifier, "code.secret", "access.secret", "refresh.secret", self.urls[0]):
            self.assertNotIn(secret, json.dumps([result, self.snapshots]))
        self.assertFalse(self.service._flows)

    def test_manual_callback_wrong_state_issuer_and_replay_fail_without_token_request(self):
        for bad in ({"state": "wrong"}, {"iss": "https://evil.test"}, {"iss": None}):
            self.command("login-manual", present_authorization=self.present)
            callback = callback_for(self.flow().attempt, **bad)
            result = self.command("complete-manual", request_input=lambda *a, **k: callback)
            self.assertEqual(result["auth"]["code"], "authentication_callback")
            self.assertFalse(self.service._flows)
            self.assertFalse(any(r.method == "POST" for r in self.fake.calls))
        self.login_manual()
        self.assertEqual(self.command("complete-manual", request_input=mock.Mock())["auth"]["code"],
                         "authentication_required")

    def test_refused_presentation_cancelled_input_and_lookup_do_not_initiate_login(self):
        self.assertIsNone(self.token())
        self.assertFalse(self.fake.calls)
        self.assertFalse(self.service._flows)
        result = self.command("login", present_authorization=lambda *a, **k: False)
        self.assertEqual(result["auth"]["state"], "cancelled")
        self.assertFalse(self.service._flows)
        self.command("login-manual", present_authorization=self.present)
        result = self.command("complete-manual", request_input=lambda *a, **k: None)
        self.assertEqual(result["auth"]["state"], "cancelled")
        self.assertIsNone(self.read_record())

    def test_owner_generation_isolation_and_retirement_shutdown_bound_flows(self):
        self.command("login", present_authorization=self.present)
        flow = self.flow()
        other_context = owner_context(session="other")
        self.assertEqual(self.command("poll", other_context)["auth"]["code"], "authentication_required")
        self.assertEqual(self.command("poll", owner_context(generation=2))["auth"]["code"], "authentication_required")
        self.service.retire_owner(owner_context())
        self.assertFalse(flow.listener._thread.is_alive())
        self.assertFalse(self.service._flows)
        for index in range(4):
            result = self.command("login", owner_context(session=f"owner-{index}"), present_authorization=self.present)
            self.assertEqual(result["auth"]["state"], "pending")
        result = self.command("login", owner_context(session="excess"), present_authorization=self.present)
        self.assertEqual(result["auth"]["code"], "authentication_busy")
        workers = [flow.listener._thread for flow in self.service._flows.values()]
        self.service.shutdown()
        self.assertTrue(all(not thread.is_alive() for thread in workers))
        self.assertFalse(self.service._flows)

    def test_refresh_rotation_persists_new_pair_and_never_sends_rt_to_changed_endpoint(self):
        self.login_manual()
        self.now = 4590  # Within 30-second refresh margin.
        self.fake.token = {**self.fake.token, "access_token": "rotated.access", "refresh_token": "rotated.refresh"}
        def assert_removed(request):
            self.assertFalse((self.path / (binding(self.config).key + ".json")).exists())
        self.fake.post_hook = assert_removed
        self.assertEqual(self.token(), "rotated.access")
        self.assertEqual(self.read_record().refresh_token, "rotated.refresh")
        posted = parse_qs(self.fake.calls[-1].body.decode())
        self.assertEqual(posted["resource"], [self.config.url])
        self.assertEqual(posted["refresh_token"], ["refresh.secret"])
        self.now = 8200
        self.fake.calls.clear()
        self.fake.metadata["token_endpoint"] = "https://other.example.test/token"
        self.assertIsNone(self.token())
        self.assertIsNone(self.read_record())
        self.assertFalse(any(r.method == "POST" for r in self.fake.calls))

    def test_refresh_failure_ambiguous_cancelled_and_unrotated_do_not_replay_or_retain_grant(self):
        failures = [RuntimeError("RAW SECRET ambiguous"),
                    OAuthHttpResponse("https://auth.example.test/token", 400, body=b"RAW SECRET invalid_grant"),
                    {**self.fake.token}, {**self.fake.token, "refresh_token": None}]
        for failure in failures:
            with self.subTest(failure=type(failure).__name__):
                self.fake.overrides.clear()
                self.now = 1000
                self.login_manual()
                self.now = 4590
                self.fake.overrides["https://auth.example.test/token"] = failure
                self.fake.calls.clear()
                self.assertIsNone(self.token())
                self.assertIsNone(self.read_record())
                self.assertEqual(len([r for r in self.fake.calls if r.method == "POST"]), 1)
                count = len(self.fake.calls)
                self.assertIsNone(self.token())
                self.assertEqual(len(self.fake.calls), count)
        self.fake.overrides.clear()
        self.now = 1000
        self.login_manual()
        self.now = 4590
        self.fake.token = {**self.fake.token, "refresh_token": "rotated"}
        cancelled = threading.Event()
        self.fake.post_hook = lambda request: cancelled.set()
        self.assertIsNone(self.token(cancel=cancelled.is_set))
        self.assertIsNone(self.read_record())
        self.assertNotIn("RAW SECRET", json.dumps(self.snapshots))

    def test_owner_change_mid_refresh_prevents_persist_or_use_and_logout_removes_grant(self):
        self.login_manual()
        self.now = 4590
        self.fake.token = {**self.fake.token, "refresh_token": "rotated"}
        self.fake.post_hook = lambda request: setattr(self, "current", False)
        self.assertIsNone(self.token())
        self.current = True
        self.assertIsNone(self.read_record())
        self.fake.post_hook = None
        self.now = 1000
        self.login_manual()
        self.command("login", present_authorization=self.present)
        listener = self.flow().listener
        result = self.command("logout")
        self.assertEqual(result["auth"]["state"], "stopped")
        self.assertIsNone(self.read_record())
        self.assertFalse(listener._thread.is_alive())
        self.assertIsNone(self.token())

    def test_manager_provider_requires_exact_host_owner_and_validated_catalog(self):
        self.login_manual()
        current = owner_context()["resource_owner"]
        provider = OwnerCredentialProvider(self.service, (self.config,),
                                           is_current=lambda owner, config: owner == current)
        self.assertEqual(provider.bearer_token("oauth_key", server_id="remote", resource_owner=current), "access.secret")
        self.assertIsNone(provider.bearer_token("oauth_key", server_id="remote", resource_owner={}))
        self.assertIsNone(provider.bearer_token("oauth_key", server_id="remote",
                                               resource_owner=owner_context(session="other")["resource_owner"]))
        self.assertIsNone(provider.bearer_token("oauth_key", server_id="unknown", resource_owner=current))
        self.assertIsNone(provider.bearer_token("wrong", server_id="remote", resource_owner=current))
        fresh = owner_context(generation=2, instance="replacement")["resource_owner"]
        current = fresh
        self.assertEqual(provider.bearer_token("oauth_key", server_id="remote", resource_owner=fresh), "access.secret")


    def test_explicit_empty_scope_ceiling_rejects_initial_and_refresh_grants_without_persistence(self):
        self.config = replace(self.config, auth=replace(self.config.auth, scopes=()))
        self.fake.token["scope"] = "admin"
        self.command("login-manual", present_authorization=self.present)
        result = self.command("complete-manual", request_input=lambda *a, **k: callback_for(self.flow().attempt))
        self.assertEqual(result["auth"]["code"], "authentication_scope")
        self.assertIsNone(self.read_record())
        self.fake.token.pop("scope")
        self.login_manual()
        self.assertEqual(self.read_record().scopes, ())
        self.now = 4590
        self.fake.token.update(scope="admin", refresh_token="rotated.refresh")
        self.fake.calls.clear()
        self.assertIsNone(self.token())
        self.assertIsNone(self.read_record())
        self.assertEqual(len([request for request in self.fake.calls if request.method == "POST"]), 1)

    def test_provider_accepts_transport_deadline_and_cancellation_without_refunding_budget(self):
        self.login_manual()
        provider = OwnerCredentialProvider(self.service, (self.config,), is_current=lambda owner, config: True)
        owner = owner_context()["resource_owner"]
        self.assertIsNone(provider.bearer_token("oauth_key", server_id="remote", resource_owner=owner,
                                              deadline=time.monotonic() - 1))
        self.assertIsNone(provider.bearer_token("oauth_key", server_id="remote", resource_owner=owner,
                                              cancel=lambda: True))
        self.now = 4590
        self.fake.calls.clear()
        self.fake.token.update(refresh_token="rotated.refresh")
        deadline = time.monotonic() + 0.25
        self.fake.post_hook = lambda request: self.assertEqual(request.deadline, deadline)
        self.assertEqual(provider.bearer_token("oauth_key", server_id="remote", resource_owner=owner,
                                              deadline=deadline), "access.secret")


if __name__ == "__main__":
    unittest.main()
