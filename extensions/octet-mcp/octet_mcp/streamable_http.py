"""Bounded MCP Streamable HTTP client.

The transport deliberately uses only the Python standard library.  Every request
uses the exact configured endpoint (no redirects, cookies, proxy discovery, or
URL credentials), while session and SSE resumption identifiers remain process
memory only.
"""

from __future__ import annotations

from contextlib import nullcontext
from dataclasses import dataclass, field
import http.client
import ipaddress
import json
import os
import socket
import ssl
import subprocess
import sys
import threading
import time
from typing import Any, Callable, Mapping, Optional, Protocol
from urllib.parse import urlsplit

from .config import Limits, ServerConfig
from .interactions import InteractionHandler
from .protocol import (
    BoundedLog,
    CLIENT_NAME,
    CLIENT_VERSION,
    MCP_PROTOCOL_VERSION,
    MAX_CURSOR_BYTES,
    McpCancelled,
    McpError,
    McpProtocolError,
    McpRemoteError,
    McpTimeout,
    McpTransportError,
    SUPPORTED_PROTOCOL_VERSIONS,
)


MAX_HTTP_SESSION_ID_BYTES = 512
MAX_HTTP_EVENT_ID_BYTES = 1024
MAX_HTTP_EVENTS = 256
MAX_HTTP_CONTROL_REQUESTS = 4
MAX_HTTP_CONTROL_MESSAGES = 16
MAX_DNS_ADDRESSES = 64
MAX_CREDENTIAL_BYTES = 64 * 1024


class CredentialProvider(Protocol):
    """Resolve a configured non-secret credential reference at request time.

    Implementations must return an ephemeral bearer token or ``None``.  The
    bridge never stores the token, puts it in configuration, logs it, or exposes
    it through presentation/result metadata.  OAuth/browser flows are outside
    this adapter and are intentionally not implemented by this transport.
    """

    def bearer_token(
        self, credential: str, *, server_id: str, deadline: Optional[float] = None,
        cancel: Callable[[], bool] = lambda: False,
    ) -> Optional[str]:
        """Return an ephemeral token, honoring this operation's deadline/cancel.

        Refresh shares the MCP request's absolute monotonic deadline. The cancel
        callback includes transport abort/shutdown and the captured parent token;
        credential work must not continue under an independent refresh timeout.
        """


class UnavailableCredentialProvider:
    """The safe default when no credential broker was explicitly composed."""

    def bearer_token(
        self, credential: str, *, server_id: str, deadline: Optional[float] = None,
        cancel: Callable[[], bool] = lambda: False,
    ) -> Optional[str]:
        del credential, server_id, deadline, cancel
        return None


class McpAuthenticationError(McpError):
    """Authentication failed without retaining server-controlled text."""


@dataclass(frozen=True, repr=False)
class _Endpoint:
    scheme: str
    host: str
    port: Optional[int]
    target: str


@dataclass(repr=False)
class _HttpRead:
    messages: list[dict[str, Any]] = field(default_factory=list)
    complete: bool = False
    is_sse: bool = False
    last_event_id: Optional[str] = None
    retry_ms: Optional[int] = None


class _HttpOperation:
    """Sockets, killable DNS work, and aggregate budgets for one operation."""

    def __init__(self) -> None:
        self.done = threading.Event()
        self.error: Optional[BaseException] = None
        self.result: Any = None
        self.request_sent = False
        self.cancellation: Any = None
        self.dispatch_guard: Optional[Callable[[], None]] = None
        self.response_bytes = 0
        self.event_count = 0
        self.control_count = 0
        # Only the modern eligible-operation worker may enable private MRTR decoding.
        self.allow_mrtr = False
        # Set once by the originating request worker; never a mutable current-call lookup.
        self.elicitation_origin: Optional[tuple[int, str, Optional[InteractionHandler]]] = None
        self.elicitation_ids: set[tuple[type, Any]] = set()
        self.elicitation_urls: list[str] = []  # bounded by the aggregate incoming frame budget
        self._aborted = threading.Event()
        self._sockets: set[socket.socket] = set()
        self._resolvers: set[subprocess.Popen[bytes]] = set()
        self._lock = threading.Lock()

    @property
    def aborted(self) -> bool:
        return self._aborted.is_set()

    def add_socket(self, sock: socket.socket) -> None:
        with self._lock:
            if self.aborted:
                _abort_socket(sock)
                raise McpTransportError("operation_interrupted", "MCP HTTP operation was interrupted")
            self._sockets.add(sock)

    def remove_socket(self, sock: socket.socket) -> None:
        with self._lock:
            self._sockets.discard(sock)

    def start_resolver(self, host: str, port: int) -> subprocess.Popen[bytes]:
        # Register under the same lock as abort: shutdown cannot miss a child
        # launched concurrently. Exec an isolated interpreter without inherited
        # credentials, Python startup hooks, or resolver threads in this process.
        with self._lock:
            if self.aborted:
                raise McpTransportError("operation_interrupted", "MCP HTTP operation was interrupted")
            process = subprocess.Popen(
                [sys.executable, "-I", "-S", "-c", _DNS_PROGRAM, host, str(port)],
                stdin=subprocess.DEVNULL, stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                env={key: os.environ[key] for key in ("SYSTEMROOT",) if key in os.environ},
                close_fds=True,
            )
            self._resolvers.add(process)
            return process

    def finish_resolver(self, process: subprocess.Popen[bytes]) -> None:
        _kill_resolver(process)
        with self._lock:
            self._resolvers.discard(process)
        if process.stdout is not None:
            process.stdout.close()

    def consume_bytes(self, count: int, maximum: int) -> None:
        self.response_bytes += count
        if self.response_bytes > maximum:
            raise McpProtocolError(
                "http_body_too_large", "MCP HTTP response exceeded the frame limit", permanent=True
            )

    def close_sockets(self) -> None:
        with self._lock:
            sockets = tuple(self._sockets)
            self._sockets.clear()
        for sock in sockets:
            _abort_socket(sock)

    def abort(self) -> None:
        with self._lock:
            self._aborted.set()
            sockets = tuple(self._sockets)
            resolvers = tuple(self._resolvers)
        # shutdown, not just close: HTTPResponse may own a makefile reference,
        # including after HTTPConnection has detached a Connection: close socket.
        for sock in sockets:
            _abort_socket(sock)
        for process in resolvers:
            _kill_resolver(process)


