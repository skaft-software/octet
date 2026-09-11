"""Credential contention through the stock provider/manager/HTTP composition.

The issuer is in-memory and the private store is temporary. Only the HTTP
connection factory is routed to a controlled numeric-loopback fixture; these
are not live OAuth, DNS/TLS, frontend or full manager-lifecycle journeys.
"""

from __future__ import annotations

from dataclasses import replace
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from unittest import mock
from urllib.parse import parse_qs

from octet_mcp.auth_service import AuthService, OwnerCredentialProvider
from octet_mcp.auth_store import PrivateTokenStore, TokenTransaction
from octet_mcp.manager import ResourceOwner, _RemoteConnection, _RemoteScope, _ScopedCredentials
from octet_mcp.oauth_http import OAuthHttp
from octet_mcp.streamable_http import McpStreamableHttpClient, _PinnedConnection, _endpoint

from .helpers import limits
from .test_auth import binding, observe_lock_contention, owner_context, server
from .test_oauth import FakeAuthExchange, callback_for
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _initialize_result, _json_result, _tool,
)


class AuthContentionTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.path = Path(temporary.name).resolve() / "private"
        self.store = PrivateTokenStore(self.path)
        self.fake = FakeAuthExchange()
        self.config = self.fake.config
        self.now = 1000
        self.current = True
        self.owner = owner_context()["resource_owner"]
        self.service = AuthService(self.store, OAuthHttp(self.fake),
                                   experimental_streamable_http_mcp=True, clock=lambda: self.now)
        self.addCleanup(self.service.shutdown)

    def command(self, action, **kwargs):
        return self.service.execute_command(
            action, self.config, owner_context(), is_current=lambda: self.current,
            trusted_user_command=True, **kwargs,
        )

    def login(self):
        if self.config.auth.type == "bearer":
            result = self.command("login", request_input=lambda *a, **k: "bearer.secret")
        else:
            result = self.command("login-manual", present_authorization=lambda *a, **k: True)
            self.assertEqual(result["auth"]["state"], "pending")
            attempt = self.service._flows[binding(self.config).lease_key].attempt
            result = self.command("complete-manual",
                                  request_input=lambda *a, **k: callback_for(attempt))
        self.assertEqual(result["auth"]["state"], "active")

    def adapter(self):
        provider = OwnerCredentialProvider(
            self.service, (self.config,),
            is_current=lambda owner, config: self.current and owner == self.owner and config == self.config,
        )
        connection = _RemoteConnection(_RemoteScope(ResourceOwner.from_context(owner_context())))
        return _ScopedCredentials(provider, connection), connection

    def start_worker(self, action):
        answers = []

        def run():
            try:
                answers.append(action())
            except Exception as error:
                answers.append(error)

        thread = threading.Thread(target=run, daemon=True)
        thread.start()
        self.addCleanup(thread.join, 2)
        return thread, answers

    def client(self):
        def respond(request):
            message = request.message()
            if message["method"] == "initialize":
                return _json_result(request, _initialize_result())
            if message["method"] == "notifications/initialized":
                return _HttpReply(status=202)
            if message["method"] == "tools/list":
                return _json_result(request, {"tools": [_tool()]})
            self.assertEqual(message["method"], "tools/call")
            return _json_result(request, {
                "content": [{"type": "text", "text": message["params"]["arguments"]["value"]}],
            })

        fixture = _LoopbackFixture(respond)
        self.addCleanup(fixture.close)
        adapter, _ = self.adapter()
        client = McpStreamableHttpClient(
            replace(self.config, startup_timeout_ms=3000, request_timeout_ms=3000),
            limits(shutdown_timeout_ms=250), credential_provider=adapter,
            on_failure=mock.Mock(),
        )
        endpoint = _endpoint(fixture.url)
        # Keep the configured auth resource/issuer exact. Inject only the local
        # test wire, with actual HTTP framing, operation sockets and cancellation.
        patches = (
            mock.patch.object(client, "_connection", side_effect=lambda operation, deadline:
                              _PinnedConnection(endpoint, (socket.AF_INET, (endpoint.host, endpoint.port)),
                                                operation, deadline)),
            mock.patch.object(socket, "getaddrinfo", side_effect=AssertionError("live DNS is forbidden")),
        )
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        self.addCleanup(client.close)
        client.start()
        self.assertEqual(client.list_tools()[0]["name"], "echo")
        return client, fixture

    def concurrent_http_calls(self, client, fixture, entered, release, token):
        threads = []
        try:
            with observe_lock_contention() as contended:
                first, first_answers = self.start_worker(lambda: client.call_tool("echo", {"value": "first"}))
                threads.append(first)
                self.assertTrue(entered.wait(1), "first lookup did not enter the held transaction")
                with client._lock:
                    winner, = client._operations
                second, second_answers = self.start_worker(lambda: client.call_tool("echo", {"value": "second"}))
                threads.append(second)
                self.assertTrue(contended.wait(1), "second lookup did not contend on the credential lock")
                second.join(0.1)
                self.assertTrue(second.is_alive(), "healthy contention must wait, not fail authentication")
                self.assertIsNone(client.fatal_error)
                self.assertFalse(winner.aborted)
                release.set()
                for thread in threads:
                    thread.join(2)
                    self.assertFalse(thread.is_alive())
            self.assertEqual(first_answers, [{"content": [{"type": "text", "text": "first"}]}])
            self.assertEqual(second_answers, [{"content": [{"type": "text", "text": "second"}]}])
            self.assertIsNone(client.fatal_error)
            self.assertTrue(client.alive)
            self.assertFalse(winner.aborted)
            client.on_failure.assert_not_called()
            self.assertFalse(client._operations)
            calls = [request for request in fixture.requests if request.message()["method"] == "tools/call"]
            self.assertEqual(len(calls), 2)  # Each admitted operation sent once, never replayed.
            self.assertEqual(len({request.message()["id"] for request in calls}), 2)
            self.assertEqual({request.message()["params"]["arguments"]["value"] for request in calls},
                             {"first", "second"})
            self.assertTrue(all(request.header("authorization") == "Bearer " + token for request in calls))
            self.assertFalse(fixture.errors)
        finally:
            release.set()
            for thread in threads:
                thread.join(2)

    def test_concurrent_http_refresh_rotates_once_and_rereads_persisted_winner(self):
        self.login()
        client, fixture = self.client()
        self.now = 4590
        self.fake.calls.clear()
        self.fake.token.update(access_token="rotated.access", refresh_token="rotated.refresh")
        entered, release = threading.Event(), threading.Event()

        def pause_refresh(request):
            self.assertFalse((self.path / (binding(self.config).key + ".json")).exists())
            entered.set()
            self.assertTrue(release.wait(2))
            self.assertFalse(request.cancel())

        self.fake.post_hook = pause_refresh
        self.concurrent_http_calls(client, fixture, entered, release, "rotated.access")
        posts = [request for request in self.fake.calls if request.method == "POST"]
        self.assertEqual(len(posts), 1)
        posted = parse_qs(posts[0].body.decode())
        self.assertEqual(posted["refresh_token"], ["refresh.secret"])
        self.assertEqual(posted["resource"], [self.config.url])
        with self.store.transaction(binding(self.config), deadline=time.monotonic() + 1,
                                    cancel=lambda: False) as transaction:
            record = transaction.load()
        self.assertEqual(record.access_token, "rotated.access")
        self.assertEqual(record.refresh_token, "rotated.refresh")
        self.assertEqual(record.expires_at, self.now + 3600)
        self.assertFalse(self.service._flows)

    def test_concurrent_http_bearer_lookups_wait_without_fatal_or_sibling_abort(self):
        self.config = server()
        self.login()
        client, fixture = self.client()
        entered, release = threading.Event(), threading.Event()
        load = TokenTransaction.load

        def pause_lookup(transaction):
            record = load(transaction)
            if not entered.is_set():
                entered.set()
                self.assertTrue(release.wait(2))
            return record

        with mock.patch.object(TokenTransaction, "load", pause_lookup):
            self.concurrent_http_calls(client, fixture, entered, release, "bearer.secret")
        self.assertFalse(self.fake.calls)  # Bearer lookup never discovers or refreshes OAuth.

    def test_waiting_provider_stops_on_deadline_cancel_revocation_or_shutdown_without_refresh(self):
        self.login()
        self.now = 4590
        self.fake.calls.clear()
        for reason in ("deadline", "cancel", "connection", "session", "owner", "shutdown"):
            with self.subTest(reason=reason), observe_lock_contention() as contended:
                adapter, connection = self.adapter()
                cancelled = threading.Event()
                deadline = time.monotonic() + (0.15 if reason == "deadline" else 5)
                with self.store.transaction(binding(self.config), deadline=time.monotonic() + 5,
                                            cancel=lambda: False) as transaction:
                    thread, answers = self.start_worker(lambda: adapter.bearer_token(
                        self.config.auth.credential, server_id=self.config.id,
                        deadline=deadline, cancel=cancelled.is_set,
                    ))
                    self.assertTrue(contended.wait(1))
                    thread.join(0.025)
                    self.assertTrue(thread.is_alive(), "provider must still be waiting before interruption")
                    if reason == "cancel":
                        cancelled.set()
                    elif reason == "connection":
                        connection.revoked.set()
                    elif reason == "session":
                        connection.scope.revoked.set()
                    elif reason == "owner":
                        self.current = False
                    elif reason == "shutdown":
                        self.service.shutdown()
                    thread.join(1)
                    self.assertFalse(thread.is_alive())  # Holder is still locked.
                    self.assertEqual(answers, [None])
                    self.assertEqual(transaction.load().refresh_token, "refresh.secret")
                self.current = True
                self.assertFalse(self.fake.calls)

    def test_contenders_do_not_replay_ambiguous_or_nonrotating_refresh(self):
        for failure in ("ambiguous", "nonrotating"):
            with self.subTest(failure=failure):
                self.now = 1000
                self.fake.post_hook = None
                self.login()
                self.now = 4590
                self.fake.calls.clear()
                entered, release = threading.Event(), threading.Event()

                def pause_refresh(request):
                    entered.set()
                    self.assertTrue(release.wait(2))
                    if failure == "ambiguous":
                        raise RuntimeError("fake issuer lost the response")
                    # Otherwise the fake issuer returns its unchanged refresh token.

                self.fake.post_hook = pause_refresh
                adapter, _ = self.adapter()
                token = lambda: adapter.bearer_token(self.config.auth.credential, server_id=self.config.id)
                threads = []
                try:
                    with observe_lock_contention() as contended:
                        first, first_answers = self.start_worker(token)
                        threads.append(first)
                        self.assertTrue(entered.wait(1))
                        second, second_answers = self.start_worker(token)
                        threads.append(second)
                        self.assertTrue(contended.wait(1))
                        second.join(0.1)
                        self.assertTrue(second.is_alive())
                        release.set()
                        for thread in threads:
                            thread.join(2)
                            self.assertFalse(thread.is_alive())
                    self.assertEqual(first_answers, [None])
                    self.assertEqual(second_answers, [None])
                    self.assertIsNone(token())
                    self.assertEqual(len([request for request in self.fake.calls if request.method == "POST"]), 1)
                    with self.store.transaction(binding(self.config), deadline=time.monotonic() + 1,
                                                cancel=lambda: False) as transaction:
                        self.assertIsNone(transaction.load())
                finally:
                    release.set()
                    for thread in threads:
                        thread.join(2)


if __name__ == "__main__":
    unittest.main()
