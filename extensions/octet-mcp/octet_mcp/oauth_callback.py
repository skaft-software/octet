"""One bounded numeric-loopback OAuth callback; no web framework or access log.

A listener is created only by an explicit trusted login command. It never opens a
browser, exchanges a token, retries a tool, redirects, or calls host services.
The controller caps active listeners and owns cancellation and generation fences.
"""

from __future__ import annotations

import socket
import threading
import time
from typing import Optional
from urllib.parse import urlsplit

from .auth import AuthError, Cancel, check_operation
from .oauth import AuthorizationAttempt


MAX_CALLBACK_BYTES = 18 * 1024
MAX_CALLBACK_CONNECTIONS = 32
CALLBACK_PATH = "/oauth/callback"


class LoopbackCallback:
    def __init__(self, port: int = 0) -> None:
        self._socket = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
        try:
            # Deliberately no SO_REUSEPORT/ADDR: an existing callback owner must
            # never be displaced. Numeric bind never performs DNS resolution.
            self._socket.bind(("127.0.0.1", port))
            self._socket.listen(4)
            self._socket.settimeout(0.05)
        except OSError:
            self._socket.close()
            raise AuthError("authentication_unavailable") from None
        self.redirect_uri = "http://127.0.0.1:%d%s" % (self._socket.getsockname()[1], CALLBACK_PATH)
        self._stop = threading.Event()
        self.done = threading.Event()
        self.code: Optional[str] = None
        self.error_code: Optional[str] = None
        self._thread: Optional[threading.Thread] = None

    def start(self, attempt: AuthorizationAttempt, *, cancel: Cancel) -> None:
        self._thread = threading.Thread(target=self._serve, args=(attempt, cancel),
                                        name="mcp-oauth-callback", daemon=True)
        self._thread.start()

    def _serve(self, attempt: AuthorizationAttempt, cancel: Cancel) -> None:
        cancelled = lambda: self._stop.is_set() or cancel()
        try:
            for _ in range(MAX_CALLBACK_CONNECTIONS):
                while True:
                    check_operation(attempt.deadline, cancelled)
                    try:
                        connection, peer = self._socket.accept()
                        break
                    except socket.timeout:
                        pass
                with connection:
                    if peer[0] != "127.0.0.1":
                        continue
                    connection.settimeout(0.05)
                    accepted = False
                    try:
                        target = self._target(connection, attempt.deadline, cancelled)
                        self.code = attempt.consume_callback(self.redirect_uri + target,
                                                             cancel=cancelled)
                        accepted = True
                    except AuthError as error:
                        if attempt._used or error.code in {
                            "authentication_timeout", "authentication_cancelled"
                        }:
                            self.error_code = error.code
                    self._reply(connection, accepted)
                    if accepted or self.error_code:
                        return
            self.error_code = "authentication_callback"
        except AuthError as error:
            self.error_code = error.code
        except OSError:
            self.error_code = "authentication_cancelled" if cancelled() else "authentication_callback"
        except Exception:
            self.error_code = "authentication_callback"
        finally:
            self._socket.close()
            self.done.set()

    def _target(self, connection: socket.socket, deadline: float, cancel: Cancel) -> str:
        request_deadline = min(deadline, time.monotonic() + 1.0)
        data = bytearray()
        while b"\r\n\r\n" not in data:
            check_operation(request_deadline, cancel)
            if len(data) >= MAX_CALLBACK_BYTES:
                raise AuthError("authentication_callback")
            try:
                chunk = connection.recv(min(4096, MAX_CALLBACK_BYTES - len(data)))
            except socket.timeout:
                continue
            if not chunk:
                raise AuthError("authentication_callback")
            data.extend(chunk)
        try:
            lines = bytes(data).split(b"\r\n\r\n", 1)[0].decode("ascii").split("\r\n")
            method, target, version = lines[0].split(" ")
            headers = {}
            if len(lines) > 64:
                raise ValueError()
            for line in lines[1:]:
                key, value = line.split(":", 1)
                key = key.lower()
                if key in headers or not key or any(c.isspace() for c in key):
                    raise ValueError()
                headers[key] = value.strip()
            if (method != "GET" or version not in {"HTTP/1.0", "HTTP/1.1"}
                    or not target.startswith(CALLBACK_PATH + "?")
                    or headers.get("host") != urlsplit(self.redirect_uri).netloc
                    or "origin" in headers or "transfer-encoding" in headers
                    or headers.get("content-length", "0") != "0"):
                raise ValueError()
            return target[len(CALLBACK_PATH):]
        except (ValueError, UnicodeError):
            raise AuthError("authentication_callback") from None

    @staticmethod
    def _reply(connection: socket.socket, accepted: bool) -> None:
        body = (b"Callback received. Return to octet and poll the login status."
                if accepted else b"Callback rejected.")
        status = b"200 OK" if accepted else b"400 Bad Request"
        response = (b"HTTP/1.1 " + status + b"\r\nContent-Type: text/plain; charset=utf-8\r\n"
                    b"Cache-Control: no-store\r\nReferrer-Policy: no-referrer\r\n"
                    b"Content-Security-Policy: default-src 'none'; frame-ancestors 'none'\r\n"
                    b"X-Content-Type-Options: nosniff\r\nConnection: close\r\nContent-Length: "
                    + str(len(body)).encode("ascii") + b"\r\n\r\n" + body)
        try:
            connection.sendall(response)
        except OSError:
            pass

    def close(self, *, wait: bool = True) -> None:
        self._stop.set()
        self._socket.close()
        if wait and self._thread is not None:
            self._thread.join(timeout=0.25)
        self.code = None