class McpStreamableHttpClient:
    """One reusable MCP session over the pinned Streamable HTTP conventions.

    Requests are POSTed as JSON-RPC.  Responses may be ``application/json`` or
    ``text/event-stream``.  A prematurely closed POST SSE response is resumed
    only with its server-issued event ID through a bounded GET; the original
    JSON-RPC request is never replayed.
    """

    def __init__(
        self,
        config: ServerConfig,
        limits: Limits,
        *,
        credential_provider: Optional[CredentialProvider] = None,
        enable_elicitation: bool = False,
        on_failure: Optional[Callable[["McpStreamableHttpClient", McpError], None]] = None,
        on_tools_changed: Optional[Callable[["McpStreamableHttpClient"], None]] = None,
    ) -> None:
        if config.transport != "streamable-http" or config.url is None:
            raise ValueError("Streamable HTTP client requires a streamable-http server configuration")
        self.config = config
        self.limits = limits
        self.on_failure = on_failure
        self.on_tools_changed = on_tools_changed
        self._credential_provider = credential_provider or UnavailableCredentialProvider()
        # Host composition promises a private per-operation handler, not effect approval.
        self._enable_elicitation = enable_elicitation is True
        self._endpoint = _endpoint(config.url)
        self.logs = BoundedLog(limits.max_log_entries, limits.max_log_line_bytes)
        self._pending_slots = threading.BoundedSemaphore(limits.max_pending_requests_per_server)
        self._control_slots = threading.BoundedSemaphore(MAX_HTTP_CONTROL_REQUESTS)
        self._lock = threading.RLock()
        self._operations: set[_HttpOperation] = set()
        self._next_id = 1
        self._started = False
        self._startup_deadline: Optional[float] = None
        self._closing = False
        self._fatal: Optional[McpError] = None
        # These server-issued values are intentionally memory-only and never
        # enter logging, presentation, configuration, or result metadata.
        self._session_id: Optional[str] = None
        self._last_event_id: Optional[str] = None
        self.server_info: dict[str, Any] = {}
        self.server_capabilities: dict[str, Any] = {}
        self.protocol_version: Optional[str] = None

    @property
    def alive(self) -> bool:
        with self._lock:
            return self._started and not self._closing and self._fatal is None

    @property
    def fatal_error(self) -> Optional[McpError]:
        with self._lock:
            return self._fatal

    def start(self) -> None:
        """Initialize one explicit remote endpoint inside the startup deadline."""

        with self._lock:
            if self._started:
                raise RuntimeError("MCP client is already started")
            self._started = True
            self._startup_deadline = time.monotonic() + self.config.startup_timeout_ms / 1000
        try:
            result = self.request(
                "initialize",
                {
                    "protocolVersion": "2025-11-25" if self._enable_elicitation else MCP_PROTOCOL_VERSION,
                    "capabilities": {"elicitation": {"form": {}, "url": {}}} if self._enable_elicitation else {},
                    "clientInfo": {"name": CLIENT_NAME, "version": CLIENT_VERSION},
                },
                timeout_ms=self.config.startup_timeout_ms,
                _deadline=self._startup_deadline,
            )
            if not isinstance(result, Mapping):
                raise McpProtocolError(
                    "invalid_initialize", "MCP initialize result was not an object", permanent=True
                )
            protocol_version = result.get("protocolVersion")
            if protocol_version not in SUPPORTED_PROTOCOL_VERSIONS:
                raise McpProtocolError(
                    "unsupported_protocol",
                    "MCP server selected an unsupported protocol version",
                    permanent=True,
                )
            server_info = result.get("serverInfo", {})
            capabilities = result.get("capabilities", {})
            if not isinstance(server_info, Mapping) or not isinstance(capabilities, Mapping):
                raise McpProtocolError(
                    "invalid_initialize", "MCP initialize metadata was malformed", permanent=True
                )
            with self._lock:
                self.protocol_version = str(protocol_version)
                self.server_info = dict(server_info)
                self.server_capabilities = dict(capabilities)
            self.notify("notifications/initialized", {}, _deadline=self._startup_deadline)
        except BaseException:
            # A failed startup must not buy another deadline for DELETE/auth/DNS.
            self._close(terminate_session=False)
            raise

    def list_tools(self) -> list[dict[str, Any]]:
        """Read a bounded, cycle-checked MCP tool catalog."""

        with self._lock:
            deadline = self._startup_deadline or (time.monotonic() + self.config.startup_timeout_ms / 1000)
        cursor: Optional[str] = None
        seen_cursors: set[str] = set()
        tools: list[dict[str, Any]] = []
        names: set[str] = set()
        for _page in range(self.limits.max_catalog_pages):
            params: dict[str, Any] = {}
            if cursor is not None:
                params["cursor"] = cursor
            result = self.request(
                "tools/list", params, timeout_ms=self.config.startup_timeout_ms, _deadline=deadline
            )
            if not isinstance(result, Mapping) or not isinstance(result.get("tools"), list):
                raise McpProtocolError(
                    "invalid_catalog", "MCP tools/list result was malformed", permanent=True
                )
            for item in result["tools"]:
                if not isinstance(item, Mapping):
                    raise McpProtocolError(
                        "invalid_catalog", "MCP tool definition was malformed", permanent=True
                    )
                name = item.get("name")
                if not isinstance(name, str) or not name:
                    raise McpProtocolError(
                        "invalid_catalog", "MCP tool name was malformed", permanent=True
                    )
                if name in names:
                    raise McpProtocolError(
                        "duplicate_tool", "MCP catalog contained a duplicate tool", permanent=True
                    )
                names.add(name)
                tools.append(dict(item))
                if len(tools) > self.limits.max_tools_per_server:
                    raise McpProtocolError(
                        "catalog_too_large",
                        "MCP catalog exceeded the configured tool limit",
                        permanent=True,
                    )
            next_cursor = result.get("nextCursor")
            if next_cursor is None:
                _remaining_timeout(deadline)
                with self._lock:
                    self._startup_deadline = None
                return tools
            if (
                not isinstance(next_cursor, str)
                or not next_cursor
                or len(next_cursor.encode("utf-8")) > MAX_CURSOR_BYTES
                or next_cursor in seen_cursors
            ):
                raise McpProtocolError(
                    "invalid_cursor", "MCP catalog cursor was invalid", permanent=True
                )
            seen_cursors.add(next_cursor)
            cursor = next_cursor
        raise McpProtocolError(
            "catalog_page_limit", "MCP catalog exceeded the pagination limit", permanent=True
        )

    def call_tool(
        self,
        name: str,
        arguments: Mapping[str, Any],
        *,
        cancellation: Any = None,
        progress: Optional[Callable[[Mapping[str, Any]], None]] = None,
        interaction_handler: Optional[InteractionHandler] = None,
        dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> Mapping[str, Any]:
        result = self.request(
            "tools/call",
            {"name": name, "arguments": dict(arguments)},
            timeout_ms=self.config.request_timeout_ms,
            cancellation=cancellation,
            progress=progress,
            include_progress_token=True,
            interaction_handler=interaction_handler,
            dispatch_guard=dispatch_guard,
        )
        if not isinstance(result, Mapping):
            raise McpProtocolError("invalid_result", "MCP tool result was malformed")
        return result

    def request(
        self,
        method: str,
        params: Mapping[str, Any],
        *,
        timeout_ms: int,
        cancellation: Any = None,
        progress: Optional[Callable[[Mapping[str, Any]], None]] = None,
        include_progress_token: bool = False,
        _deadline: Optional[float] = None,
        interaction_handler: Optional[InteractionHandler] = None,
        dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> Any:
        """Send one JSON-RPC request without replaying it after uncertainty."""

        if not isinstance(method, str) or not method:
            raise ValueError("MCP request method must be non-empty")
        deadline = time.monotonic() + timeout_ms / 1000
        if _deadline is not None:
            deadline = min(deadline, _deadline)
        if interaction_handler is not None:
            if not self._enable_elicitation:
                raise McpError("unsupported_interaction", "MCP private interactions were not enabled for this connection")
            deadline = min(deadline, interaction_handler.deadline)
        scope = (interaction_handler.operation(method, deadline=deadline, cancellation=cancellation)
                 if interaction_handler else nullcontext())
        with scope:
            self._acquire_slot(deadline, cancellation)
            launched = False
            try:
                with self._lock:
                    if self._closing:
                        raise McpTransportError("server_stopped", "MCP server is stopped")
                    if self._fatal is not None:
                        raise self._fatal
                    request_id = self._next_id
                    self._next_id += 1
                progress_token = f"octet-mcp:{request_id}"
                request_params = dict(params)
                if include_progress_token:
                    metadata = request_params.get("_meta", {})
                    if not isinstance(metadata, Mapping):
                        metadata = {}
                    request_params["_meta"] = {**dict(metadata), "progressToken": progress_token}
                message = {
                    "jsonrpc": "2.0",
                    "id": request_id,
                    "method": method,
                    "params": request_params,
                }
                payload = _encode_message(message, self.limits.max_frame_bytes)

                def run_request(active: _HttpOperation) -> Any:
                    active.elicitation_origin = (request_id, method, interaction_handler)
                    active.cancellation = cancellation
                    messages = self._post_request(active, payload, request_id, method, deadline)
                    _check_operation_deadline(active, deadline)
                    private_progress = progress
                    if progress is not None and (interaction_handler is not None or active.elicitation_urls):
                        def private_progress(event: Mapping[str, Any]) -> None:
                            value = _redact_value(dict(event), tuple(active.elicitation_urls))
                            if interaction_handler is not None:
                                interaction_handler.check_active()
                                value = interaction_handler.redact_payload(value)
                            progress(value)
                    result = self._route_request_messages(
                        messages, request_id=request_id, progress_token=progress_token,
                        progress=private_progress, operation=active, deadline=deadline,
                    )
                    return _redact_value(result, tuple(active.elicitation_urls)) if active.elicitation_urls else result

                operation = self._launch(run_request, release_slot=True, dispatch_guard=dispatch_guard)
                launched = True
                result = self._await(
                    operation, deadline, cancellation=cancellation,
                    cancellation_request=(request_id, method),
                )
                if interaction_handler:
                    interaction_handler.check_active()
                    return interaction_handler.redact_result(result, method=method)
                return result
            except (McpProtocolError, McpTransportError, McpAuthenticationError) as error:
                self._fail(error)
                raise
            finally:
                if not launched:
                    self._pending_slots.release()

    def notify(
        self, method: str, params: Mapping[str, Any], *, _deadline: Optional[float] = None
    ) -> None:
        """Send one bounded notification and accept the protocol's 202 response."""

        if not isinstance(method, str) or not method:
            raise ValueError("MCP notification method must be non-empty")
        deadline = time.monotonic() + self.config.request_timeout_ms / 1000
        if _deadline is not None:
            deadline = min(deadline, _deadline)
        self._acquire_slot(deadline, None)
        launched = False
        try:
            payload = _encode_message(
                {"jsonrpc": "2.0", "method": method, "params": dict(params)},
                self.limits.max_frame_bytes,
            )
            operation = self._launch(
                lambda active: self._exchange(
                    active,
                    verb="POST",
                    payload=payload,
                    expected_id=None,
                    deadline=deadline,
                    accept_session=False,
                    response_required=False,
                    phase="notification",
                ),
                release_slot=True,
            )
            launched = True
            self._await(operation, deadline)
        except (McpProtocolError, McpTransportError, McpAuthenticationError) as error:
            self._fail(error)
            raise
        finally:
            if not launched:
                self._pending_slots.release()

    def close(self) -> None:
        """Abort active sockets and best-effort DELETE the negotiated session."""

        self._close(terminate_session=True)

    def _close(self, *, terminate_session: bool) -> None:
        deadline = (time.monotonic() + self.limits.shutdown_timeout_ms / 1000
                    if terminate_session else time.monotonic())
        with self._lock:
            if self._closing:
                return
            self._closing = True
            if self._startup_deadline is not None:
                deadline = min(deadline, self._startup_deadline)
            operations = tuple(self._operations)
            session_id = self._session_id
        for operation in operations:
            operation.abort()

        # Streamable HTTP permits (but does not require) session termination.
        # It is deliberately bounded and ignores unavailable/revoked credentials.
        if terminate_session and session_id is not None and time.monotonic() < deadline:
            try:
                operation = self._launch(
                    lambda active: self._exchange(
                        active,
                        verb="DELETE",
                        payload=None,
                        expected_id=None,
                        deadline=deadline,
                        accept_session=False,
                        response_required=False,
                        phase="delete",
                        allow_closing=True,
                    ),
                    release_slot=False,
                    allow_closing=True,
                )
                self._await(operation, deadline)
            except McpError:
                pass
        for operation in operations:
            operation.done.wait(max(0.0, deadline - time.monotonic()))
        with self._lock:
            self._session_id = None
            self._last_event_id = None

    def _post_request(
        self,
        operation: _HttpOperation,
        payload: bytes,
        request_id: int,
        method: str,
        deadline: float,
    ) -> list[dict[str, Any]]:
        first = self._exchange(
            operation,
            verb="POST",
            payload=payload,
            expected_id=request_id,
            deadline=deadline,
            accept_session=method == "initialize",
            response_required=True,
            phase="request",
        )
        if first.complete:
            return first.messages
        # A direct JSON response without the matching ID is malformed. A closed
        # SSE response can be resumed only with its server-issued event ID; when
        # there is no ID, surface a transport loss so the lifecycle can build a
        # fresh session without replaying the uncertain request.
        if not first.is_sse:
            raise McpProtocolError(
                "missing_response", "MCP HTTP response did not contain the request result", permanent=True
            )
        if first.last_event_id is None:
            raise McpTransportError(
                "sse_response_interrupted",
                "MCP response stream closed before its result and cannot be resumed safely",
                ambiguous=method == "tools/call",
            )
        messages = list(first.messages)
        last_event_id = first.last_event_id
        retry_ms = first.retry_ms
        for _attempt in range(self.config.max_restarts):
            self._wait_for_resumption(operation, deadline, retry_ms)
            resumed = self._exchange(
                operation,
                verb="GET",
                payload=None,
                expected_id=request_id,
                deadline=deadline,
                accept_session=False,
                response_required=True,
                phase="resume",
                last_event_id=last_event_id,
            )
            messages.extend(resumed.messages)
            if resumed.complete:
                return messages
            if resumed.last_event_id is None:
                raise McpTransportError(
                    "sse_response_interrupted",
                    "MCP response stream reset its cursor and cannot be resumed safely",
                    ambiguous=method == "tools/call",
                )
            last_event_id = resumed.last_event_id
            retry_ms = resumed.retry_ms if resumed.retry_ms is not None else retry_ms
        raise McpTransportError(
            "sse_resumption_exhausted",
            "MCP response stream ended before a result and bounded resumption was exhausted",
            ambiguous=method == "tools/call",
        )

    def _exchange(
        self,
        operation: _HttpOperation,
        *,
        verb: str,
        payload: Optional[bytes],
        expected_id: Optional[int],
        deadline: float,
        accept_session: bool,
        response_required: bool,
        phase: str,
        last_event_id: Optional[str] = None,
        allow_closing: bool = False,
    ) -> _HttpRead:
        _check_operation_deadline(operation, deadline)
        headers, redactions = self._request_headers(
            operation=operation, deadline=deadline,
            verb=verb,
            has_payload=payload is not None,
            last_event_id=last_event_id,
            allow_closing=allow_closing,
        )
        connection: Optional[http.client.HTTPConnection] = None
        response: Optional[http.client.HTTPResponse] = None
        try:
            _check_operation_deadline(operation, deadline)
            connection = self._connection(operation, deadline)
            connection.connect()
            _check_operation_deadline(operation, deadline)
            # Host-approved owner/catalog binding may have retired during DNS/TLS.
            # This operation retains the same guard for replies and GET resumption.
            if operation.dispatch_guard is not None:
                operation.dispatch_guard()
            operation.request_sent = True
            connection.request(verb, self._endpoint.target, body=payload, headers=headers)
            _check_operation_deadline(operation, deadline)
            response = connection.getresponse()
            _check_operation_deadline(operation, deadline)
            session_id = None
            if 200 <= response.status < 300:
                session_id = self._consume_session_header(response, accept_session=accept_session)
            response_redactions = self._redactions((*redactions, session_id))
            return self._read_response(
                response,
                operation=operation,
                expected_id=expected_id,
                deadline=deadline,
                response_required=response_required,
                phase=phase,
                redactions=response_redactions,
                last_event_id=last_event_id,
            )
        except McpError:
            raise
        except http.client.IncompleteRead as error:
            raise McpProtocolError(
                "http_body_truncated", "MCP HTTP response framing was truncated", permanent=True
            ) from error
        except ssl.SSLError as error:
            raise McpProtocolError(
                "tls_failed", "MCP HTTPS connection failed certificate or TLS validation", permanent=True
            ) from error
        except (socket.timeout, TimeoutError) as error:
            raise McpTimeout(
                "request_timeout", "MCP HTTP request timed out; its external outcome was not retried",
                ambiguous=expected_id is not None,
            ) from error
        except (http.client.HTTPException, OSError, ValueError) as error:
            if operation.aborted:
                raise McpTransportError(
                    "operation_interrupted", "MCP HTTP operation was interrupted", ambiguous=expected_id is not None
                ) from error
            raise McpTransportError(
                "http_transport_lost",
                "MCP HTTP transport was lost",
                ambiguous=expected_id is not None,
            ) from error
        finally:
            if response is not None:
                _close_response(response)
            if connection is not None:
                try:
                    connection.close()
                except OSError:
                    pass
            # Header strings can contain an adapter-returned bearer token; keep
            # them local to this exchange and discard them promptly.
            headers.clear()

    def _read_response(
        self,
        response: http.client.HTTPResponse,
        *,
        operation: _HttpOperation,
        expected_id: Optional[int],
        deadline: float,
        response_required: bool,
        phase: str,
        redactions: tuple[str, ...],
        last_event_id: Optional[str] = None,
    ) -> _HttpRead:
        status = response.status
        if status == 202:
            if response_required:
                _close_response(response)
                raise McpTransportError(
                    "http_accepted_without_response",
                    "MCP HTTP endpoint accepted a request without returning its result",
                    ambiguous=expected_id is not None,
                )
            _discard_bounded_body(response, operation, deadline, self.limits.max_frame_bytes)
            return _HttpRead(complete=True)
        if not 200 <= status < 300:
            error = self._status_error(status, response, phase=phase, expected_id=expected_id)
            _close_response(response)
            raise error
        if not response_required:
            _discard_bounded_body(response, operation, deadline, self.limits.max_frame_bytes)
            return _HttpRead(complete=True)

        content_type = _content_type(response)
        if content_type == "application/json":
            if phase == "resume":
                _close_response(response)
                raise McpProtocolError(
                    "invalid_content_type",
                    "MCP SSE resumption did not return an event stream",
                    permanent=True,
                )
            raw = _read_bounded_body(response, operation, deadline, self.limits.max_frame_bytes)
            message = _decode_json_message(
                raw, redactions, mrtr_response_id=expected_id if operation.allow_mrtr else None,
            )
            if expected_id is None:
                raise McpProtocolError(
                    "unexpected_response", "MCP notification received an unexpected JSON response"
                )
            return _HttpRead(
                messages=[message], complete=_matching_response_id(message, expected_id)
            )
        if content_type == "text/event-stream":
            return self._read_sse(
                response,
                operation=operation,
                expected_id=expected_id,
                deadline=deadline,
                redactions=redactions,
                last_event_id=last_event_id,
            )
        _close_response(response)
        raise McpProtocolError(
            "invalid_content_type",
            "MCP HTTP response used an unsupported content type",
            permanent=True,
        )

    def _read_sse(
        self,
        response: http.client.HTTPResponse,
        *,
        operation: _HttpOperation,
        expected_id: Optional[int],
        deadline: float,
        redactions: tuple[str, ...],
        last_event_id: Optional[str] = None,
    ) -> _HttpRead:
        declared = _validate_content_length(response, self.limits.max_frame_bytes - operation.response_bytes)
        result = _HttpRead(is_sse=True, last_event_id=last_event_id)
        data_lines: list[str] = []
        event_type = "message"
        event_bytes = 0
        pending_event_id: Any = _MISSING

        def dispatch() -> bool:
            nonlocal data_lines, event_type, event_bytes, pending_event_id
            if pending_event_id is not _MISSING:
                result.last_event_id = pending_event_id
                self._remember_event_id(pending_event_id)
                pending_event_id = _MISSING
            if not data_lines:
                event_type = "message"
                event_bytes = 0
                return False
            operation.event_count += 1
            if operation.event_count > MAX_HTTP_EVENTS:
                raise McpProtocolError(
                    "sse_event_limit", "MCP SSE response exceeded the event limit", permanent=True
                )
            event_redactions = self._redactions((*redactions, result.last_event_id))
            # Decode before redaction: secrets may equal envelope names, IDs,
            # or JSON syntax. Rewriting raw JSON confuses the two RPC peers.
            data = "\n".join(data_lines)
            data_lines = []
            current_event_type = event_type
            event_type = "message"
            event_bytes = 0
            if current_event_type not in {"", "message"}:
                return False
            try:
                message = json.loads(data)
            except (json.JSONDecodeError, RecursionError) as error:
                raise McpProtocolError(
                    "malformed_sse_event", "MCP SSE event contained malformed JSON", permanent=True
                ) from error
            if not isinstance(message, dict) or message.get("jsonrpc") != "2.0":
                raise McpProtocolError(
                    "invalid_sse_event", "MCP SSE event was not a JSON-RPC message", permanent=True
                )
            if "method" in message and ("id" in message or message["method"] == "notifications/tools/list_changed"):
                operation.control_count += 1
                if operation.control_count > MAX_HTTP_CONTROL_MESSAGES:
                    raise McpProtocolError(
                        "http_control_limit", "MCP HTTP control-message limit exceeded", permanent=True
                    )
            if message.get("method") == "elicitation/create" and "id" in message:
                if result.complete:
                    raise McpProtocolError("unsolicited_interaction", "MCP elicitation arrived after the operation settled", permanent=True)
                # Raw protocol params go only to the exact bound live handler.
                # It validates the request envelope before any private callback;
                # reverse requests never enter buffered/public result routing.
                self._dispatch_live_elicitation(message, operation, expected_id, deadline)
                return False
            try:
                message = _redact_jsonrpc_message(
                    message, event_redactions,
                    mrtr_response_id=expected_id if operation.allow_mrtr else None,
                )
            except RecursionError as error:
                raise McpProtocolError(
                    "invalid_sse_event", "MCP SSE event exceeded the nesting limit", permanent=True
                ) from error
            result.messages.append(message)
            return expected_id is not None and _matching_response_id(message, expected_id)

        total_bytes = 0
        while True:
            _check_operation_deadline(operation, deadline)
            try:
                line = response.readline(self.limits.max_frame_bytes + 1)
            except (socket.timeout, TimeoutError) as error:
                raise McpTimeout(
                    "request_timeout",
                    "MCP SSE response timed out; its external outcome was not retried",
                    ambiguous=expected_id is not None,
                ) from error
            if not line:
                if declared is not None and total_bytes != declared:
                    raise McpProtocolError(
                        "http_body_truncated", "MCP HTTP response framing was truncated", permanent=True
                    )
                if event_bytes:
                    raise McpProtocolError(
                        "truncated_sse_event", "MCP SSE stream ended within an event", permanent=True
                    )
                break
            total_bytes += len(line)
            event_bytes += len(line)
            operation.consume_bytes(len(line), self.limits.max_frame_bytes)
            if len(line) > self.limits.max_frame_bytes or event_bytes > self.limits.max_frame_bytes:
                raise McpProtocolError(
                    "sse_event_too_large", "MCP SSE event exceeded the frame limit", permanent=True
                )
            try:
                text = line.decode("utf-8")
            except UnicodeDecodeError as error:
                raise McpProtocolError(
                    "malformed_sse_event", "MCP SSE event was not UTF-8", permanent=True
                ) from error
            if text.endswith("\n"):
                text = text[:-1]
            if text.endswith("\r"):
                text = text[:-1]
            if not text:
                if dispatch():
                    result.complete = True
                    # Finite HTTP framing must be verified even if a complete
                    # RPC result was buffered before a truncated body/chunk.
                    if declared is None and not response.chunked:
                        break
                continue
            if text.startswith(":"):
                continue
            field_name, separator, field_value = text.partition(":")
            if separator and field_value.startswith(" "):
                field_value = field_value[1:]
            if field_name == "data":
                data_lines.append(field_value)
            elif field_name == "event":
                event_type = field_value
            elif field_name == "id":
                pending_event_id = _validate_event_id(field_value)
            elif field_name == "retry" and field_value.isascii() and field_value.isdecimal():
                result.retry_ms = (self.limits.backoff_max_ms if len(field_value) > 10
                                   else min(int(field_value), self.limits.backoff_max_ms))
        return result

    def _dispatch_live_elicitation(
        self, message: Mapping[str, Any], operation: _HttpOperation,
        expected_id: Optional[int], deadline: float,
    ) -> None:
        """Service a waiting SSE peer on THIS operation, without another worker.

        Reply bytes, events, DNS, sockets and deadline share the original budget.
        Peer IDs are echoed literally and are never confused with our own ID.
        Modern MCP has no reverse requests. Stdio does not use this path.
        """
        if self.protocol_version == "2026-07-28":
            raise McpProtocolError("unsupported_interaction", "Modern MCP reverse requests are unsupported", permanent=True)
        origin = operation.elicitation_origin
        peer_id = message.get("id")
        if (type(peer_id) not in (str, int) or (isinstance(peer_id, str) and len(peer_id.encode("utf-8")) > 256)
                or set(message) - {"jsonrpc", "id", "method", "params"}):
            raise McpProtocolError("invalid_request", "MCP elicitation request was malformed", permanent=True)
        key = (type(peer_id), peer_id)
        if key in operation.elicitation_ids:
            raise McpProtocolError("duplicate_request", "MCP elicitation repeated a peer request ID; input was not replayed", permanent=True)
        operation.elicitation_ids.add(key)
        if origin is None or origin[0] != expected_id:
            raise McpProtocolError("unbound_interaction", "MCP elicitation has no exact originating operation", permanent=True)
        _, method, handler = origin
        params = message.get("params")
        if isinstance(params, Mapping) and isinstance(params.get("url"), str) and params["url"]:
            url = params["url"]
            operation.elicitation_urls.extend((url, json.dumps(url)[1:-1], json.dumps(url, ensure_ascii=False)[1:-1]))
        reply: dict[str, Any] = {"jsonrpc": "2.0", "id": peer_id}
        if handler is None or not self._enable_elicitation or method not in {"tools/call", "resources/read"}:
            reply["error"] = {"code": -32601, "message": "Method not found"}
        elif not isinstance(message.get("params"), Mapping):
            reply["error"] = {"code": -32602, "message": "Invalid params"}
        elif message["params"].get("mode") == "url" and self.protocol_version != "2025-11-25":
            reply["result"] = {"action": "decline"}
        else:
            _check_operation_deadline(operation, deadline)
            reply["result"] = handler("elicitation/create", message["params"])
            handler.check_active()
        _check_operation_deadline(operation, deadline)
        if not self._control_slots.acquire(blocking=False):
            raise McpProtocolError("http_control_limit", "MCP HTTP control-request limit exceeded", permanent=True)
        try:
            self._exchange(
                operation, verb="POST", payload=_encode_message(reply, self.limits.max_frame_bytes),
                expected_id=None, deadline=deadline, accept_session=False,
                response_required=False, phase="server_request_reply",
            )
        finally:
            self._control_slots.release()

    def _route_request_messages(
        self,
        messages: list[dict[str, Any]],
        *,
        request_id: int,
        progress_token: str,
        progress: Optional[Callable[[Mapping[str, Any]], None]],
        operation: _HttpOperation,
        deadline: float,
    ) -> Any:
        terminal: Any = _MISSING
        tools_changed = False
        for message in messages:
            _check_operation_deadline(operation, deadline)
            if message.get("jsonrpc") != "2.0":
                raise McpProtocolError(
                    "invalid_response", "MCP HTTP response was not JSON-RPC", permanent=True
                )
            if "method" in message:
                if "id" not in message and message["method"] == "notifications/tools/list_changed":
                    if tools_changed:
                        continue
                    tools_changed = True
                self._route_server_message(message, progress_token=progress_token, progress=progress,
                                           dispatch_guard=operation.dispatch_guard)
                continue
            if not _matching_response_id(message, request_id):
                self.logs.append(b"Unmatched MCP HTTP response ignored")
                continue
            has_result = "result" in message
            has_error = "error" in message
            if has_result == has_error:
                raise McpProtocolError(
                    "invalid_response", "MCP response did not have one terminal value", permanent=True
                )
            if terminal is not _MISSING:
                raise McpProtocolError(
                    "duplicate_response", "MCP HTTP response repeated a terminal result", permanent=True
                )
            if has_error:
                error = message["error"]
                if not isinstance(error, Mapping):
                    raise McpProtocolError(
                        "invalid_response", "MCP JSON-RPC error was malformed", permanent=True
                    )
                code = error.get("code")
                if isinstance(code, bool) or not isinstance(code, int):
                    raise McpProtocolError(
                        "invalid_response", "MCP JSON-RPC error code was malformed", permanent=True
                    )
                raise McpRemoteError(code)
            result = message["result"]
            _validate_result_size(result, self.limits.max_result_bytes)
            terminal = result
        if terminal is _MISSING:
            raise McpProtocolError(
                "missing_response", "MCP HTTP response did not contain the request result", permanent=True
            )
        return terminal

    def _route_server_message(
        self,
        message: Mapping[str, Any],
        *,
        progress_token: str,
        progress: Optional[Callable[[Mapping[str, Any]], None]],
        dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> None:
        method = message.get("method")
        if not isinstance(method, str) or not method:
            raise McpProtocolError(
                "invalid_sse_event", "MCP server emitted an invalid method", permanent=True
            )
        if "id" in message:
            request_id = message.get("id")
            if isinstance(request_id, bool) or not isinstance(request_id, (int, str)):
                raise McpProtocolError(
                    "invalid_request", "MCP server request id was invalid", permanent=True
                )
            self._reply_method_not_found(request_id, dispatch_guard=dispatch_guard)
            return
        params = message.get("params", {})
        if method == "notifications/tools/list_changed":
            callback = self.on_tools_changed
            if callback is not None:
                callback(self)
        elif method == "notifications/progress" and isinstance(params, Mapping):
            token = params.get("progressToken")
            if (
                progress is not None
                and not isinstance(token, bool)
                and isinstance(token, (str, int))
                and str(token) == progress_token
            ):
                try:
                    progress(dict(params))
                except Exception:
                    return
        elif method == "notifications/message":
            # Never retain untrusted remote log text (which may contain a token).
            self.logs.append(b"MCP HTTP log notification received")

    def _reply_method_not_found(self, request_id: Any, *, dispatch_guard: Optional[Callable[[], None]] = None) -> None:
        payload = _encode_message(
            {
                "jsonrpc": "2.0",
                "id": request_id,
                "error": {"code": -32601, "message": "Method not found"},
            },
            self.limits.max_frame_bytes,
        )
        deadline = time.monotonic() + min(0.5, self.limits.shutdown_timeout_ms / 1000)
        with self._lock:
            if self._startup_deadline is not None:
                deadline = min(deadline, self._startup_deadline)
        if time.monotonic() >= deadline:
            return
        try:
            self._launch(
                lambda active: self._exchange(
                    active,
                    verb="POST",
                    payload=payload,
                    expected_id=None,
                    deadline=deadline,
                    accept_session=False,
                    response_required=False,
                    phase="server_request_reply",
                ),
                release_slot=False,
                control_deadline=deadline, dispatch_guard=dispatch_guard,
            )
        except McpError as error:
            if error.code == "http_control_limit":
                raise

    def _send_cancellation(
        self, request_id: int, reason: str, *, dispatch_guard: Optional[Callable[[], None]] = None
    ) -> None:
        payload = _encode_message(
            {
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": request_id, "reason": reason[:256]},
            },
            self.limits.max_frame_bytes,
        )
        deadline = time.monotonic() + min(0.5, self.limits.shutdown_timeout_ms / 1000)
        with self._lock:
            if self._startup_deadline is not None:
                deadline = min(deadline, self._startup_deadline)
        if time.monotonic() >= deadline:
            return

        try:
            self._launch(
                lambda active: self._exchange(
                    active, verb="POST", payload=payload, expected_id=None,
                    deadline=deadline, accept_session=False, response_required=False,
                    phase="cancellation",
                ),
                release_slot=False, control_deadline=deadline, dispatch_guard=dispatch_guard,
            )
        except McpError:
            # Cancellation is best effort; no unbounded queue or post-close work.
            return

    def _await(
        self,
        operation: _HttpOperation,
        deadline: float,
        *,
        cancellation: Any = None,
        cancellation_request: Optional[tuple[int, str]] = None,
    ) -> Any:
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                operation.abort()
                if (cancellation_request is not None and operation.request_sent
                        and cancellation_request[1] != "initialize"):
                    self._send_cancellation(cancellation_request[0], "timeout", **(
                        {"dispatch_guard": operation.dispatch_guard} if operation.dispatch_guard is not None else {}
                    ))
                raise McpTimeout(
                    "request_timeout",
                    "MCP HTTP request timed out; its external outcome was not retried",
                    ambiguous=cancellation_request is not None and cancellation_request[1] == "tools/call",
                )
            if cancellation is not None and bool(getattr(cancellation, "cancelled", False)):
                operation.abort()
                if (cancellation_request is not None and operation.request_sent
                        and cancellation_request[1] != "initialize"):
                    reason = getattr(cancellation, "reason", None) or "cancelled"
                    self._send_cancellation(cancellation_request[0], str(reason), **(
                        {"dispatch_guard": operation.dispatch_guard} if operation.dispatch_guard is not None else {}
                    ))
                raise McpCancelled(
                    "request_cancelled",
                    "MCP request cancellation was forwarded; rollback is not claimed",
                    ambiguous=cancellation_request is not None and cancellation_request[1] == "tools/call",
                )
            if operation.done.wait(min(0.05, remaining)):
                # A worker may observe the same token during credential refresh
                # or framing before this waiter does. Still run the one bounded
                # cancellation path for an operation that already sent bytes.
                if cancellation is not None and bool(getattr(cancellation, "cancelled", False)):
                    continue
                if operation.error is not None:
                    if isinstance(operation.error, McpError):
                        raise operation.error
                    raise McpTransportError(
                        "http_transport_failed", "MCP HTTP transport failed", ambiguous=cancellation_request is not None
                    )
                _check_operation_deadline(operation, deadline)
                return operation.result

    def _launch(
        self,
        task: Callable[[_HttpOperation], Any],
        *,
        release_slot: bool,
        allow_closing: bool = False,
        control_deadline: Optional[float] = None,
        dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> _HttpOperation:
        operation = _HttpOperation()
        operation.dispatch_guard = dispatch_guard
        control = control_deadline is not None
        with self._lock:
            if self._closing and not allow_closing:
                raise McpTransportError("server_stopped", "MCP server is stopped")
            if control and not self._control_slots.acquire(blocking=False):
                raise McpProtocolError(
                    "http_control_limit", "MCP HTTP control-request limit exceeded", permanent=True
                )
            self._operations.add(operation)

        timer = None
        if control_deadline is not None:
            # Controls have no caller in _await. A bounded timer must interrupt
            # slow-trickled headers too, not just rely on per-recv timeouts.
            timer = threading.Timer(max(0.0, control_deadline - time.monotonic()), operation.abort)
            timer.daemon = True
            timer.name = f"mcp-{self.config.id}-http-control-deadline"

        def run() -> None:
            try:
                operation.result = task(operation)
            except BaseException as error:
                operation.error = error
            finally:
                if timer is not None:
                    timer.cancel()
                    timer.join()
                operation.close_sockets()
                with self._lock:
                    self._operations.discard(operation)
                operation.done.set()
                if release_slot:
                    self._pending_slots.release()
                if control:
                    self._control_slots.release()

        try:
            if timer is not None:
                timer.start()
            threading.Thread(
                target=run, name=f"mcp-{self.config.id}-http", daemon=True,
            ).start()
        except BaseException:
            if timer is not None:
                timer.cancel()
            with self._lock:
                self._operations.discard(operation)
            if control:
                self._control_slots.release()
            raise
        return operation

    def _acquire_slot(self, deadline: float, cancellation: Any) -> None:
        while True:
            with self._lock:
                if self._closing:
                    raise McpTransportError("server_stopped", "MCP server is stopped")
                if self._fatal is not None:
                    raise self._fatal
            if cancellation is not None and bool(getattr(cancellation, "cancelled", False)):
                raise McpCancelled(
                    "request_cancelled", "MCP request was cancelled before admission"
                )
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise McpTimeout("request_queue_timeout", "MCP request queue wait timed out")
            if self._pending_slots.acquire(timeout=min(0.05, remaining)):
                return

    def _request_headers(
        self,
        *,
        operation: _HttpOperation,
        deadline: float,
        verb: str,
        has_payload: bool,
        last_event_id: Optional[str],
        allow_closing: bool,
    ) -> tuple[dict[str, str], tuple[str, ...]]:
        with self._lock:
            if self._closing and not allow_closing:
                raise McpTransportError("server_stopped", "MCP server is stopped")
            if self._fatal is not None and not allow_closing:
                raise self._fatal
            session_id = self._session_id
            protocol_version = self.protocol_version
            remembered_event_id = self._last_event_id
        headers = {
            "Accept": "text/event-stream" if verb == "GET" else "application/json, text/event-stream",
            "User-Agent": f"{CLIENT_NAME}/{CLIENT_VERSION}",
        }
        if has_payload:
            headers["Content-Type"] = "application/json"
        if session_id is not None:
            headers["Mcp-Session-Id"] = session_id
        if protocol_version is not None:
            headers["MCP-Protocol-Version"] = protocol_version
        if last_event_id is not None:
            headers["Last-Event-ID"] = last_event_id
        token: Optional[str] = None
        if self.config.auth is not None:
            try:
                token = self._credential_provider.bearer_token(
                    self.config.auth.credential, server_id=self.config.id,
                    deadline=deadline,
                    cancel=lambda: operation.aborted or bool(getattr(operation.cancellation, "cancelled", False)),
                )
            except Exception:
                raise McpAuthenticationError(
                    "authentication_unavailable",
                    "MCP authentication is unavailable from the configured credential adapter",
                    permanent=True,
                ) from None
            finally:
                # A late token is never permission to send the MCP operation.
                _check_operation_deadline(operation, deadline)
            if not _valid_bearer_token(token):
                raise McpAuthenticationError(
                    "authentication_unavailable",
                    "MCP authentication is unavailable from the configured credential adapter",
                    permanent=True,
                )
            headers["Authorization"] = f"Bearer {token}"
        return headers, self._redactions((token, session_id, remembered_event_id, last_event_id))

    def _consume_session_header(
        self, response: http.client.HTTPResponse, *, accept_session: bool
    ) -> Optional[str]:
        values = response.headers.get_all("Mcp-Session-Id", [])
        if not values:
            return None
        if len(values) != 1:
            raise McpProtocolError(
                "invalid_session_identity",
                "MCP server returned multiple session identities",
                permanent=True,
            )
        value = _validate_session_id(values[0])
        with self._lock:
            existing = self._session_id
            if accept_session:
                if existing is not None and existing != value:
                    raise McpProtocolError(
                        "session_identity_changed",
                        "MCP server changed the negotiated session identity",
                        permanent=True,
                    )
                self._session_id = value
            elif existing is None or existing != value:
                raise McpProtocolError(
                    "session_identity_changed",
                    "MCP server changed the negotiated session identity",
                    permanent=True,
                )
        return value

    def _remember_event_id(self, event_id: Optional[str]) -> None:
        with self._lock:
            self._last_event_id = event_id

    def _redactions(self, values: tuple[Optional[str], ...]) -> tuple[str, ...]:
        return tuple(value for value in values if isinstance(value, str) and value)

    def _connection(self, operation: _HttpOperation, deadline: float) -> http.client.HTTPConnection:
        addresses = _resolve_addresses(self._endpoint, operation, deadline)
        # Choose once before sending: never retry/re-POST an uncertain request.
        return _PinnedConnection(self._endpoint, addresses[0], operation, deadline)

    def _wait_for_resumption(
        self, operation: _HttpOperation, deadline: float, retry_ms: Optional[int]
    ) -> None:
        delay = (retry_ms or 0) / 1000
        while delay > 0:
            _check_operation_deadline(operation, deadline)
            step = min(0.05, delay, max(0.0, deadline - time.monotonic()))
            if step <= 0:
                _check_operation_deadline(operation, deadline)
            time.sleep(step)
            delay -= step

    def _status_error(
        self,
        status: int,
        response: http.client.HTTPResponse,
        *,
        phase: str,
        expected_id: Optional[int],
    ) -> McpError:
        ambiguous = expected_id is not None
        if 300 <= status < 400:
            return McpProtocolError(
                "redirect_rejected",
                "MCP HTTP endpoint returned a redirect, which the configured origin policy rejects",
                permanent=True,
            )
        if status in {401, 403}:
            return McpAuthenticationError(
                "authentication_required" if status == 401 else "authentication_denied",
                "MCP HTTP authentication was required or denied",
                permanent=True,
            )
        if phase == "resume" and status == 405:
            return McpTransportError(
                "sse_resumption_unavailable",
                "MCP server did not allow SSE response resumption",
                ambiguous=ambiguous,
            )
        if phase == "delete" and status == 405:
            return McpTransportError("session_delete_unsupported", "MCP session deletion is unsupported")
        if status == 404:
            with self._lock:
                had_session = self._session_id is not None
                self._session_id = None
            if had_session:
                return McpTransportError(
                    "session_expired",
                    "MCP HTTP session expired and requires a fresh connection",
                    ambiguous=ambiguous,
                )
            return McpProtocolError(
                "endpoint_not_found", "configured MCP HTTP endpoint was not found", permanent=True
            )
        if status == 429:
            return McpTransportError(
                "http_rate_limited",
                "MCP HTTP endpoint rate limited the connection",
                ambiguous=ambiguous,
                retry_after_ms=_retry_after_ms(response, self.limits.backoff_max_ms),
            )
        if 500 <= status < 600:
            return McpTransportError(
                "http_server_error", "MCP HTTP endpoint returned a transient server error", ambiguous=ambiguous
            )
        if status in {400, 405, 406, 415} or 400 <= status < 500:
            return McpProtocolError(
                "http_request_rejected", "MCP HTTP endpoint rejected the protocol request", permanent=True
            )
        return McpTransportError(
            "http_status_invalid", "MCP HTTP endpoint returned an invalid status", ambiguous=ambiguous
        )

    def _fail(self, error: McpError) -> None:
        callback: Optional[Callable[["McpStreamableHttpClient", McpError], None]]
        with self._lock:
            if self._closing or self._fatal is not None:
                return
            self._fatal = error
            operations = tuple(self._operations)
            callback = self.on_failure
        for operation in operations:
            operation.abort()
        if callback is not None:
            try:
                callback(self, error)
            except Exception:
                pass


_MISSING = object()
_JSONRPC_ENVELOPE_VALUES = frozenset({"jsonrpc", "id", "method"})

# libc DNS is not cancellable in a Python thread. A tiny isolated interpreter
# performs only resolution, emits a fixed bounded schema, and is killed/reaped
# by its operation on every exit path. Never pass credentials to this child.
_DNS_PROGRAM = f"""
import json, socket, sys
records = socket.getaddrinfo(sys.argv[1], int(sys.argv[2]), type=socket.SOCK_STREAM)
if len(records) > {MAX_DNS_ADDRESSES}:
    sys.exit(2)
addresses = [(family, sockaddr[0]) for family, kind, proto, canon, sockaddr in records
             if family in (socket.AF_INET, socket.AF_INET6) and kind == socket.SOCK_STREAM]
if any(len(address) > 128 for family, address in addresses):
    sys.exit(2)
sys.stdout.write(json.dumps(addresses))
"""


def _kill_resolver(process: subprocess.Popen[bytes]) -> None:
    if process.poll() is None:
        try:
            process.kill()
        except ProcessLookupError:
            pass
    # No detached reaper or DNS thread: SIGKILL/TerminateProcess precedes wait.
    process.wait()


def _abort_socket(sock: socket.socket) -> None:
    try:
        sock.shutdown(socket.SHUT_RDWR)
    except OSError:
        pass
    try:
        sock.close()
    except OSError:
        pass


def _resolve_dns(host: str, port: int, operation: _HttpOperation, deadline: float) -> list[tuple[int, str]]:
    _check_operation_deadline(operation, deadline)
    process = operation.start_resolver(host, port)
    try:
        while True:
            _check_operation_deadline(operation, deadline)
            try:
                raw, _ = process.communicate(timeout=min(0.05, _remaining_timeout(deadline)))
                break
            except subprocess.TimeoutExpired:
                continue
        _check_operation_deadline(operation, deadline)
        if process.returncode != 0 or len(raw) > 16 * 1024:
            raise McpTransportError("dns_failed", "MCP endpoint DNS resolution failed")
        records = json.loads(raw)
        if not isinstance(records, list) or not 1 <= len(records) <= MAX_DNS_ADDRESSES:
            raise McpTransportError("dns_failed", "MCP endpoint DNS resolution failed")
        for record in records:
            if (not isinstance(record, list) or len(record) != 2
                    or record[0] not in (socket.AF_INET, socket.AF_INET6)
                    or not isinstance(record[1], str) or len(record[1]) > 128):
                raise McpTransportError("dns_failed", "MCP endpoint DNS resolution failed")
        return [(family, address) for family, address in records]
    except (ValueError, OSError) as error:
        raise McpTransportError("dns_failed", "MCP endpoint DNS resolution failed") from error
    finally:
        operation.finish_resolver(process)


def _resolve_addresses(
    endpoint: _Endpoint, operation: _HttpOperation, deadline: float
) -> list[tuple[int, tuple[Any, ...]]]:
    _check_operation_deadline(operation, deadline)
    port = endpoint.port or (443 if endpoint.scheme == "https" else 80)
    try:
        literal = ipaddress.ip_address(endpoint.host)
    except ValueError:
        literal = None
        records = _resolve_dns(endpoint.host, port, operation, deadline)
    else:
        records = [(socket.AF_INET if literal.version == 4 else socket.AF_INET6, str(literal))]
    addresses = []
    for family, value in records:
        address = ipaddress.ip_address(value)
        embedded = getattr(address, "ipv4_mapped", None) or getattr(address, "sixtofour", None)
        reviewed_loopback = literal is not None and literal.is_loopback
        if ("%" in value or address.is_unspecified or address.is_multicast
                or (not reviewed_loopback and (not address.is_global or address.is_reserved
                    or (embedded is not None and (not embedded.is_global or embedded.is_multicast))))
                or (endpoint.scheme == "http" and not reviewed_loopback)):
            raise McpProtocolError(
                "destination_rejected", "MCP endpoint resolved to a prohibited network address", permanent=True
            )
        if (family == socket.AF_INET) != (address.version == 4):
            raise McpTransportError("dns_failed", "MCP endpoint DNS resolution failed")
        sockaddr = (str(address), port) if address.version == 4 else (str(address), port, 0, 0)
        addresses.append((family, sockaddr))
    return addresses


class _StrictHttpResponse(http.client.HTTPResponse):
    def _peek_chunked(self, n: int) -> bytes:
        # IOBase.readline uses peek. stdlib's peek swallows IncompleteRead,
        # turning a missing chunk terminator into apparently clean SSE EOF.
        chunk_left = self._get_chunk_left()
        if chunk_left is None:
            return b""
        data = self.fp.peek(chunk_left)[:chunk_left]
        if not data:
            raise http.client.IncompleteRead(b"")
        return data

    def _read_and_discard_trailer(self) -> None:
        # stdlib accepts EOF as a trailer terminator and has no trailer-count
        # bound. Require the actual empty line, under the normal header bounds.
        for _ in range(http.client._MAXHEADERS + 1):
            line = self.fp.readline(http.client._MAXLINE + 1)
            if not line.endswith(b"\r\n"):
                raise http.client.IncompleteRead(b"")
            if len(line) > http.client._MAXLINE:
                raise http.client.LineTooLong("trailer line")
            if line == b"\r\n":
                return
        raise http.client.HTTPException("too many trailers")


class _PinnedConnection(http.client.HTTPConnection):
    """Connect to the checked numeric address, retaining the original HTTP/TLS host."""

    response_class = _StrictHttpResponse

    def __init__(
        self, endpoint: _Endpoint, resolved: tuple[int, tuple[Any, ...]],
        operation: _HttpOperation, deadline: float,
    ) -> None:
        self.default_port = 443 if endpoint.scheme == "https" else 80
        super().__init__(endpoint.host, endpoint.port, timeout=_remaining_timeout(deadline))
        self._resolved = resolved
        self._operation = operation
        self._deadline = deadline
        self._context = ssl.create_default_context() if endpoint.scheme == "https" else None

    def connect(self) -> None:
        _check_operation_deadline(self._operation, self._deadline)
        family, sockaddr = self._resolved
        raw = socket.socket(family, socket.SOCK_STREAM)
        self._operation.add_socket(raw)
        try:
            raw.settimeout(_remaining_timeout(self._deadline))
            raw.connect(sockaddr)  # Numeric sockaddr: no unchecked second DNS lookup.
            _check_operation_deadline(self._operation, self._deadline)
            if self._context is not None:
                # Register the TLS socket before the potentially blocking
                # handshake; wrap_socket detaches the raw socket's descriptor.
                wrapped = self._context.wrap_socket(
                    raw, server_hostname=self.host, do_handshake_on_connect=False
                )
                self._operation.add_socket(wrapped)
                self._operation.remove_socket(raw)
                self.sock = wrapped
                wrapped.settimeout(_remaining_timeout(self._deadline))
                wrapped.do_handshake()
            else:
                self.sock = raw
            _check_operation_deadline(self._operation, self._deadline)
            self.sock.settimeout(_remaining_timeout(self._deadline))
        except BaseException:
            raw.close()
            self.close()
            raise


def _endpoint(url: str) -> _Endpoint:
    parts = urlsplit(url)
    host = parts.hostname
    if host is None:  # Config validation makes this unreachable for normal callers.
        raise ValueError("Streamable HTTP endpoint has no host")
    return _Endpoint(
        scheme=parts.scheme,
        host=host,
        port=parts.port,
        target=parts.path or "/",
    )


def _encode_message(message: Mapping[str, Any], maximum: int) -> bytes:
    try:
        payload = json.dumps(
            message, ensure_ascii=False, separators=(",", ":"), allow_nan=False
        ).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise McpProtocolError(
            "invalid_outbound", "bridge could not encode an MCP request"
        ) from error
    if len(payload) > maximum:
        raise McpProtocolError("outbound_too_large", "MCP request exceeded the frame limit")
    return payload


def _remaining_timeout(deadline: float) -> float:
    remaining = deadline - time.monotonic()
    if remaining <= 0:
        raise McpTimeout("request_timeout", "MCP HTTP request timed out")
    return max(0.001, remaining)


def _check_operation_deadline(operation: _HttpOperation, deadline: float) -> None:
    if bool(getattr(operation.cancellation, "cancelled", False)):
        raise McpCancelled("request_cancelled", "MCP request was cancelled before further transport work")
    if operation.aborted:
        raise McpTransportError("operation_interrupted", "MCP HTTP operation was interrupted")
    if time.monotonic() >= deadline:
        raise McpTimeout("request_timeout", "MCP HTTP request timed out")


def _content_type(response: http.client.HTTPResponse) -> str:
    value = response.getheader("Content-Type")
    if value is None:
        return ""
    return value.split(";", 1)[0].strip().lower()


def _validate_content_length(response: http.client.HTTPResponse, maximum: int) -> Optional[int]:
    values = response.headers.get_all("Content-Length", [])
    transfer = response.headers.get_all("Transfer-Encoding", [])
    if transfer and (values or len(transfer) != 1 or transfer[0].strip().lower() != "chunked"):
        raise McpProtocolError(
            "invalid_http_framing", "MCP HTTP response framing was ambiguous or unsupported", permanent=True
        )
    if not values:
        return None
    if len(values) != 1 or not values[0].isascii() or not values[0].isdecimal():
        raise McpProtocolError(
            "invalid_content_length", "MCP HTTP response had an invalid content length", permanent=True
        )
    if len(values[0]) > 10 or int(values[0]) > maximum:
        raise McpProtocolError(
            "http_body_too_large", "MCP HTTP response exceeded the frame limit", permanent=True
        )
    return int(values[0])


def _read_bounded_body(
    response: http.client.HTTPResponse,
    operation: _HttpOperation,
    deadline: float,
    maximum: int,
) -> bytes:
    declared = _validate_content_length(response, maximum - operation.response_bytes)
    chunks: list[bytes] = []
    total = 0
    while True:
        _check_operation_deadline(operation, deadline)
        try:
            chunk = response.read(min(64 * 1024, maximum + 1 - operation.response_bytes))
        except (socket.timeout, TimeoutError) as error:
            raise McpTimeout("request_timeout", "MCP HTTP request timed out") from error
        if not chunk:
            break
        total += len(chunk)
        operation.consume_bytes(len(chunk), maximum)
        chunks.append(chunk)
    if declared is not None and total != declared:
        raise McpProtocolError(
            "http_body_truncated", "MCP HTTP response framing was truncated", permanent=True
        )
    return b"".join(chunks)


def _discard_bounded_body(
    response: http.client.HTTPResponse,
    operation: _HttpOperation,
    deadline: float,
    maximum: int,
) -> None:
    _read_bounded_body(response, operation, deadline, maximum)


def _close_response(response: http.client.HTTPResponse) -> None:
    try:
        response.close()
    except OSError:
        pass


def _decode_json_message(
    raw: bytes, redactions: tuple[str, ...], *, mrtr_response_id: Optional[int] = None,
) -> dict[str, Any]:
    try:
        value = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError, RecursionError) as error:
        raise McpProtocolError(
            "malformed_http_body", "MCP HTTP response contained malformed JSON", permanent=True
        ) from error
    try:
        value = _redact_jsonrpc_message(value, redactions, mrtr_response_id=mrtr_response_id)
    except RecursionError as error:
        raise McpProtocolError(
            "invalid_http_body", "MCP HTTP response exceeded the nesting limit", permanent=True
        ) from error
    if not isinstance(value, dict) or value.get("jsonrpc") != "2.0":
        raise McpProtocolError(
            "invalid_http_body", "MCP HTTP response was not a JSON-RPC message", permanent=True
        )
    return value


def _matching_response_id(message: Mapping[str, Any], request_id: int) -> bool:
    value = message.get("id")
    return ("method" not in message and isinstance(value, int)
            and not isinstance(value, bool) and value == request_id)


def _validate_result_size(result: Any, maximum: int) -> None:
    try:
        encoded = json.dumps(result, separators=(",", ":"), allow_nan=False).encode("utf-8")
    except (TypeError, ValueError, RecursionError) as error:
        raise McpProtocolError(
            "invalid_response", "MCP result was not valid JSON", permanent=True
        ) from error
    if len(encoded) > maximum:
        raise McpProtocolError("result_too_large", "MCP result exceeded the configured result limit")


def _validate_session_id(value: str) -> str:
    if (
        not value
        or len(value.encode("utf-8")) > MAX_HTTP_SESSION_ID_BYTES
        or any(ord(character) < 33 or ord(character) > 126 for character in value)
    ):
        raise McpProtocolError(
            "invalid_session_identity", "MCP server returned an invalid session identity", permanent=True
        )
    return value


def _validate_event_id(value: str) -> Optional[str]:
    if not value:
        return None
    if (
        len(value.encode("utf-8")) > MAX_HTTP_EVENT_ID_BYTES
        or any(ord(character) < 33 or ord(character) > 126 for character in value)
    ):
        raise McpProtocolError(
            "invalid_event_identity", "MCP server returned an invalid SSE event identity", permanent=True
        )
    return value


def _valid_bearer_token(value: Any) -> bool:
    return (
        isinstance(value, str)
        and bool(value)
        and len(value.encode("utf-8")) <= MAX_CREDENTIAL_BYTES
        and all(33 <= ord(character) <= 126 for character in value)
    )


def _redact_text(value: str, redactions: tuple[str, ...]) -> str:
    for secret in redactions:
        value = value.replace(secret, "[redacted]")
    return value


def _redact_jsonrpc_message(
    value: Any, redactions: tuple[str, ...], *, mrtr_response_id: Optional[int] = None,
) -> Any:
    """Default-safe decoding; private MRTR requires trusted operation attribution.

    Validate result semantics before redaction can rewrite a discriminator/key.
    Only the matched modern helper may receive exact opaque continuation fields;
    neither server payloads nor reverse-request methods grant that permission.
    """
    if not isinstance(value, dict):
        return _redact_value(value, redactions)
    if "method" in value and ("result" in value or "error" in value):
        raise McpProtocolError("invalid_response", "MCP request mixed request and response fields", permanent=True)
    result = value.get("result")
    private_result = None
    if isinstance(result, dict):
        kind = result.get("resultType", "complete")
        if kind == "input_required":
            if (value.get("jsonrpc") != "2.0" or "error" in value or "params" in value
                    or (mrtr_response_id is not None and not _matching_response_id(value, mrtr_response_id))):
                raise McpProtocolError("invalid_response", "MCP continuation response did not match its request", permanent=True)
            if mrtr_response_id is None:
                # A well-formed unsupported operation result is not transport loss.
                # Reject it without publishing data or retiring unrelated operations.
                raise McpError("unsupported_interaction", "MCP input_required has no eligible modern operation", permanent=True)
            private_fields = {"resultType", "requestState", "inputRequests"}
            private_result = {
                key if key in private_fields else _redact_text(key, redactions):
                item if key in private_fields else _redact_value(item, redactions)
                for key, item in result.items()
            }
        elif kind != "complete":
            raise McpProtocolError("invalid_result_type", "MCP result type is unsupported", permanent=True)
        elif {"requestState", "inputRequests"} & set(result):
            raise McpProtocolError("invalid_result", "MCP complete result contained continuation data", permanent=True)
    return {
        key: (private_result if key == "result" and private_result is not None else
              item if key in _JSONRPC_ENVELOPE_VALUES else _redact_value(item, redactions))
        for key, item in value.items()
    }


def _redact_value(value: Any, redactions: tuple[str, ...]) -> Any:
    if isinstance(value, str):
        return _redact_text(value, redactions)
    if isinstance(value, list):
        return [_redact_value(item, redactions) for item in value]
    if isinstance(value, dict):
        return {
            _redact_text(key, redactions) if isinstance(key, str) else key: _redact_value(item, redactions)
            for key, item in value.items()
        }
    return value


def _retry_after_ms(response: http.client.HTTPResponse, maximum: int) -> Optional[int]:
    value = response.getheader("Retry-After")
    if value is None or not value.isascii() or not value.isdecimal():
        return None
    if len(value) > 10:
        return maximum
    return min(int(value) * 1000, maximum)
