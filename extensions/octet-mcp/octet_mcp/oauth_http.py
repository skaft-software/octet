"""Bounded OAuth HTTP composition, independent of the MCP message transport.

The injected exchange is a TRUSTED transport primitive, not an OAuth adapter a
user must write. Runtime wiring must supply the package's DNS-pinned, cancellable
HTTPS exchange. It MUST validate all DNS answers as public, connect only to a
validated numeric address with original-host TLS verification/SNI, reject
redirects BEFORE following them, enforce deadline/cancellation and the byte/header
limits DURING reads, and close all sockets/workers on return. No urllib fallback,
proxies, cookies, ambient credentials, redirects, retries or DNS re-resolution is
permitted. Post-read validation below is defense-in-depth, not an SSRF substitute.

Absent that production composition, OAuth fails closed without network access.
Fixtures inject this same narrow interface and never contact a live auth server.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import ipaddress
import re
from typing import Callable, Optional, Protocol
from urllib.parse import urlsplit

from .auth import (AuthError, Cancel, MAX_DOCUMENT_BYTES, check_operation,
                   decode_document)


MAX_HTTP_REQUESTS = 8
MAX_HTTP_BYTES = 256 * 1024
MAX_HEADER_BYTES = 16 * 1024


def https_url(value) -> str:
    try:
        if (not isinstance(value, str) or not 1 <= len(value) <= 4096
                or not value.isascii() or any(ord(c) < 33 or ord(c) > 126 for c in value)
                or any(c in value for c in ("\\", "?", "#"))):
            raise ValueError()
        parts = urlsplit(value)
        if (parts.scheme != "https" or not parts.hostname or not parts.netloc
                or parts.username is not None or parts.password is not None
                or (parts.port is not None and not 1 <= parts.port <= 65535)):
            raise ValueError()
        host = parts.hostname.lower()
        if host == "localhost" or host.endswith((".localhost", ".local", ".internal")):
            raise ValueError()
        try:
            literal = ipaddress.ip_address(host)
        except ValueError:
            if not re.fullmatch(r"[a-z0-9](?:[a-z0-9.-]{0,251}[a-z0-9])?", host):
                raise ValueError()
        else:
            if not literal.is_global or literal.is_multicast:
                raise ValueError()
        return value
    except (ValueError, TypeError):
        raise AuthError("authentication_metadata") from None


@dataclass(frozen=True, repr=False)
class OAuthHttpRequest:
    method: str
    url: str
    headers: tuple[tuple[str, str], ...]
    body: Optional[bytes]
    deadline: float
    cancel: Cancel
    max_response_bytes: int
    max_header_bytes: int = MAX_HEADER_BYTES


@dataclass(frozen=True, repr=False)
class OAuthHttpResponse:
    url: str
    status: int
    headers: tuple[tuple[str, str], ...] = ()
    body: bytes = field(default=b"", repr=False)


class SafeHttpExchange(Protocol):
    def __call__(self, request: OAuthHttpRequest) -> OAuthHttpResponse:
        """One bounded no-redirect DNS-pinned HTTPS exchange; no retries."""


class OAuthHttp:
    def __init__(self, exchange: Optional[SafeHttpExchange] = None) -> None:
        self._exchange = exchange

    def operation(self, *, deadline: float, cancel: Cancel) -> "OAuthHttpOperation":
        return OAuthHttpOperation(self._exchange, deadline, cancel)


class OAuthHttpOperation:
    def __init__(self, exchange: Optional[SafeHttpExchange], deadline: float,
                 cancel: Cancel) -> None:
        self._exchange = exchange
        self.deadline = deadline
        self.cancel = cancel
        self._requests = 0
        self._bytes = 0

    def request(self, method: str, url: str, body: Optional[bytes] = None) -> OAuthHttpResponse:
        check_operation(self.deadline, self.cancel)
        https_url(url)
        if self._exchange is None:
            raise AuthError("authentication_unavailable")
        if (method not in {"GET", "POST"} or (body is not None and len(body) > MAX_DOCUMENT_BYTES)
                or self._requests >= MAX_HTTP_REQUESTS or self._bytes >= MAX_HTTP_BYTES):
            raise AuthError("authentication_metadata")
        self._requests += 1
        headers = (("Accept", "application/json"), ("Accept-Encoding", "identity"))
        if body is not None:
            headers += (("Content-Type", "application/x-www-form-urlencoded"),)
        maximum = min(MAX_DOCUMENT_BYTES, MAX_HTTP_BYTES - self._bytes)
        request = OAuthHttpRequest(method, url, headers, body, self.deadline, self.cancel, maximum)
        try:
            response = self._exchange(request)
        except AuthError:
            raise
        except Exception:
            check_operation(self.deadline, self.cancel)
            raise AuthError("authentication_unavailable") from None
        check_operation(self.deadline, self.cancel)
        # Identity MUST be the exact request URL. Never inspect Location or a
        # body before the transport's source/no-redirect contract is checked.
        if (response.url != url or type(response.status) is not int
                or not 200 <= response.status <= 599 or 300 <= response.status < 400
                or not isinstance(response.body, bytes) or len(response.body) > maximum):
            raise AuthError("authentication_metadata")
        header_bytes = sum(len(k) + len(v) for k, v in response.headers)
        if len(response.headers) > 64 or header_bytes > MAX_HEADER_BYTES:
            raise AuthError("authentication_metadata")
        self._bytes += len(response.body) + header_bytes
        if self._bytes > MAX_HTTP_BYTES:
            raise AuthError("authentication_metadata")
        encodings = [v.lower() for k, v in response.headers if k.lower() == "content-encoding"]
        if encodings and encodings != ["identity"]:
            raise AuthError("authentication_metadata")
        return response

    def document(self, url: str, *, body: Optional[bytes] = None,
                 allow_missing: bool = False) -> Optional[dict]:
        response = self.request("POST" if body is not None else "GET", url, body)
        if allow_missing and response.status in {404, 405}:
            return None
        if response.status != 200:
            raise AuthError("authentication_token" if body is not None else "authentication_metadata")
        types = [v.split(";", 1)[0].strip().lower() for k, v in response.headers
                 if k.lower() == "content-type"]
        if types != ["application/json"]:
            raise AuthError("authentication_metadata")
        return decode_document(response.body)
