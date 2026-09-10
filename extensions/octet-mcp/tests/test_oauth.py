from __future__ import annotations

import base64
from dataclasses import replace
import hashlib
import json
import io
import socket
import threading
import time
import unittest
from unittest import mock
from urllib.parse import parse_qs, urlencode, urlsplit

from octet_mcp.auth import AuthError
from octet_mcp.oauth import (AuthChallenge, AuthorizationAttempt, discover,
                             issuer_metadata_urls, parse_challenge,
                             resource_metadata_urls, token_response)
from octet_mcp.oauth_callback import LoopbackCallback
from octet_mcp.oauth_http import OAuthHttp, OAuthHttpResponse
from octet_mcp.oauth_transport import PinnedOAuthExchange

from .test_auth import binding, server


class FakeAuthExchange:
    """Only in-memory public-URL fixtures; no DNS or live credential access."""
    def __init__(self):
        self.config = server("oauth")
        self.calls = []
        self.protected = {"resource": self.config.url,
                          "authorization_servers": [self.config.auth.issuer],
                          "scopes_supported": ["read"]}
        self.metadata = {"issuer": self.config.auth.issuer,
                         "authorization_endpoint": "https://auth.example.test/authorize",
                         "token_endpoint": "https://auth.example.test/token",
                         "code_challenge_methods_supported": ["S256"],
                         "response_types_supported": ["code"],
                         "token_endpoint_auth_methods_supported": ["none"],
                         "authorization_response_iss_parameter_supported": True}
        self.token = {"access_token": "access.secret", "refresh_token": "refresh.secret",
                      "token_type": "Bearer", "expires_in": 3600, "scope": "read"}
        self.overrides = {}
        self.post_hook = None

    def __call__(self, request):
        self.calls.append(request)
        if request.method == "POST":
            if self.post_hook:
                self.post_hook(request)
            payload = self.token
        elif request.url in resource_metadata_urls(self.config.url):
            payload = self.protected
        elif request.url in issuer_metadata_urls(self.config.auth.issuer):
            payload = self.metadata
        else:
            raise AssertionError("unconfigured fixture URL")
        if request.url in self.overrides:
            value = self.overrides[request.url]
            if isinstance(value, Exception):
                raise value
            if isinstance(value, OAuthHttpResponse):
                return value
            payload = value
        return OAuthHttpResponse(request.url, 200, (("Content-Type", "application/json"),),
                                 json.dumps(payload).encode())

    def operation(self, **kwargs):
        return OAuthHttp(self).operation(deadline=kwargs.get("deadline", time.monotonic() + 5),
                                          cancel=kwargs.get("cancel", lambda: False))


def attempt_for(fake=None):
    fake = fake or FakeAuthExchange()
    bound = binding(fake.config)
    return AuthorizationAttempt(bound, discover(bound, fake.operation()),
                                "http://127.0.0.1:12345/oauth/callback", time.monotonic() + 5)


def callback_for(attempt, **overrides):
    fields = {"code": "code.secret", "state": attempt._state, "iss": attempt.metadata.issuer}
    fields.update(overrides)
    return attempt.redirect_uri + "?" + urlencode({k: v for k, v in fields.items() if v is not None})


