"""Offline/loopback regressions for the experimental 2025 HTTP transport."""
from __future__ import annotations

from contextlib import contextmanager
from dataclasses import replace
from http.client import HTTPConnection
import io
import json
import socket
import ssl
import subprocess
import threading
import time
import unittest
from unittest import mock

from octet_mcp import streamable_http as http
from octet_mcp.config import HttpAuthConfig
from octet_mcp.protocol import McpError, McpCancelled, McpTimeout
from .helpers import FakeCancellation, limits, wait_for
from .test_streamable_http import (
    _HttpReply, _HttpRequest, _LoopbackFixture, _initialize_result, _json_result, _tool,
    _remote_config, _sse_event, _TokenProvider,
)


@contextmanager
def _memory_http(client, responder):
    """Real HTTP serialization/framing/JSON/SSE parsing, without DNS or sockets."""
    requests = []

    def connection(operation, deadline):
        sock = mock.Mock()

        def makefile(*args):
            head, _, body = b"".join(call.args[0] for call in sock.sendall.call_args_list).partition(b"\r\n\r\n")
            lines = head.decode("ascii").split("\r\n")
            method, target, _ = lines[0].split(" ")
            headers = dict((key.lower(), value.strip()) for key, value in (line.split(":", 1) for line in lines[1:]))
            request = _HttpRequest(method, target, headers, body)
            requests.append(request)
            reply = responder(request)
            headers = {"Content-Length": str(len(reply.body)), "Connection": "close", **reply.headers}
            wire = f"HTTP/1.1 {reply.status} Fixture\r\n" + "".join(f"{key}: {value}\r\n" for key, value in headers.items())
            return io.BytesIO(wire.encode("ascii") + b"\r\n" + reply.body)

        sock.makefile.side_effect = makefile
        transport = HTTPConnection("fixture.invalid")
        transport.response_class = http._StrictHttpResponse
        transport.connect = lambda: setattr(transport, "sock", sock)
        return transport

    with mock.patch.object(client, "_connection", side_effect=connection):
        yield requests


