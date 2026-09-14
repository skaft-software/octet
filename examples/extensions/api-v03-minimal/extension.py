#!/usr/bin/env python3
"""A dependency-free, ordinary-process API 0.3 extension example.

The host owns supervision and permissions.  This process owns one small echo
handler and implements only the foundation API 0.3 contract that it selects:
initialization, tool/call, cooperative cancellation, and shutdown.
"""

from __future__ import annotations

import json
import os
import sys
import threading
import time
from typing import Any, BinaryIO, Optional


API_VERSION = "0.3"
SCHEMA_ID = "octet.extension.api/0.3"
CANONICAL_ENCODING = "octet-canonical-json-v1"

MAX_PORTABLE_JSON_INTEGER = 9_007_199_254_740_991
MAX_FRAME_BYTES = 1_048_576
MAX_CONCURRENT_REQUESTS = 64
MAX_TOOLS = 256
MAX_EXTENSION_FLAGS = 64
MAX_CAPABILITIES = 32
MAX_METHODS = 64
MAX_CAPABILITY_NAME_BYTES = 64
MAX_METHOD_NAME_BYTES = 128
MAX_REASON_BYTES = 4_096
MAX_JSON_DEPTH = 32
MAX_TOOL_NAME_BYTES = 128
MAX_TOOL_DESCRIPTION_BYTES = 4_096
MAX_JSON_RPC_ID_BYTES = 256
MAX_TEXT_BYTES = 4_096
MAX_DELAY_MS = 5_000

ERRORS = {
    "parse_error": (-32_700, "parse error"),
    "invalid_request": (-32_600, "invalid request"),
    "unknown_method": (-32_601, "unknown or unnegotiated method"),
    "invalid_params": (-32_602, "invalid params"),
    "internal_error": (-32_603, "internal error"),
    "version_mismatch": (-32_010, "extension API version mismatch"),
    "capability_mismatch": (-32_011, "extension capability mismatch"),
    "resource_exhausted": (-32_012, "extension resource exhausted"),
    "request_cancelled": (-32_800, "request cancelled"),
}

REQUIRED_CAPABILITIES = [
    "content_parts",
    "core",
    "request_cancellation",
    "tool_call",
]
OPTIONAL_CAPABILITIES = [
    "lifecycle_events",
    "migration.adapter.v1",
    "provider_auth",
    "provider_catalog",
    "provider_stream",
    "session_lifecycle",
]
# These are known to the generated API, including deferred methods.  Deferred
# names are deliberately not in ALLOWED_OPTIONAL_METHODS below.
ALL_CAPABILITIES = set(REQUIRED_CAPABILITIES + OPTIONAL_CAPABILITIES + ["dynamic_tools"])

REQUIRED_METHODS = ["$/cancelRequest", "initialize", "shutdown", "tool/call"]
ALLOWED_OPTIONAL_METHODS = [
    "hook/run",
    "migration/detect",
    "migration/import",
    "provider/auth/request",
    "provider/auth/revoke",
    "provider/cancel",
    "provider/event",
    "provider/stream",
    "providers/complete",
    "providers/register",
    "providers/unregister",
    "providers/update",
    "session/create",
    "session/fork",
    "session/reload",
    "session/switch",
]
ALL_METHODS = set(
    REQUIRED_METHODS
    + ALLOWED_OPTIONAL_METHODS
    + ["context/collect", "tools/register", "tools/unregister"]
)
METHOD_CAPABILITY = {
    "$/cancelRequest": "request_cancellation",
    "context/collect": "lifecycle_events",
    "hook/run": "lifecycle_events",
    "initialize": "core",
    "migration/detect": "migration.adapter.v1",
    "migration/import": "migration.adapter.v1",
    "provider/auth/request": "provider_auth",
    "provider/auth/revoke": "provider_auth",
    "provider/cancel": "provider_stream",
    "provider/event": "provider_stream",
    "provider/stream": "provider_stream",
    "providers/complete": "provider_catalog",
    "providers/register": "provider_catalog",
    "providers/unregister": "provider_catalog",
    "providers/update": "provider_catalog",
    "session/create": "session_lifecycle",
    "session/fork": "session_lifecycle",
    "session/reload": "session_lifecycle",
    "session/switch": "session_lifecycle",
    "shutdown": "core",
    "tool/call": "tool_call",
    "tools/register": "dynamic_tools",
    "tools/unregister": "dynamic_tools",
}
SUPPORTED_METHODS = set(REQUIRED_METHODS)
# Generated API 0.3 marks these methods as notifications.  The remaining
# generated methods are requests and therefore require an ID at the envelope
# boundary, even when this example will reject them as unnegotiated.
NOTIFICATION_METHODS = {"$/cancelRequest", "provider/cancel", "provider/event", "providers/complete"}
REQUEST_METHODS = ALL_METHODS - NOTIFICATION_METHODS

