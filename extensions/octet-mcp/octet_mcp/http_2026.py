"""Explicit, tools/resources MCP 2026-07-28 adapter over hardened Streamable HTTP.

Select this class only for ServerConfig.protocol_version == "2026-07-28".
The existing HTTP implementation owns networking, DNS/address policy, credentials,
framing, aggregate budgets, admission, deadlines and socket cancellation. This
adapter changes only the modern wire: discovery/per-request metadata, mirrored
headers, and bounded successful input_required continuations. It never initializes,
creates a session, sends GET/DELETE, or replays a failed/ambiguous operation.
Private elicitation requires enable_elicitation AND an actual per-call handler.

Manager integration: pass the epoch-pinned binding.input_schema as call_tool's
input_schema keyword so a catalog refresh cannot change an older call's headers.
Without that keyword, direct API callers use the current accepted catalog.
"""

from __future__ import annotations

import http.client
import threading
import time
from typing import Any, Callable, Mapping, Optional

from .catalog import CatalogError, normalize_schema, validate_arguments
from .interactions import ELIGIBLE_METHODS, InteractionHandler, run_operation
from .protocol import McpError, McpProtocolError, McpTransportError
from .protocol_2026 import (
    HeaderParameter,
    MCP_PROTOCOL_VERSION_2026,
    META_SERVER_INFO,
    header_parameters,
    modern_remote_error,
    request_headers,
    request_metadata,
    supported_versions,
)
from .streamable_http import (
    McpStreamableHttpClient,
    _HttpOperation,
    _HttpRead,
    _content_type,
    _decode_json_message,
    _matching_response_id,
    _read_bounded_body,
    _remaining_timeout,
)