class OAuthDiscoveryTests(unittest.TestCase):
    def test_ordered_resource_and_rfc8414_oidc_fallbacks(self):
        fake = FakeAuthExchange()
        expected = resource_metadata_urls(fake.config.url) + issuer_metadata_urls(fake.config.auth.issuer)
        for url in expected[:-1]:
            fake.overrides[url] = OAuthHttpResponse(url, 404)
        # Last resource metadata URL must succeed before AS discovery.
        del fake.overrides[resource_metadata_urls(fake.config.url)[-1]]
        metadata = discover(binding(fake.config), fake.operation())
        self.assertEqual(metadata.issuer, fake.config.auth.issuer)
        self.assertEqual([r.url for r in fake.calls], list(expected))
        self.assertEqual(issuer_metadata_urls("https://example.test"), (
            "https://example.test/.well-known/oauth-authorization-server",
            "https://example.test/.well-known/openid-configuration"))

    def test_source_pinned_resource_issuer_and_public_endpoints(self):
        cases = [("protected", "resource", "https://other.test/mcp"),
                 ("protected", "authorization_servers", ["https://other.test"]),
                 ("metadata", "issuer", "https://auth.example.test/tenant/"),
                 ("metadata", "issuer", "https://AUTH.example.test/tenant"),
                 ("metadata", "code_challenge_methods_supported", ["plain"]),
                 ("metadata", "code_challenge_methods_supported", None),
                 ("metadata", "authorization_endpoint", "http://auth.example.test/authorize"),
                 ("metadata", "token_endpoint", "https://127.0.0.1/token"),
                 ("metadata", "token_endpoint", "https://auth.example.test/token?secret=no"),
                 ("metadata", "token_endpoint_auth_methods_supported", ["client_secret_basic"])]
        for target, field, value in cases:
            fake = FakeAuthExchange()
            getattr(fake, target)[field] = value
            with self.subTest(field=field, value=value), self.assertRaises(AuthError):
                discover(binding(fake.config), fake.operation())
            self.assertFalse(any(r.method == "POST" for r in fake.calls))

    def test_challenge_parser_quotes_multiple_schemes_and_source_binding(self):
        fake = FakeAuthExchange()
        url = resource_metadata_urls(fake.config.url)[0]
        challenge = parse_challenge(fake.config.url, [
            'Basic realm="a,b", Bearer resource_metadata="' + url + '", scope="write", '
            'error_description="RAW TOKEN, do not display", Newauth realm="ignored"'])
        self.assertEqual(challenge.scopes, ("write",))
        self.assertNotIn("RAW TOKEN", repr(challenge))
        metadata = discover(binding(fake.config), fake.operation(), challenge, ("read",))
        self.assertEqual(metadata.scopes, ("read", "write"))
        self.assertEqual(fake.calls[0].url, url)
        with self.assertRaises(AuthError):
            discover(binding(fake.config), fake.operation(), replace(challenge, resource="https://other.test"))
        for header in ('Bearer scope="a", scope="b"', 'Bearer scope="a", Bearer scope="b"',
                       'Bearer scope="unclosed', 'Bearer resource_metadata="http://private.test"',
                       'Bearer scope="a\nb"'):
            with self.subTest(header=header), self.assertRaises(AuthError):
                parse_challenge(fake.config.url, [header])

    def test_scope_challenge_authoritative_but_cannot_widen_user_config_ceiling(self):
        fake = FakeAuthExchange()
        auth = replace(fake.config.auth, scopes=("read",))
        bound = binding(replace(fake.config, auth=auth))
        with self.assertRaisesRegex(AuthError, "scope"):
            discover(bound, fake.operation(), AuthChallenge(fake.config.url, scopes=("admin",)))
        fake.protected.pop("scopes_supported")
        self.assertEqual(discover(binding(fake.config), fake.operation()).scopes, ())

    def test_metadata_redirect_wrong_source_duplicate_json_and_http_budgets(self):
        for response in (
                OAuthHttpResponse("https://other.test/metadata", 200, body=b"{}"),
                OAuthHttpResponse(server("oauth").url, 302),
                OAuthHttpResponse(resource_metadata_urls(server("oauth").url)[0], 200,
                                  (("Content-Type", "application/json"),), b'{"issuer":1,"issuer":2}'),
                OAuthHttpResponse(resource_metadata_urls(server("oauth").url)[0], 200,
                                  (("Content-Type", "application/json"),), b"x" * 65537)):
            fake = FakeAuthExchange()
            fake.overrides[resource_metadata_urls(fake.config.url)[0]] = response
            with self.assertRaises(AuthError):
                discover(binding(fake.config), fake.operation())
            self.assertEqual(len(fake.calls), 1)
        fake = FakeAuthExchange()
        op = fake.operation()
        for _ in range(8):
            op.document(resource_metadata_urls(fake.config.url)[0])
        with self.assertRaises(AuthError):
            op.document(resource_metadata_urls(fake.config.url)[0])
        fake = FakeAuthExchange()
        with self.assertRaisesRegex(AuthError, "cancelled"):
            discover(binding(fake.config), fake.operation(cancel=lambda: True))
        self.assertFalse(fake.calls)
        with self.assertRaisesRegex(AuthError, "deadline"):
            discover(binding(fake.config), fake.operation(deadline=time.monotonic() - 1))
        self.assertFalse(fake.calls)