TOOL_DEFINITION = {
    "name": "echo",
    "description": "Echo text, optionally waiting for cooperative cancellation.",
    "parameters": {
        "type": "object",
        "properties": {
            "text": {"type": "string", "maxLength": MAX_TEXT_BYTES},
            "delay_ms": {
                "type": "integer",
                "minimum": 0,
                "maximum": MAX_DELAY_MS,
            },
        },
        "additionalProperties": False,
    },
    "output_schema": {
        "type": "object",
        "properties": {"text": {"type": "string"}},
        "required": ["text"],
        "additionalProperties": False,
    },
}


class ProtocolFailure(Exception):
    """A bounded failure that can be represented by the API error table."""

    def __init__(
        self,
        name: str,
        detail: str = "",
        data: Any = None,
        request_id: Any = None,
    ) -> None:
        self.name = name
        self.detail = detail
        self.data = data
        self.request_id = request_id
        super().__init__(detail or name)


def failure(
    name: str,
    detail: str = "",
    data: Any = None,
    request_id: Any = None,
) -> ProtocolFailure:
    return ProtocolFailure(name, detail, data, request_id)


def _utf8_bytes(value: str, label: str) -> int:
    if any(0xD800 <= ord(character) <= 0xDFFF for character in value):
        raise failure("invalid_params", f"{label} contains an invalid surrogate")
    try:
        return len(value.encode("utf-8"))
    except UnicodeEncodeError as error:
        raise failure("invalid_params", f"{label} must be valid UTF-8") from error


def validate_canonical_value(value: Any, depth: int = 0) -> None:
    """Validate the JSON value subset accepted by octet-canonical-json-v1."""
    if depth > MAX_JSON_DEPTH:
        raise failure("invalid_params", "canonical JSON nesting exceeds max_json_depth")
    if value is None or isinstance(value, bool):
        return
    if isinstance(value, int):
        if abs(value) > MAX_PORTABLE_JSON_INTEGER:
            raise failure(
                "invalid_params",
                "canonical JSON integer exceeds portable range",
            )
        return
    if isinstance(value, float):
        raise failure("invalid_params", "canonical JSON does not permit floating-point values")
    if isinstance(value, str):
        _utf8_bytes(value, "canonical JSON string")
        return
    if isinstance(value, list):
        for item in value:
            validate_canonical_value(item, depth + 1)
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise failure("invalid_params", "canonical JSON object keys must be strings")
            _utf8_bytes(key, "canonical JSON object key")
            validate_canonical_value(item, depth + 1)
        return
    raise failure(
        "invalid_params",
        f"canonical JSON value is unsupported: {type(value).__name__}",
    )


def canonical_bytes(value: Any) -> bytes:
    validate_canonical_value(value)
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, UnicodeEncodeError, ValueError) as error:
        raise failure("invalid_params", f"cannot encode canonical JSON: {error}") from error


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    value: dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate object key {key!r}")
        value[key] = item
    return value


def _reject_float(value: str) -> Any:
    raise ValueError(f"floating-point value {value!r} is not canonical")