class McpHttp2026Client(McpStreamableHttpClient):
    """Modern-only, explicitly configured HTTP client; legacy remains unchanged."""

    def __init__(self, config: Any, limits: Any, **kwargs: Any) -> None:
        if config.protocol_version != MCP_PROTOCOL_VERSION_2026:
            raise ValueError("modern MCP HTTP requires explicit protocolVersion 2026-07-28")
        super().__init__(config, limits, **kwargs)
        self.protocol_version = MCP_PROTOCOL_VERSION_2026
        self._discovered = False
        self._wire = threading.local()
        self._tool_schemas: dict[str, tuple[dict[str, Any], tuple[HeaderParameter, ...]]] = {}
        self.catalog_rejections = 0

    def start(self) -> None:
        with self._lock:
            if self._started:
                raise RuntimeError("MCP client is already started")
            self._started = True
            self._startup_deadline = time.monotonic() + self.config.startup_timeout_ms / 1000
        try:
            result = self.request(
                "server/discover", {}, timeout_ms=self.config.startup_timeout_ms,
                _deadline=self._startup_deadline,
            )
            versions = supported_versions(result.get("supportedVersions"))
            if MCP_PROTOCOL_VERSION_2026 not in versions:
                raise McpProtocolError(
                    "unsupported_protocol", "MCP discovery does not support the explicitly selected 2026-07-28 protocol", permanent=True
                )
            capabilities = result.get("capabilities")
            metadata = result.get("_meta", {})
            if (not isinstance(capabilities, Mapping)
                    or any(not isinstance(item, Mapping) for item in capabilities.values())
                    or not isinstance(metadata, Mapping)):
                raise McpProtocolError("invalid_discovery", "MCP discovery metadata was malformed", permanent=True)
            info = metadata.get(META_SERVER_INFO, {})
            if (not isinstance(info, Mapping) or (info and (
                not isinstance(info.get("name"), str) or not isinstance(info.get("version"), str)
            ))):
                raise McpProtocolError("invalid_discovery", "MCP discovery server identity was malformed", permanent=True)
            _remaining_timeout(self._startup_deadline)
            with self._lock:
                self.server_info = dict(info)
                self.server_capabilities = dict(capabilities)
                self._discovered = True
            # Server instructions, icons, ttl/cache scope are not promoted to
            # authority, fetched, or used to share a catalog between owners.
        except BaseException:
            self._close(terminate_session=False)
            raise

    def list_tools(self) -> list[dict[str, Any]]:
        if not self._discovered:
            raise McpProtocolError("discovery_required", "MCP discovery must complete before reading tools")
        if "tools" not in self.server_capabilities:
            with self._lock:
                self._startup_deadline = None
            return []
        with self._lock:
            deadline = self._startup_deadline or time.monotonic() + self.config.startup_timeout_ms / 1000
        raw_tools = super().list_tools()
        accepted: list[dict[str, Any]] = []
        schemas: dict[str, tuple[dict[str, Any], tuple[HeaderParameter, ...]]] = {}
        rejected = 0
        for tool in raw_tools:
            _remaining_timeout(deadline)
            try:
                raw_schema = tool.get("inputSchema")
                parameters = header_parameters(raw_schema)
                schema = normalize_schema(raw_schema, require_object=True)
                if "outputSchema" in tool:
                    normalize_schema(tool["outputSchema"], require_object=False)
            except CatalogError:
                rejected += 1
                self.logs.append(b"MCP tool excluded: unsupported schema or invalid HTTP header annotation")
                continue
            accepted.append(tool)
            schemas[tool["name"]] = (schema, parameters)
        _remaining_timeout(deadline)
        with self._lock:
            self._tool_schemas = schemas
            self.catalog_rejections = rejected
        return accepted

    def call_tool(
        self, name: str, arguments: Mapping[str, Any], *, cancellation: Any = None,
        progress: Optional[Callable[[Mapping[str, Any]], None]] = None,
        input_schema: Optional[Mapping[str, Any]] = None,
        interaction_handler: Optional[InteractionHandler] = None,
        dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> Mapping[str, Any]:
        return self.request(
            "tools/call", {"name": name, "arguments": dict(arguments)},
            timeout_ms=self.config.request_timeout_ms, cancellation=cancellation,
            progress=progress, include_progress_token=True, _input_schema=input_schema,
            interaction_handler=interaction_handler, dispatch_guard=dispatch_guard,
        )

    def request(
        self, method: str, params: Mapping[str, Any], *, timeout_ms: int,
        cancellation: Any = None, progress: Optional[Callable[[Mapping[str, Any]], None]] = None,
        include_progress_token: bool = False, _deadline: Optional[float] = None,
        _input_schema: Optional[Mapping[str, Any]] = None,
        interaction_handler: Optional[InteractionHandler] = None,
        dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> Any:
        deadline = time.monotonic() + timeout_ms / 1000
        if _deadline is not None:
            deadline = min(deadline, _deadline)
        if interaction_handler is not None and (
            not self._enable_elicitation or method not in ELIGIBLE_METHODS
            or not isinstance(interaction_handler, InteractionHandler)
            or not callable(interaction_handler.request_input) or not callable(interaction_handler.confirm)
        ):
            raise McpError("unsupported_interaction", "MCP private interactions are not enabled and bound for this operation")
        if method not in {"server/discover", "tools/list", "tools/call", "resources/list", "resources/templates/list", "resources/read"}:
            raise McpError("unsupported_interaction", "This MCP mode implements discovery, tools, and explicit resources only", permanent=True)
        if method != "server/discover" and not self._discovered:
            raise McpProtocolError("discovery_required", "MCP discovery must complete before using tools or resources")
        if method.startswith("resources/") and "resources" not in self.server_capabilities:
            raise McpError("unsupported_interaction", "MCP server does not declare resource support", permanent=True)
        parameters: tuple[HeaderParameter, ...] = ()
        outgoing = dict(params)
        if method == "tools/call":
            name = outgoing.get("name")
            if not isinstance(name, str):
                raise McpProtocolError("invalid_outbound", "MCP tool name was malformed")
            with self._lock:
                entry = self._tool_schemas.get(name)
            if _input_schema is not None:
                # Input comes from a retained host/bridge epoch, never from a
                # model argument. A refresh/removal cannot alter that contract.
                parameters = header_parameters(_input_schema)
                schema = dict(_input_schema)
            elif entry is not None:
                schema, parameters = entry
            else:
                raise McpError("unknown_tool", "MCP tool is not in the accepted HTTP catalog")
            outgoing["arguments"] = validate_arguments(outgoing.get("arguments", {}), schema)
        if "inputResponses" in outgoing or "requestState" in outgoing:
            raise McpError("unsupported_interaction", "MCP continuation fields may only come from the private operation helper", permanent=True)
        metadata = outgoing.get("_meta", {})
        if not isinstance(metadata, Mapping):
            raise McpProtocolError("invalid_outbound", "MCP request metadata was malformed")
        outgoing["_meta"] = {**dict(metadata), **request_metadata()}
        headers = request_headers(method, outgoing, parameters)
        # Capture the accepted schema/header contract ONCE for all rounds.
        # This closure, not public params or model arguments, admits continuation
        # fields. The helper owns the single handler scope and private replies.
        budget = [0, 0, 0] if method in ELIGIBLE_METHODS else None
        round_progress = progress
        if progress is not None and interaction_handler is not None:
            round_progress = lambda value: progress(interaction_handler.redact_result(value))

        def send(round_method: str, round_params: Mapping[str, Any], **kwargs: Any) -> Any:
            previous_headers = getattr(self._wire, "headers", None)
            previous_budget = getattr(self._wire, "mrtr_budget", None)
            self._wire.headers = headers
            self._wire.mrtr_budget = budget
            try:
                # Do not pass interaction_handler to the legacy base request:
                # it would claim the handler again for each individual RPC.
                return super(McpHttp2026Client, self).request(
                    round_method, round_params, progress=round_progress,
                    include_progress_token=include_progress_token,
                    dispatch_guard=dispatch_guard, **kwargs,
                )
            finally:
                self._wire.headers = previous_headers
                self._wire.mrtr_budget = previous_budget

        if budget is not None:
            return run_operation(
                send, method, outgoing, handler=interaction_handler,
                deadline=deadline, cancellation=cancellation,
            )
        return send(method, outgoing, timeout_ms=timeout_ms, _deadline=deadline, cancellation=cancellation)

    def _launch(self, task: Callable[[_HttpOperation], Any], **kwargs: Any) -> _HttpOperation:
        # Snapshot metadata into the existing bounded worker. A single shared
        # mutable header map would mix parallel calls or catalog epochs.
        headers = self._wire.headers
        budget = self._wire.mrtr_budget

        def run(operation: _HttpOperation) -> Any:
            # This budget exists only inside the eligible modern operation helper,
            # never for discovery/catalogs or because a server supplied resultType.
            operation.allow_mrtr = budget is not None
            self._wire.headers = headers
            self._wire.mrtr_budget = budget
            try:
                return task(operation)
            finally:
                self._wire.headers = None
                self._wire.mrtr_budget = None

        return super()._launch(run, **kwargs)

    def _request_headers(self, **kwargs: Any) -> tuple[dict[str, str], tuple[str, ...]]:
        headers, redactions = super()._request_headers(**kwargs)
        headers.update(self._wire.headers)
        return headers, redactions

    def _post_request(
        self, operation: _HttpOperation, payload: bytes, request_id: int, method: str, deadline: float
    ) -> list[dict[str, Any]]:
        budget = self._wire.mrtr_budget
        if budget is not None:
            operation.response_bytes, operation.event_count, operation.control_count = budget
        try:
            result = self._exchange(
                operation, verb="POST", payload=payload, expected_id=request_id,
                deadline=deadline, accept_session=False, response_required=True, phase="request",
            )
        finally:
            if budget is not None:
                # Carry raw framing/events, including ignored SSE data, across
                # continuations in addition to the helper's JSON/round budget.
                budget[:] = [operation.response_bytes, operation.event_count, operation.control_count]
        if not result.complete:
            raise McpTransportError(
                "response_interrupted", "MCP response ended without its result; modern streams cannot be resumed and the request was not replayed",
                ambiguous=method == "tools/call",
            )
        return result.messages

    def _read_response(
        self, response: http.client.HTTPResponse, *, operation: _HttpOperation,
        expected_id: Optional[int], deadline: float, response_required: bool,
        phase: str, redactions: tuple[str, ...], last_event_id: Optional[str] = None,
    ) -> _HttpRead:
        # Modern 400/404 can carry protocol errors, not just generic HTTP errors.
        # Parse through the same bounded/framing-checked/redacting body helpers.
        if response.status in {400, 404} and _content_type(response) == "application/json":
            raw = _read_bounded_body(response, operation, deadline, self.limits.max_frame_bytes)
            message = _decode_json_message(raw, redactions)
            if (expected_id is None or not _matching_response_id(message, expected_id)
                    or "error" not in message or "result" in message or "method" in message):
                raise McpProtocolError("invalid_response", "MCP HTTP error did not match its request", permanent=True)
            return _HttpRead(messages=[message], complete=True)
        # Only actual request credentials are sensitive on the modern wire.
        # A stateless SSE event ID must not rewrite server content or schemas.
        self._wire.response_redactions = redactions
        try:
            return super()._read_response(
                response, operation=operation, expected_id=expected_id, deadline=deadline,
                response_required=response_required, phase=phase, redactions=redactions, last_event_id=last_event_id,
            )
        finally:
            self._wire.response_redactions = None

    def _redactions(self, values: tuple[Optional[str], ...]) -> tuple[str, ...]:
        response_redactions = getattr(self._wire, "response_redactions", None)
        if response_redactions is not None:
            return response_redactions
        return super()._redactions(values)

    def _route_request_messages(self, messages: list[dict[str, Any]], **kwargs: Any) -> Any:
        # Validate the entire bounded sequence before emitting callbacks. No
        # unsolicited reverse request can cause a second POST in the modern era.
        request_id = kwargs["request_id"]
        terminals = 0
        for message in messages:
            if message.get("jsonrpc") != "2.0":
                raise McpProtocolError("invalid_response", "MCP HTTP response was not JSON-RPC", permanent=True)
            if "method" in message:
                if ("id" in message or not isinstance(message["method"], str)
                        or message["method"] not in {"notifications/progress", "notifications/message"}):
                    raise McpProtocolError("unsupported_interaction", "MCP server sent an unsupported reverse request or subscription notification", permanent=True)
                if "result" in message or "error" in message or not isinstance(message.get("params", {}), Mapping):
                    raise McpProtocolError("invalid_response", "MCP notification envelope was malformed", permanent=True)
            else:
                if not _matching_response_id(message, request_id) or ("result" in message) == ("error" in message):
                    raise McpProtocolError("invalid_response", "MCP response did not match its request or terminal shape", permanent=True)
                terminals += 1
        if terminals != 1:
            raise McpProtocolError("invalid_response", "MCP response must contain exactly one terminal value", permanent=True)
        # Delegate envelope/terminal/result bounds and callback routing to the
        # hardened transport, then classify modern result/error semantics.
        try:
            result = super()._route_request_messages(messages, **kwargs)
        except McpError as error:
            if error.code != "remote_error":
                raise
            terminal = next(message for message in messages if "error" in message)
            raise modern_remote_error(terminal["error"]) from None
        if not isinstance(result, Mapping):
            raise McpProtocolError("invalid_result", "MCP modern result must be an object", permanent=True)
        result_type = result.get("resultType", "complete")
        if result_type == "input_required":
            if self._wire.mrtr_budget is not None:
                return result  # Private helper validates eligible method/inputs/state.
            raise McpError("unsupported_interaction", "MCP input_required is unsupported for this method", permanent=True)
        if result_type != "complete":
            raise McpProtocolError("invalid_result_type", "MCP result type is unsupported", permanent=True)
        return result

    def _dispatch_live_elicitation(
        self, message: Mapping[str, Any], operation: _HttpOperation,
        expected_id: Optional[int], deadline: float,
    ) -> None:
        # The legacy reader routes elicitation while its POST is still open.
        # Modern elicitation MUST use input_required, never a reverse request.
        raise McpProtocolError("unsupported_interaction", "Modern MCP does not accept reverse elicitation requests", permanent=True)

    def _consume_session_header(self, response: http.client.HTTPResponse, *, accept_session: bool) -> None:
        if response.headers.get_all("Mcp-Session-Id", []):
            raise McpProtocolError("unexpected_session", "Modern MCP does not establish protocol sessions", permanent=True)
        return None

    def _remember_event_id(self, event_id: Optional[str]) -> None:
        # SSE ids confer no resume permission in 2026-07-28.
        pass

    def _send_cancellation(
        self, request_id: int, reason: str, *, dispatch_guard: Optional[Callable[[], None]] = None,
    ) -> None:
        # The base _await aborts the request socket. That is the modern HTTP
        # cancellation signal; there is no notifications/cancelled POST.
        pass

    def notify(self, method: str, params: Mapping[str, Any], **kwargs: Any) -> None:
        raise McpError("unsupported_interaction", "Modern MCP HTTP has no client notifications in this supported slice", permanent=True)
