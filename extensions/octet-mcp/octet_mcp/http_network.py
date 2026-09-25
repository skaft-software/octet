"""Direct, address-pinned HTTP sockets and killable DNS for the MCP transport."""

from __future__ import annotations

import http.client
import ipaddress
import json
import os
import socket
import ssl
import subprocess
import sys
import time
from typing import Any

from .protocol import McpProtocolError, McpTransportError


# No endpoint, token, ambient application environment or error text is executed.
# Bound the helper's output at the source; the only blocking work is libc DNS.
RESOLVER_PROGRAM = """
import json, socket, sys
try:
    answers = socket.getaddrinfo(sys.argv[1], None, type=socket.SOCK_STREAM)
    addresses = sorted(set(answer[4][0] for answer in answers))
    if not addresses or len(addresses) > 16:
        raise ValueError()
    print(json.dumps(addresses))
except Exception:
    sys.exit(1)
"""


def _approved_address(value: str, *, literal_loopback: bool) -> str:
    try:
        address = ipaddress.ip_address(value)
    except ValueError:
        raise McpProtocolError("unsafe_address", "MCP endpoint resolved to an unsafe address", permanent=True) from None
    # Keep special-use denials consistent on older supported Python releases,
    # whose ipaddress registries predate newer IANA classifications.
    special = ("192.0.0.0/24", "192.88.99.0/24", "2001::/23", "3fff::/20")
    # Mapped/translated/transition addresses can hide a private IPv4 destination.
    if "%" in value or any(address in ipaddress.ip_network(network) for network in special) or (
        isinstance(address, ipaddress.IPv6Address)
        and (address.ipv4_mapped is not None or address.sixtofour is not None
             or address.teredo is not None or address in ipaddress.ip_network("64:ff9b::/96")
             or address in ipaddress.ip_network("64:ff9b:1::/48"))
    ):
        allowed = False
    else:
        allowed = (address.is_global and not address.is_multicast and not address.is_reserved) or (
            literal_loopback and address.is_loopback
        )
    if not allowed:
        raise McpProtocolError("unsafe_address", "MCP endpoint resolved to an unsafe address", permanent=True)
    return str(address)


def resolve_addresses(host: str, operation: Any, deadline: float) -> tuple[str, ...]:
    """Review all DNS answers once; never permit DNS names to reach loopback/LAN."""
    operation.check(deadline)
    try:
        ipaddress.ip_address(host)
    except ValueError:
        pass
    else:
        return (_approved_address(host, literal_loopback=True),)

    process = subprocess.Popen(
        [sys.executable, "-I", "-c", RESOLVER_PROGRAM, host],
        stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
        env={key: value for key, value in os.environ.items() if key in {"SYSTEMROOT"}},
        close_fds=True,
    )
    operation.add_process(process)
    try:
        while True:
            operation.check(deadline)
            try:
                output, _ = process.communicate(timeout=min(0.05, max(0.001, deadline - time.monotonic())))
                break
            except subprocess.TimeoutExpired:
                continue
        operation.check(deadline)
        if process.returncode != 0 or len(output) > 4096:
            raise McpTransportError("dns_failed", "MCP endpoint DNS lookup failed")
        try:
            values = json.loads(output)
            if not isinstance(values, list) or not 1 <= len(values) <= 16 or not all(isinstance(v, str) for v in values):
                raise ValueError
        except (ValueError, UnicodeError):
            raise McpTransportError("dns_failed", "MCP endpoint DNS lookup failed") from None
        return tuple(_approved_address(value, literal_loopback=False) for value in values)
    finally:
        if process.poll() is None:
            process.kill()
        # The helper cannot spawn children; kill followed by communicate reaps it.
        process.communicate()
        operation.remove_process(process)


class StrictHttpResponse(http.client.HTTPResponse):
    """Do not inherit http.client's permissive truncated-chunk acceptance."""

    def _get_chunk_left(self):
        remaining = self.chunk_left
        if not remaining:
            if remaining is not None and self._safe_read(2) != b"\r\n":
                raise McpProtocolError("invalid_http_framing", "MCP chunk delimiter was invalid", permanent=True)
            line = self.fp.readline(8193)
            if not line.endswith(b"\r\n"):
                raise McpProtocolError("truncated_http_body", "MCP chunk size line was truncated", permanent=True)
            size = line[:-2].split(b";", 1)[0]
            if len(line) > 8192 or not size or len(size) > 16 or any(c not in b"0123456789abcdefABCDEF" for c in size):
                raise McpProtocolError("invalid_http_framing", "MCP chunk size was invalid", permanent=True)
            remaining = int(size, 16)
            if remaining == 0:
                total = 0
                while True:
                    trailer = self.fp.readline(8193)
                    total += len(trailer)
                    if total > 16384:
                        raise McpProtocolError("invalid_http_framing", "MCP chunk trailers exceeded the limit", permanent=True)
                    if not trailer.endswith(b"\r\n"):
                        raise McpProtocolError("truncated_http_body", "MCP chunk trailers were truncated", permanent=True)
                    if trailer == b"\r\n":
                        break
                self._close_conn()
                remaining = None
            self.chunk_left = remaining
        return remaining


class PinnedConnection(http.client.HTTPConnection):
    """Connect only a reviewed numeric address while retaining Host and TLS SNI."""

    response_class = StrictHttpResponse

    def __init__(self, host: str, port: int, *, address: str, tls: bool, operation: Any, deadline: float) -> None:
        super().__init__(host, port, timeout=max(0.001, deadline - time.monotonic()))
        self._address = address
        self._tls = tls
        self._operation = operation
        self._deadline = deadline

    def connect(self) -> None:
        self._operation.check(self._deadline)
        family = socket.AF_INET6 if ":" in self._address else socket.AF_INET
        sock = socket.socket(family, socket.SOCK_STREAM)
        self.sock = sock
        self._operation.add_socket(sock)
        sock.settimeout(max(0.001, self._deadline - time.monotonic()))
        # socket.connect with a canonical literal does no getaddrinfo lookup.
        sock.connect((self._address, self.port))
        self._operation.check(self._deadline)
        if self._tls:
            context = ssl.create_default_context()
            context.minimum_version = ssl.TLSVersion.TLSv1_2
            wrapped = context.wrap_socket(sock, server_hostname=self.host, do_handshake_on_connect=False)
            self.sock = wrapped
            self._operation.add_socket(wrapped)
            self._operation.check(self._deadline)
            wrapped.do_handshake()
        self._operation.check(self._deadline)