def _reject_constant(value: str) -> Any:
    raise ValueError(f"non-finite value {value!r} is not canonical")


def read_frame(stream: BinaryIO, max_frame_bytes: int) -> Optional[Any]:
    """Read one bounded UTF-8 canonical frame, including exactly one LF."""
    # The extra byte distinguishes a frame at the bound from one over it while
    # keeping the read itself bounded.  A missing LF is a framing failure.
    raw = stream.readline(max_frame_bytes + 2)
    if not raw:
        return None
    if len(raw) > max_frame_bytes + 1 or not raw.endswith(b"\n"):
        raise failure("invalid_request", "frame must be canonical JSON followed by exactly one LF")
    payload = raw[:-1]
    if not payload:
        raise failure("parse_error", "empty frame")
    try:
        text = payload.decode("utf-8")
        value = json.loads(
            text,
            object_pairs_hook=_reject_duplicate_keys,
            parse_float=_reject_float,
            parse_constant=_reject_constant,
        )
        validate_canonical_value(value)
        if canonical_bytes(value) != payload:
            raise ValueError("frame is not canonical JSON")
        return value
    except ProtocolFailure:
        raise
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as error:
        raise failure("parse_error", str(error)) from error


def validate_rpc_id(value: Any) -> None:
    if isinstance(value, bool):
        raise failure("invalid_request", "JSON-RPC id must be a bounded string or unsigned integer")
    if isinstance(value, int):
        if 0 <= value <= MAX_PORTABLE_JSON_INTEGER:
            return
    elif isinstance(value, str):
        if value and _utf8_bytes(value, "JSON-RPC id") <= MAX_JSON_RPC_ID_BYTES:
            return
    raise failure("invalid_request", "JSON-RPC id must be a bounded string or unsigned integer")


def _candidate_id(value: Any) -> Any:
    if isinstance(value, dict) and "id" in value:
        candidate = value["id"]
        try:
            validate_rpc_id(candidate)
        except ProtocolFailure:
            return None
        return candidate
    return None


def parse_envelope(value: Any) -> tuple[Any, str, Any, bool]:
    """Return (id, method, params, notification), rejecting envelope drift."""
    request_id = _candidate_id(value)
    if not isinstance(value, dict):
        raise failure("invalid_request", "JSON-RPC envelope must be an object")
    if value.get("jsonrpc") != "2.0":
        raise failure("invalid_request", "JSON-RPC version must be 2.0", request_id=request_id)
    has_id = "id" in value
    if has_id:
        try:
            validate_rpc_id(value["id"])
        except ProtocolFailure as error:
            error.request_id = request_id
            raise
    allowed = {"jsonrpc", "method", "params"}
    if has_id:
        allowed.add("id")
    unknown = set(value) - allowed
    if unknown:
        raise failure(
            "invalid_request",
            "JSON-RPC envelope has unknown fields",
            data={"fields": sorted(unknown)},
            request_id=request_id,
        )
    if "method" not in value or "params" not in value:
        raise failure(
            "invalid_request",
            "JSON-RPC envelope requires method and params",
            request_id=request_id,
        )
    if value["params"] is None:
        raise failure(
            "invalid_request",
            "JSON-RPC params must not be null",
            request_id=request_id,
        )
    method = value["method"]
    if not isinstance(method, str) or not method:
        raise failure("invalid_request", "JSON-RPC method must be a non-empty string", request_id=request_id)
    if _utf8_bytes(method, "JSON-RPC method") > MAX_METHOD_NAME_BYTES:
        raise failure("resource_exhausted", "JSON-RPC method exceeds max_method_name_bytes", request_id=request_id)
    if method in REQUEST_METHODS and not has_id:
        raise failure("invalid_request", f"request method {method!r} requires an id")
    if method in NOTIFICATION_METHODS and has_id:
        raise failure("invalid_request", f"notification method {method!r} forbids an id", request_id=request_id)
    return (value.get("id"), method, value["params"], not has_id)


