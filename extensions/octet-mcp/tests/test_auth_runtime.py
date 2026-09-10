"""Stock runtime + actual SDK private helpers + real manager, with hermetic peers."""
from __future__ import annotations

from concurrent.futures import Future
import io
import json
from pathlib import Path
import socket
import tempfile
import threading
import time
import unittest
from unittest import mock
from urllib.parse import urlsplit

from octet_extension import CancellationToken
from octet_mcp.auth_store import PrivateTokenStore
from octet_mcp.config import BridgeConfig
from octet_mcp.oauth_http import OAuthHttp
from octet_mcp.oauth_transport import PinnedOAuthExchange
from octet_mcp.runtime import build_runtime
from octet_mcp.streamable_http import McpAuthenticationError

from .helpers import wait_for
from .test_auth import server
from .test_oauth import FakeAuthExchange, callback_for
from .test_owner_lifecycle import RemoteClient, context


class CredentialClient(RemoteClient):
    def credential(self):
        token = self.provider.bearer_token(self.config.auth.credential, server_id=self.config.id)
        self.tokens.append(token)
        if token is None:
            raise McpAuthenticationError("authentication_unavailable", "MCP authentication unavailable", permanent=True)
        return token


class AuthHost:
    """Actual Extension dispatch/correlation; only external peers/UI are faked."""
    def __init__(self, path, config, *, gate=True, lifecycle=True, confirmations=True,
                 exchange=None, real_http=False):
        self.clients = []
        self.public = []
        self.private = []
        self.answers = lambda params: ("continue" if params["prompt"].startswith("Manual MCP authorization\n")
                                       else "bearer.secret")
        self.confirmed = lambda params: True
        self._id = 0
        self._revision = 0
        self._names = set()
        self.owner = context()
        self.path = path
        def client(*args, **kwargs):
            value = CredentialClient(*args, **kwargs)
            self.clients.append(value)
            return value
        with mock.patch("octet_mcp.runtime.load_config", return_value=BridgeConfig((config,))):
            self.extension, self.manager = build_runtime(
                experimental_streamable_http_mcp=gate, auth_store=PrivateTokenStore(path),
                oauth_http=OAuthHttp(exchange) if exchange else None,
                client_factory=None if real_http else client)
        self.extension._send = self.send
        self.logs = io.StringIO()
        self.extension.logger.stream = self.logs
        self.initialized = self.extension._initialize({
            "api_version": "0.2", "contributes": {"tools": [], "commands": ["mcp"],
                "hooks": ["before_prompt"], "ui": ["status"], "presentation": True,
                "confirmations": confirmations},
            "protocol": {"version": "0.2", "required_features": ["request_cancellation", "content_parts"],
                "optional_features": ["dynamic_tools", "lifecycle_events"] if lifecycle else ["dynamic_tools"],
                "limits": {"max_concurrent_requests": 4}},
        })
        self.event("session/started", self.owner["host"]["session_id"])

    def send(self, message):
        method = message.get("method")
        if method in {"input/request", "confirmation/request"}:
            self.private.append(message)
            assert isinstance(message["params"]["parent_request_id"], int)
            if method == "input/request":
                assert message["params"]["secret"] is True
                result = {"value": self.answers(message["params"])}
            else:
                assert message["params"]["default"] is False
                result = {"confirmed": self.confirmed(message["params"])}
        else:
            self.public.append(message)
            if method == "tools/register":
                self._names.update(item["name"] for item in message["params"]["tools"])
            elif method == "tools/unregister":
                self._names.difference_update(message["params"]["names"])
            else:
                return
            self._revision += 1
            result = {"revision": self._revision, "tools": sorted(self._names)}
        self.extension._resolve_response({"jsonrpc": "2.0", "id": message["id"], "result": result})

    def invoke(self, method, params, *, cancellation=None):
        self._id += 1
        request_id = self._id
        self.extension._handle_request(request_id, method, params, cancellation or CancellationToken(request_id))
        return next(message for message in self.public if message.get("id") == request_id)

    def command(self, action, owner=None, *, cancellation=None):
        arguments = action if isinstance(action, list) else ["auth", action, "remote"]
        return self.invoke("command/execute", {"name": "mcp", "arguments": arguments,
                           "context": self.owner if owner is None else owner}, cancellation=cancellation)

    def event(self, method, host_session):
        self.extension._submit_notification(method, {"session_id": host_session})

    def bearer(self, owner=None):
        config = self.manager.config.servers[0]
        return self.manager._credential_provider.bearer_token(config.auth.credential, server_id=config.id,
                  resource_owner=(owner or self.owner)["resource_owner"])

    def restart(self):
        result = self.command(["restart", "remote"])
        assert "requested" in result["result"]["text"], result
        wait_for(lambda: self.manager._servers["remote"].state in {"ready", "parked"})
        return self.clients[-1]

    def close(self):
        self.extension._shutdown_handler({})
        self.extension._executor.shutdown(wait=True)


