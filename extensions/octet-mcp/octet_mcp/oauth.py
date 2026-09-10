"""MCP OAuth public-client logic pinned to the 2026-07-28 authorization baseline.

Source: modelcontextprotocol/modelcontextprotocol commit
 aa8ce049f089f92618340190d4ece141f663310d,
 docs/specification/2026-07-28/basic/authorization/*.mdx.

RFC 9728 discovery, RFC 8414/OIDC ordered discovery with exact source issuer
validation, RFC 9207 response validation, PKCE S256, RFC 8707 resource binding.
Only pre-registered public clients are implemented. DCR is deprecated in this
baseline and is intentionally omitted; no confidential client or client secret
configuration is offered. Hosted CIMD publication/validation is not implemented.
OAuth endpoints must be query-free public HTTPS URLs. Token expiry is required;
public-client refresh MUST rotate. These conservative restrictions can reject
otherwise interoperable servers. No operation here retries an MCP tool call.
"""

from __future__ import annotations

import base64
from dataclasses import dataclass, field
import hashlib
import hmac
import re
import secrets
import threading
import time
from typing import Any, Optional, Sequence
from urllib.parse import parse_qsl, urlencode, urlsplit

from .auth import (AuthBinding, AuthError, Cancel, TokenRecord, check_operation,
                   secret_text)
from .oauth_http import MAX_HEADER_BYTES, OAuthHttpOperation, https_url


_SCOPE = re.compile(r"[\x21\x23-\x5b\x5d-\x7e]{1,128}")
_TOKEN = r"[!#$%&'*+.^_`|~0-9A-Za-z-]+"
_PARAM = re.compile(r'(' + _TOKEN + r')\s*=\s*(?:"((?:[^"\\]|\\.)*)"|(' + _TOKEN + r'))\s*$')


def scope_list(value: Any) -> tuple[str, ...]:
    if (not isinstance(value, (list, tuple)) or len(value) > 32
            or any(not isinstance(item, str) or _SCOPE.fullmatch(item) is None for item in value)
            or len(set(value)) != len(value)):
        raise AuthError("authentication_scope")
    return tuple(value)


def scope_string(value: Any) -> tuple[str, ...]:
    if not isinstance(value, str):
        raise AuthError("authentication_scope")
    return scope_list(value.split(" ") if value else [])


@dataclass(frozen=True, repr=False)
class AuthChallenge:
    """Ephemeral data from a source-verified 401/403, not authority to log in."""
    resource: str
    resource_metadata: Optional[str] = None
    scopes: Optional[tuple[str, ...]] = None


def parse_challenge(resource: str, headers: Sequence[str]) -> AuthChallenge:
    """Parse quoted auth-params and multiple schemes; ambiguous Bearer fails closed.

    ``resource`` must be the exact TLS/no-redirect response source supplied by
    the trusted transport, not the model or a resource_metadata parameter.
    Raw realm/error descriptions are never retained or displayed.
    """
    if len(headers) > 16 or sum(len(item) for item in headers) > MAX_HEADER_BYTES:
        raise AuthError("authentication_metadata")
    bearer = None
    scheme = None
    for header in headers:
        if not header.isascii() or any(ord(c) < 32 or ord(c) > 126 for c in header):
            raise AuthError("authentication_metadata")
        # Split comma boundaries outside quoted strings; escapes do not create
        # fake challenge boundaries or duplicate hidden parameters.
        chunks, start, quoted, escaped = [], 0, False, False
        for index, char in enumerate(header):
            if escaped:
                escaped = False
            elif quoted and char == "\\":
                escaped = True
            elif char == '"':
                quoted = not quoted
            elif char == "," and not quoted:
                chunks.append(header[start:index].strip())
                start = index + 1
        if quoted or escaped:
            raise AuthError("authentication_metadata")
        chunks.append(header[start:].strip())
        scheme = None
        for chunk in chunks:
            if not chunk:
                continue
            match = re.match(r"^(" + _TOKEN + r")(?:\s+(.*))?$", chunk)
            if match and (match.group(2) is None or not match.group(2).lstrip().startswith("=")):
                scheme = match.group(1).lower()
                chunk = match.group(2) or ""
                if scheme == "bearer":
                    if bearer is not None:
                        raise AuthError("authentication_metadata")
                    bearer = {}
            if scheme != "bearer" or not chunk:
                continue
            param = _PARAM.fullmatch(chunk)
            if not param:
                raise AuthError("authentication_metadata")
            key = param.group(1).lower()
            if key in bearer:
                raise AuthError("authentication_metadata")
            raw = param.group(2)
            bearer[key] = re.sub(r"\\(.)", r"\1", raw) if raw is not None else param.group(3)
    bearer = bearer or {}
    metadata = bearer.get("resource_metadata")
    if metadata is not None:
        https_url(metadata)
    return AuthChallenge(resource, metadata,
                         scope_string(bearer["scope"]) if "scope" in bearer else None)


