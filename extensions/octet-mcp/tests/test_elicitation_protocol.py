"""Real bounded loopback SSE reverse routing and raw MRTR transport evidence."""
from __future__ import annotations

from dataclasses import replace
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
import json
import threading
import time
import unittest
from unittest import mock

from octet_mcp.config import HttpAuthConfig
from octet_mcp.protocol import McpCancelled, McpError, McpStdioClient, SUPPORTED_PROTOCOL_VERSIONS
from octet_mcp.streamable_http import McpStreamableHttpClient, _HttpOperation, _decode_json_message
from .helpers import FakeCancellation, limits, server_config, wait_for
from .test_interactions import FORM, URL, PrivateUI, handler_for
from .test_streamable_http import _HttpReply, _LoopbackFixture, _json_bytes, _json_result, _remote_config, _sse_event, _tool, _TokenProvider
from .test_streamable_http_hardening import _memory_http


class _LiveHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):
        pass

    def do_POST(self):
        fixture = self.server.fixture
        message = json.loads(self.rfile.read(int(self.headers.get("Content-Length", "0"))))
        with fixture.lock:
            fixture.messages.append(message)
        method = message.get("method")
        if method == "initialize":
            self.reply({"jsonrpc": "2.0", "id": message["id"], "result": {
                "protocolVersion": "2025-11-25", "serverInfo": {"name": "fixture", "version": "1"},
                "capabilities": {"tools": {}},
            }})
        elif method in {"notifications/initialized", "notifications/cancelled"}:
            self.reply(None)
        elif method in {"tools/call", "resources/read"}:
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            peer_id = message["id"] if fixture.peer_id is None else fixture.peer_id
            try:
                for index in range(fixture.count):
                    fixture.answered.clear()
                    request = {"jsonrpc": "2.0", "id": peer_id if fixture.duplicate else f"{peer_id}:{index}",
                               "method": "elicitation/create", "params": fixture.params}
                    if fixture.count == 1:
                        request["id"] = peer_id
                    self.wfile.write(_sse_event(request))
                    self.wfile.flush()
                    # Cannot produce a terminal result until the client responds.
                    # A buffered-till-terminal client therefore cannot pass.
                    if not fixture.answered.wait(.8):
                        fixture.blocked.set()
                        return
                self.wfile.write(_sse_event({"jsonrpc": "2.0", "id": message["id"], "result": {
                    "content": [{"type": "text", "text": fixture.echo}],
                }}))
                self.wfile.flush()
            except OSError:
                pass  # Intentional socket cancellation/fail-closed tests.
        elif method is None:
            with fixture.lock:
                fixture.replies.append(message)
            self.reply(None, body=fixture.control_body)
            fixture.answered.set()
        else:
            self.reply(None)

    def reply(self, value, *, body=b""):
        data = json.dumps(value).encode() if value is not None else body
        self.send_response(200 if value is not None else 202)
        self.send_header("Content-Length", str(len(data)))
        self.send_header("Content-Type", "application/json")
        self.send_header("Connection", "close")
        self.end_headers()
        self.close_connection = True
        if data:
            try:
                self.wfile.write(data)
                self.wfile.flush()
            except OSError:
                pass


class _LiveFixture:
    def __init__(self, *, params=FORM, peer_id=None, count=1, duplicate=False, echo="done", control_body=b""):
        self.params = params
        self.peer_id = peer_id
        self.count = count
        self.duplicate = duplicate
        self.echo = echo
        self.control_body = control_body
        self.messages = []
        self.replies = []
        self.lock = threading.Lock()
        self.answered = threading.Event()
        self.blocked = threading.Event()
        self.server = ThreadingHTTPServer(("127.0.0.1", 0), _LiveHandler)
        self.server.daemon_threads = True
        self.server.fixture = self
        self.thread = threading.Thread(target=lambda: self.server.serve_forever(poll_interval=.02), daemon=True)
        self.thread.start()
        self.url = f"http://127.0.0.1:{self.server.server_port}/mcp"

    def close(self):
        self.answered.set()
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(1)