def require_exact_object(value: Any, fields: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise failure("invalid_params", f"{label} must be an object")
    unknown = set(value) - fields
    if unknown:
        raise failure("invalid_params", f"{label} has unknown fields: {sorted(unknown)}")
    missing = fields - set(value)
    if missing:
        raise failure("invalid_params", f"{label} is missing fields: {sorted(missing)}")
    return value


def validate_name_list(
    value: Any,
    label: str,
    known: set[str],
    maximum: int,
    byte_limit: int,
) -> list[str]:
    if not isinstance(value, list):
        raise failure("invalid_params", f"{label} names must be an array")
    if len(value) > maximum:
        raise failure("resource_exhausted", f"{label} count exceeds {maximum}")
    result: list[str] = []
    seen: set[str] = set()
    for name in value:
        if not isinstance(name, str) or not name:
            raise failure("invalid_params", f"invalid {label} name")
        if _utf8_bytes(name, f"{label} name") > byte_limit:
            raise failure("resource_exhausted", f"{label} name exceeds byte bound")
        if name in seen:
            raise failure("capability_mismatch", f"duplicate {label} {name!r}")
        if name not in known:
            raise failure("capability_mismatch", f"unknown {label} {name!r}", data={label: name})
        seen.add(name)
        result.append(name)
    return result


def validate_limits(value: Any) -> dict[str, int]:
    limits = require_exact_object(
        value,
        {"max_frame_bytes", "max_concurrent_requests", "max_tools"},
        "limits",
    )
    result: dict[str, int] = {}
    maxima = {
        "max_frame_bytes": MAX_FRAME_BYTES,
        "max_concurrent_requests": MAX_CONCURRENT_REQUESTS,
        "max_tools": MAX_TOOLS,
    }
    for name, maximum in maxima.items():
        item = limits[name]
        if isinstance(item, bool) or not isinstance(item, int) or item <= 0:
            raise failure("invalid_params", f"limits.{name} must be a positive integer")
        if item > maximum:
            raise failure("resource_exhausted", f"limits.{name} exceeds API 0.3 maximum")
        result[name] = item
    return result


def validate_contract_offer(value: Any) -> dict[str, Any]:
    offer = require_exact_object(
        value,
        {
            "schema",
            "encoding",
            "required_capabilities",
            "optional_capabilities",
            "required_methods",
            "optional_methods",
            "limits",
        },
        "ContractOffer",
    )
    if not isinstance(offer["schema"], str):
        raise failure("invalid_params", "ContractOffer.schema must be a string")
    if offer["schema"] != SCHEMA_ID:
        raise failure(
            "version_mismatch",
            "host selected a different API schema",
            data={"expected": SCHEMA_ID, "received": offer["schema"]},
        )
    if not isinstance(offer["encoding"], str):
        raise failure("invalid_params", "ContractOffer.encoding must be a string")
    if offer["encoding"] != CANONICAL_ENCODING:
        raise failure(
            "invalid_params",
            "host selected a different canonical encoding",
            data={"expected": CANONICAL_ENCODING, "received": offer["encoding"]},
        )
    required_capabilities = validate_name_list(
        offer["required_capabilities"],
        "capability",
        ALL_CAPABILITIES,
        MAX_CAPABILITIES,
        MAX_CAPABILITY_NAME_BYTES,
    )
    optional_capabilities = validate_name_list(
        offer["optional_capabilities"],
        "capability",
        ALL_CAPABILITIES,
        MAX_CAPABILITIES,
        MAX_CAPABILITY_NAME_BYTES,
    )
    if set(required_capabilities) != set(REQUIRED_CAPABILITIES):
        raise failure(
            "capability_mismatch",
            "host required capability set differs from API 0.3",
            data={"required_capabilities": required_capabilities},
        )
    if set(required_capabilities) & set(optional_capabilities):
        raise failure("capability_mismatch", "a capability is both required and optional")
    if any(name not in OPTIONAL_CAPABILITIES for name in optional_capabilities):
        raise failure(
            "capability_mismatch",
            "host offered an unavailable or deferred capability",
            data={"optional_capabilities": optional_capabilities},
        )

    required_methods = validate_name_list(
        offer["required_methods"],
        "method",
        ALL_METHODS,
        MAX_METHODS,
        MAX_METHOD_NAME_BYTES,
    )
    optional_methods = validate_name_list(
        offer["optional_methods"],
        "method",
        ALL_METHODS,
        MAX_METHODS,
        MAX_METHOD_NAME_BYTES,
    )
    if set(required_methods) != set(REQUIRED_METHODS):
        raise failure(
            "capability_mismatch",
            "host required method set differs from API 0.3",
            data={"required_methods": required_methods},
        )
    if set(required_methods) & set(optional_methods):
        raise failure("capability_mismatch", "a method is both required and optional")
    if any(name not in ALLOWED_OPTIONAL_METHODS for name in optional_methods):
        raise failure(
            "capability_mismatch",
            "host offered an unavailable or deferred method",
            data={"optional_methods": optional_methods},
        )
    offered_capabilities = set(required_capabilities) | set(optional_capabilities)
    offered_methods = set(required_methods) | set(optional_methods)
    for method in offered_methods:
        capability = METHOD_CAPABILITY[method]
        if capability not in offered_capabilities:
            raise failure(
                "capability_mismatch",
                f"method {method!r} lacks its offered capability",
                data={"method": method, "capability": capability},
            )
    return {
        "schema": SCHEMA_ID,
        "encoding": CANONICAL_ENCODING,
        "required_capabilities": required_capabilities,
        "optional_capabilities": optional_capabilities,
        "required_methods": required_methods,
        "optional_methods": optional_methods,
        "limits": validate_limits(offer["limits"]),
    }


def validate_tool_arguments(value: Any) -> tuple[str, int]:
    if not isinstance(value, dict):
        raise failure("invalid_params", "echo arguments must be an object")
    unknown = set(value) - {"text", "delay_ms"}
    if unknown:
        raise failure("invalid_params", f"echo arguments have unknown fields: {sorted(unknown)}")
    text = value.get("text", "hello, octet!")
    if not isinstance(text, str):
        raise failure("invalid_params", "echo.text must be a string")
    if _utf8_bytes(text, "echo.text") > MAX_TEXT_BYTES:
        raise failure("invalid_params", "echo.text exceeds maxLength")
    delay_ms = value.get("delay_ms", 0)
    if isinstance(delay_ms, bool) or not isinstance(delay_ms, int):
        raise failure("invalid_params", "echo.delay_ms must be an integer")
    if not 0 <= delay_ms <= MAX_DELAY_MS:
        raise failure("invalid_params", "echo.delay_ms is outside its supported range")
    return text, delay_ms


def error_response(request_id: Any, name: str, data: Any = None) -> dict[str, Any]:
    code, message = ERRORS[name]
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


def success_response(request_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


class ActiveCall:
    def __init__(self) -> None:
        self.cancelled = threading.Event()
        self.reason = "request cancelled"


class Extension:
    """Small API 0.3 process loop with bounded concurrent tool calls."""

    def __init__(self, stdin: BinaryIO, stdout: BinaryIO) -> None:
        self.stdin = stdin
        self.stdout = stdout
        self.max_frame_bytes = MAX_FRAME_BYTES
        self.contract: Optional[dict[str, Any]] = None
        self.initialized = False
        self.stop_event = threading.Event()
        self.output_lock = threading.Lock()
        self.active_lock = threading.Lock()
        self.active: dict[Any, ActiveCall] = {}
        self.workers: list[threading.Thread] = []

    def diagnostic(self, message: str) -> None:
        print(f"api-v03-minimal: {message}", file=sys.stderr, flush=True)

    def send(self, value: Any) -> None:
        frame = canonical_bytes(value)
        if len(frame) > self.max_frame_bytes:
            raise failure("resource_exhausted", "outbound frame exceeds max_frame_bytes")
        with self.output_lock:
            self.stdout.write(frame + b"\n")
            self.stdout.flush()

    def send_error(self, request_id: Any, error: ProtocolFailure) -> None:
        try:
            self.send(error_response(request_id, error.name, error.data))
        except (BrokenPipeError, OSError, ProtocolFailure) as send_error:
            self.diagnostic(f"could not send {error.name}: {send_error}")
            self.stop_event.set()

    def require_method(self, method: str) -> None:
        if self.contract is None or method not in self.contract["methods"]:
            raise failure("unknown_method", f"method {method!r} is not negotiated", data={"method": method})

    def initialize(self, params: Any, request_id: Any) -> None:
        fields = {
            "api_version",
            "octet_version",
            "extension",
            "workspace",
            "capabilities",
            "contributes",
            "flag_values",
            "host",
            "contract",
        }
        if not isinstance(params, dict):
            raise failure("invalid_params", "InitializeRequest must be an object")
        if "api_version" not in params:
            raise failure("invalid_params", "InitializeRequest.api_version is required")
        if not isinstance(params["api_version"], str):
            raise failure("invalid_params", "InitializeRequest.api_version must be a string")
        if params["api_version"] != API_VERSION:
            raise failure(
                "version_mismatch",
                "the host did not request API 0.3",
                data={"expected": API_VERSION, "received": params["api_version"]},
            )
        params = require_exact_object(params, fields, "InitializeRequest")
        if not isinstance(params["octet_version"], str):
            raise failure("invalid_params", "InitializeRequest.octet_version must be a string")
        if not isinstance(params["workspace"], str):
            raise failure("invalid_params", "InitializeRequest.workspace must be a string")
        for name in ("extension", "capabilities", "host"):
            if params[name] is None:
                raise failure("invalid_params", f"InitializeRequest.{name} must not be null")
        if not isinstance(params["flag_values"], list):
            raise failure("invalid_params", "InitializeRequest.flag_values must be an array")
        if len(params["flag_values"]) > MAX_EXTENSION_FLAGS:
            raise failure("resource_exhausted", "InitializeRequest.flag_values exceeds max_extension_flags")
        for flag in params["flag_values"]:
            flag = require_exact_object(flag, {"name", "value"}, "InitializeFlagValue")
            if not isinstance(flag["name"], str):
                raise failure("invalid_params", "InitializeFlagValue.name must be a string")
            if flag["value"] is None:
                raise failure("invalid_params", "InitializeFlagValue.value must not be null")
            validate_canonical_value(flag["value"])
        if params["flag_values"]:
            raise failure(
                "capability_mismatch",
                "api-v03-minimal declares no extension flags",
                data={"flag_values": params["flag_values"]},
            )
        contributes = params["contributes"]
        allowed_contribution_fields = {
            "flags",
            "tools",
            "commands",
            "shortcuts",
            "hooks",
            "ui",
            "context",
            "tool_renderers",
            "notifications",
            "confirmations",
            "presentation",
            "providers",
        }
        if not isinstance(contributes, dict):
            raise failure("invalid_params", "InitializeRequest.contributes must be an object")
        unknown_contributions = set(contributes) - allowed_contribution_fields
        if unknown_contributions:
            raise failure(
                "capability_mismatch",
                "InitializeRequest.contributes has unknown features",
                data={"features": sorted(unknown_contributions)},
            )
        if contributes.get("tools") != ["echo"]:
            raise failure(
                "capability_mismatch",
                "host contribution catalog does not contain exactly the echo tool",
                data={"tools": contributes.get("tools")},
            )
        for name in (
            "flags",
            "commands",
            "shortcuts",
            "hooks",
            "ui",
            "tool_renderers",
        ):
            if contributes.get(name):
                raise failure(
                    "capability_mismatch",
                    f"api-v03-minimal does not implement {name}",
                    data={name: contributes[name]},
                )
        for name in (
            "context",
            "notifications",
            "confirmations",
            "presentation",
            "providers",
        ):
            if contributes.get(name):
                raise failure(
                    "capability_mismatch",
                    f"api-v03-minimal does not implement {name}",
                    data={name: contributes[name]},
                )
        offer = validate_contract_offer(params["contract"])
        self.contract = {
            "capabilities": set(offer["required_capabilities"]),
            "methods": set(offer["required_methods"]),
            "limits": offer["limits"],
        }
        result = {
            "api_version": API_VERSION,
            "tools": [TOOL_DEFINITION],
            "contract": {
                "schema": offer["schema"],
                "encoding": offer["encoding"],
                "capabilities": list(offer["required_capabilities"]),
                "methods": list(offer["required_methods"]),
                "limits": dict(offer["limits"]),
            },
        }
        self.send(success_response(request_id, result))
        self.max_frame_bytes = offer["limits"]["max_frame_bytes"]
        self.initialized = True

    def start_tool(self, params: Any, request_id: Any) -> None:
        self.require_method("tool/call")
        fields = {"name", "arguments", "context"}
        params = require_exact_object(params, fields, "ToolCallParams")
        if not isinstance(params["name"], str):
            raise failure("invalid_params", "ToolCallParams.name must be a string")
        if params["name"] != "echo":
            raise failure(
                "unknown_method",
                f"tool {params['name']!r} is not declared",
                data={"method": params["name"]},
            )
        if params["context"] is None:
            raise failure("invalid_params", "ToolCallParams.context must not be null")
        text, delay_ms = validate_tool_arguments(params["arguments"])
        with self.active_lock:
            if request_id in self.active:
                raise failure("invalid_request", "request id is already active")
            assert self.contract is not None
            if len(self.active) >= self.contract["limits"]["max_concurrent_requests"]:
                raise failure("resource_exhausted", "max_concurrent_requests is exhausted")
            call = ActiveCall()
            self.active[request_id] = call
        worker = threading.Thread(
            target=self.run_tool,
            args=(request_id, call, text, delay_ms),
            name=f"api-v03-tool-{request_id}",
            daemon=True,
        )
        self.workers.append(worker)
        try:
            worker.start()
        except RuntimeError as error:
            with self.active_lock:
                self.active.pop(request_id, None)
            raise failure("internal_error", str(error)) from error

    def run_tool(self, request_id: Any, call: ActiveCall, text: str, delay_ms: int) -> None:
        deadline = time.monotonic() + delay_ms / 1000.0
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            if call.cancelled.wait(min(remaining, 0.02)):
                break
        with self.active_lock:
            current = self.active.pop(request_id, None)
            if current is None:
                return
            cancelled = current.cancelled.is_set()
            reason = current.reason
        if cancelled:
            self.send_error(
                request_id,
                failure(
                    "request_cancelled",
                    data={"reason": reason},
                ),
            )
            return
        try:
            self.send(
                success_response(
                    request_id,
                    {
                        "content": [{"type": "text", "text": text}],
                        "is_error": False,
                        "metadata": {"delay_ms": delay_ms},
                        "structured_content": {"text": text},
                    },
                )
            )
        except (BrokenPipeError, OSError, ProtocolFailure) as error:
            self.diagnostic(f"could not send tool result: {error}")
            self.stop_event.set()

    def cancel(self, params: Any) -> None:
        self.require_method("$/cancelRequest")
        if not isinstance(params, dict):
            raise failure("invalid_params", "CancelRequestParams must be an object")
        unknown = set(params) - {"id", "reason"}
        if unknown:
            raise failure("invalid_params", f"CancelRequestParams has unknown fields: {sorted(unknown)}")
        if "id" not in params:
            raise failure("invalid_params", "CancelRequestParams.id is required")
        validate_rpc_id(params["id"])
        reason = params.get("reason", "request cancelled")
        if not isinstance(reason, str):
            raise failure("invalid_params", "CancelRequestParams.reason must be a string")
        if _utf8_bytes(reason, "cancellation reason") > MAX_REASON_BYTES:
            raise failure("resource_exhausted", "cancellation reason exceeds max_reason_bytes")
        with self.active_lock:
            call = self.active.get(params["id"])
            if call is not None:
                call.reason = reason or "request cancelled"
                call.cancelled.set()

    def cancel_all(self, reason: str) -> None:
        with self.active_lock:
            for call in self.active.values():
                call.reason = reason
                call.cancelled.set()

    def shutdown(self, params: Any, request_id: Any) -> None:
        self.require_method("shutdown")
        require_exact_object(params, set(), "ShutdownParams")
        self.cancel_all("shutdown")
        deadline = time.monotonic() + 1.0
        for worker in self.workers:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                break
            worker.join(remaining)
        self.send(success_response(request_id, {"terminal": "shutdown"}))
        self.stop_event.set()

    def dispatch(self, request_id: Any, method: str, params: Any, notification: bool) -> None:
        if not self.initialized:
            if method != "initialize":
                raise failure(
                    "unknown_method",
                    "only initialize is available before negotiation",
                    data={"method": method},
                    request_id=request_id,
                )
            self.initialize(params, request_id)
            return
        if method == "initialize":
            raise failure("invalid_request", "initialize may only be called once", request_id=request_id)
        if method == "tool/call":
            if notification:
                raise failure("invalid_request", "tool/call requires an id", request_id=request_id)
            self.start_tool(params, request_id)
        elif method == "$/cancelRequest":
            if not notification:
                raise failure("invalid_request", "$/cancelRequest is a notification", request_id=request_id)
            self.cancel(params)
        elif method == "shutdown":
            if notification:
                raise failure("invalid_request", "shutdown requires an id", request_id=request_id)
            self.shutdown(params, request_id)
        else:
            raise failure(
                "unknown_method",
                f"method {method!r} is not implemented or negotiated",
                data={"method": method},
                request_id=request_id,
            )

    def _reply_or_diagnose(
        self,
        request_id: Any,
        notification: bool,
        error: ProtocolFailure,
    ) -> None:
        if not notification and request_id is not None:
            self.send_error(request_id, error)
        else:
            self.diagnostic(f"{error.name}: {error.detail or ERRORS[error.name][1]}")

    def run(self) -> None:
        try:
            while not self.stop_event.is_set():
                try:
                    message = read_frame(self.stdin, self.max_frame_bytes)
                except ProtocolFailure as error:
                    # A malformed/noncanonical frame has no trustworthy ID and
                    # terminates the stream before dispatch.
                    self.diagnostic(f"{error.name}: {error.detail or ERRORS[error.name][1]}")
                    break
                if message is None:
                    break
                request_id: Any = None
                method = ""
                notification = True
                try:
                    request_id, method, params, notification = parse_envelope(message)
                    self.dispatch(request_id, method, params, notification)
                except ProtocolFailure as error:
                    if error.request_id is not None:
                        request_id = error.request_id
                        notification = False
                    self._reply_or_diagnose(request_id, notification, error)
                    if method == "initialize" or not self.initialized:
                        self.stop_event.set()
                except Exception as error:  # pragma: no cover - defensive process boundary
                    self.diagnostic(f"internal error: {error}")
                    if not notification and request_id is not None:
                        self.send_error(request_id, failure("internal_error"))
                    if method == "initialize":
                        self.stop_event.set()
        finally:
            self.cancel_all("process stopped")
            deadline = time.monotonic() + 0.25
            for worker in self.workers:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    break
                worker.join(remaining)


def main() -> int:
    expected_version = os.environ.get("OCTET_EXTENSION_API_VERSION")
    if expected_version is not None and expected_version != API_VERSION:
        print(
            f"api-v03-minimal: version mismatch: host requested {expected_version!r}, expected {API_VERSION!r}",
            file=sys.stderr,
            flush=True,
        )
        return 2
    Extension(sys.stdin.buffer, sys.stdout.buffer).run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