class HttpHardeningTests(unittest.TestCase):
    def setUp(self):
        self.fixtures = []
        self.clients = []

    def tearDown(self):
        for client in self.clients:
            client.close()
        for fixture in self.fixtures:
            fixture.close()

    def client(self, responder, *, bound=None, **config):
        fixture = _LoopbackFixture(responder)
        self.fixtures.append(fixture)
        client = http.McpStreamableHttpClient(
            replace(_remote_config(fixture.url), **config),
            bound or limits(shutdown_timeout_ms=100),
        )
        self.clients.append(client)
        return client, fixture

    @staticmethod
    def sse(body, **headers):
        return _HttpReply(headers={"Content-Type": "text/event-stream", **headers}, body=body)

    def test_server_request_with_same_id_is_not_the_client_response(self):
        def responder(request):
            message = request.message()
            if "method" not in message:
                return _HttpReply(status=202)
            return self.sse(
                _sse_event({"jsonrpc": "2.0", "id": message["id"], "method": "ping"})
                + _sse_event({"jsonrpc": "2.0", "id": message["id"], "result": {"ok": True}})
            )
        client, fixture = self.client(responder)
        self.assertEqual(client.request("tools/call", {}, timeout_ms=1000), {"ok": True})
        wait_for(lambda: any("error" in r.message() for r in fixture.requests))
        reply = next(r.message() for r in fixture.requests if "error" in r.message())
        self.assertEqual(reply["id"], 1)
        self.assertEqual(reply["error"]["code"], -32601)

    def test_sse_redacts_payload_not_json_syntax_or_peer_ids(self):
        for secret in ("jsonrpc", "id", "1", '"'):
            with self.subTest(secret=secret):
                def responder(request):
                    return self.sse(_sse_event({
                        "jsonrpc": "2.0", "id": request.message()["id"],
                        "result": {"echo": secret},
                    }))
                client, _ = self.client(responder, auth=HttpAuthConfig(credential="fixture"))
                client._credential_provider = _TokenProvider(secret)
                self.assertEqual(client.request("tools/call", {}, timeout_ms=1000), {"echo": "[redacted]"})

    def test_legacy_catalog_discriminator_cannot_disable_redaction_or_publish_noncomplete_results(self):
        from octet_mcp.catalog import normalize_catalog_tool

        for is_sse in (False, True):
            for extra, code in (({}, None), ({"resultType": "complete"}, None),
                                ({"resultType": "input_required"}, "unsupported_interaction"),
                                ({"resultType": "unknown"}, "invalid_result_type"),
                                ({"requestState": "opaque"}, "invalid_result")):
                with self.subTest(is_sse=is_sse, extra=extra):
                    secret = "BEARER_SECRET"
                    tool = {**_tool(), "description": "untrusted " + secret}
                    def responder(request):
                        method = request.message()["method"]
                        if method == "initialize":
                            return _json_result(request, _initialize_result())
                        if method == "notifications/initialized":
                            return _HttpReply(status=202)
                        result = {"tools": [tool], **extra}
                        if is_sse:
                            return self.sse(_sse_event({"jsonrpc": "2.0", "id": request.message()["id"], "result": result}))
                        return _json_result(request, result)
                    client = http.McpStreamableHttpClient(
                        _remote_config("http://127.0.0.1:9/mcp", auth=HttpAuthConfig(credential="fixture")),
                        limits(), credential_provider=_TokenProvider(secret),
                    )
                    self.clients.append(client)
                    published = []
                    with _memory_http(client, responder) as requests:
                        client.start()
                        if code:
                            with self.assertRaises(McpError) as raised:
                                published.extend(client.list_tools())
                            self.assertEqual(raised.exception.code, code)
                            self.assertNotIn(secret, str(raised.exception))
                            self.assertEqual(published, [])
                        else:
                            published = client.list_tools()
                            self.assertEqual(published[0]["description"], "untrusted [redacted]")
                            binding = normalize_catalog_tool("remote", "Reviewed", published[0], server_catalog_revision=1)
                            self.assertNotIn(secret, binding.description)
                    self.assertNotIn(secret, json.dumps(published))
                    self.assertNotIn(secret, repr(client.logs.snapshot()))
                    self.assertEqual([r.message()["method"] for r in requests], ["initialize", "notifications/initialized", "tools/list"])
                    self.assertTrue(all(r.header("authorization") == "Bearer " + secret for r in requests))

    def test_real_loopback_catalog_cannot_promote_bearer_through_mrtr_discriminator(self):
        from octet_mcp.catalog import normalize_catalog_tool

        for is_sse in (False, True):
            with self.subTest(is_sse=is_sse):
                secret = "REAL_LOOPBACK_BEARER_SENTINEL"
                poison = False
                def responder(request):
                    if request.method == "DELETE":
                        return _HttpReply()
                    method = request.message()["method"]
                    if method == "initialize":
                        return _json_result(request, _initialize_result())
                    if method == "notifications/initialized":
                        return _HttpReply(status=202)
                    result = {"tools": [{**_tool(), "description": secret}]}
                    if poison:
                        result["resultType"] = "input_required"
                    if is_sse:
                        return self.sse(_sse_event({"jsonrpc": "2.0", "id": request.message()["id"], "result": result}))
                    return _json_result(request, result)
                client, fixture = self.client(responder, auth=HttpAuthConfig(credential="fixture"))
                client._credential_provider = _TokenProvider(secret)
                client.start()
                catalog = client.list_tools()
                binding = normalize_catalog_tool("remote", "Reviewed", catalog[0], server_catalog_revision=1)
                self.assertNotIn(secret, binding.description)
                poison = True
                with self.assertRaises(McpError) as raised:
                    client.list_tools()
                self.assertEqual(raised.exception.code, "unsupported_interaction")
                self.assertNotIn(secret, str(raised.exception))
                self.assertNotIn(secret, repr(client.logs.snapshot()))
                self.assertEqual([r.message()["method"] for r in fixture.requests],
                                 ["initialize", "notifications/initialized", "tools/list", "tools/list"])
                self.assertTrue(all(r.header("authorization") == "Bearer " + secret for r in fixture.requests))


    def test_aggregate_body_budget_survives_post_to_resume(self):
        def responder(request):
            if request.method == "GET":
                return self.sse(_sse_event({"jsonrpc": "2.0", "id": 1, "result": {"text": "x" * 600}}))
            return self.sse(_sse_event({"jsonrpc": "2.0", "method": "notifications/message",
                                       "params": {"data": "x" * 600}}, event_id="cursor"))
        client, fixture = self.client(responder, bound=limits(max_frame_bytes=1024))
        with self.assertRaises(McpError) as raised:
            client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "http_body_too_large")
        self.assertEqual([r.method for r in fixture.requests], ["POST", "GET"])

    def test_aggregate_event_budget_survives_multiple_resumptions(self):
        note = {"jsonrpc": "2.0", "method": "notifications/message"}
        def responder(request):
            events = _sse_event(note, event_id="cursor") * 130
            if request.method == "GET":
                events += _sse_event({"jsonrpc": "2.0", "id": 1, "result": {}})
            return self.sse(events)
        client, _ = self.client(responder)
        with self.assertRaises(McpError) as raised:
            client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "sse_event_limit")

    def test_empty_event_id_on_resume_resets_cursor_and_prohibits_another_get(self):
        note = {"jsonrpc": "2.0", "method": "notifications/message"}
        def responder(request):
            if request.method == "GET":
                return self.sse(_sse_event(note, event_id=""))
            return self.sse(_sse_event(note, event_id="old-cursor"))
        client, fixture = self.client(responder, max_restarts=3)
        with self.assertRaises(McpError) as raised:
            client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "sse_response_interrupted")
        self.assertIsNone(client._last_event_id)
        self.assertEqual([r.method for r in fixture.requests], ["POST", "GET"])

    def test_resume_without_id_field_keeps_previous_cursor(self):
        note = {"jsonrpc": "2.0", "method": "notifications/message"}
        gets = []
        def responder(request):
            if request.method == "GET":
                gets.append(request.header("last-event-id"))
                if len(gets) == 2:
                    return self.sse(_sse_event({"jsonrpc": "2.0", "id": 1, "result": {}}))
                return self.sse(_sse_event(note))
            return self.sse(_sse_event(note, event_id="old-cursor"))
        client, _ = self.client(responder, max_restarts=2)
        self.assertEqual(client.request("tools/call", {}, timeout_ms=1000), {})
        self.assertEqual(gets, ["old-cursor", "old-cursor"])

    def test_unterminated_sse_event_cannot_complete_a_request(self):
        def responder(request):
            return self.sse(_sse_event({"jsonrpc": "2.0", "id": 1, "result": {}}).rstrip(b"\n") + b"\n")
        client, _ = self.client(responder)
        with self.assertRaises(McpError) as raised:
            client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "truncated_sse_event")

    def test_short_declared_body_is_rejected_even_after_valid_terminal_json_or_sse(self):
        for is_sse in (False, True):
            with self.subTest(is_sse=is_sse):
                def responder(request):
                    if is_sse:
                        reply = self.sse(_sse_event({"jsonrpc": "2.0", "id": 1, "result": {}}))
                    else:
                        reply = _json_result(request, {})
                    return replace(reply, headers={**reply.headers, "Content-Length": str(len(reply.body) + 8)})
                client, _ = self.client(responder)
                with self.assertRaises(McpError) as raised:
                    client.request("tools/call", {}, timeout_ms=1000)
                self.assertEqual(raised.exception.code, "http_body_truncated")

    def test_chunked_terminal_sse_requires_chunk_terminator_and_trailer_end(self):
        body = _sse_event({"jsonrpc": "2.0", "id": 1, "result": {}})
        for suffix in (b"", b"0\r\n", b"0\r\nX-Trailer: unfinished\r\n"):
            with self.subTest(suffix=suffix):
                def responder(request):
                    return _HttpReply(headers={"Content-Type": "text/event-stream", "Transfer-Encoding": "chunked"},
                                      body=f"{len(body):x}\r\n".encode() + body + b"\r\n" + suffix,
                                      include_content_length=False)
                client, _ = self.client(responder)
                with self.assertRaises(McpError) as raised:
                    client.request("tools/call", {}, timeout_ms=1000)
                self.assertEqual(raised.exception.code, "http_body_truncated")

    def test_conflicting_transfer_and_length_headers_are_rejected(self):
        def responder(request):
            return _HttpReply(headers={"Content-Type": "application/json", "Transfer-Encoding": "chunked",
                                       "Content-Length": "5"}, body=b"0\r\n\r\n")
        client, _ = self.client(responder)
        with self.assertRaises(McpError) as raised:
            client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "invalid_http_framing")

    def test_initialize_and_initialized_notification_share_one_deadline(self):
        def responder(request):
            if request.method == "DELETE":
                time.sleep(.4)
                return _HttpReply()
            if request.message().get("method") == "initialize":
                time.sleep(.08)
                return _json_result(request, _initialize_result(), headers={"Mcp-Session-Id": "fixture-session"})
            time.sleep(.25)
            return _HttpReply(status=202)
        client, fixture = self.client(responder, startup_timeout_ms=180, request_timeout_ms=1000)
        started = time.monotonic()
        with self.assertRaises(McpTimeout):
            client.start()
        self.assertLess(time.monotonic() - started, .32)
        self.assertFalse(any(r.method == "DELETE" for r in fixture.requests))

    def test_startup_catalog_pages_share_remaining_initialize_deadline(self):
        def responder(request):
            if request.method == "DELETE":
                time.sleep(.6)
                return _HttpReply()
            method = request.message().get("method")
            if method == "initialize":
                time.sleep(.07)
                return _json_result(request, _initialize_result(), headers={"Mcp-Session-Id": "startup-session"})
            if method == "notifications/initialized":
                return _HttpReply(status=202)
            time.sleep(.09)
            cursor = request.message().get("params", {}).get("cursor")
            return _json_result(request, {"tools": [], **({"nextCursor": "page2"} if cursor is None else {})})
        client, fixture = self.client(responder, startup_timeout_ms=210, bound=limits(shutdown_timeout_ms=500))
        started = time.monotonic()
        client.start()
        with self.assertRaises(McpTimeout):
            client.list_tools()
        client.close()
        self.assertLess(time.monotonic() - started, .32)
        self.assertFalse(any(r.method == "DELETE" for r in fixture.requests))

    def test_control_fanout_budget_is_shared_across_post_and_resume(self):
        def responder(request):
            events = b"".join(_sse_event({"jsonrpc": "2.0", "id": f"server-{i}", "method": "ping"},
                                         event_id="cursor") for i in range(9))
            if request.method == "GET":
                events += _sse_event({"jsonrpc": "2.0", "id": 1, "result": {}})
            return self.sse(events)
        client, fixture = self.client(responder)
        with mock.patch.object(client, "_reply_method_not_found") as reply:
            with self.assertRaises(McpError) as raised:
                client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "http_control_limit")
        reply.assert_not_called()
        self.assertEqual([r.method for r in fixture.requests], ["POST", "GET"])

    def test_control_operations_expire_without_an_awaiting_caller(self):
        client = http.McpStreamableHttpClient(_remote_config("http://127.0.0.1:9/mcp"),
                                              limits(shutdown_timeout_ms=80))
        self.clients.append(client)
        operations = []
        def blocked_exchange(operation, **kwargs):
            operations.append(operation)
            while not operation.aborted:
                time.sleep(.005)
        with mock.patch.object(client, "_exchange", side_effect=blocked_exchange):
            client._reply_method_not_found("server-request")
            client._send_cancellation(1, "test")
            wait_for(lambda: len(operations) == 2)
            wait_for(lambda: not client._operations, timeout=.4, message="control deadline cleanup")
            self.assertTrue(all(op.aborted and op.done.is_set() for op in operations))

    def test_valid_chunked_json_and_sse_still_work(self):
        for is_sse in (False, True):
            with self.subTest(is_sse=is_sse):
                def responder(request):
                    if is_sse:
                        reply = self.sse(_sse_event({"jsonrpc": "2.0", "id": 1, "result": {}}))
                    else:
                        reply = _json_result(request, {})
                    encoded = f"{len(reply.body):x}\r\n".encode() + reply.body + b"\r\n0\r\n\r\n"
                    return replace(reply, headers={**reply.headers, "Transfer-Encoding": "chunked"},
                                   body=encoded, include_content_length=False)
                client, _ = self.client(responder)
                self.assertEqual(client.request("tools/call", {}, timeout_ms=1000), {})

    def test_empty_id_only_record_resets_cursor_but_partial_id_is_not_committed(self):
        for suffix, code, remembered in ((b"id:\n\n", "sse_response_interrupted", None),
                                         (b"id: partial", "truncated_sse_event", "cursor")):
            with self.subTest(suffix=suffix):
                def responder(request):
                    return self.sse(_sse_event({"jsonrpc": "2.0", "method": "notifications/message"},
                                               event_id="cursor") + suffix)
                client, fixture = self.client(responder)
                with self.assertRaises(McpError) as raised:
                    client.request("tools/call", {}, timeout_ms=1000)
                self.assertEqual(raised.exception.code, code)
                self.assertEqual(client._last_event_id, remembered)
                self.assertEqual([r.method for r in fixture.requests], ["POST"])

    def test_duplicate_terminal_responses_in_framed_sse_are_rejected(self):
        def responder(request):
            return self.sse(_sse_event({"jsonrpc": "2.0", "id": 1, "result": {}}) * 2)
        client, _ = self.client(responder)
        with self.assertRaises(McpError) as raised:
            client.request("tools/call", {}, timeout_ms=1000)
        self.assertEqual(raised.exception.code, "duplicate_response")

    def test_control_replies_and_cancellations_use_one_bounded_tracked_pool(self):
        client = http.McpStreamableHttpClient(_remote_config("http://127.0.0.1:9/mcp"), limits())
        self.clients.append(client)
        entered = threading.Event()
        def blocked_exchange(operation, **kwargs):
            entered.set()
            while not operation.aborted:
                time.sleep(.005)
        with mock.patch.object(client, "_exchange", side_effect=blocked_exchange):
            for request_id in range(40):
                client._send_cancellation(request_id, "test")
            self.assertTrue(entered.wait(1))
            active = tuple(client._operations)
            self.assertGreater(len(active), 0)
            self.assertLessEqual(len(active), 4)
            client.close()
            wait_for(lambda: all(op.done.is_set() for op in active), timeout=1)


