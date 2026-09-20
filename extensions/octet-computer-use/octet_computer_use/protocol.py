"""Canonical API 0.3 wire helpers for the computer-use extension.

The host owns the process boundary and policy. This module owns only strict
canonical framing, negotiation, and bounded JSON-RPC value validation. It
intentionally does not provide desktop, browser, screenshot, or network
transport authority.
"""

from __future__ import annotations

import json
from typing import Any, BinaryIO, Dict, Iterable, List, Mapping, Optional, Set, Tuple

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
MAX_CONTENT_PARTS = 256

ERRORS: Dict[str, Tuple[int, str]] = {
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

REQUIRED_CAPABILITIES = ["content_parts", "core", "request_cancellation", "tool_call"]
OPTIONAL_CAPABILITIES = [
    "event_bus",
    "lifecycle_events",
    "migration.adapter.v1",
    "provider_auth",
    "provider_catalog",
    "provider_stream",
    "session_lifecycle",
]
ALL_CAPABILITIES = set(REQUIRED_CAPABILITIES + OPTIONAL_CAPABILITIES + ["dynamic_tools"])
REQUIRED_METHODS = ["$/cancelRequest", "initialize", "shutdown", "tool/call"]
ALLOWED_OPTIONAL_METHODS = [
    "bus/declare",
    "bus/event",
    "bus/lifecycle",
    "bus/publish",
    "bus/subscribe",
    "bus/unsubscribe",
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
ALL_METHODS = set(REQUIRED_METHODS + ALLOWED_OPTIONAL_METHODS + [
    "context/collect",
    "tools/register",
    "tools/unregister",
])
METHOD_CAPABILITY = {
    "bus/declare": "event_bus",
    "bus/event": "event_bus",
    "bus/lifecycle": "event_bus",
    "bus/publish": "event_bus",
    "bus/subscribe": "event_bus",
    "bus/unsubscribe": "event_bus",
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
NOTIFICATION_METHODS = {"bus/event", "bus/lifecycle", "$/cancelRequest", "provider/cancel", "provider/event", "providers/complete"}
REQUEST_METHODS = ALL_METHODS - NOTIFICATION_METHODS


class ProtocolFailure(Exception):
    """A bounded failure that maps to one generated API 0.3 error."""

    def __init__(
        self,
        name: str,
        detail: str = "",
        data: Any = None,
        request_id: Any = None,
    ) -> None:
        if name not in ERRORS:
            name = "internal_error"
        self.name = name
        self.detail = detail
        self.data = data
        self.request_id = request_id
        super().__init__(detail or ERRORS[name][1])


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
            raise failure("invalid_params", "canonical JSON integer exceeds portable range")
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
    raise failure("invalid_params", f"canonical JSON value is unsupported: {type(value).__name__}")


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


def _reject_duplicate_keys(pairs: List[Tuple[str, Any]]) -> Dict[str, Any]:
    value: Dict[str, Any] = {}
    for key, item in pairs:
        if key in value:
            raise ValueError(f"duplicate object key {key!r}")
        value[key] = item
    return value


def _reject_float(value: str) -> Any:
    raise ValueError(f"floating-point value {value!r} is not canonical")


def _reject_constant(value: str) -> Any:
    raise ValueError(f"non-finite value {value!r} is not canonical")


def read_frame(stream: BinaryIO, max_frame_bytes: int = MAX_FRAME_BYTES) -> Optional[Any]:
    """Read one bounded UTF-8 canonical frame followed by exactly one LF."""
    if isinstance(max_frame_bytes, bool) or not isinstance(max_frame_bytes, int):
        raise failure("invalid_params", "max_frame_bytes must be an integer")
    if max_frame_bytes <= 0 or max_frame_bytes > MAX_FRAME_BYTES:
        raise failure("resource_exhausted", "max_frame_bytes is outside API 0.3 bounds")
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


def write_frame(stream: BinaryIO, value: Any, max_frame_bytes: int = MAX_FRAME_BYTES) -> None:
    payload = canonical_bytes(value)
    if len(payload) > max_frame_bytes:
        raise failure("resource_exhausted", "outbound frame exceeds max_frame_bytes")
    stream.write(payload + b"\n")
    stream.flush()


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


def parse_envelope(value: Any) -> Tuple[Any, str, Any, bool]:
    """Return ``(id, method, params, is_notification)``."""
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
        raise failure("invalid_request", "JSON-RPC params must not be null", request_id=request_id)
    method = value["method"]
    if not isinstance(method, str) or not method:
        raise failure("invalid_request", "JSON-RPC method must be a non-empty string", request_id=request_id)
    if _utf8_bytes(method, "JSON-RPC method") > MAX_METHOD_NAME_BYTES:
        raise failure("resource_exhausted", "JSON-RPC method exceeds max_method_name_bytes", request_id=request_id)
    if method in REQUEST_METHODS and not has_id:
        raise failure("invalid_request", f"request method {method!r} requires an id")
    if method in NOTIFICATION_METHODS and has_id:
        raise failure("invalid_request", f"notification method {method!r} forbids an id", request_id=request_id)
    return value.get("id"), method, value["params"], not has_id


def require_exact_object(value: Any, fields: Set[str], label: str) -> Dict[str, Any]:
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
    known: Set[str],
    maximum: int,
    byte_limit: int,
) -> List[str]:
    if not isinstance(value, list):
        raise failure("invalid_params", f"{label} names must be an array")
    if len(value) > maximum:
        raise failure("resource_exhausted", f"{label} count exceeds {maximum}")
    result: List[str] = []
    seen: Set[str] = set()
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


def validate_limits(value: Any, *, maximums: Optional[Mapping[str, int]] = None) -> Dict[str, int]:
    limits = require_exact_object(
        value,
        {"max_frame_bytes", "max_concurrent_requests", "max_tools"},
        "limits",
    )
    maxima = dict(maximums or {
        "max_frame_bytes": MAX_FRAME_BYTES,
        "max_concurrent_requests": MAX_CONCURRENT_REQUESTS,
        "max_tools": MAX_TOOLS,
    })
    result: Dict[str, int] = {}
    for name, maximum in maxima.items():
        item = limits[name]
        if isinstance(item, bool) or not isinstance(item, int) or item <= 0:
            raise failure("invalid_params", f"limits.{name} must be a positive integer")
        if item > maximum:
            raise failure("resource_exhausted", f"limits.{name} exceeds API 0.3 maximum")
        result[name] = item
    return result


def validate_contract_offer(value: Any) -> Dict[str, Any]:
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
    if offer["schema"] != SCHEMA_ID:
        raise failure(
            "version_mismatch",
            "host selected a different API schema",
            data={"expected": SCHEMA_ID, "received": offer["schema"]},
        )
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
        raise failure("capability_mismatch", "host required capability set differs from API 0.3")
    if set(required_capabilities) & set(optional_capabilities):
        raise failure("capability_mismatch", "a capability is both required and optional")
    if any(name not in OPTIONAL_CAPABILITIES for name in optional_capabilities):
        raise failure("capability_mismatch", "host offered an unavailable or deferred capability")
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
        raise failure("capability_mismatch", "host required method set differs from API 0.3")
    if set(required_methods) & set(optional_methods):
        raise failure("capability_mismatch", "a method is both required and optional")
    if any(name not in ALLOWED_OPTIONAL_METHODS for name in optional_methods):
        raise failure("capability_mismatch", "host offered an unavailable or deferred method")
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


def validate_contract_selection(value: Any, offer: Mapping[str, Any]) -> Dict[str, Any]:
    selection = require_exact_object(
        value,
        {"schema", "encoding", "capabilities", "methods", "limits"},
        "ContractSelection",
    )
    if selection["schema"] != SCHEMA_ID or selection["encoding"] != CANONICAL_ENCODING:
        raise failure("version_mismatch", "selected contract is not API 0.3 canonical JSON")
    capabilities = validate_name_list(
        selection["capabilities"],
        "capability",
        ALL_CAPABILITIES,
        MAX_CAPABILITIES,
        MAX_CAPABILITY_NAME_BYTES,
    )
    methods = validate_name_list(
        selection["methods"],
        "method",
        ALL_METHODS,
        MAX_METHODS,
        MAX_METHOD_NAME_BYTES,
    )
    offered_capabilities = set(offer["required_capabilities"]) | set(offer["optional_capabilities"])
    offered_methods = set(offer["required_methods"]) | set(offer["optional_methods"])
    if not set(REQUIRED_CAPABILITIES).issubset(capabilities) or not set(capabilities).issubset(offered_capabilities):
        raise failure("capability_mismatch", "selected capability set is not bounded by the host offer")
    if not set(REQUIRED_METHODS).issubset(methods) or not set(methods).issubset(offered_methods):
        raise failure("capability_mismatch", "selected method set is not bounded by the host offer")
    if set(capabilities) & set(REQUIRED_CAPABILITIES) != set(REQUIRED_CAPABILITIES):
        raise failure("capability_mismatch", "selected contract omitted a required capability")
    for method in methods:
        capability = METHOD_CAPABILITY[method]
        if capability not in capabilities:
            raise failure("capability_mismatch", f"selected method {method!r} lacks its capability")
    offer_limits = offer["limits"]
    limits = validate_limits(
        selection["limits"],
        maximums={name: offer_limits[name] for name in offer_limits},
    )
    return {
        "schema": SCHEMA_ID,
        "encoding": CANONICAL_ENCODING,
        "capabilities": capabilities,
        "methods": methods,
        "limits": limits,
    }


def negotiate_contract(offer_value: Any) -> Dict[str, Any]:
    """Select only the required, implemented API 0.3 contract."""
    offer = validate_contract_offer(offer_value)
    selection = {
        "schema": SCHEMA_ID,
        "encoding": CANONICAL_ENCODING,
        "capabilities": list(REQUIRED_CAPABILITIES),
        "methods": list(REQUIRED_METHODS),
        "limits": dict(offer["limits"]),
    }
    return validate_contract_selection(selection, offer)


def validate_initialize_request(value: Any) -> Dict[str, Any]:
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
    params = require_exact_object(value, fields, "InitializeRequest")
    if params["api_version"] != API_VERSION:
        raise failure(
            "version_mismatch",
            "the host did not request API 0.3",
            data={"expected": API_VERSION, "received": params["api_version"]},
        )
    for name in ("api_version", "octet_version", "workspace"):
        if not isinstance(params[name], str) or not params[name]:
            raise failure("invalid_params", f"InitializeRequest.{name} must be a non-empty string")
        _utf8_bytes(params[name], f"InitializeRequest.{name}")
    for name in ("extension", "capabilities", "host"):
        if params[name] is None:
            raise failure("invalid_params", f"InitializeRequest.{name} must not be null")
        validate_canonical_value(params[name])
    flag_values = params["flag_values"]
    if not isinstance(flag_values, list):
        raise failure("invalid_params", "InitializeRequest.flag_values must be an array")
    if len(flag_values) > MAX_EXTENSION_FLAGS:
        raise failure("resource_exhausted", "InitializeRequest.flag_values exceeds max_extension_flags")
    for flag in flag_values:
        flag = require_exact_object(flag, {"name", "value"}, "InitializeFlagValue")
        if not isinstance(flag["name"], str) or not flag["name"]:
            raise failure("invalid_params", "InitializeFlagValue.name must be a non-empty string")
        _utf8_bytes(flag["name"], "InitializeFlagValue.name")
        if flag["value"] is None:
            raise failure("invalid_params", "InitializeFlagValue.value must not be null")
        validate_canonical_value(flag["value"])
    if not isinstance(params["contributes"], dict):
        raise failure("invalid_params", "InitializeRequest.contributes must be an object")
    validate_canonical_value(params["contributes"])
    offer = validate_contract_offer(params["contract"])
    return dict(params, contract=offer)


def validate_tool_definition(value: Any) -> Dict[str, Any]:
    if not isinstance(value, dict):
        raise failure("invalid_params", "ToolDefinition must be an object")
    fields = {"name", "description", "parameters", "output_schema"}
    if set(value) - fields or {"name", "description", "parameters"} - set(value):
        raise failure("invalid_params", "ToolDefinition has invalid fields")
    if not isinstance(value["name"], str) or not value["name"]:
        raise failure("invalid_params", "ToolDefinition.name must be a non-empty string")
    if _utf8_bytes(value["name"], "ToolDefinition.name") > MAX_TOOL_NAME_BYTES:
        raise failure("resource_exhausted", "ToolDefinition.name exceeds its byte bound")
    if not isinstance(value["description"], str):
        raise failure("invalid_params", "ToolDefinition.description must be a string")
    if _utf8_bytes(value["description"], "ToolDefinition.description") > MAX_TOOL_DESCRIPTION_BYTES:
        raise failure("resource_exhausted", "ToolDefinition.description exceeds its byte bound")
    if value["parameters"] is None:
        raise failure("invalid_params", "ToolDefinition.parameters must not be null")
    validate_canonical_value(value["parameters"])
    if "output_schema" in value and value["output_schema"] is not None:
        validate_canonical_value(value["output_schema"])
    return value


def validate_tool_catalog(value: Any, *, max_tools: int = MAX_TOOLS) -> List[Dict[str, Any]]:
    if not isinstance(value, list):
        raise failure("invalid_params", "tools must be an array")
    if len(value) > max_tools:
        raise failure("resource_exhausted", "tool catalog exceeds max_tools")
    result: List[Dict[str, Any]] = []
    seen: Set[str] = set()
    for item in value:
        tool = validate_tool_definition(item)
        if tool["name"] in seen:
            raise failure("capability_mismatch", f"duplicate tool {tool['name']!r}")
        seen.add(tool["name"])
        result.append(tool)
    return result


def validate_tool_call_params(value: Any) -> Dict[str, Any]:
    params = require_exact_object(value, {"name", "arguments", "context"}, "ToolCallParams")
    if not isinstance(params["name"], str) or not params["name"]:
        raise failure("invalid_params", "ToolCallParams.name must be a non-empty string")
    _utf8_bytes(params["name"], "ToolCallParams.name")
    if params["arguments"] is None or params["context"] is None:
        raise failure("invalid_params", "ToolCallParams arguments and context must not be null")
    validate_canonical_value(params["arguments"])
    validate_canonical_value(params["context"])
    return params


def validate_content(value: Any) -> List[Dict[str, Any]]:
    if not isinstance(value, list):
        raise failure("invalid_params", "ToolCallResult.content must be an array")
    if len(value) > MAX_CONTENT_PARTS:
        raise failure("resource_exhausted", "tool result content exceeds max_content_parts")
    result: List[Dict[str, Any]] = []
    for part in value:
        if not isinstance(part, dict) or set(part) != {"type", "text"}:
            raise failure("invalid_params", "only text content parts are available to this extension")
        if part["type"] != "text" or not isinstance(part["text"], str):
            raise failure("invalid_params", "text content part is malformed")
        _utf8_bytes(part["text"], "content text")
        result.append(part)
    return result


def validate_tool_call_result(value: Any) -> Dict[str, Any]:
    if not isinstance(value, dict):
        raise failure("invalid_params", "ToolCallResult must be an object")
    allowed = {"content", "is_error", "metadata", "structured_content"}
    required = {"content", "is_error", "metadata"}
    if set(value) - allowed or required - set(value):
        raise failure("invalid_params", "ToolCallResult has invalid fields")
    validate_content(value["content"])
    if not isinstance(value["is_error"], bool):
        raise failure("invalid_params", "ToolCallResult.is_error must be a boolean")
    if value["metadata"] is not None:
        validate_canonical_value(value["metadata"])
    if "structured_content" in value and value["structured_content"] is not None:
        validate_canonical_value(value["structured_content"])
    return value


def validate_cancel_params(value: Any) -> Dict[str, Any]:
    if not isinstance(value, dict):
        raise failure("invalid_params", "CancelRequestParams must be an object")
    if set(value) - {"id", "reason"} or "id" not in value:
        raise failure("invalid_params", "CancelRequestParams has invalid fields")
    validate_rpc_id(value["id"])
    if "reason" in value:
        if not isinstance(value["reason"], str):
            raise failure("invalid_params", "CancelRequestParams.reason must be a string")
        if _utf8_bytes(value["reason"], "cancellation reason") > MAX_REASON_BYTES:
            raise failure("resource_exhausted", "cancellation reason exceeds max_reason_bytes")
    return value


def validate_shutdown_params(value: Any) -> Dict[str, Any]:
    return require_exact_object(value, set(), "ShutdownParams")


def error_response(request_id: Any, name: str, data: Any = None) -> Dict[str, Any]:
    if name not in ERRORS:
        name = "internal_error"
    code, message = ERRORS[name]
    error: Dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        validate_canonical_value(data)
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


def success_response(request_id: Any, result: Any) -> Dict[str, Any]:
    validate_rpc_id(request_id)
    validate_canonical_value(result)
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


class ProtocolState:
    """Small state machine that binds negotiation before any tool dispatch."""

    def __init__(self) -> None:
        self.initialized = False
        self.shutting_down = False
        self.stopped = False
        self.contract: Optional[Dict[str, Any]] = None
        self.max_frame_bytes = MAX_FRAME_BYTES

    def initialize(self, params: Any, tools: Any) -> Dict[str, Any]:
        if self.initialized or self.stopped:
            raise failure("invalid_request", "initialize may only be called once")
        request = validate_initialize_request(params)
        catalog = validate_tool_catalog(tools, max_tools=request["contract"]["limits"]["max_tools"])
        # This entry point has no flags and exposes only its declared catalog.
        if request["flag_values"]:
            raise failure("capability_mismatch", "computer-use extension declares no extension flags")
        offer = request["contract"]
        selected = negotiate_contract(offer)
        self.contract = selected
        self.max_frame_bytes = selected["limits"]["max_frame_bytes"]
        self.initialized = True
        return {
            "api_version": API_VERSION,
            "tools": catalog,
            "contract": selected,
        }

    def require_method(self, method: str) -> None:
        if self.stopped:
            raise failure("invalid_request", "extension protocol is stopped")
        if not self.initialized or self.contract is None:
            if method != "initialize":
                raise failure("unknown_method", "only initialize is available before negotiation", data={"method": method})
            return
        if method not in self.contract["methods"]:
            raise failure("unknown_method", f"method {method!r} is not negotiated", data={"method": method})

    def begin_shutdown(self) -> None:
        self.require_method("shutdown")
        self.shutting_down = True

    def finish_shutdown(self) -> None:
        if not self.shutting_down:
            raise failure("invalid_request", "shutdown was not started")
        self.stopped = True


__all__ = [
    "ALL_CAPABILITIES",
    "ALL_METHODS",
    "API_VERSION",
    "ALLOWED_OPTIONAL_METHODS",
    "CANONICAL_ENCODING",
    "ERRORS",
    "MAX_FRAME_BYTES",
    "MAX_JSON_DEPTH",
    "METHOD_CAPABILITY",
    "NOTIFICATION_METHODS",
    "OPTIONAL_CAPABILITIES",
    "ProtocolFailure",
    "ProtocolState",
    "REQUIRED_CAPABILITIES",
    "REQUIRED_METHODS",
    "SCHEMA_ID",
    "canonical_bytes",
    "error_response",
    "failure",
    "negotiate_contract",
    "parse_envelope",
    "read_frame",
    "require_exact_object",
    "success_response",
    "validate_cancel_params",
    "validate_canonical_value",
    "validate_contract_offer",
    "validate_contract_selection",
    "validate_initialize_request",
    "validate_rpc_id",
    "validate_shutdown_params",
    "validate_tool_call_params",
    "validate_tool_call_result",
    "validate_tool_catalog",
    "write_frame",
]
