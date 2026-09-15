"""Deterministic regressions for the nine gated Streamable HTTP defects."""
from __future__ import annotations

from dataclasses import replace
import json
from pathlib import Path
import tempfile
import ssl
import threading
import time
import unittest
from unittest.mock import patch, Mock

from octet_mcp.config import BridgeConfig, HttpAuthConfig
from octet_mcp.http_network import PinnedConnection, resolve_addresses
from octet_mcp.manager import BridgeManager
from octet_mcp.protocol import McpError, McpTimeout
from octet_mcp.streamable_http import MAX_HTTP_CONTROLS, McpStreamableHttpClient, _HttpOperation

from .helpers import FakeCancellation, FakeExtension, limits, wait_for
from .test_streamable_http import (
    OWNER, CONTEXT, _HttpReply, _LoopbackFixture, _TokenProvider,
    _initialize_result, _json_result, _remote_config, _sse_event, _tool,
)


class HttpHardeningTests(unittest.TestCase):
    def fixture(self, responder):
        fixture = _LoopbackFixture(responder)
        self.addCleanup(fixture.close)
        return fixture

    def client(self, fixture, **kwargs):
        client = McpStreamableHttpClient(
            _remote_config(fixture.url), limits(shutdown_timeout_ms=150),
            resource_owner=OWNER, **kwargs,
        )
        self.addCleanup(client.close)
        return client

    def ready_responder(self, request):
        if request.method == "DELETE":
            return _HttpReply()
        message = request.message()
        if message.get("method") == "initialize":
            return _json_result(request, _initialize_result(), headers={"Mcp-Session-Id": "session"})
        if message.get("method") == "notifications/initialized":
            return _HttpReply(status=202)
        return None

    def test_dns_rejects_all_nonpublic_and_transition_answers(self):
        unsafe = ["127.0.0.1", "10.0.0.1", "169.254.169.254", "::1", "fc00::1",
                  "::ffff:127.0.0.1", "64:ff9b::7f00:1", "2002:7f00:1::", "224.0.0.1", "192.0.0.8", "4000::1", "3fff::1"]
        for address in unsafe:
            with self.subTest(address=address), patch(
                "octet_mcp.http_network.RESOLVER_PROGRAM",
                f"print({json.dumps(['8.8.8.8', address])!r})",
            ):
                with self.assertRaises(McpError) as raised:
                    resolve_addresses("reviewed.invalid", _HttpOperation(), time.monotonic() + 2)
                self.assertEqual(raised.exception.code, "unsafe_address")
        for address in ("127.0.0.1", "::1"):
            self.assertEqual(resolve_addresses(address, _HttpOperation(), time.monotonic() + 1), (address,))

    def test_pins_survive_rebinding_and_numeric_connect_performs_no_dns(self):
        fixture = self.fixture(lambda request: _json_result(request, _initialize_result()) if request.message().get("id") else _HttpReply(status=202))
        client = self.client(fixture)
        with patch("octet_mcp.streamable_http.resolve_addresses", side_effect=[("127.0.0.1",), ("10.0.0.1",)]) as resolve, patch(
            "socket.getaddrinfo", side_effect=AssertionError("secondary DNS lookup")
        ):
            client.start()
            self.assertEqual(resolve.call_count, 1)
        self.assertEqual(fixture.errors, ())

    def test_tls_uses_verified_context_and_original_hostname_not_numeric_pin(self):
        operation = _HttpOperation()
        sock = Mock()
        context = Mock()
        with patch("octet_mcp.http_network.socket.socket", return_value=sock), patch(
            "octet_mcp.http_network.ssl.create_default_context", return_value=context
        ) as factory:
            connection = PinnedConnection("reviewed.example", 443, address="8.8.8.8", tls=True,
                                          operation=operation, deadline=time.monotonic() + 1)
            connection.connect()
            sock.connect.assert_called_once_with(("8.8.8.8", 443))
            factory.assert_called_once_with()
            context.wrap_socket.assert_called_once_with(sock, server_hostname="reviewed.example", do_handshake_on_connect=False)
            context.wrap_socket.return_value.do_handshake.assert_called_once_with()
        operation.abort()

    def test_loopback_tls_success_sni_hostname_and_trust_failure(self):
        certificate = Path(__file__).resolve().parents[1] / "fixtures/tls/loopback-cert.pem"
        key = certificate.with_name("loopback-key.pem")
        server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        server_context.load_cert_chain(certificate, key)
        sni = []
        server_context.set_servername_callback(lambda sock, name, context: sni.append(name))
        fixture = _LoopbackFixture(lambda request: self.ready_responder(request) or _json_result(request, {}), tls_context=server_context)
        self.addCleanup(fixture.close)
        trusted = ssl.create_default_context(cafile=str(certificate))
        untrusted = ssl.create_default_context()
        for hostname, context, succeeds in (("fixture.invalid", trusted, True), ("wrong.invalid", trusted, False), ("fixture.invalid", untrusted, False)):
            with self.subTest(hostname=hostname, succeeds=succeeds):
                client = McpStreamableHttpClient(_remote_config(fixture.url.replace("127.0.0.1", hostname)), limits(shutdown_timeout_ms=150), resource_owner=OWNER)
                with patch("octet_mcp.streamable_http.resolve_addresses", return_value=("127.0.0.1",)), patch("octet_mcp.http_network.ssl.create_default_context", return_value=context):
                    if succeeds:
                        client.start()
                        self.assertTrue(client.alive)
                    else:
                        with self.assertRaises(McpError) as raised:
                            client.start()
                        self.assertEqual(raised.exception.code, "tls_failed")
                    client.close()
        self.assertIn("fixture.invalid", sni)
        self.assertIn("wrong.invalid", sni)
        self.assertNotIn("127.0.0.1", sni)
        self.assertEqual(fixture.errors, ())

    def test_complete_chunked_json_and_sse_are_accepted(self):
        for sse in (False, True):
            with self.subTest(sse=sse):
                def responder(request):
                    if request.message().get("method") != "initialize":
                        return _HttpReply(status=202)
                    reply = _json_result(request, _initialize_result())
                    body = _sse_event({"jsonrpc": "2.0", "id": request.message()["id"], "result": _initialize_result()}) if sse else reply.body
                    return _HttpReply(headers={"Content-Type": "text/event-stream" if sse else "application/json", "Transfer-Encoding": "chunked"},
                                      body=f"{len(body):x}\r\n".encode() + body + b"\r\n0\r\n\r\n", include_content_length=False)
                fixture = self.fixture(responder)
                client = self.client(fixture)
                client.start()
                self.assertTrue(client.alive)

    def test_dns_process_is_killed_and_reaped_on_cancellation_and_close(self):
        for close in (False, True):
            with self.subTest(close=close), patch("octet_mcp.http_network.RESOLVER_PROGRAM", "import time; time.sleep(60)"):
                client = McpStreamableHttpClient(_remote_config("https://unused.invalid/mcp"), limits(shutdown_timeout_ms=500), resource_owner=OWNER)
                cancellation = FakeCancellation()
                errors = []
                def run():
                    try:
                        client.request("tools/call", {}, timeout_ms=2000, cancellation=cancellation)
                    except McpError as error:
                        errors.append(error)
                caller = threading.Thread(target=run)
                caller.start()
                wait_for(lambda: any(op._processes for op in client._operations), message="DNS helper started")
                processes = [process for op in tuple(client._operations) for process in tuple(op._processes)]
                if close:
                    client.close()
                else:
                    cancellation.cancel("test")
                caller.join(2)
                client.close()
                self.assertFalse(caller.is_alive())
                self.assertTrue(errors)
                self.assertFalse(client._operations)
                self.assertTrue(all(process.poll() is not None for process in processes))

    def test_peer_request_is_answered_before_terminal_and_identity_is_not_redacted(self):
        replied = threading.Event()
        peer_ids = []
        def responder(request):
            ready = self.ready_responder(request)
            if ready is not None:
                return ready
            message = request.message()
            if "error" in message:
                peer_ids.append(message["id"])
                replied.set()
                return _HttpReply(status=202)
            def stream(output):
                # Same numeric ID as the outbound request is not its response.
                for peer_id in (message["id"], "jsonrpc"):
                    output.write(_sse_event({"jsonrpc": "2.0", "id": peer_id, "method": "sampling/createMessage"}))
                    output.flush()
                    self.assertTrue(replied.wait(1))
                    replied.clear()
                output.write(_sse_event({"jsonrpc": "2.0", "id": message["id"], "result": {"content": [{"type": "text", "text": "jsonrpc"}]}}))
                output.flush()
            return _HttpReply(headers={"Content-Type": "text/event-stream"}, include_content_length=False, stream=stream)
        fixture = self.fixture(responder)
        provider = _TokenProvider("jsonrpc")
        client = McpStreamableHttpClient(_remote_config(fixture.url, auth=HttpAuthConfig(credential="test")), limits(shutdown_timeout_ms=150), resource_owner=OWNER, credential_provider=provider)
        self.addCleanup(client.close)
        client.start()
        result = client.call_tool("echo", {})
        self.assertEqual(peer_ids, [2, "jsonrpc"])
        self.assertEqual(result["content"][0]["text"], "[redacted]")
        self.assertEqual(set(provider.owners), {OWNER})

    def test_foreign_stream_terminal_is_not_routed_to_another_request(self):
        fixture = self.fixture(lambda request: self.ready_responder(request) or _HttpReply(
            headers={"Content-Type": "text/event-stream"},
            body=_sse_event({"jsonrpc": "2.0", "id": request.message()["id"] + 1, "result": {}}),
        ))
        client = self.client(fixture)
        client.start()
        with self.assertRaises(McpError) as raised:
            client.call_tool("echo", {})
        self.assertEqual(raised.exception.code, "unexpected_response")

    def test_control_concurrency_is_bounded_and_close_tracks_every_worker(self):
        release = threading.Event()
        received = []
        def responder(request):
            received.append(request)
            release.wait(2)
            return _HttpReply(status=202)
        fixture = self.fixture(responder)
        client = self.client(fixture)
        try:
            # Event-blocked exchanges and frozen watchdog scheduling isolate
            # admission from socket backlog/expiry on a loaded shared runner.
            with patch("octet_mcp.streamable_http.threading.Timer"), patch.object(
                client, "_exchange", side_effect=lambda active, **kwargs: active._aborted.wait(5)
            ):
                for index in range(MAX_HTTP_CONTROLS):
                    client._reply_method_not_found(index)
                self.assertEqual(len(client._operations), MAX_HTTP_CONTROLS)
                with self.assertRaises(McpError) as raised:
                    client._reply_method_not_found(999)
                self.assertEqual(raised.exception.code, "control_message_limit")
            client.close()
            wait_for(lambda: not client._operations, timeout=1, message="all control workers drained")
        finally:
            release.set()

    def test_control_watchdog_interrupts_a_response_without_a_waiting_caller(self):
        entered = threading.Event()
        release = threading.Event()
        def stream(output):
            output.write(b"x")
            output.flush()
            entered.set()
            release.wait(2)
        fixture = self.fixture(lambda request: _HttpReply(status=202, include_content_length=False, stream=stream))
        client = self.client(fixture)
        try:
            client._reply_method_not_found(1)
            self.assertTrue(entered.wait(1))
            wait_for(lambda: not client._operations, timeout=1, message="control deadline aborted socket")
        finally:
            release.set()

    def test_control_budget_cannot_reset_across_resumption(self):
        self._resumption_budget("control_message_limit", _sse_event({"jsonrpc": "2.0", "method": "notifications/tools/list_changed"}) * 9, frame_limit=8192)

    def test_byte_budget_cannot_reset_across_resumption(self):
        self._resumption_budget("http_body_too_large", b"event: ignored\ndata: " + b"x" * 600 + b"\n\n", frame_limit=1024)

    def test_event_budget_cannot_reset_across_resumption(self):
        self._resumption_budget("sse_event_limit", b"event: ignored\ndata: {}\n\n" * 129, frame_limit=16384)

    def _resumption_budget(self, expected, events, frame_limit):
        def responder(request):
            if request.method == "GET":
                return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=events)
            ready = self.ready_responder(request)
            if ready is not None:
                return ready
            return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=b"id: cursor\n\n" + events)
        fixture = self.fixture(responder)
        client = McpStreamableHttpClient(_remote_config(fixture.url, max_restarts=3), limits(max_frame_bytes=frame_limit, shutdown_timeout_ms=150), resource_owner=OWNER)
        self.addCleanup(client.close)
        client.start()
        with self.assertRaises(McpError) as raised:
            client.call_tool("echo", {})
        self.assertEqual(raised.exception.code, expected)
        self.assertEqual(sum(request.method == "GET" for request in fixture.requests), 1)
        self.assertEqual(sum(request.method == "POST" and request.message().get("method") == "tools/call" for request in fixture.requests), 1)

    def test_truncated_content_length_sse_event_and_chunked_bodies_fail_closed(self):
        def responder(request):
            result = _json_result(request, _initialize_result())
            event = _sse_event({"jsonrpc": "2.0", "id": request.message()["id"], "result": _initialize_result()})
            if request.target == "/json":
                return replace(result, headers={**result.headers, "Content-Length": str(len(result.body) + 20)})
            if request.target == "/sse-length":
                return _HttpReply(headers={"Content-Type": "text/event-stream", "Content-Length": str(len(event) + 20)}, body=event)
            if request.target == "/sse-event":
                return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=event.rstrip(b"\n"))
            if request.target == "/sse-blank":
                return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=event[:-1])
            suffix = {"/chunked": b"", "/chunked-trailer": b"0\r\n", "/chunked-size": b"0"}[request.target]
            return _HttpReply(headers={"Content-Type": "application/json", "Transfer-Encoding": "chunked"},
                              body=f"{len(result.body):x}\r\n".encode() + result.body + b"\r\n" + suffix, include_content_length=False)
        fixture = self.fixture(responder)
        for target, code in (("json", "truncated_http_body"), ("sse-length", "truncated_http_body"), ("sse-event", "truncated_sse_event"), ("sse-blank", "truncated_sse_event"), ("chunked", "truncated_http_body"), ("chunked-trailer", "truncated_http_body"), ("chunked-size", "truncated_http_body")):
            with self.subTest(target=target):
                client = McpStreamableHttpClient(_remote_config(fixture.url.replace("/mcp", "/" + target)), limits(shutdown_timeout_ms=150), resource_owner=OWNER)
                with self.assertRaises(McpError) as raised:
                    client.start()
                self.assertEqual(raised.exception.code, code)
                self.assertIsNone(client._session_id)

    def test_empty_id_clears_cursor_but_absent_id_preserves_it(self):
        for reset in (True, False):
            with self.subTest(reset=reset):
                gets = []
                request_id = []
                def responder(request):
                    if request.method == "GET":
                        gets.append(request.header("last-event-id"))
                        body = b"id:\n\n" if reset else b": keep cursor\n\n"
                        if len(gets) == 2:
                            body = _sse_event({"jsonrpc": "2.0", "id": request_id[0], "result": {}})
                        return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=body)
                    ready = self.ready_responder(request)
                    if ready is not None:
                        return ready
                    request_id.append(request.message()["id"])
                    return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=b"id: old-cursor\n\n")
                fixture = self.fixture(responder)
                client = McpStreamableHttpClient(_remote_config(fixture.url, max_restarts=3), limits(shutdown_timeout_ms=150), resource_owner=OWNER)
                self.addCleanup(client.close)
                client.start()
                if reset:
                    with self.assertRaises(McpError) as raised:
                        client.call_tool("echo", {})
                    self.assertEqual(raised.exception.code, "sse_response_interrupted")
                    self.assertEqual(gets, ["old-cursor"])
                else:
                    self.assertEqual(client.call_tool("echo", {}), {})
                    self.assertEqual(gets, ["old-cursor", "old-cursor"])

    def test_startup_deadline_includes_initialized_and_every_catalog_page(self):
        # Use a controlled clock and stub transport, not scheduler-sensitive sleeps.
        for slow_phase in ("initialized", "catalog"):
            with self.subTest(slow_phase=slow_phase):
                fixture = self.fixture(lambda request: _HttpReply(status=500))
                client = self.client(fixture)
                clock = [10.0]
                deadlines = []
                def request(method, params, *, timeout_ms, deadline=None):
                    deadlines.append(deadline)
                    if deadline <= clock[0]:
                        raise McpTimeout("request_timeout", "test deadline")
                    if method == "initialize":
                        clock[0] += 0.6
                        return _initialize_result()
                    clock[0] += 0.3
                    return {"tools": [], "nextCursor": str(clock[0])}
                def notify(method, params, *, deadline=None):
                    deadlines.append(deadline)
                    clock[0] += 0.6 if slow_phase == "initialized" else 0.1
                    if deadline <= clock[0]:
                        raise McpTimeout("request_timeout", "test deadline")
                with patch("octet_mcp.streamable_http.time.monotonic", side_effect=lambda: clock[0]), patch.object(client, "request", side_effect=request), patch.object(client, "notify", side_effect=notify):
                    with self.assertRaises(McpTimeout):
                        client.start()
                        client.list_tools()
                self.assertTrue(all(deadline == 11.0 for deadline in deadlines))

    def test_ownerless_and_cross_owner_sessions_never_reach_credentials_or_network(self):
        provider = _TokenProvider("fixture-token")
        def responder(request):
            return self.ready_responder(request) or _json_result(request, {"tools": [_tool()]})
        fixture = self.fixture(responder)
        with tempfile.TemporaryDirectory() as directory:
            extension = FakeExtension(Path(directory))
            manager = BridgeManager(extension, BridgeConfig(servers=(_remote_config(fixture.url, auth=HttpAuthConfig(credential="test")),), limits=limits(shutdown_timeout_ms=150)), credential_provider=provider, experimental_streamable_http_mcp=True)
            try:
                manager.start()
                self.assertIsNone(manager._executor)
                self.assertEqual(provider.calls, [])
                self.assertEqual(fixture.requests, ())
                self.assertIn("resource_owner_required", manager.execute_command(["show", "remote"])["text"])
                manager.execute_command(["restart", "remote"], CONTEXT)
                wait_for(lambda: bool(extension._tools), message="owner-bound catalog ready")
                handler = next(iter(extension._tools.values()))["handler"]
                before = len(provider.calls)
                for context in ({}, {"resource_owner": {**OWNER.wire(), "session_id": "foreign"}}, {"resource_owner": {**OWNER.wire(), "extension_instance_id": "foreign"}}, {"resource_owner": {**OWNER.wire(), "process_generation": 2}}):
                    result = handler({"value": "do not send", "resource_owner": OWNER.wire()}, context)
                    self.assertTrue(result["is_error"])
                    self.assertIn("owner", str(result))
                    self.assertNotIn("echo", manager.execute_command(["show", "remote"], context)["text"])
                self.assertEqual(before, len(provider.calls))
                self.assertEqual(set(provider.owners), {OWNER})
            finally:
                manager.shutdown()

    def test_late_credential_callback_cannot_connect_after_deadline(self):
        entered = threading.Event()
        release = threading.Event()
        class SlowProvider:
            def bearer_token(self, *args, **kwargs):
                entered.set()
                release.wait(2)
                return "late-token"
        fixture = self.fixture(lambda request: _HttpReply(status=500))
        client = McpStreamableHttpClient(_remote_config(fixture.url, auth=HttpAuthConfig(credential="test")), limits(shutdown_timeout_ms=50), resource_owner=OWNER, credential_provider=SlowProvider())
        errors = []
        def run():
            try:
                client.request("tools/list", {}, timeout_ms=100)
            except McpError as error:
                errors.append(error)
        caller = threading.Thread(target=run)
        caller.start()
        self.assertTrue(entered.wait(1))
        caller.join(1)
        self.assertFalse(caller.is_alive())
        client.close()
        release.set()
        wait_for(lambda: not client._operations, message="late callback drained")
        self.assertTrue(errors)
        self.assertEqual(fixture.requests, ())