def resource_metadata_urls(resource: str) -> tuple[str, ...]:
    parts = urlsplit(https_url(resource))
    origin = parts.scheme + "://" + parts.netloc
    root = origin + "/.well-known/oauth-protected-resource"
    return tuple(dict.fromkeys((root + parts.path, root)))


def issuer_metadata_urls(issuer: str) -> tuple[str, ...]:
    parts = urlsplit(https_url(issuer))
    origin = parts.scheme + "://" + parts.netloc
    path = parts.path.rstrip("/")
    urls = (origin + "/.well-known/oauth-authorization-server" + path,
            origin + "/.well-known/openid-configuration" + path)
    if path:
        urls += (origin + path + "/.well-known/openid-configuration",)
    return urls


@dataclass(frozen=True, repr=False)
class OAuthMetadata:
    issuer: str
    authorization_endpoint: str
    token_endpoint: str
    require_iss: bool
    scopes: tuple[str, ...]


def discover(binding: AuthBinding, operation: OAuthHttpOperation,
             challenge: Optional[AuthChallenge] = None,
             previous_scopes: tuple[str, ...] = ()) -> OAuthMetadata:
    if binding.kind != "oauth" or not binding.issuer or not binding.client_id:
        raise AuthError("authentication_metadata")
    if challenge is not None and challenge.resource != binding.endpoint:
        raise AuthError("authentication_metadata")
    urls = ((challenge.resource_metadata,) if challenge and challenge.resource_metadata
            else resource_metadata_urls(binding.endpoint))
    protected = None
    for url in urls:
        protected = operation.document(url, allow_missing=True)
        if protected is not None:
            break
    if protected is None or protected.get("resource") != binding.endpoint:
        raise AuthError("authentication_metadata")
    issuers = protected.get("authorization_servers")
    if (not isinstance(issuers, list) or not 1 <= len(issuers) <= 16
            or any(not isinstance(item, str) for item in issuers)
            or binding.issuer not in issuers):
        raise AuthError("authentication_metadata")
    # The public client ID is registered at the configured issuer. Never select
    # a different issuer simply because the protected resource advertises one.
    metadata = None
    for url in issuer_metadata_urls(binding.issuer):
        metadata = operation.document(url, allow_missing=True)
        if metadata is not None:
            break
    if metadata is None or metadata.get("issuer") != binding.issuer:
        raise AuthError("authentication_metadata")
    def supported(key, expected, default=()):
        values = metadata.get(key, default)
        return isinstance(values, (list, tuple)) and expected in values
    if (not supported("code_challenge_methods_supported", "S256")
            or not supported("response_types_supported", "code")
            or not supported("grant_types_supported", "authorization_code", ("authorization_code",))
            or not supported("token_endpoint_auth_methods_supported", "none", ("none",))):
        raise AuthError("authentication_metadata")
    require_iss = metadata.get("authorization_response_iss_parameter_supported", False)
    if type(require_iss) is not bool:
        raise AuthError("authentication_metadata")
    if challenge is not None and challenge.scopes is not None:
        requested = challenge.scopes
    elif binding.scopes is not None:
        requested = binding.scopes
    else:
        requested = scope_list(protected.get("scopes_supported", []))
    scopes = scope_list(tuple(dict.fromkeys(previous_scopes + requested)))
    if binding.scopes is not None and not set(scopes).issubset(binding.scopes):
        raise AuthError("authentication_scope")
    return OAuthMetadata(binding.issuer, https_url(metadata.get("authorization_endpoint")),
                         https_url(metadata.get("token_endpoint")), require_iss, scopes)