class OAuthAttemptTests(unittest.TestCase):
    def test_s256_state_issuer_and_resource_on_both_requests_and_no_code_replay(self):
        fake = FakeAuthExchange()
        attempt = attempt_for(fake)
        url = attempt.authorization_url()
        params = parse_qs(urlsplit(url).query)
        self.assertEqual(params["resource"], [fake.config.url])
        self.assertEqual(params["code_challenge_method"], ["S256"])
        self.assertGreaterEqual(len(attempt._verifier), 43)
        self.assertLessEqual(len(attempt._verifier), 128)
        challenge = base64.urlsafe_b64encode(hashlib.sha256(attempt._verifier.encode()).digest()).rstrip(b"=").decode()
        self.assertEqual(params["code_challenge"], [challenge])
        self.assertNotIn(attempt._verifier, url)
        self.assertNotIn(attempt._state, repr(attempt))
        code = attempt.consume_callback(callback_for(attempt), cancel=lambda: False)
        result = attempt.exchange(code, fake.operation(), now=1000)
        self.assertEqual(result.access_token, "access.secret")
        post = parse_qs(fake.calls[-1].body.decode())
        self.assertEqual(post["resource"], [fake.config.url])
        self.assertEqual(post["redirect_uri"], [attempt.redirect_uri])
        self.assertEqual(post["code_verifier"], [attempt._verifier])
        self.assertNotIn("client_secret", post)
        with self.assertRaises(AuthError):
            attempt.consume_callback(callback_for(attempt), cancel=lambda: False)
        with self.assertRaises(AuthError):
            attempt.exchange(code, fake.operation(), now=1000)
        self.assertEqual(len([r for r in fake.calls if r.method == "POST"]), 1)

    def test_wrong_and_replayed_state_issuer_validation_precedes_error(self):
        attempt = attempt_for()
        with self.assertRaises(AuthError):
            attempt.consume_callback(callback_for(attempt, state="wrong"), cancel=lambda: False)
        self.assertFalse(attempt._used)
        with self.assertRaises(AuthError) as caught:
            attempt.consume_callback(callback_for(attempt, iss="https://evil.test", error="RAW SECRET"),
                                     cancel=lambda: False)
        self.assertEqual(caught.exception.code, "authentication_callback")
        self.assertNotIn("RAW SECRET", str(caught.exception))
        with self.assertRaises(AuthError):
            attempt.consume_callback(callback_for(attempt), cancel=lambda: False)

    def test_rfc9207_advertisement_matrix_and_exact_issuer_comparison(self):
        for advertised in (True, False, None):
            for issuer in (None, "https://auth.example.test/tenant", "https://AUTH.example.test/tenant",
                           "https://auth.example.test:443/tenant", "https://auth.example.test/tenant/"):
                fake = FakeAuthExchange()
                if advertised is None:
                    del fake.metadata["authorization_response_iss_parameter_supported"]
                else:
                    fake.metadata["authorization_response_iss_parameter_supported"] = advertised
                attempt = attempt_for(fake)
                accepted = issuer == attempt.metadata.issuer or (issuer is None and advertised is not True)
                with self.subTest(advertised=advertised, issuer=issuer):
                    if accepted:
                        self.assertEqual(attempt.consume_callback(callback_for(attempt, iss=issuer),
                                                                  cancel=lambda: False), "code.secret")
                    else:
                        with self.assertRaises(AuthError):
                            attempt.consume_callback(callback_for(attempt, iss=issuer), cancel=lambda: False)

    def test_callback_redirect_duplicate_fields_expiry_and_cancellation(self):
        for transform in (lambda u: u.replace("127.0.0.1", "localhost"),
                          lambda u: u + "&state=duplicate", lambda u: u + "#fragment",
                          lambda u: u.replace("code.secret", "%GG")):
            attempt = attempt_for()
            with self.assertRaises(AuthError):
                attempt.consume_callback(transform(callback_for(attempt)), cancel=lambda: False)
        attempt = attempt_for()
        with self.assertRaisesRegex(AuthError, "cancelled"):
            attempt.consume_callback(callback_for(attempt), cancel=lambda: True)
        attempt.deadline = time.monotonic() - 1
        with self.assertRaisesRegex(AuthError, "deadline"):
            attempt.consume_callback(callback_for(attempt), cancel=lambda: False)

    def test_token_validation_and_refresh_rotation(self):
        fake = FakeAuthExchange()
        for changes in ({"token_type": "MAC"}, {"access_token": "secret\nInjected"},
                        {"expires_in": True}, {"expires_in": 0}, {"scope": "admin"},
                        {"refresh_token": "old-secret"}, {"refresh_token": None}):
            with self.subTest(changes=changes), self.assertRaises(AuthError):
                token_response({**fake.token, **changes}, endpoint=fake.metadata["token_endpoint"],
                               scopes=("read",), now=1000, previous_refresh="old-secret")