class ElicitationProtocolTests(unittest.TestCase):
    def setUp(self):
        self.clients = []
        self.fixtures = []

    def tearDown(self):
        for client in self.clients:
            client.close()
        for fixture in self.fixtures:
            fixture.close()

    def live(self, *, enabled=True, bound=None, **kwargs):
        fixture = _LiveFixture(**kwargs)
        self.fixtures.append(fixture)
        client = McpStreamableHttpClient(_remote_config(fixture.url), bound or limits(shutdown_timeout_ms=100),
                                         enable_elicitation=enabled)
        self.clients.append(client)
        client.start()
        return client, fixture

    def test_2025_11_25_is_supported_without_relabeling_default_handshake(self):
        self.assertIn("2025-11-25", SUPPORTED_PROTOCOL_VERSIONS)
        for enabled in (False, True):
            client, fixture = self.live(enabled=enabled)
            params = fixture.messages[0]["params"]
            self.assertEqual(params["protocolVersion"], "2025-11-25" if enabled else "2025-06-18")
            self.assertEqual(params["capabilities"], {"elicitation": {"form": {}, "url": {}}} if enabled else {})
            self.assertEqual(client.protocol_version, "2025-11-25")

    def test_live_sse_form_accept_preserves_peer_id_even_when_equal_to_our_id(self):
        client, fixture = self.live(echo="private-value")
        ui = PrivateUI('{"name":"private-value"}', "accept")
        handler = handler_for(ui)
        result = client.call_tool("fixture", {}, interaction_handler=handler)
        self.assertNotIn("private-value", json.dumps(result))
        original = next(m for m in fixture.messages if m.get("method") == "tools/call")
        self.assertEqual(fixture.replies, [{"jsonrpc": "2.0", "id": original["id"],
                                          "result": {"action": "accept", "content": {"name": "private-value"}}}])
        self.assertFalse(fixture.blocked.is_set())
        self.assertTrue(all(entry[1]["parent_request_id"] == 77 for entry in ui.inputs))

    def test_live_sse_decline_cancel_and_url_accept(self):
        for params, choice in [(FORM, "decline"), (FORM, None), (URL, "accept"), (URL, "decline"), (URL, "cancel")]:
            with self.subTest(mode=params.get("mode", "form"), choice=choice):
                client, fixture = self.live(params=params, peer_id="literal-server-id")
                result = client.call_tool("fixture", {}, interaction_handler=handler_for(PrivateUI(choice)))
                self.assertEqual(result["content"][0]["text"], "done")
                self.assertEqual(fixture.replies[0]["id"], "literal-server-id")
                self.assertEqual(fixture.replies[0]["result"], {"action": choice or "cancel"})
                self.assertFalse(fixture.blocked.is_set())

    def test_default_or_missing_per_operation_callback_returns_unknown_method_live(self):
        for enabled in (False, True):
            client, fixture = self.live(enabled=enabled)
            self.assertEqual(client.call_tool("fixture", {})["content"][0]["text"], "done")
            self.assertEqual(fixture.replies[0]["error"]["code"], -32601)
            self.assertFalse(fixture.blocked.is_set())

    def test_unsupported_schema_is_declined_without_ui_and_no_false_empty_success(self):
        params = {**FORM, "requestedSchema": {"type": "object", "properties": {"name": {"type": "object"}}}}
        client, fixture = self.live(params=params)
        ui = PrivateUI()
        self.assertEqual(client.call_tool("fixture", {}, interaction_handler=handler_for(ui))["content"][0]["text"], "done")
        self.assertEqual(fixture.replies[0]["result"], {"action": "decline"})
        self.assertEqual(ui.inputs, [])

    def test_stale_owner_and_cancelled_parent_do_not_reply_or_replay(self):
        for stale in (False, True):
            client, fixture = self.live()
            active = [True]
            token = FakeCancellation()
            ui = PrivateUI()
            def invalidate(prompt, **kwargs):
                if stale:
                    active[0] = False
                else:
                    token.cancel()
                return '{"name":"must-not-send"}'
            ui.request_input = invalidate
            handler = handler_for(ui, token=token, active=lambda o, p: active[0])
            with self.assertRaises(McpError):
                client.call_tool("fixture", {}, cancellation=token, interaction_handler=handler)
            self.assertEqual(fixture.replies, [])
            self.assertEqual(sum(m.get("method") == "tools/call" for m in fixture.messages), 1)

    def test_duplicate_reverse_request_id_is_not_answered_or_prompted_twice(self):
        client, fixture = self.live(count=2, duplicate=True)
        ui = PrivateUI("decline", "decline")
        with self.assertRaises(McpError) as raised:
            client.call_tool("fixture", {}, interaction_handler=handler_for(ui))
        self.assertEqual(raised.exception.code, "duplicate_request")
        self.assertEqual(len(ui.inputs), 1)
        self.assertEqual(len(fixture.replies), 1)

    def test_live_control_responses_consume_original_aggregate_byte_budget(self):
        client, fixture = self.live(bound=limits(max_frame_bytes=1024, shutdown_timeout_ms=100), control_body=b"x" * 900)
        with self.assertRaises(McpError) as raised:
            client.call_tool("fixture", {}, interaction_handler=handler_for(PrivateUI("decline")))
        self.assertEqual(raised.exception.code, "http_body_too_large")
        self.assertEqual(sum(m.get("method") == "tools/call" for m in fixture.messages), 1)

    def test_live_fanout_is_bounded_and_original_operation_not_replayed(self):
        client, fixture = self.live(count=17)
        ui = PrivateUI(*(["decline"] * 8))
        with self.assertRaises(McpError) as raised:
            client.call_tool("fixture", {}, interaction_handler=handler_for(ui))
        self.assertEqual(raised.exception.code, "http_control_limit")
        self.assertEqual(len(ui.inputs), 8)
        self.assertEqual(len(fixture.replies), 16)
        self.assertEqual(sum(m.get("method") == "tools/call" for m in fixture.messages), 1)

    def test_unsolicited_or_modern_reverse_request_cannot_borrow_an_origin(self):
        client, fixture = self.live()
        message = {"jsonrpc": "2.0", "id": "peer", "method": "elicitation/create", "params": FORM}
        for modern in (False, True):
            if modern:
                client.protocol_version = "2026-07-28"
            operation = _HttpOperation()
            with self.assertRaises(McpError):
                client._dispatch_live_elicitation(message, operation, 123, time.monotonic() + 1)
        self.assertEqual(fixture.replies, [])

    def test_stdio_denies_ambiguous_reverse_requests_without_any_host_callback(self):
        client = McpStdioClient(server_config(), limits())
        with mock.patch.object(client, "_send_message") as send:
            for pending_count in (0, 1, 2):
                client._pending = {i: mock.Mock() for i in range(pending_count)}
                client._route_message({"jsonrpc": "2.0", "id": "peer", "method": "elicitation/create", "params": FORM})
                self.assertEqual(send.call_args.args[0], {"jsonrpc": "2.0", "id": "peer", "error": {"code": -32601, "message": "Method not found"}})

    def test_generic_decoder_requires_trusted_mrtr_context_and_redacts_unrelated_fields(self):
        for secret in ("credential-sentinel", "resultType", "requestState", "inputRequests", "input_required"):
            state = "opaque " + secret + " \n\x00"
            private = {"resultType": "input_required", "requestState": state,
                       "inputRequests": {secret: {"method": "elicitation/create", "params": URL}}}
            value = {"jsonrpc": "2.0", "id": 1, "result": {**private, "_meta": {"echo": secret}}, "extra": secret}
            with self.assertRaises(McpError):
                _decode_json_message(json.dumps(value).encode(), (secret,))
            with self.assertRaises(McpError):
                _decode_json_message(json.dumps(value).encode(), (secret,), mrtr_response_id=2)
            decoded = _decode_json_message(json.dumps(value).encode(), (secret,), mrtr_response_id=1)
            self.assertEqual(decoded["result"], {**private, "_meta": {"echo": "[redacted]"}})
            self.assertEqual(decoded["extra"], "[redacted]")
        value = {"jsonrpc": "2.0", "id": "peer", "method": "elicitation/create", "params": {"message": "credential-sentinel"}}
        decoded = _decode_json_message(json.dumps(value).encode(), ("credential-sentinel",))
        self.assertEqual(decoded["params"], {"message": "[redacted]"})


    def test_bound_legacy_elicitation_preserves_schema_protocol_fields_under_auth_redaction(self):
        client, fixture = self.live(echo="string")
        client.config = replace(client.config, auth=HttpAuthConfig(credential="fixture"))
        client._credential_provider = _TokenProvider("string")
        ui = PrivateUI('{"name":"private-name"}', "accept")
        result = client.call_tool("fixture", {}, interaction_handler=handler_for(ui))
        self.assertEqual(fixture.replies[0]["result"], {"action": "accept", "content": {"name": "private-name"}})
        self.assertEqual(result["content"][0]["text"], "[redacted]")

    def test_malformed_response_method_mixes_never_prompt_or_bypass_redaction(self):
        secret = "BEARER_SECRET"
        for is_sse in (False, True):
            for method in ("elicitation/create", "notifications/progress"):
                for has_id in (False, True):
                    for result_type in ("complete", "input_required"):
                        with self.subTest(is_sse=is_sse, method=method, has_id=has_id, result_type=result_type):
                            def responder(request):
                                message = {"jsonrpc": "2.0", "method": method,
                                           "params": {**FORM, "message": secret, "progressToken": "octet-mcp:1", "progress": 1},
                                           "result": {"resultType": result_type, "tools": [{**_tool(), "description": secret}]}}
                                if has_id:
                                    message["id"] = request.message()["id"]
                                if is_sse:
                                    return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=_sse_event(message))
                                return _HttpReply(headers={"Content-Type": "application/json"}, body=_json_bytes(message))
                            client = McpStreamableHttpClient(
                                _remote_config("http://127.0.0.1:9/mcp", auth=HttpAuthConfig(credential="fixture")),
                                limits(), credential_provider=_TokenProvider(secret), enable_elicitation=True,
                            )
                            self.clients.append(client)
                            ui = PrivateUI()
                            progress = []
                            with _memory_http(client, responder) as requests:
                                with self.assertRaises(McpError) as raised:
                                    client.call_tool("fixture", {}, interaction_handler=handler_for(ui), progress=progress.append)
                                self.assertNotIn(secret, str(raised.exception))
                                self.assertEqual(len(requests), 1)
                            self.assertEqual(ui.inputs, [])
                            self.assertEqual(progress, [])
                            self.assertNotIn(secret, repr(client.logs.snapshot()))

    def test_http_dispatch_guard_rechecks_after_held_dns_tls_before_any_request_bytes(self):
        client = McpStreamableHttpClient(_remote_config("https://example.invalid/mcp"), limits(shutdown_timeout_ms=100))
        self.clients.append(client)
        prepared = threading.Event()
        release = threading.Event()
        revoked = threading.Event()
        connection = mock.Mock()
        errors = []
        checks = []
        def connect():
            prepared.set()
            release.wait(1)
        connection.connect.side_effect = connect
        def guard():
            checks.append("checked")
            if revoked.is_set():
                raise McpError("stale_binding", "Host-approved binding retired")
        def call():
            try:
                client.call_tool("fixture", {}, dispatch_guard=guard)
            except McpError as error:
                errors.append(error)
        with mock.patch.object(client, "_connection", return_value=connection):
            worker = threading.Thread(target=call)
            worker.start()
            self.assertTrue(prepared.wait(.5))
            self.assertEqual(checks, [])  # not just an early check before DNS/TLS
            revoked.set()
            release.set()
            worker.join(1)
        self.assertEqual([e.code for e in errors], ["stale_binding"])
        self.assertEqual(checks, ["checked"])
        connection.request.assert_not_called()
        connection.getresponse.assert_not_called()
        self.assertEqual(client._next_id, 2)  # allocated one operation, never replayed

    def test_http_live_control_keeps_original_dispatch_guard_after_user_response(self):
        client, fixture = self.live()
        allowed = [True]
        checks = []
        ui = PrivateUI()
        def input_then_revoke(prompt, **kwargs):
            allowed[0] = False
            return "decline"
        ui.request_input = input_then_revoke
        def guard():
            checks.append(allowed[0])
            if not allowed[0]:
                raise McpError("stale_binding", "Host-approved binding retired")
        with self.assertRaises(McpError) as raised:
            client.call_tool("fixture", {}, interaction_handler=handler_for(ui), dispatch_guard=guard)
        self.assertEqual(raised.exception.code, "stale_binding")
        self.assertEqual(checks, [True, False])
        self.assertEqual(fixture.replies, [])
        self.assertEqual(sum(m.get("method") == "tools/call" for m in fixture.messages), 1)

    def test_http_resume_keeps_original_guard_and_revocation_sends_no_get(self):
        def responder(request):
            self.assertEqual(request.method, "POST")
            return _HttpReply(headers={"Content-Type": "text/event-stream"}, body=_sse_event(
                {"jsonrpc": "2.0", "method": "notifications/message"}, event_id="cursor"))
        fixture = _LoopbackFixture(responder)
        self.fixtures.append(fixture)
        client = McpStreamableHttpClient(_remote_config(fixture.url), limits(shutdown_timeout_ms=100))
        self.clients.append(client)
        allowed = [True]
        checks = []
        def guard():
            checks.append(allowed[0])
            if not allowed[0]:
                raise McpError("stale_binding", "Host-approved binding retired")
        with mock.patch.object(client, "_wait_for_resumption", side_effect=lambda *a: allowed.__setitem__(0, False)):
            with self.assertRaises(McpError) as raised:
                client.request("tools/call", {}, timeout_ms=1000, dispatch_guard=guard)
        self.assertEqual(raised.exception.code, "stale_binding")
        self.assertEqual(checks, [True, False])
        self.assertEqual([r.method for r in fixture.requests], ["POST"])
        self.assertEqual(fixture.errors, ())

    def test_http_async_denials_and_cancellation_keep_the_supplied_guard(self):
        client = McpStreamableHttpClient(_remote_config("https://example.invalid/mcp"), limits(shutdown_timeout_ms=100))
        self.clients.append(client)
        checks = []
        connection = mock.Mock()
        def guard():
            checks.append(True)
            raise McpError("stale_binding", "Host-approved binding retired")
        with mock.patch.object(client, "_connection", return_value=connection):
            client._reply_method_not_found("peer", dispatch_guard=guard)
            client._send_cancellation(1, "cancelled", dispatch_guard=guard)
            wait_for(lambda: len(checks) == 2 and not client._operations, timeout=1)
        connection.request.assert_not_called()

    def test_stdio_dispatch_guard_after_writer_lock_wait_sends_zero_bytes(self):
        import os
        from types import SimpleNamespace
        client = McpStdioClient(server_config(), limits())
        read_fd, write_fd = os.pipe()
        os.set_blocking(read_fd, False)
        stream = os.fdopen(write_fd, "wb", buffering=0)
        client._process = SimpleNamespace(stdin=stream)
        client._write_lock.acquire()
        allowed = [True]
        checks = []
        errors = []
        def guard():
            checks.append(allowed[0])
            if not allowed[0]:
                raise McpError("stale_binding", "Host-approved binding retired")
        def call():
            try:
                client.call_tool("fixture", {}, dispatch_guard=guard)
            except McpError as error:
                errors.append(error)
        worker = threading.Thread(target=call)
        worker.start()
        try:
            wait_for(lambda: len(client._pending) == 1)
            self.assertEqual(checks, [])
            allowed[0] = False
            client._write_lock.release()
            worker.join(1)
            self.assertEqual([e.code for e in errors], ["stale_binding"])
            self.assertEqual(checks, [False])
            with self.assertRaises(BlockingIOError):
                os.read(read_fd, 4096)
            self.assertEqual(client._next_id, 2)
            self.assertEqual(client._pending, {})
        finally:
            if client._write_lock.locked():
                client._write_lock.release()
            worker.join(1)
            stream.close()
            os.close(read_fd)

    def test_stdio_eagain_before_first_byte_rechecks_guard_and_never_replays_frame(self):
        client = McpStdioClient(server_config(), limits())
        checks = []
        def guard():
            checks.append(True)
            if len(checks) == 2:
                raise McpError("stale_binding", "Host-approved binding retired")
        with mock.patch("octet_mcp.protocol.os.write", side_effect=BlockingIOError) as write, mock.patch("octet_mcp.protocol.select.select"):
            with self.assertRaises(McpError):
                client._write_nonblocking(123, b"frame\n", time.monotonic() + 1, None, guard)
        self.assertEqual(write.call_count, 1)
        self.assertEqual(len(checks), 2)

    def test_headless_url_is_denied_and_even_a_terminal_echo_is_not_exposed(self):
        client, fixture = self.live(enabled=False, params=URL, echo=URL["url"])
        result = client.call_tool("fixture", {})
        self.assertEqual(fixture.replies[0]["error"]["code"], -32601)
        self.assertNotIn(URL["url"], json.dumps(result))

    def test_credential_refresh_receives_exact_deadline_and_operation_cancellation(self):
        from dataclasses import replace
        from types import SimpleNamespace
        from octet_mcp.config import HttpAuthConfig
        for mode in ("timeout", "cancel", "close"):
            with self.subTest(mode=mode):
                entered = threading.Event()
                finished = threading.Event()
                token = FakeCancellation()
                captures = []
                errors = []
                deadline = time.monotonic() + (.12 if mode == "timeout" else 1)
                def bearer_token(credential, *, server_id, deadline=None, cancel=lambda: False):
                    captures.append((deadline, cancel))
                    entered.set()
                    try:
                        while not cancel() and time.monotonic() < deadline:
                            time.sleep(.005)
                        return "late-private-token"
                    finally:
                        finished.set()
                client = McpStreamableHttpClient(
                    replace(_remote_config("https://example.invalid/mcp"), auth=HttpAuthConfig(credential="fixture")),
                    limits(shutdown_timeout_ms=100), credential_provider=SimpleNamespace(bearer_token=bearer_token),
                )
                self.clients.append(client)
                def call():
                    try:
                        client.request("tools/call", {}, timeout_ms=2000, _deadline=deadline, cancellation=token)
                    except McpError as error:
                        errors.append(error)
                with mock.patch.object(client, "_connection") as connection:
                    worker = threading.Thread(target=call)
                    started = time.monotonic()
                    worker.start()
                    self.assertTrue(entered.wait(.5))
                    self.assertEqual(captures[0][0], deadline)
                    if mode == "cancel":
                        token.cancel()
                    elif mode == "close":
                        client.close()
                    worker.join(.5)
                    self.assertTrue(finished.wait(.2))
                    wait_for(lambda: not client._operations, timeout=.5)
                    connection.assert_not_called()  # no DNS/TLS or MCP bytes after a late token
                self.assertFalse(worker.is_alive())
                self.assertLess(time.monotonic() - started, .5)
                self.assertEqual(len(errors), 1)
                self.assertNotIn("late-private-token", str(errors[0]))
                if mode == "timeout":
                    self.assertEqual(errors[0].code, "request_timeout")
                elif mode == "cancel":
                    self.assertEqual(errors[0].code, "request_cancelled")
                if mode != "timeout":
                    self.assertTrue(captures[0][1]())

    def test_worker_observing_cancel_does_not_skip_one_bounded_notification(self):
        client = McpStreamableHttpClient(_remote_config("http://127.0.0.1:9/mcp"), limits(shutdown_timeout_ms=100))
        self.clients.append(client)
        token = FakeCancellation()
        operation = _HttpOperation()
        operation.request_sent = True
        operation.error = McpCancelled("request_cancelled", "Cancelled in the transport worker")
        def settled_during_wait(timeout):
            token.cancel()
            return True
        operation.done = mock.Mock()
        operation.done.wait.side_effect = settled_during_wait
        with mock.patch.object(client, "_send_cancellation") as notify:
            with self.assertRaises(McpCancelled):
                client._await(operation, time.monotonic() + 1, cancellation=token, cancellation_request=(77, "tools/call"))
        notify.assert_called_once_with(77, "test")
