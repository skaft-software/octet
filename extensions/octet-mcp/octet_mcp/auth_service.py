"""First-party MCP authentication controller and scoped CredentialProvider.

Runtime integration contract (call from the declared /mcp command dispatcher):

* Construct AuthService(PrivateTokenStore(host_selected_path), OAuthHttp(the
  package's safe pinned exchange), experimental_streamable_http_mcp=owner_gate).
* execute_command(action, server, HOST_CONTEXT, trusted_user_command=True,
  is_current=HOST_OWNER_AND_CONFIG_LEASE_CHECK, request_input=ext.request_input,
  present_authorization=TRUSTED_USER_ONLY_URL_CALLBACK). Never source the trust
  boolean or owner/lease from model arguments, a server challenge, or config.
* Actions: login, login-manual, poll, complete-manual, cancel, logout, status.
  OAuth login returns promptly after discovery/presentation, NOT after browser
  authentication. Loopback waits in at most four bounded 120-second workers;
  poll performs the one-use exchange. Manual completion uses secret=True input
  of the FULL callback URL (not a code that would skip state/issuer checks).
* present_authorization(url, issuer=..., resource=...) must obtain an explicit
  trusted user choice and present/open only to that user; True means accepted.
  Do not use model results, notifications, logs, progress or persistent semantic
  snapshots for the URL (it contains state). The controller never opens a browser.
  All returned command/presentation data is fixed, URL/credential-free text.
* scoped_provider(server, HOST_CONTEXT, is_current=...) supplies the existing
  bearer_token(reference, server_id=...) protocol, bound to one immutable owner,
  config and endpoint. Lookup never prompts, starts a flow or replays a tool.
  Refresh of an already authorized grant is allowed, serialized, and one-use.
  Forward the transport operation's absolute deadline and cancellation into
  OwnerCredentialProvider.bearer_token(..., deadline=..., cancel=...) so its
  ten-second ceiling never refunds a shorter request/startup budget.
* A 401/403 parks the MCP connection; do NOT invoke login or retry the tool.
  invalidate(server, context, is_current=...) may discard a rejected grant.
  Pass an AuthChallenge from that exact source only to a subsequent explicit
  login command. The controller never reacts to server data on its own.
* retire_owner(context) cancels flows on owner settlement/replacement. shutdown()
  cancels all flows. Lease checks must reject retired owners, config changes and
  closed connections, including after credential lookup. A freshly host-issued
  lease for the SAME durable session owner may load its persisted credentials.

The store is owner-private PLAINTEXT (not an encrypted host vault). Logout
removes the local scoped grant and cancels login; it does NOT revoke upstream
sessions or erase backups. Existing MCP connections must be closed by runtime on
logout/invalidation; an already sent request cannot be recalled. No automatic
replay or rollback is promised. DCR/CIMD hosting and remote revocation are absent.
"""

from __future__ import annotations

from dataclasses import dataclass, field
import threading
import time
from typing import Any, Callable, Mapping, Optional

from .auth import (AuthBinding, AuthError, AuthOwner, Cancel, TokenRecord,
                   check_operation, secret_text)
from .auth_store import PrivateTokenStore
from .config import ServerConfig
from .oauth import AuthChallenge, AuthorizationAttempt, discover, refresh_token
from .oauth_callback import LoopbackCallback
from .oauth_http import OAuthHttp


MAX_ACTIVE_LOGINS = 4
LOGIN_LIFETIME_SECONDS = 120.0
HTTP_OPERATION_SECONDS = 10.0
COMMAND_OPERATION_SECONDS = 25.0


@dataclass(repr=False)
class _Flow:
    attempt: AuthorizationAttempt
    listener: Optional[LoopbackCallback]
    is_current: Callable[[], bool]
    cancelled: threading.Event = field(default_factory=threading.Event)

    def close(self, *, wait: bool = True) -> None:
        self.cancelled.set()
        if self.listener:
            self.listener.close(wait=wait)
        self.attempt._state = ""
        self.attempt._verifier = ""


def _result(state: str, text: str, code: Optional[str] = None) -> dict[str, Any]:
    return {"text": text, "auth": {"state": state, "code": code}}


