"""Production OAuth exchange composed with the hardened MCP network primitives.

No second DNS/urllib/proxy implementation: reuse Streamable HTTP's isolated,
killable resolver, all-address validation and numeric-pinned TLS/SNI connection.
OAuth does not create an MCP session or use its credential adapter. A small
cancellation watcher aborts every socket/resolver during slow/trickled headers,
body or TLS work; it is joined before return. The default runtime gate remains
independent and must be checked before constructing/using the auth controller.
"""

from __future__ import annotations

import http.client
import re
import threading
import time

from .auth import AuthError, check_operation
from .oauth_http import OAuthHttpRequest, OAuthHttpResponse, https_url
from .streamable_http import (_HttpOperation, _PinnedConnection, _StrictHttpResponse,
                              _endpoint, _resolve_addresses)


class _LineBudget:
    """Bound status/header/chunk-framing bytes during reads, not after parsing."""
    def __init__(self, stream, maximum: int) -> None:
        self._stream = stream
        self._remaining = maximum

    def readline(self, maximum=-1):
        limit = self._remaining + 1
        if maximum >= 0:
            limit = min(limit, maximum)
        line = self._stream.readline(limit)
        self._remaining -= len(line)
        if self._remaining < 0 or (line and not line.endswith(b"\r\n")):
            raise AuthError("authentication_metadata")
        return line

    def __getattr__(self, name):
        return getattr(self._stream, name)


class PinnedOAuthExchange:
    """SafeHttpExchange for OAuthHttp(PinnedOAuthExchange()); at most four I/O calls."""
    def __init__(self) -> None:
        self._slots = threading.BoundedSemaphore(4)

    def __call__(self, request: OAuthHttpRequest) -> OAuthHttpResponse:
        check_operation(request.deadline, request.cancel)
        https_url(request.url)  # Never grant MCP's numeric-loopback exception to OAuth.
        if not self._slots.acquire(blocking=False):
            raise AuthError("authentication_busy")
        operation = _HttpOperation()
        finished = threading.Event()
        connection = None
        watcher = None
        try:
            def watch():
                while not finished.wait(0.025):
                    if request.cancel() or time.monotonic() >= request.deadline:
                        operation.abort()
                        return
            watcher = threading.Thread(target=watch, name="mcp-oauth-http-cancel", daemon=True)
            watcher.start()
            endpoint = _endpoint(request.url)
            addresses = _resolve_addresses(endpoint, operation, request.deadline)
            check_operation(request.deadline, request.cancel)
            connection = _PinnedConnection(endpoint, addresses[0], operation, request.deadline)

            class Response(_StrictHttpResponse):
                def __init__(self, *args, **kwargs):
                    super().__init__(*args, **kwargs)
                    self.fp = _LineBudget(self.fp, request.max_header_bytes)

            connection.response_class = Response
            connection.request(request.method, endpoint.target, body=request.body,
                               headers=dict(request.headers))
            response = connection.getresponse()
            headers = tuple(response.getheaders())
            if (len(headers) > 64 or sum(len(k) + len(v) for k, v in headers) > request.max_header_bytes
                    or 300 <= response.status < 400):
                raise AuthError("authentication_metadata")
            check_operation(request.deadline, request.cancel)
            # Error bodies and Location headers are never interpreted or kept.
            if response.status != 200:
                return OAuthHttpResponse(request.url, response.status, headers)
            length = [v for k, v in headers if k.lower() == "content-length"]
            transfer = [v.lower() for k, v in headers if k.lower() == "transfer-encoding"]
            encoding = [v.lower() for k, v in headers if k.lower() == "content-encoding"]
            if (len(length) > 1 or (length and transfer) or (transfer and transfer != ["chunked"])
                    or (encoding and encoding != ["identity"])):
                raise AuthError("authentication_metadata")
            if length and (not re.fullmatch(r"[0-9]{1,10}", length[0])
                           or int(length[0]) > request.max_response_bytes):
                raise AuthError("authentication_metadata")
            data = response.read(request.max_response_bytes + 1)
            check_operation(request.deadline, request.cancel)
            if len(data) > request.max_response_bytes or (length and len(data) != int(length[0])):
                raise AuthError("authentication_metadata")
            return OAuthHttpResponse(request.url, response.status, headers, data)
        except AuthError:
            raise
        except Exception:
            check_operation(request.deadline, request.cancel)
            raise AuthError("authentication_unavailable") from None
        finally:
            finished.set()
            operation.abort()
            operation.close_sockets()
            if connection is not None:
                connection.close()
            if watcher is not None:
                watcher.join(timeout=0.25)
            self._slots.release()