class RuntimeAuthTests(unittest.TestCase):
    def setUp(self):
        temporary = tempfile.TemporaryDirectory()
        self.addCleanup(temporary.cleanup)
        self.root = Path(temporary.name).resolve()

    def host(self, config=None, **kwargs):
        host = AuthHost(self.root / "private", config or server(), **kwargs)
        self.addCleanup(host.close)
        return host

    def test_observations_and_passive_auth_commands_never_launch_or_create_private_store(self):
        host = self.host()
        host.manager.start()
        for args in ([], ["status"], ["list"], ["snapshot"], ["show", "remote"],
                     ["auth", "status", "remote"], ["auth", "poll", "remote"],
                     ["auth", "cancel", "remote"], ["auth", "logout", "remote"]):
            host.command(args)
        self.assertFalse(host.clients)
        self.assertFalse(host.private)
        self.assertFalse(host.path.exists())
        self.assertIsNone(host.manager._remote_scope)

    def test_bearer_private_command_to_provider_catalog_call_logout_and_stale_handler(self):
        host = self.host()
        self.assertIsInstance(host.extension.authentication.service._http._exchange, PinnedOAuthExchange)
        self.assertEqual(host.initialized["tools"], [])
        host.manager.start()
        self.assertFalse(host.clients)
        self.assertIsNone(host.bearer())
        self.assertIn("stored privately", host.command("login")["result"]["text"])
        self.assertEqual(len(host.private), 1)
        self.assertEqual(host.private[0]["method"], "input/request")
        client = host.restart()
        self.assertEqual(client.tokens, ["bearer.secret", "bearer.secret"])
        name = next(iter(host.extension._tools))
        params = {"name": name, "arguments": {"value": "safe echo"},
                  "catalog_revision": host.extension.tool_catalog_revision, "context": host.owner}
        response = host.invoke("tool/call", params)
        self.assertEqual(response["result"]["content"][0]["text"], "safe echo")
        self.assertEqual(len(client.calls), 1)
        self.assertIn("removed", host.command("logout")["result"]["text"])
        self.assertTrue(client.closed.is_set())
        self.assertIsNone(host.bearer())
        self.assertIsNone(client.provider.bearer_token("mcp_key", server_id="remote"))
        self.assertTrue(host.invoke("tool/call", params)["result"]["is_error"])
        self.assertEqual(len(client.calls), 1)
        self.assertNotIn("bearer.secret", (json.dumps(host.public) + host.logs.getvalue()))
        self.assertFalse(list(host.path.glob("*.json")))

    def test_missing_negotiation_owner_gate_and_model_tool_cannot_login_or_borrow(self):
        for options, owner in (({"gate": False}, context()), ({"lifecycle": False}, context()),
                               ({}, {}), ({}, context(host_session="foreign-host"))):
            host = self.host(**options)
            before = len(host.clients)
            result = host.command("login", owner)
            self.assertNotIn("stored", result["result"]["text"])
            self.assertEqual(len(host.clients), before)
            self.assertFalse(host.private)
            self.assertIsNone(host.bearer())
        host = self.host()
        response = host.invoke("tool/call", {"name": "mcp", "arguments": {"action": "login"},
                               "context": context(), "catalog_revision": 0})
        self.assertEqual(response["error"]["code"], -32601)
        self.assertFalse(host.private)
        self.assertFalse(host.path.exists())
        host.command("login")
        for foreign in (context("owner-b"), context(generation=2), context(instance="replacement")):
            self.assertIsNone(host.bearer(foreign))
            before = len(host.private)
            self.assertNotIn("stored", host.command("login", foreign)["result"]["text"])
            self.assertEqual(before, len(host.private))

    def test_oauth_begin_poll_cancel_then_loopback_completion_private_url_only(self):
        exchange = FakeAuthExchange()
        host = self.host(exchange.config, exchange=exchange)
        self.assertIn("login started", host.command("login")["result"]["text"])
        flow = next(iter(host.extension.authentication.service._flows.values()))
        state, verifier, url = flow.attempt._state, flow.attempt._verifier, flow.attempt.authorization_url()
        self.assertEqual([item["method"] for item in host.private], ["confirmation/request", "input/request"])
        self.assertIn(url, host.private[-1]["params"]["prompt"])
        self.assertNotIn(url, json.dumps(host.private[0]))
        self.assertNotIn(state, json.dumps(host.private[0]))
        self.assertIn("awaits", host.command("poll")["result"]["text"])
        self.assertFalse(any(r.method == "POST" for r in exchange.calls))
        parts = urlsplit(callback_for(flow.attempt))
        with socket.create_connection(("127.0.0.1", parts.port), timeout=1) as sock:
            sock.sendall((f"GET {parts.path}?{parts.query} HTTP/1.1\r\nHost: {parts.netloc}\r\n\r\n").encode())
            self.assertIn(b"200 OK", sock.recv(4096))
        self.assertTrue(flow.listener.done.wait(1))
        self.assertIn("stored privately", host.command("poll")["result"]["text"])
        self.assertEqual(host.bearer(), "access.secret")
        self.assertEqual(len([r for r in exchange.calls if r.method == "POST"]), 1)
        self.assertFalse(host.clients)  # Authentication never replays/starts a tool or MCP session.
        self.assertIn("explicit user login", host.command("poll")["result"]["text"])
        self.assertIn("login started", host.command("login")["result"]["text"])
        other = next(iter(host.extension.authentication.service._flows.values()))
        self.assertIn("cancelled", host.command("cancel")["result"]["text"])
        self.assertTrue(other.listener.done.wait(1))
        for value in (state, verifier, url, "code.secret", "access.secret", "refresh.secret"):
            self.assertNotIn(value, (json.dumps(host.public) + host.logs.getvalue()))

    def test_manual_completion_headless_declined_confirmation_and_private_input_failure(self):
        exchange = FakeAuthExchange()
        host = self.host(exchange.config, exchange=exchange)
        host.confirmed = lambda params: False
        self.assertIn("cancelled", host.command("login")["result"]["text"])
        self.assertFalse(host.extension.authentication.service._flows)
        host.confirmed = lambda params: True
        self.assertIn("started", host.command("login-manual")["result"]["text"])
        flow = next(iter(host.extension.authentication.service._flows.values()))
        host.answers = lambda params: callback_for(flow.attempt)
        self.assertIn("stored privately", host.command("complete-manual")["result"]["text"])
        self.assertEqual(host.private[-1]["method"], "input/request")
        no_confirmation = self.host(exchange.config, exchange=FakeAuthExchange(), confirmations=False)
        self.assertIn("unavailable", no_confirmation.command("login")["result"]["text"])
        self.assertFalse(no_confirmation.private)
        bearer = self.host()
        bearer.answers = lambda params: None
        self.assertIn("cancelled", bearer.command("login")["result"]["text"])
        bearer.answers = mock.Mock(side_effect=RuntimeError("PRIVATE EXCEPTION SECRET"))
        self.assertIn("unavailable", bearer.command("login")["result"]["text"])
        self.assertNotIn("PRIVATE EXCEPTION SECRET", (json.dumps(bearer.public) + bearer.logs.getvalue()))

    def test_oauth_unavailable_or_declined_private_context_cannot_exchange_or_persist(self):
        for answer in (None, "cancel", "yes"):
            with self.subTest(answer=answer):
                exchange = FakeAuthExchange()
                host = self.host(exchange.config, exchange=exchange)
                host.answers = lambda params, value=answer: value
                self.assertIn("cancelled", host.command("login")["result"]["text"])
                self.assertFalse(host.extension.authentication.service._flows)
                self.assertEqual([item["method"] for item in host.private],
                                 ["confirmation/request", "input/request"])
                self.assertFalse(any(request.method == "POST" for request in exchange.calls))
                self.assertFalse(host.clients)
                self.assertFalse(list(host.path.glob("*.json")))

    def test_display_session_settlement_not_durable_id_revokes_flows_and_generation(self):
        exchange = FakeAuthExchange()
        host = self.host(exchange.config, exchange=exchange)
        host.command("login")
        flow = next(iter(host.extension.authentication.service._flows.values()))
        host.event("session/settled", host.owner["resource_owner"]["session_id"])
        self.assertTrue(host.extension.authentication.service._flows)
        started = time.monotonic()
        host.event("session/settled", host.owner["host"]["session_id"])
        self.assertLess(time.monotonic() - started, 0.25)
        self.assertFalse(host.extension.authentication.service._flows)
        self.assertTrue(flow.listener.done.wait(1))
        host.event("session/started", host.owner["host"]["session_id"])
        self.assertNotIn("started", host.command("login")["result"]["text"])
        self.assertIsNone(host.bearer())

    def test_same_durable_owner_fresh_runtime_recovers_but_other_owner_does_not(self):
        first = self.host()
        first.command("login")
        first.close()
        fresh = self.host()
        fresh.owner = context(instance="replacement", generation=2)
        client = fresh.restart()  # No prompt/status activation or reverse input borrowed.
        self.assertEqual(client.tokens, ["bearer.secret", "bearer.secret"])
        self.assertFalse(fresh.private)
        other = self.host()
        other.owner = context("owner-b")
        client = other.restart()
        self.assertEqual(client.tokens, [None])
        self.assertFalse(other.private)

    def test_cancel_logout_and_settlement_do_not_block_behind_private_input(self):
        for terminal in ("cancel", "logout", "settlement", "shutdown"):
            host = self.host()
            entered, release = threading.Event(), threading.Event()
            def private(params):
                entered.set()
                release.wait(2)
                return "late.secret"
            host.answers = private
            result = []
            thread = threading.Thread(target=lambda: result.append(host.command("login")))
            thread.start()
            self.assertTrue(entered.wait(1))
            started = time.monotonic()
            if terminal == "settlement":
                host.event("session/settled", host.owner["host"]["session_id"])
            elif terminal == "shutdown":
                host.extension.authentication.shutdown()
            else:
                host.command(terminal)
            self.assertLess(time.monotonic() - started, 0.25)
            release.set()
            thread.join(1)
            self.assertFalse(thread.is_alive())
            self.assertIn("cancelled", result[0]["result"]["text"])
            self.assertFalse(list(host.path.glob("*.json")))
            self.assertNotIn("late.secret", (json.dumps(host.public) + host.logs.getvalue()))

    def test_runtime_refresh_rotation_is_owner_bound_and_does_not_borrow_reverse_input(self):
        exchange = FakeAuthExchange()
        host = self.host(exchange.config, exchange=exchange)
        now = 1000
        host.extension.authentication.service._clock = lambda: now
        host.command("login-manual")
        flow = next(iter(host.extension.authentication.service._flows.values()))
        host.answers = lambda params: callback_for(flow.attempt)
        host.command("complete-manual")
        now = 4590
        exchange.token.update(access_token="rotated.access", refresh_token="rotated.refresh")
        private_count = len(host.private)
        self.assertEqual(host.bearer(), "rotated.access")
        self.assertEqual(len(host.private), private_count)
        self.assertIsNone(host.bearer(context("foreign")))
        self.assertNotIn("rotated.access", (json.dumps(host.public) + host.logs.getvalue()))
        self.assertNotIn("rotated.refresh", (json.dumps(host.public) + host.logs.getvalue()))

    def test_logout_invalidates_login_already_waiting_for_connection_retirement(self):
        host = self.host()
        pending, completed = Future(), Future()
        completed.set_result(True)
        entered = threading.Event()
        def stop(*args, **kwargs):
            if not entered.is_set():
                entered.set()
                return pending
            return completed
        result = []
        with mock.patch.object(host.manager, "request_action", side_effect=stop):
            thread = threading.Thread(target=lambda: result.append(host.command("login")))
            thread.start()
            self.assertTrue(entered.wait(1))
            self.assertIn("removed", host.command("logout")["result"]["text"])
            pending.set_result(True)
            thread.join(1)
        self.assertFalse(thread.is_alive())
        self.assertNotIn("stored", result[0]["result"]["text"])
        self.assertFalse(host.private)
        self.assertFalse(list(host.path.glob("*.json")))

    def test_command_cancellation_and_untrusted_arguments_have_only_fixed_results(self):
        host = self.host()
        token = CancellationToken(1)
        def private(params):
            token._cancel("test")
            return "cancelled.secret"
        host.answers = private
        self.assertEqual(host.command("login", cancellation=token)["error"]["code"], -32800)
        self.assertFalse(list(host.path.glob("*.json")))
        for args in (["auth", "login", "remote", "raw-secret"], ["auth", "raw-secret", "remote"],
                     ["auth", "login", "raw-secret"], ["auth", {"token": "raw-secret"}, "remote"]):
            self.assertNotIn("raw-secret", json.dumps(host.command(args)))
        self.assertNotIn("cancelled.secret", (json.dumps(host.public) + host.logs.getvalue()))


if __name__ == "__main__":
    unittest.main()