@dataclass(repr=False)
class AuthorizationAttempt:
    binding: AuthBinding
    metadata: OAuthMetadata
    redirect_uri: str
    deadline: float
    _state: str = field(default_factory=lambda: secrets.token_urlsafe(32))
    _verifier: str = field(default_factory=lambda: secrets.token_urlsafe(64))
    _used: bool = False
    _redeemed: bool = False
    _lock: threading.Lock = field(default_factory=threading.Lock)

    def authorization_url(self) -> str:
        challenge = base64.urlsafe_b64encode(hashlib.sha256(self._verifier.encode("ascii")).digest())
        params = {"response_type": "code", "client_id": self.binding.client_id,
                  "redirect_uri": self.redirect_uri, "state": self._state,
                  "code_challenge": challenge.rstrip(b"=").decode("ascii"),
                  "code_challenge_method": "S256", "resource": self.binding.endpoint}
        if self.metadata.scopes:
            params["scope"] = " ".join(self.metadata.scopes)
        return self.metadata.authorization_endpoint + "?" + urlencode(params)

    def consume_callback(self, callback: str, *, cancel: Cancel) -> str:
        with self._lock:
            check_operation(self.deadline, cancel)
            if (self._used or not isinstance(callback, str) or len(callback) > 16 * 1024
                    or not callback.isascii() or any(ord(c) < 33 or ord(c) > 126 for c in callback)
                    or not callback.startswith(self.redirect_uri + "?") or "#" in callback):
                raise AuthError("authentication_callback")
            query = callback[len(self.redirect_uri) + 1:]
            try:
                if re.search(r"%(?![0-9a-fA-F]{2})", query):
                    raise ValueError()
                pairs = parse_qsl(query, keep_blank_values=True, strict_parsing=True,
                                  max_num_fields=16, encoding="utf-8", errors="strict")
                fields = dict(pairs)
                if len(fields) != len(pairs):
                    raise ValueError()
                state = fields.get("state", "")
                if not state.isascii() or not hmac.compare_digest(state, self._state):
                    raise ValueError()
            except (ValueError, UnicodeError):
                raise AuthError("authentication_callback") from None
            # A matching state is one-use even on an issuer/error failure. A
            # wrong state is rejected without letting local CSRF steal a flow.
            self._used = True
            issuer = fields.get("iss")
            if ((issuer is None and self.metadata.require_iss)
                    or (issuer is not None and issuer != self.metadata.issuer)):
                raise AuthError("authentication_callback")
            if "error" in fields:
                raise AuthError("authentication_token")
            try:
                return secret_text(fields.get("code"))
            except AuthError:
                raise AuthError("authentication_callback") from None

    def exchange(self, code: str, operation: OAuthHttpOperation, *, now: float) -> TokenRecord:
        check_operation(self.deadline, operation.cancel)
        with self._lock:
            if not self._used or self._redeemed:
                raise AuthError("authentication_callback")
            self._redeemed = True
        body = urlencode({"grant_type": "authorization_code", "code": code,
                          "client_id": self.binding.client_id, "redirect_uri": self.redirect_uri,
                          "code_verifier": self._verifier, "resource": self.binding.endpoint}).encode("ascii")
        document = operation.document(self.metadata.token_endpoint, body=body)
        ceiling = (self.metadata.scopes if self.metadata.scopes or self.binding.scopes is not None else None)
        return token_response(document, endpoint=self.metadata.token_endpoint, scopes=ceiling, now=now)


def token_response(document: dict, *, endpoint: str, scopes: Optional[tuple[str, ...]],
                   now: float, previous_refresh: Optional[str] = None) -> TokenRecord:
    if (not isinstance(document, dict) or "error" in document
            or not isinstance(document.get("token_type"), str)
            or document["token_type"].lower() != "bearer"):
        raise AuthError("authentication_token")
    access = secret_text(document.get("access_token"), bearer=True)
    expires = document.get("expires_in")
    if type(expires) is not int or not 1 <= expires <= 366 * 24 * 60 * 60:
        raise AuthError("authentication_token")
    refresh = document.get("refresh_token")
    if refresh is not None:
        refresh = secret_text(refresh)
    if previous_refresh is not None and (refresh is None or refresh == previous_refresh):
        raise AuthError("authentication_token")
    granted = scope_string(document["scope"]) if "scope" in document else (scopes or ())
    if scopes is not None and not set(granted).issubset(scopes):
        raise AuthError("authentication_scope")
    return TokenRecord(access, now + expires, refresh, endpoint, granted)


def refresh_token(binding: AuthBinding, record: TokenRecord,
                  operation: OAuthHttpOperation, *, now: float) -> TokenRecord:
    if not record.refresh_token or not record.token_endpoint:
        raise AuthError("authentication_required")
    # Discover again without any server challenge. An issuer or token-endpoint
    # change MUST NOT receive an old refresh token, even if metadata is valid.
    metadata = discover(binding, operation)
    if metadata.token_endpoint != record.token_endpoint:
        raise AuthError("authentication_metadata")
    body = urlencode({"grant_type": "refresh_token", "refresh_token": record.refresh_token,
                      "client_id": binding.client_id, "resource": binding.endpoint}).encode("ascii")
    document = operation.document(metadata.token_endpoint, body=body)
    ceiling = record.scopes if record.scopes or binding.scopes is not None else None
    return token_response(document, endpoint=metadata.token_endpoint, scopes=ceiling,
                          now=now, previous_refresh=record.refresh_token)
