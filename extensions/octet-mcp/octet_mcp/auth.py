"""Shared MCP authentication boundaries (no network or user interaction on import).

All owner values MUST come from octet's command/tool context, never arguments.
The full host owner triple fences live leases and login flows. Private credentials
use the host-derived durable session owner so a fresh host-issued generation may
resume that owner's login, never another session's.
Python strings and the private file store are not encrypted or reliably wiped.
"""

from __future__ import annotations

from dataclasses import asdict, dataclass
import hashlib
import json
import re
import time
from typing import Any, Callable, Mapping, Optional

from .config import ServerConfig


Cancel = Callable[[], bool]
MAX_SECRET_BYTES = 16 * 1024
MAX_DOCUMENT_BYTES = 64 * 1024
_MESSAGES = {
    "authentication_required": "MCP authentication requires an explicit user login command.",
    "authentication_cancelled": "MCP authentication was cancelled.",
    "authentication_timeout": "MCP authentication reached its deadline.",
    "authentication_unavailable": "MCP authentication is unavailable.",
    "authentication_denied": "MCP authentication requires a trusted user command and active owner.",
    "authentication_storage": "MCP private credential storage failed a safety check.",
    "authentication_metadata": "MCP OAuth discovery failed a source or metadata check.",
    "authentication_callback": "MCP OAuth callback failed a state, issuer, or redirect check.",
    "authentication_token": "MCP OAuth token exchange failed; explicit login is required.",
    "authentication_scope": "MCP OAuth scope request is outside the configured scope limit.",
    "authentication_busy": "MCP authentication is already in progress.",
    "authentication_gate": "MCP remote authentication requires the process-owner experimental gate.",
}


class AuthError(ValueError):
    """Only fixed bridge-authored diagnostics; never response or exception text."""

    def __init__(self, code: str = "authentication_unavailable") -> None:
        self.code = code
        self.safe_message = _MESSAGES[code]
        super().__init__(self.safe_message)


def check_operation(deadline: float, cancel: Cancel) -> None:
    if cancel():
        raise AuthError("authentication_cancelled")
    if time.monotonic() >= deadline:
        raise AuthError("authentication_timeout")


def secret_text(value: Any, *, bearer: bool = False) -> str:
    # RFC 6750 b64token is deliberately narrower than arbitrary header text.
    pattern = r"[A-Za-z0-9._~+/-]+=*" if bearer else r"[\x21-\x7e]+"
    if (not isinstance(value, str) or len(value) > MAX_SECRET_BYTES
            or re.fullmatch(pattern, value) is None):
        raise AuthError("authentication_token")
    return value


def decode_document(data: bytes) -> dict[str, Any]:
    def pairs(items):
        result = {}
        for key, value in items:
            if key in result:
                raise ValueError()
            result[key] = value
        return result

    try:
        if len(data) > MAX_DOCUMENT_BYTES:
            raise ValueError()
        document = json.loads(data.decode("utf-8"), object_pairs_hook=pairs,
                              parse_constant=lambda _: (_ for _ in ()).throw(ValueError()))
        if not isinstance(document, dict):
            raise ValueError()
        return document
    except (ValueError, UnicodeError, RecursionError):
        raise AuthError("authentication_metadata") from None


@dataclass(frozen=True, repr=False)
class AuthOwner:
    session_id: str
    extension_instance_id: str
    process_generation: int

    @classmethod
    def from_context(cls, context: Mapping[str, Any]) -> "AuthOwner":
        owner = context.get("resource_owner")
        if not isinstance(owner, Mapping) or set(owner) != {
            "session_id", "extension_instance_id", "process_generation"
        }:
            raise AuthError("authentication_denied")
        for key in ("session_id", "extension_instance_id"):
            item = owner[key]
            if (not isinstance(item, str) or not 1 <= len(item) <= 256
                    or not item.isascii() or any(ord(c) < 33 or ord(c) > 126 for c in item)):
                raise AuthError("authentication_denied")
        generation = owner["process_generation"]
        if type(generation) is not int or not 1 <= generation <= 2**53 - 1:
            raise AuthError("authentication_denied")
        return cls(owner["session_id"], owner["extension_instance_id"], generation)


@dataclass(frozen=True, repr=False)
class AuthBinding:
    owner: AuthOwner
    server_id: str
    endpoint: str
    credential: str
    kind: str
    config_scope: str
    issuer: Optional[str]
    client_id: Optional[str]
    scopes: Optional[tuple[str, ...]]
    redirect_port: int
    protocol_version: Optional[str]

    @classmethod
    def from_server(cls, owner: AuthOwner, server: ServerConfig) -> "AuthBinding":
        if server.transport != "streamable-http" or not server.url or server.auth is None:
            raise AuthError("authentication_unavailable")
        auth = server.auth
        return cls(owner, server.id, server.url, auth.credential, auth.type, server.scope,
                   auth.issuer, auth.client_id, auth.scopes, auth.redirect_port,
                   server.protocol_version)

    @property
    def key(self) -> str:
        # No caller/model string ever becomes a path component. Exact endpoint,
        # issuer, public client ID and configuration changes create new scopes.
        fields = asdict(self)
        fields["owner"] = self.owner.session_id
        encoded = json.dumps(fields, sort_keys=True, separators=(",", ":"))
        return hashlib.sha256(encoded.encode("utf-8")).hexdigest()

    @property
    def lease_key(self) -> str:
        encoded = json.dumps([self.key, asdict(self.owner)], sort_keys=True,
                             separators=(",", ":"))
        return hashlib.sha256(encoded.encode("utf-8")).hexdigest()


@dataclass(frozen=True, repr=False)
class TokenRecord:
    access_token: str
    expires_at: Optional[float] = None
    refresh_token: Optional[str] = None
    token_endpoint: Optional[str] = None
    scopes: tuple[str, ...] = ()

    def usable(self, now: float) -> bool:
        return self.expires_at is None or now + 30 < self.expires_at