class DnsPinningTests(unittest.TestCase):
    def test_checked_address_is_the_connect_address_and_original_hostname_is_tls_identity(self):
        operation = http._HttpOperation()
        client = http.McpStreamableHttpClient(_remote_config("https://mcp.example.invalid:8443/mcp"), limits())
        raw = mock.Mock()
        tls = mock.Mock()
        context = ssl.create_default_context()
        self.assertTrue(context.check_hostname)
        self.assertEqual(context.verify_mode, ssl.CERT_REQUIRED)
        with mock.patch.object(http, "_resolve_dns", side_effect=[[(socket.AF_INET, "8.8.8.8")],
                                                                 [(socket.AF_INET, "127.0.0.1")]]) as resolve, \
             mock.patch.object(socket, "socket", return_value=raw), \
             mock.patch.object(socket, "getaddrinfo", side_effect=AssertionError("second DNS lookup")), \
             mock.patch.object(ssl, "create_default_context", return_value=context), \
             mock.patch.object(context, "wrap_socket", return_value=tls) as wrap:
            connection = client._connection(operation, time.monotonic() + 1)
            connection.connect()
            resolve.assert_called_once()
            self.assertEqual(resolve.call_args.args[:2], ("mcp.example.invalid", 8443))
            raw.connect.assert_called_once_with(("8.8.8.8", 8443))
            self.assertEqual(wrap.call_args.kwargs["server_hostname"], "mcp.example.invalid")
            self.assertFalse(wrap.call_args.kwargs["do_handshake_on_connect"])
            tls.do_handshake.assert_called_once()
            self.assertEqual(connection.host, "mcp.example.invalid")
            with self.assertRaises(McpError) as rebound:
                client._connection(operation, time.monotonic() + 1)
            self.assertEqual(rebound.exception.code, "destination_rejected")
            raw.connect.assert_called_once()
            operation.abort()
            tls.shutdown.assert_called_with(socket.SHUT_RDWR)
            connection.close()

    def test_all_dns_answers_are_checked_and_numeric_loopback_is_the_only_private_exception(self):
        operation = http._HttpOperation()
        for answer in ("127.0.0.1", "10.0.0.1", "169.254.169.254", "100.64.0.1", "224.0.0.1",
                       "::1", "fe80::1", "::ffff:10.0.0.1", "2002:7f00:1::", "2002:a9fe:a9fe::"):
            with self.subTest(answer=answer):
                records = [(socket.AF_INET, "8.8.8.8"),
                           (socket.AF_INET6 if ":" in answer else socket.AF_INET, answer)]
                with mock.patch.object(http, "_resolve_dns", return_value=records, create=True):
                    with self.assertRaises(McpError) as raised:
                        http._resolve_addresses(http._endpoint("https://example.invalid/mcp"), operation, time.monotonic()+1)
                    self.assertEqual(raised.exception.code, "destination_rejected")
        with mock.patch.object(http, "_resolve_dns", side_effect=AssertionError("literal must bypass DNS"), create=True):
            addresses = http._resolve_addresses(http._endpoint("http://127.0.0.1:99/mcp"), operation, time.monotonic()+1)
            self.assertEqual(addresses, [(socket.AF_INET, ("127.0.0.1", 99))])
            addresses = http._resolve_addresses(http._endpoint("http://[::1]:99/mcp"), operation, time.monotonic()+1)
            self.assertEqual(addresses, [(socket.AF_INET6, ("::1", 99, 0, 0))])

    def test_resolver_helper_output_and_environment_are_bounded(self):
        operation = http._HttpOperation()
        program = http._DNS_PROGRAM
        for count in (1, http.MAX_DNS_ADDRESSES + 1):
            with self.subTest(count=count):
                fake_dns = ("import socket\n"
                            "socket.getaddrinfo = lambda *a, **k: "
                            f"[(socket.AF_INET, socket.SOCK_STREAM, 6, '', ('8.8.8.8', 443))] * {count}\n")
                with mock.patch.object(http, "_DNS_PROGRAM", fake_dns + program), \
                     mock.patch.object(subprocess, "Popen", wraps=subprocess.Popen) as spawn:
                    if count == 1:
                        self.assertEqual(http._resolve_dns("fixture.invalid", 443, operation, time.monotonic()+2),
                                         [(socket.AF_INET, "8.8.8.8")])
                    else:
                        with self.assertRaises(McpError) as raised:
                            http._resolve_dns("fixture.invalid", 443, operation, time.monotonic()+2)
                        self.assertEqual(raised.exception.code, "dns_failed")
                    self.assertEqual(spawn.call_args.args[0][1:4], ["-I", "-S", "-c"])
                    self.assertLessEqual(set(spawn.call_args.kwargs["env"]), {"SYSTEMROOT"})
                    self.assertEqual(operation._resolvers, set())

    def test_certificate_failure_never_sends_http_or_retries(self):
        client = http.McpStreamableHttpClient(_remote_config("https://mcp.example.invalid/mcp"), limits())
        raw, tls = mock.Mock(), mock.Mock()
        context = ssl.create_default_context()
        tls.do_handshake.side_effect = ssl.SSLCertVerificationError("fixture hostname mismatch")
        with mock.patch.object(http, "_resolve_dns", return_value=[(socket.AF_INET, "8.8.8.8")]), \
             mock.patch.object(socket, "socket", return_value=raw), \
             mock.patch.object(ssl, "create_default_context", return_value=context), \
             mock.patch.object(context, "wrap_socket", return_value=tls):
            with self.assertRaises(McpError) as raised:
                client.request("tools/call", {}, timeout_ms=1000)
            self.assertEqual(raised.exception.code, "tls_failed")
            raw.connect.assert_called_once_with(("8.8.8.8", 443))
            raw.sendall.assert_not_called()
            tls.sendall.assert_not_called()
            client.close()

    def test_stalled_dns_process_is_reaped_on_timeout_cancellation_and_close(self):
        for action in ("timeout", "cancel", "close"):
            with self.subTest(action=action):
                client = http.McpStreamableHttpClient(_remote_config("https://example.invalid/mcp"), limits())
                cancellation = FakeCancellation()
                processes = []
                errors = []
                popen = subprocess.Popen
                def record_process(*args, **kwargs):
                    process = popen(*args, **kwargs)
                    processes.append(process)
                    return process
                def request():
                    try:
                        client.request("tools/call", {}, timeout_ms=180 if action == "timeout" else 1000,
                                       cancellation=cancellation)
                    except BaseException as error:
                        errors.append(error)
                with mock.patch.object(http, "_DNS_PROGRAM", "import time; time.sleep(60)", create=True), \
                     mock.patch.object(subprocess, "Popen", side_effect=record_process), \
                     mock.patch.object(socket, "getaddrinfo", side_effect=AssertionError("DNS must run only in the killable child")):
                    caller = threading.Thread(target=request)
                    caller.start()
                    try:
                        wait_for(lambda: bool(processes), timeout=.7, message="DNS process")
                        if action == "cancel":
                            cancellation.cancel()
                        elif action == "close":
                            client.close()
                        caller.join(timeout=1)
                        self.assertFalse(caller.is_alive())
                        self.assertEqual(len(errors), 1)
                        if action == "cancel":
                            self.assertIsInstance(errors[0], McpCancelled)
                        elif action == "timeout":
                            self.assertIsInstance(errors[0], McpTimeout)
                        # Assert transport cleanup before the test cleanup fence.
                        client.close()
                        self.assertTrue(all(process.poll() is not None for process in processes))
                        self.assertEqual(len(processes), 1)
                    finally:
                        client.close()
                        for process in processes:
                            if process.poll() is None:
                                process.kill()
                                process.wait()
                        caller.join(timeout=1)
                    self.assertTrue(all(process.poll() is not None for process in processes))
                    wait_for(lambda: not client._operations, timeout=1, message="DNS operation cleanup")


if __name__ == "__main__":
    unittest.main()