class AuthService:
    def __init__(self, store: PrivateTokenStore, http: Optional[OAuthHttp] = None, *,
                 experimental_streamable_http_mcp: bool = False,
                 clock: Callable[[], float] = time.time) -> None:
        self._store = store
        self._http = http or OAuthHttp()
        self._gate = experimental_streamable_http_mcp
        self._clock = clock
        self._flows: dict[str, _Flow] = {}
        self._lock = threading.RLock()
        self._command_lock = threading.Lock()
        self._active_command: Optional[tuple[AuthOwner, str, threading.Event]] = None
        self._closed = threading.Event()

    def _binding(self, server: ServerConfig, context: Mapping[str, Any],
                 is_current: Callable[[], bool]) -> AuthBinding:
        if not self._gate:
            raise AuthError("authentication_gate")
        if self._closed.is_set() or not is_current():
            raise AuthError("authentication_denied")
        return AuthBinding.from_server(AuthOwner.from_context(context), server)

    def execute_command(self, action: str, server: ServerConfig, context: Mapping[str, Any], *,
                        is_current: Callable[[], bool], trusted_user_command: bool = False,
                        request_input: Optional[Callable[..., Optional[str]]] = None,
                        present_authorization: Optional[Callable[..., bool]] = None,
                        present_status: Optional[Callable[[dict], None]] = None,
                        challenge: Optional[AuthChallenge] = None,
                        cancel: Cancel = lambda: False,
                        deadline: Optional[float] = None) -> dict[str, Any]:
        """Trusted user command only. Never expose this method as a model tool."""
        try:
            if trusted_user_command is not True:
                raise AuthError("authentication_denied")
            binding = self._binding(server, context, is_current)
            cancelled = lambda: cancel() or self._closed.is_set() or not is_current()
            deadline = min(deadline if deadline is not None else float("inf"),
                           time.monotonic() + COMMAND_OPERATION_SECONDS)
            check_operation(deadline, cancelled)
            # Cancel/logout must not queue behind private UI, discovery, or a
            # token exchange. Signal the admitted command before touching disk.
            if action in {"cancel", "logout"}:
                with self._lock:
                    active = self._active_command
                    if active and active[1] == binding.lease_key:
                        active[2].set()
                result = self._command(action, binding, is_current, request_input,
                                       present_authorization, challenge, deadline, cancelled)
            else:
                if not self._command_lock.acquire(blocking=False):
                    raise AuthError("authentication_busy")
                interrupted = threading.Event()
                with self._lock:
                    self._active_command = (binding.owner, binding.lease_key, interrupted)
                try:
                    result = self._command(action, binding, is_current, request_input,
                                           present_authorization, challenge, deadline,
                                           lambda: cancelled() or interrupted.is_set())
                finally:
                    with self._lock:
                        self._active_command = None
                    self._command_lock.release()
        except AuthError as error:
            result = _result("cancelled" if error.code == "authentication_cancelled" else "unavailable",
                             error.safe_message, error.code)
        except Exception:
            error = AuthError()
            result = _result("unavailable", error.safe_message, error.code)
        if present_status is not None:
            # Only this fixed safe result, never request URL/issuer/state, is
            # suitable for parent-correlated semantic presentation.
            present_status(result["auth"].copy())
        return result

    def _command(self, action, binding, is_current, request_input, present_authorization,
                 challenge, deadline, cancel):
        check_operation(deadline, cancel)
        key = binding.lease_key
        if action in {"cancel", "logout"}:
            self._remove_flow(key)
            if action == "logout":
                with self._store.transaction(binding, deadline=deadline, cancel=cancel) as transaction:
                    transaction.delete()
                return _result("stopped", "MCP local credentials removed. Remote sessions were not revoked.")
            return _result("cancelled", "MCP login cancelled.")
        if action == "status":
            with self._lock:
                flow = self._flows.get(key)
            if flow:
                if time.monotonic() >= flow.attempt.deadline:
                    self._remove_flow(key)
                    raise AuthError("authentication_timeout")
                if flow.listener and flow.listener.done.is_set():
                    if flow.listener.error_code:
                        error = flow.listener.error_code
                        self._remove_flow(key)
                        raise AuthError(error)
                    return _result("pending", "MCP callback received; poll to finish login.")
                return _result("pending", "MCP login awaits manual authentication.")
            with self._store.transaction(binding, deadline=deadline, cancel=cancel) as transaction:
                record = transaction.load()
            return (_result("active", "MCP credentials are stored for this owner and endpoint.") if record
                    else _result("unavailable", "MCP login is required.", "authentication_required"))
        if action in {"login", "login-manual"}:
            if binding.kind == "bearer":
                if request_input is None:
                    raise AuthError("authentication_unavailable")
                value = request_input("MCP bearer token for the configured server (private plaintext storage):",
                                      secret=True)
                check_operation(deadline, cancel)
                if value is None:
                    raise AuthError("authentication_cancelled")
                token = TokenRecord(secret_text(value, bearer=True))
                with self._store.transaction(binding, deadline=deadline, cancel=cancel) as transaction:
                    transaction.save(token)
                return _result("active", "MCP bearer credential stored privately for this owner and endpoint.")
            if present_authorization is None:
                raise AuthError("authentication_unavailable")
            with self._lock:
                expired = [old_key for old_key, flow in self._flows.items()
                           if not flow.is_current() or time.monotonic() >= flow.attempt.deadline]
            for old_key in expired:
                self._remove_flow(old_key)
            with self._lock:
                if key in self._flows or len(self._flows) >= MAX_ACTIVE_LOGINS:
                    raise AuthError("authentication_busy")
            with self._store.transaction(binding, deadline=deadline, cancel=cancel) as transaction:
                previous = transaction.load()
            metadata = discover(binding, self._http.operation(
                deadline=min(deadline, time.monotonic() + HTTP_OPERATION_SECONDS), cancel=cancel),
                                challenge, previous.scopes if previous else ())
            listener = LoopbackCallback(binding.redirect_port)
            attempt = AuthorizationAttempt(binding, metadata, listener.redirect_uri,
                                           time.monotonic() + LOGIN_LIFETIME_SECONDS)
            if action == "login-manual":
                listener.close()
                listener = None
            flow = _Flow(attempt, listener, is_current)
            try:
                with self._lock:
                    check_operation(deadline, cancel)
                    self._flows[key] = flow
                    if listener:
                        listener.start(attempt, cancel=lambda: self._closed.is_set()
                                       or flow.cancelled.is_set() or not is_current())
                accepted = present_authorization(attempt.authorization_url(),
                                                 issuer=metadata.issuer, resource=binding.endpoint)
                check_operation(deadline, cancel)
                if accepted is not True:
                    raise AuthError("authentication_cancelled")
            except BaseException:
                self._remove_flow(key)
                flow.close()
                raise
            return _result("pending", "MCP login started. Authenticate manually, then poll or complete-manual.")
        if action in {"poll", "complete-manual"}:
            with self._lock:
                flow = self._flows.get(key)
            if flow is None:
                raise AuthError("authentication_required")
            try:
                check_operation(flow.attempt.deadline, cancel)
                if action == "complete-manual":
                    if flow.listener is not None or request_input is None:
                        raise AuthError("authentication_unavailable")
                    callback = request_input("Paste the full OAuth callback URL privately (not only the code):",
                                             secret=True)
                    check_operation(deadline, cancel)
                    if callback is None:
                        raise AuthError("authentication_cancelled")
                    code = flow.attempt.consume_callback(callback, cancel=cancel)
                else:
                    if flow.listener is None or not flow.listener.done.is_set():
                        return _result("pending", "MCP login awaits manual authentication.")
                    if flow.listener.error_code:
                        raise AuthError(flow.listener.error_code)
                    code = flow.listener.code
                operation = self._http.operation(
                    deadline=min(deadline, time.monotonic() + HTTP_OPERATION_SECONDS), cancel=cancel)
                with self._store.transaction(binding, deadline=deadline, cancel=cancel) as transaction:
                    # Erase an old grant before the single token request. A
                    # failed replacement never leaves a stale credential usable.
                    transaction.delete()
                    record = flow.attempt.exchange(code, operation, now=self._clock())
                    check_operation(deadline, cancel)
                    transaction.save(record)
                return _result("active", "MCP OAuth credential stored privately. No MCP tool was replayed.")
            finally:
                # A still-waiting poll is passive and must keep the pending flow.
                if (action == "complete-manual" or (flow.listener and flow.listener.done.is_set())
                        or time.monotonic() >= flow.attempt.deadline or cancel()):
                    self._remove_flow(key)
        raise AuthError("authentication_unavailable")

    def _remove_flow(self, key: str, *, wait: bool = True) -> None:
        with self._lock:
            flow = self._flows.pop(key, None)
        if flow:
            flow.close(wait=wait)

    def scoped_provider(self, server: ServerConfig, context: Mapping[str, Any], *,
                        is_current: Callable[[], bool], cancel: Cancel = lambda: False,
                        deadline: Optional[float] = None) -> "ScopedCredentialProvider":
        binding = self._binding(server, context, is_current)
        return ScopedCredentialProvider(self, binding, is_current, cancel, deadline)

    def _token(self, binding, is_current, cancel, deadline=None) -> Optional[str]:
        if not self._gate or self._closed.is_set() or not is_current():
            return None
        cancelled = lambda: cancel() or self._closed.is_set() or not is_current()
        deadline = min(deadline if deadline is not None else float("inf"),
                       time.monotonic() + HTTP_OPERATION_SECONDS)
        try:
            with self._store.transaction(binding, deadline=deadline, cancel=cancelled) as transaction:
                record = transaction.load()
                if record is None:
                    return None
                if not record.usable(self._clock()):
                    # Erase+fsync BEFORE any refresh I/O: crashes, cancellation,
                    # invalid_grant and ambiguous failures cannot replay an RT.
                    transaction.delete()
                    record = refresh_token(binding, record,
                                           self._http.operation(deadline=deadline, cancel=cancelled),
                                           now=self._clock())
                    check_operation(deadline, cancelled)
                    transaction.save(record)
                check_operation(deadline, cancelled)
                return record.access_token
        except Exception:
            # No raw server/provider errors or credentials cross this adapter.
            return None

    def invalidate(self, server: ServerConfig, context: Mapping[str, Any], *,
                   is_current: Callable[[], bool]) -> None:
        binding = self._binding(server, context, is_current)
        with self._store.transaction(binding, deadline=time.monotonic() + HTTP_OPERATION_SECONDS,
                                     cancel=lambda: self._closed.is_set() or not is_current()) as transaction:
            transaction.delete()

    def retire_owner(self, context: Mapping[str, Any], *, wait: bool = True) -> None:
        owner = AuthOwner.from_context(context)
        with self._lock:
            if self._active_command and self._active_command[0] == owner:
                self._active_command[2].set()
            keys = [key for key, flow in self._flows.items() if flow.attempt.binding.owner == owner]
        for key in keys:
            self._remove_flow(key, wait=wait)

    def shutdown(self) -> None:
        self._closed.set()
        with self._lock:
            keys = list(self._flows)
        for key in keys:
            self._remove_flow(key)