class LoopbackTests(unittest.TestCase):
    def callback(self):
        listener = LoopbackCallback()
        self.addCleanup(listener.close)
        attempt = attempt_for()
        attempt.redirect_uri = listener.redirect_uri
        listener.start(attempt, cancel=lambda: False)
        return listener, attempt

    def send(self, listener, target, host=None, method="GET", extra=""):
        parts = urlsplit(listener.redirect_uri)
        with socket.create_connection(("127.0.0.1", parts.port), timeout=1) as sock:
            sock.sendall((f"{method} {target} HTTP/1.1\r\nHost: {host or parts.netloc}\r\n{extra}\r\n").encode())
            return sock.recv(4096)

    def test_bounded_loopback_rejects_wrong_host_origin_and_state_then_accepts_once(self):
        listener, attempt = self.callback()
        good = "/oauth/callback?" + urlsplit(callback_for(attempt)).query
        for kwargs in ({"host": "evil.test"}, {"extra": "Origin: https://evil.test\r\n"}, {"method": "POST"}):
            self.assertIn(b"400 Bad Request", self.send(listener, good, **kwargs))
        self.assertIn(b"400 Bad Request", self.send(listener, good.replace(attempt._state, "wrong")))
        response = self.send(listener, good)
        self.assertIn(b"200 OK", response)
        self.assertIn(b"no-store", response)
        self.assertNotIn(attempt._state.encode(), response)
        self.assertNotIn(b"code.secret", response)
        self.assertTrue(listener.done.wait(1))
        self.assertEqual(listener.code, "code.secret")
        with self.assertRaises(OSError):
            self.send(listener, good)

    def test_callback_cancellation_and_timeout_close_and_join_worker(self):
        listener, attempt = self.callback()
        listener.close()
        self.assertTrue(listener.done.wait(0.5))
        self.assertFalse(listener._thread.is_alive())
        self.assertEqual(listener.error_code, "authentication_cancelled")
        listener = LoopbackCallback()
        self.addCleanup(listener.close)
        attempt = attempt_for()
        attempt.redirect_uri = listener.redirect_uri
        attempt.deadline = time.monotonic() + 0.05
        listener.start(attempt, cancel=lambda: False)
        self.assertTrue(listener.done.wait(0.5))
        self.assertEqual(listener.error_code, "authentication_timeout")


