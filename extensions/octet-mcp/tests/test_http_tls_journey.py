"""Real local TLS handshakes; no public DNS/provider qualification is implied."""

from __future__ import annotations

from http.server import ThreadingHTTPServer
from pathlib import Path
import shutil
import socket
import ssl
import subprocess
import tempfile
import threading
import unittest
from unittest import mock

from octet_mcp import streamable_http as http
from octet_mcp.protocol import McpError

from .helpers import limits
from .test_streamable_http import (
    _HttpReply, _LoopbackFixture, _LoopbackHandler, _initialize_result,
    _json_result, _remote_config, _tool,
)


HOST = "mcp-tls.example.invalid"


class _TlsFixture(_LoopbackFixture):
    def __init__(self, context):
        self.responder = self.respond
        self._lock = threading.Lock()
        self._requests = []
        self._errors = []
        self.server_names = []
        context.set_servername_callback(lambda connection, name, context: self.server_names.append(name))
        self._server = ThreadingHTTPServer(("127.0.0.1", 0), _LoopbackHandler)
        self._server.daemon_threads = True
        self._server.fixture = self
        self._server.socket = context.wrap_socket(self._server.socket, server_side=True)
        self._thread = threading.Thread(target=self._server.serve_forever, daemon=True)
        self._thread.start()

    def respond(self, request):
        message = request.message()
        if message["method"] == "initialize":
            return _json_result(request, _initialize_result("local-tls-fixture"))
        if message["method"] == "notifications/initialized":
            return _HttpReply(status=202)
        if message["method"] == "tools/list":
            return _json_result(request, {"tools": [_tool()]})
        return _json_result(request, {"content": [{"type": "text", "text": "verified local TLS"}]})


class TlsJourneyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        openssl = shutil.which("openssl")
        if openssl is None:
            raise unittest.SkipTest("openssl is required to generate the ephemeral local TLS fixture")
        temporary = tempfile.TemporaryDirectory()
        cls.addClassCleanup(temporary.cleanup)
        root = Path(temporary.name)
        cls.certificate = root / "fixture.crt"
        cls.key = root / "fixture.key"
        # A throwaway self-signed fixture, never a provider/user credential.
        config = root / "openssl.cnf"
        config.write_text(
            f"[req]\nprompt=no\ndistinguished_name=dn\nx509_extensions=ext\n"
            f"[dn]\nCN={HOST}\n[ext]\nsubjectAltName=DNS:{HOST}\n"
            "basicConstraints=critical,CA:TRUE\n"
        )
        subprocess.run([
            openssl, "req", "-x509", "-newkey", "rsa:2048", "-nodes", "-days", "1",
            "-config", str(config), "-keyout", str(cls.key), "-out", str(cls.certificate),
        ], check=True, timeout=15, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)

    def fixture(self):
        context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
        context.load_cert_chain(self.certificate, self.key)
        fixture = _TlsFixture(context)
        self.addCleanup(fixture.close)
        return fixture

    def connect(self, fixture, hostname, trust):
        port = fixture._server.server_port
        client = http.McpStreamableHttpClient(
            _remote_config(f"https://{hostname}:{port}/mcp"), limits(shutdown_timeout_ms=100),
        )
        self.addCleanup(client.close)
        # Network policy has separate negative regressions. Inject only the
        # reviewed numeric route here so the real TLS layer can be tested
        # offline against a certificate for an original non-IP hostname.
        patches = (
            mock.patch.object(http, "_resolve_addresses", return_value=[
                (socket.AF_INET, ("127.0.0.1", port)),
            ]),
            mock.patch.object(http.ssl, "create_default_context", return_value=trust),
            mock.patch.object(socket, "getaddrinfo", side_effect=AssertionError(
                "pinned connection must not resolve its original hostname again"
            )),
        )
        for patch in patches:
            patch.start()
            self.addCleanup(patch.stop)
        return client

    def test_numeric_connection_preserves_real_tls_sni_verification_and_host_header(self):
        trust = ssl.create_default_context(cafile=str(self.certificate))
        self.assertTrue(trust.check_hostname)
        self.assertEqual(trust.verify_mode, ssl.CERT_REQUIRED)
        fixture = self.fixture()
        client = self.connect(fixture, HOST, trust)
        client.start()
        self.assertEqual(client.list_tools()[0]["name"], "echo")
        result = client.call_tool("echo", {"value": "tls"})
        self.assertEqual(result["content"][0]["text"], "verified local TLS")
        self.assertEqual(fixture.server_names, [HOST] * 4)
        self.assertTrue(all(request.header("host") == f"{HOST}:{fixture._server.server_port}"
                            for request in fixture.requests))
        self.assertFalse(fixture.errors)

    def test_wrong_hostname_fails_before_sending_mcp_request(self):
        trust = ssl.create_default_context(cafile=str(self.certificate))
        fixture = self.fixture()
        client = self.connect(fixture, "wrong.example.invalid", trust)
        with self.assertRaises(McpError):
            client.start()
        self.assertEqual(fixture.server_names, ["wrong.example.invalid"])
        self.assertEqual(fixture.requests, ())

    def test_untrusted_certificate_fails_before_sending_mcp_request(self):
        trust = ssl.create_default_context()
        fixture = self.fixture()
        client = self.connect(fixture, HOST, trust)
        with self.assertRaises(McpError):
            client.start()
        self.assertEqual(fixture.requests, ())