class ScopedCredentialProvider:
    """CredentialProvider-compatible immutable owner/server/endpoint lease."""

    def __init__(self, service: AuthService, binding: AuthBinding,
                 is_current: Callable[[], bool], cancel: Cancel, deadline: Optional[float] = None) -> None:
        self._service = service
        self._binding = binding
        self._is_current = is_current
        self._cancel = cancel
        self._deadline = deadline

    def bearer_token(self, credential: str, *, server_id: str) -> Optional[str]:
        if credential != self._binding.credential or server_id != self._binding.server_id:
            return None
        return self._service._token(self._binding, self._is_current, self._cancel, self._deadline)


class OwnerCredentialProvider:
    """Manager-compatible first-party provider; owner MUST come from its wrapper.

    ``is_current(owner_triple, server_config)`` and optional
    ``cancel(owner_triple, server_config)`` are host/runtime lease callbacks,
    while bearer_token's deadline/cancel belong to the individual transport request,
    never supplied by a model. Configure this once from the validated server
    catalog. It deliberately has no default-owner or ambient-token fallback.
    """

    def __init__(self, service: AuthService, servers, *,
                 is_current: Callable[[Mapping[str, Any], ServerConfig], bool],
                 cancel: Callable[[Mapping[str, Any], ServerConfig], bool] = lambda owner, server: False) -> None:
        self._service = service
        self._servers = {server.id: server for server in servers}
        self._is_current = is_current
        self._cancel = cancel

    def bearer_token(self, credential: str, *, server_id: str,
                     resource_owner: Mapping[str, Any], deadline: Optional[float] = None,
                     cancel: Cancel = lambda: False) -> Optional[str]:
        server = self._servers.get(server_id)
        if server is None or server.auth is None or credential != server.auth.credential:
            return None
        try:
            owner = dict(resource_owner)  # Freeze the exact host-issued triple.
            provider = self._service.scoped_provider(
                server, {"resource_owner": owner},
                is_current=lambda: self._is_current(owner, server),
                cancel=lambda: cancel() or self._cancel(owner, server), deadline=deadline)
            return provider.bearer_token(credential, server_id=server_id)
        except Exception:
            return None