class PinnedOAuthTests(unittest.TestCase):
    class Socket:
        def __init__(self, raw, *, blocked=False):
            self.raw = raw
            self.blocked = blocked
            self.closed = threading.Event()
            self.sent = []
            self.address = None
            self.handshakes = 0

        def settimeout(self, timeout):
            self.timeout = timeout

        def connect(self, address):
            self.address = address

        def sendall(self, data):
            self.sent.append(data)

        def do_handshake(self):
            self.handshakes += 1

        def makefile(self, *args):
            owner = self
            class Stream(io.BytesIO):
                def readline(self, *args):
                    if owner.blocked:
                        if not owner.closed.wait(1):
                            raise AssertionError("OAuth read escaped cancellation/deadline")
                        return b""
                    return super().readline(*args)
            return Stream(self.raw)

        def shutdown(self, *args):
            self.closed.set()

        def close(self):
            self.closed.set()

    def exchange(self, raw, *, deadline=None, cancel=lambda: False, blocked=False):
        sock = self.Socket(raw, blocked=blocked)
        wrapped = self.Socket(raw, blocked=blocked)
        wrapped.closed = sock.closed
        tls = mock.Mock()
        tls.wrap_socket.return_value = wrapped
        self.sock, self.raw_socket, self.tls = wrapped, sock, tls
        with mock.patch("octet_mcp.streamable_http._resolve_dns", return_value=[(socket.AF_INET, "93.184.215.14")]) as dns, \
             mock.patch("octet_mcp.streamable_http.socket.socket", return_value=sock), \
             mock.patch("octet_mcp.streamable_http.ssl.create_default_context", return_value=tls) as context_factory:
            result = OAuthHttp(PinnedOAuthExchange()).operation(
                deadline=deadline or time.monotonic() + 2, cancel=cancel).document("https://auth.example.test/token")
        self.assertEqual(dns.call_count, 1)
        context_factory.assert_called_once_with()
        return result

    def test_production_composition_pins_dns_uses_original_tls_host_and_closes(self):
        result = self.exchange(b'HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\n\r\n{}')
        self.assertEqual(result, {})
        self.assertEqual(self.raw_socket.address, ("93.184.215.14", 443))
        self.tls.wrap_socket.assert_called_once_with(self.raw_socket, server_hostname="auth.example.test",
                                                    do_handshake_on_connect=False)
        self.assertEqual(self.sock.handshakes, 1)
        self.assertTrue(self.sock.closed.is_set())
        sent = b"".join(self.sock.sent)
        self.assertIn(b"Host: auth.example.test", sent)
        self.assertNotIn(b"Authorization:", sent)
        self.assertNotIn(b"Cookie:", sent)

    def test_every_dns_answer_is_checked_before_connect_and_private_literals_are_denied(self):
        for address in ("127.0.0.1", "10.0.0.1", "169.254.169.254", "0.0.0.0"):
            with mock.patch("octet_mcp.streamable_http._resolve_dns",
                            return_value=[(socket.AF_INET, "93.184.215.14"), (socket.AF_INET, address)]), \
                 mock.patch("octet_mcp.oauth_transport._PinnedConnection") as connection:
                with self.assertRaises(AuthError):
                    OAuthHttp(PinnedOAuthExchange()).operation(deadline=time.monotonic() + 1,
                        cancel=lambda: False).document("https://auth.example.test/metadata")
                connection.assert_not_called()
        for url in ("https://127.0.0.1/token", "http://127.0.0.1/token", "https://localhost/token"):
            with mock.patch("octet_mcp.oauth_transport._resolve_addresses") as resolve:
                with self.assertRaises(AuthError):
                    OAuthHttp(PinnedOAuthExchange()).operation(deadline=time.monotonic() + 1,
                        cancel=lambda: False).document(url)
                resolve.assert_not_called()

    def test_redirect_truncation_duplicate_lengths_and_header_byte_limit_are_rejected(self):
        replies = [b"HTTP/1.1 302 Found\r\nLocation: https://127.0.0.1/token\r\nContent-Length: 0\r\n\r\n",
                   b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 3\r\n\r\n{}",
                   b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 2\r\nContent-Length: 2\r\n\r\n{}",
                   b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nTransfer-Encoding: chunked\r\n\r\n2\r\n{}\r\n0\r\n",
                   b"HTTP/1.1 200 OK\r\nX-Long: " + b"s" * 16384 + b"\r\n\r\n"]
        for raw in replies:
            with self.subTest(size=len(raw)), self.assertRaises(AuthError):
                self.exchange(raw)
            self.assertTrue(self.sock.closed.is_set())

    def test_deadline_and_cancellation_abort_blocked_headers_and_join_watcher(self):
        for cancellation in (False, True):
            stop = threading.Event()
            timer = threading.Timer(0.05, stop.set)
            if cancellation:
                timer.start()
            started = time.monotonic()
            try:
                with self.assertRaises(AuthError) as caught:
                    self.exchange(b"", deadline=started + (2 if cancellation else 0.05),
                                  cancel=stop.is_set, blocked=True)
                self.assertEqual(caught.exception.code, "authentication_cancelled" if cancellation else "authentication_timeout")
                self.assertLess(time.monotonic() - started, 0.5)
                self.assertTrue(self.sock.closed.is_set())
            finally:
                if cancellation:
                    timer.join()
        self.assertFalse(any(thread.name == "mcp-oauth-http-cancel" for thread in threading.enumerate()))

    def test_empty_requested_scope_is_still_a_ceiling(self):
        fake = FakeAuthExchange()
        with self.assertRaises(AuthError):
            token_response(fake.token, endpoint=fake.metadata["token_endpoint"], scopes=(), now=1000)
        record = token_response(fake.token, endpoint=fake.metadata["token_endpoint"], scopes=None, now=1000)
        self.assertEqual(record.scopes, ("read",))


if __name__ == "__main__":
    unittest.main()
