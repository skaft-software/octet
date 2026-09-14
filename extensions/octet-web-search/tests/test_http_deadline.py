#!/usr/bin/env python3
"""Local transport regressions: quiet sockets, partial frames, and cancellation."""

from __future__ import annotations

from contextlib import contextmanager
import errno
from pathlib import Path
import socket
import ssl
import sys
import threading
import time
import unittest
from unittest import mock


ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT))

from provider import (  # noqa: E402
    Deadline, HttpClient, Offline, RequestTimedOut,
    _connect_socket, _DeadlineSocket,
)
from test_provider import Cancellation, FakeCancelled, FixtureResolver  # noqa: E402


BODY = b"complete response"
HEADERS = (
    b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n"
    b"Content-Length: 17\r\nConnection: close\r\n\r\n"
)
DRIP_BODY = BODY * 10
DRIP_HEADERS = HEADERS.replace(b"Content-Length: 17", b"Content-Length: 170")


class HttpDeadlineTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        certificate = str(ROOT / "tests" / "fixtures" / "tls-localhost.pem")
        cls.server_context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        cls.server_context.load_cert_chain(certificate)
        cls.client_context = ssl.create_default_context(cafile=certificate)

    @contextmanager
    def peer(
        self, prefix=b"", suffix=HEADERS + BODY, *, tls=False,
        pause_handshake=False, drip=False, hostname="provider.test", context=None,
    ):
        """One local connection, gated at a known I/O phase and joined on exit."""
        paused = threading.Event()
        release = threading.Event()
        requests = []
        listener = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        listener.bind(("127.0.0.1", 0))
        listener.listen(1)
        listener.settimeout(3)
        client = HttpClient(
            resolver=FixtureResolver(listener.getsockname()[1]),
            ssl_context=context or self.client_context,
        )

        def serve():
            connection, _ = listener.accept()
            try:
                connection.settimeout(3)
                if tls:
                    if pause_handshake:
                        paused.set()
                        release.wait(5)
                    connection = self.server_context.wrap_socket(connection, server_side=True)
                request = b""
                while b"\r\n\r\n" not in request:
                    chunk = connection.recv(4096)
                    if not chunk:
                        return
                    request += chunk
                requests.append(request)
                connection.sendall(prefix)
                paused.set()
                if drip:
                    for byte in suffix:
                        if release.wait(0.03):
                            return
                        connection.sendall(bytes([byte]))
                else:
                    release.wait(5)
                    connection.sendall(suffix)
            except OSError:
                # Timeout/cancellation/certificate rejection closes the client.
                pass
            finally:
                connection.close()

        def fetch(deadline):
            scheme = "https" if tls else "http"
            return client.fetch(
                "%s://%s/page" % (scheme, hostname), deadline=deadline,
                max_bytes=32768, max_redirects=3,
            )

        worker = threading.Thread(target=serve, daemon=True)
        worker.start()
        try:
            yield fetch, paused, release, requests
        finally:
            release.set()
            worker.join(timeout=3.5)
            listener.close()
            self.assertFalse(worker.is_alive(), "fixture connection did not settle")

    def delayed_fetch(self, fetch, release):
        timer = threading.Timer(1.1, release.set)
        timer.start()
        try:
            return fetch(Deadline(8))
        finally:
            timer.cancel()
            timer.join(timeout=2)

    def test_slow_tls_handshake_headers_and_body_succeed(self):
        for phase, prefix, suffix in (
            ("handshake", b"", HEADERS + BODY),
            ("headers", b"", HEADERS + BODY),
            ("body", HEADERS + BODY[:5], BODY[5:]),
        ):
            with self.subTest(phase=phase), self.peer(
                prefix, suffix, tls=True, pause_handshake=(phase == "handshake"),
            ) as (fetch, _, release, requests):
                result = self.delayed_fetch(fetch, release)
                self.assertEqual(result.body, BODY)
                self.assertEqual(len(requests), 1)
                self.assertIn(b"Host: provider.test\r\n", requests[0])

    def test_partial_chunked_and_eof_delimited_responses_survive_quiet_intervals(self):
        chunked = (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n"
            b"Connection: close\r\n\r\n"
        )
        for prefix, suffix in (
            (chunked + b"11", b"\r\n" + BODY + b"\r\n0\r\n\r\n"),
            (chunked + b"11\r\n" + BODY[:5], BODY[5:] + b"\r\n0\r\n\r\n"),
            (b"HTTP/1.0 200 OK\r\n\r\n" + BODY[:5], BODY[5:]),
        ):
            with self.subTest(prefix=prefix), self.peer(prefix, suffix) as (
                fetch, _, release, requests,
            ):
                self.assertEqual(self.delayed_fetch(fetch, release).body, BODY)
                self.assertEqual(len(requests), 1)

    def test_stalled_and_dripping_responses_obey_overall_deadline(self):
        for tls, handshake, prefix, suffix, drip in (
            (False, False, b"", HEADERS + BODY, False),
            (False, False, b"", HEADERS + BODY, True),
            (False, False, HEADERS, BODY, False),
            (False, False, DRIP_HEADERS, DRIP_BODY, True),
            (True, True, b"", HEADERS + BODY, False),
            (True, False, HEADERS, BODY, False),
        ):
            with self.subTest(tls=tls, handshake=handshake, prefix=prefix, drip=drip):
                with self.peer(
                    prefix, suffix, tls=tls, pause_handshake=handshake, drip=drip,
                ) as (fetch, _, _, _):
                    started = time.monotonic()
                    with self.assertRaises(RequestTimedOut):
                        fetch(Deadline(0.2))
                    self.assertLess(time.monotonic() - started, 1.0)

    def test_cancellation_interrupts_stalled_and_dripping_io_with_a_long_budget(self):
        for tls, handshake, prefix, suffix, drip in (
            (False, False, b"", HEADERS + BODY, False),
            (False, False, HEADERS[:9], HEADERS[9:] + BODY, True),
            (False, False, HEADERS, BODY, False),
            (False, False, DRIP_HEADERS, DRIP_BODY, True),
            (True, True, b"", HEADERS + BODY, False),
            (True, False, b"", HEADERS + BODY, False),
            (True, False, HEADERS, BODY, False),
        ):
            with self.subTest(tls=tls, handshake=handshake, prefix=prefix, drip=drip):
                with self.peer(
                    prefix, suffix, tls=tls, pause_handshake=handshake, drip=drip,
                ) as (fetch, paused, release, _):
                    cancellation = Cancellation()
                    observed = []

                    def run():
                        try:
                            fetch(Deadline(20, cancellation))
                        except BaseException as error:
                            observed.append(error)

                    worker = threading.Thread(target=run, daemon=True)
                    worker.start()
                    try:
                        self.assertTrue(paused.wait(2), "request did not reach the I/O wait")
                        cancellation.cancelled.set()
                        worker.join(timeout=1.5)
                        self.assertFalse(worker.is_alive(), "cancellation exceeded its grace")
                        self.assertEqual(len(observed), 1)
                        self.assertIsInstance(observed[0], FakeCancelled)
                    finally:
                        cancellation.cancelled.set()
                        release.set()
                        worker.join(timeout=3)

    def test_tls_certificate_and_hostname_verification_remain_required(self):
        for hostname, context in (
            ("provider.test", ssl.create_default_context()),
            ("other.test", self.client_context),
        ):
            with self.subTest(hostname=hostname), self.peer(
                tls=True, hostname=hostname, context=context,
            ) as (fetch, _, release, _):
                release.set()
                with self.assertRaises(Offline) as raised:
                    fetch(Deadline(2))
                self.assertIsInstance(raised.exception.__cause__, ssl.SSLCertVerificationError)

    def test_connect_polls_without_reconnecting_and_closes_on_failure(self):
        resolved = FixtureResolver(1234)("provider.test", 80, Deadline(8))[0]
        for failure in (None, OSError(errno.ECONNREFUSED, "fixture refused"), FakeCancelled()):
            with self.subTest(failure=failure):
                sock = mock.Mock()
                sock.connect_ex.return_value = errno.EINPROGRESS
                sock.getsockopt.return_value = 0
                with mock.patch("provider.socket.socket", return_value=sock), mock.patch(
                    "provider.select.select",
                    side_effect=[([], [], []), failure or ([], [sock], [])],
                ) as select_socket:
                    if failure is None:
                        self.assertIs(_connect_socket(resolved, Deadline(8)), sock)
                        sock.close.assert_not_called()
                    else:
                        with self.assertRaises(type(failure)):
                            _connect_socket(resolved, Deadline(8))
                        sock.close.assert_called_once()
                    self.assertEqual(select_socket.call_count, 2)
                    sock.setblocking.assert_called_once_with(False)
                    sock.connect_ex.assert_called_once_with(resolved.sockaddr)

    def test_refused_connection_remains_an_offline_error(self):
        # Inject refusal: a non-listening loopback port can silently drop SYNs.
        client = HttpClient(resolver=FixtureResolver(1234))
        for initial_error in (errno.ECONNREFUSED, errno.EINPROGRESS):
            with self.subTest(initial_error=initial_error):
                sock = mock.Mock()
                sock.connect_ex.return_value = initial_error
                sock.getsockopt.return_value = errno.ECONNREFUSED
                with mock.patch("provider.socket.socket", return_value=sock), mock.patch(
                    "provider.select.select", return_value=([], [sock], []),
                ):
                    with self.assertRaises(Offline) as raised:
                        client.fetch(
                            "http://provider.test/page", deadline=Deadline(2),
                            max_bytes=1024, max_redirects=0,
                        )
                    self.assertIsInstance(raised.exception.__cause__, ConnectionRefusedError)
                    sock.close.assert_called_once()

    def test_partial_writes_preserve_bytes_and_poll_tls_readiness(self):
        sock = mock.Mock()
        sock.send.side_effect = [
            BlockingIOError(), ssl.SSLWantWriteError(), ssl.SSLWantReadError(), 2, 3,
        ]
        deadline = mock.Mock(spec=Deadline)
        _DeadlineSocket(sock, deadline).sendall(b"hello")
        self.assertEqual(
            [bytes(call.args[0]) for call in sock.send.call_args_list],
            [b"hello", b"hello", b"hello", b"hello", b"llo"],
        )
        self.assertEqual(deadline.wait_for_socket.call_args_list, [
            mock.call(sock, writing=True), mock.call(sock, writing=True),
            mock.call(sock, writing=False),
        ])

    def test_response_reader_retains_socket_until_it_is_closed(self):
        client, peer = socket.socketpair()
        self.addCleanup(client.close)
        self.addCleanup(peer.close)
        client.setblocking(False)
        transport = _DeadlineSocket(client, Deadline(2))
        with transport.makefile("rb") as reader:
            transport.close()
            peer.sendall(BODY)
            peer.close()
            self.assertEqual(reader.read(), BODY)
        self.assertEqual(client.fileno(), -1)


if __name__ == "__main__":
    unittest.main()
