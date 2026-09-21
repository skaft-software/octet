#!/usr/bin/env python3
"""API 0.3 process entrypoint for the read-only Cline migration adapter."""

from __future__ import annotations

import json
import os
import sys
from pathlib import PurePosixPath
from typing import Any, BinaryIO, Optional

from cline_import import AdapterError, detect, import_setup


API_VERSION = "0.4"
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
MAX_JSON_RPC_ID_BYTES = 256
MAX_MIGRATION_ITEMS = 128
MAX_MIGRATION_PATH_BYTES = 4_096
MAX_MIGRATION_NAME_BYTES = 128
MAX_MIGRATION_SKILL_BYTES = 131_072
MAX_MIGRATION_COMMAND_BYTES = 4_096
MAX_MIGRATION_ARGUMENT_BYTES = 16_384
MAX_MIGRATION_DIAGNOSTIC_BYTES = 4_096

_WINDOWS_RESERVED_NAMES = {
    "CON",
    "PRN",
    "AUX",
    "NUL",
    *(f"COM{index}" for index in range(1, 10)),
    *(f"LPT{index}" for index in range(1, 10)),
}
_WINDOWS_FORBIDDEN_COMPONENT_CHARS = set('<>:"|?*')


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
ALL_CAPABILITIES = set(REQUIRED_CAPABILITIES + OPTIONAL_CAPABILITIES + ["dynamic_tools"])
REQUIRED_METHODS = ["$/cancelRequest", "initialize", "shutdown", "tool/call"]
OPTIONAL_METHODS = [
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
ALL_METHODS = set(REQUIRED_METHODS + OPTIONAL_METHODS + ["context/collect", "tools/register", "tools/unregister"])
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
NOTIFICATION_METHODS = {"$/cancelRequest", "provider/cancel", "provider/event", "providers/complete"}
REQUEST_METHODS = ALL_METHODS - NOTIFICATION_METHODS


class ProtocolFailure(Exception):
    """A bounded protocol error with the API 0.3 error-table name."""

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


def failure(name: str, detail: str = "", data: Any = None, request_id: Any = None) -> ProtocolFailure:
    return ProtocolFailure(name, detail, data, request_id)


def _utf8_bytes(value: str, label: str) -> int:
    if any(0xD800 <= ord(character) <= 0xDFFF for character in value):
        raise failure("invalid_params", f"{label} contains an invalid surrogate")
    try:
        return len(value.encode("utf-8"))
    except UnicodeEncodeError as error:
        raise failure("invalid_params", f"{label} must be valid UTF-8") from error


def validate_canonical_value(value: Any, depth: int = 0) -> None:
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


def _reject_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, value in pairs:
        if key in result:
            raise ValueError("duplicate object key")
        result[key] = value
    return result


def _reject_number(value: str) -> Any:
    raise ValueError("canonical JSON does not permit floating-point values")


def read_frame(stream: BinaryIO, max_frame_bytes: int) -> Optional[Any]:
    raw = stream.readline(max_frame_bytes + 2)
    if not raw:
        return None
    if len(raw) > max_frame_bytes + 1 or not raw.endswith(b"\n"):
        raise failure("invalid_request", "frame must be canonical JSON followed by exactly one LF")
    payload = raw[:-1]
    if not payload:
        raise failure("parse_error", "empty frame")
    try:
        value = json.loads(
            payload.decode("utf-8", "strict"),
            object_pairs_hook=_reject_duplicate_keys,
            parse_float=_reject_number,
            parse_int=int,
            parse_constant=_reject_number,
        )
        validate_canonical_value(value)
        if canonical_bytes(value) != payload:
            raise ValueError("frame is not canonical JSON")
        return value
    except ProtocolFailure:
        raise
    except (UnicodeDecodeError, ValueError, TypeError, json.JSONDecodeError) as error:
        raise failure("parse_error", "invalid canonical JSON frame") from error


def validate_rpc_id(value: Any) -> None:
    if isinstance(value, bool):
        raise failure("invalid_request", "JSON-RPC id must be a bounded string or unsigned integer")
    if isinstance(value, int) and 0 <= value <= MAX_PORTABLE_JSON_INTEGER:
        return
    if isinstance(value, str) and value and _utf8_bytes(value, "JSON-RPC id") <= MAX_JSON_RPC_ID_BYTES:
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
    if set(value) - allowed:
        raise failure("invalid_request", "JSON-RPC envelope has unknown fields", request_id=request_id)
    if "method" not in value or "params" not in value:
        raise failure("invalid_request", "JSON-RPC envelope requires method and params", request_id=request_id)
    if value["params"] is None:
        raise failure("invalid_request", "JSON-RPC params must not be null", request_id=request_id)
    method = value["method"]
    if not isinstance(method, str) or not method:
        raise failure("invalid_request", "JSON-RPC method must be a non-empty string", request_id=request_id)
    if _utf8_bytes(method, "JSON-RPC method") > MAX_METHOD_NAME_BYTES:
        raise failure("resource_exhausted", "JSON-RPC method exceeds max_method_name_bytes", request_id=request_id)
    if method in REQUEST_METHODS and not has_id:
        raise failure("invalid_request", "request method requires an id", request_id=request_id)
    if method in NOTIFICATION_METHODS and has_id:
        raise failure("invalid_request", "notification method forbids an id", request_id=request_id)
    return value.get("id"), method, value["params"], not has_id


def require_exact_object(value: Any, fields: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict):
        raise failure("invalid_params", f"{label} must be an object")
    if set(value) - fields:
        raise failure("invalid_params", f"{label} has unknown fields")
    if fields - set(value):
        raise failure("invalid_params", f"{label} is missing required fields")
    return value


def validate_name_list(value: Any, label: str, known: set[str], maximum: int, byte_limit: int) -> list[str]:
    if not isinstance(value, list):
        raise failure("invalid_params", f"{label} names must be an array")
    if len(value) > maximum:
        raise failure("resource_exhausted", f"{label} count exceeds its bound")
    result: list[str] = []
    seen: set[str] = set()
    for name in value:
        if not isinstance(name, str) or not name:
            raise failure("invalid_params", f"invalid {label} name")
        if _utf8_bytes(name, f"{label} name") > byte_limit:
            raise failure("resource_exhausted", f"{label} name exceeds its bound")
        if name in seen:
            raise failure("capability_mismatch", f"duplicate {label}")
        if name not in known:
            raise failure("capability_mismatch", f"unknown {label}")
        seen.add(name)
        result.append(name)
    return result


def validate_limits(value: Any) -> dict[str, int]:
    limits = require_exact_object(value, {"max_frame_bytes", "max_concurrent_requests", "max_tools"}, "limits")
    maxima = {
        "max_frame_bytes": MAX_FRAME_BYTES,
        "max_concurrent_requests": MAX_CONCURRENT_REQUESTS,
        "max_tools": MAX_TOOLS,
    }
    result: dict[str, int] = {}
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
    if offer["schema"] != SCHEMA_ID:
        raise failure("version_mismatch", "host selected a different API schema")
    if offer["encoding"] != CANONICAL_ENCODING:
        raise failure("invalid_params", "host selected a different canonical encoding")
    required_caps = validate_name_list(
        offer["required_capabilities"], "capability", ALL_CAPABILITIES, MAX_CAPABILITIES, MAX_CAPABILITY_NAME_BYTES
    )
    optional_caps = validate_name_list(
        offer["optional_capabilities"], "capability", ALL_CAPABILITIES, MAX_CAPABILITIES, MAX_CAPABILITY_NAME_BYTES
    )
    if set(required_caps) != set(REQUIRED_CAPABILITIES):
        raise failure("capability_mismatch", "host required capability set differs from API 0.3")
    if set(required_caps) & set(optional_caps):
        raise failure("capability_mismatch", "a capability is both required and optional")
    if any(name not in OPTIONAL_CAPABILITIES for name in optional_caps):
        raise failure("capability_mismatch", "host offered an unavailable capability")
    required_methods = validate_name_list(
        offer["required_methods"], "method", ALL_METHODS, MAX_METHODS, MAX_METHOD_NAME_BYTES
    )
    optional_methods = validate_name_list(
        offer["optional_methods"], "method", ALL_METHODS, MAX_METHODS, MAX_METHOD_NAME_BYTES
    )
    if set(required_methods) != set(REQUIRED_METHODS):
        raise failure("capability_mismatch", "host required method set differs from API 0.3")
    if set(required_methods) & set(optional_methods):
        raise failure("capability_mismatch", "a method is both required and optional")
    if any(name not in OPTIONAL_METHODS for name in optional_methods):
        raise failure("capability_mismatch", "host offered an unavailable method")
    capabilities = set(required_caps) | set(optional_caps)
    methods = set(required_methods) | set(optional_methods)
    for method in methods:
        if METHOD_CAPABILITY[method] not in capabilities:
            raise failure("capability_mismatch", "an offered method lacks its capability")
    return {
        "schema": SCHEMA_ID,
        "encoding": CANONICAL_ENCODING,
        "required_capabilities": required_caps,
        "optional_capabilities": optional_caps,
        "required_methods": required_methods,
        "optional_methods": optional_methods,
        "limits": validate_limits(offer["limits"]),
    }


def select_contract(offer: dict[str, Any]) -> dict[str, Any]:
    offered_caps = set(offer["required_capabilities"]) | set(offer["optional_capabilities"])
    offered_methods = set(offer["required_methods"]) | set(offer["optional_methods"])
    if "migration.adapter.v1" not in offered_caps:
        raise failure("capability_mismatch", "host did not offer migration.adapter.v1")
    if not {"migration/detect", "migration/import"} <= offered_methods:
        raise failure("capability_mismatch", "host did not offer both migration methods")
    capabilities = REQUIRED_CAPABILITIES + ["migration.adapter.v1"]
    methods = REQUIRED_METHODS + ["migration/detect", "migration/import"]
    return {
        "schema": SCHEMA_ID,
        "encoding": CANONICAL_ENCODING,
        "capabilities": capabilities,
        "methods": methods,
        "limits": dict(offer["limits"]),
    }


def _bounded_string(value: Any, label: str, maximum: int, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        raise failure("invalid_params", f"{label} must be a non-empty string")
    size = _utf8_bytes(value, label)
    if size > maximum:
        raise failure("resource_exhausted", f"{label} exceeds its bound")
    return value


def _portable_path_component(value: str) -> bool:
    if any(character in _WINDOWS_FORBIDDEN_COMPONENT_CHARS for character in value):
        return False
    if value.endswith((".", " ")):
        return False
    return value.split(".", 1)[0].upper() not in _WINDOWS_RESERVED_NAMES


def _bounded_source_relative_path(value: Any, label: str, allow_root_marker: bool = False) -> str:
    path = _bounded_string(value, label, MAX_MIGRATION_PATH_BYTES)
    if allow_root_marker and path == "$":
        return path
    if path == "$" or path.startswith("/") or "\\" in path:
        raise failure("invalid_params", f"{label} must be source-relative")
    if any(character in "\x00\n\r\t" or ord(character) < 0x20 for character in path):
        raise failure("invalid_params", f"{label} contains an invalid path character")
    parts = path.split("/")
    if any(not part or part in (".", "..") for part in parts):
        raise failure("invalid_params", f"{label} must use normalized path components")
    if any(not _portable_path_component(part) for part in parts):
        raise failure("invalid_params", f"{label} is not portable across supported platforms")
    if PurePosixPath(path).as_posix() != path:
        raise failure("invalid_params", f"{label} must use normalized path components")
    return path


def validate_migration_detect_result(value: Any) -> dict[str, Any]:
    result = require_exact_object(value, {"detected", "config_paths", "diagnostics"}, "MigrationDetectResult")
    if not isinstance(result["detected"], bool):
        raise failure("invalid_params", "MigrationDetectResult.detected must be boolean")
    paths = result["config_paths"]
    if not isinstance(paths, list) or len(paths) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "MigrationDetectResult.config_paths exceeds its bound")
    seen: set[str] = set()
    for path in paths:
        path = _bounded_source_relative_path(path, "migration path")
        if path in seen:
            raise failure("invalid_params", "MigrationDetectResult has duplicate config paths")
        seen.add(path)
    validate_diagnostics(result["diagnostics"])
    return result


def validate_diagnostics(value: Any) -> None:
    if not isinstance(value, list) or len(value) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "migration diagnostics exceed their bound")
    for diagnostic in value:
        item = require_exact_object(diagnostic, {"path", "severity", "reason"}, "MigrationDiagnostic")
        _bounded_source_relative_path(item["path"], "migration diagnostic path", allow_root_marker=True)
        if not isinstance(item["severity"], str) or item["severity"] not in {"warning", "error"}:
            raise failure("invalid_params", "MigrationDiagnostic.severity is invalid")
        _bounded_string(item["reason"], "migration diagnostic reason", MAX_MIGRATION_DIAGNOSTIC_BYTES)


def validate_migration_import_result(value: Any) -> dict[str, Any]:
    result = require_exact_object(value, {"models", "skills", "mcp_servers", "diagnostics"}, "MigrationImportResult")
    for field in ("models", "skills", "mcp_servers"):
        if not isinstance(result[field], list) or len(result[field]) > MAX_MIGRATION_ITEMS:
            raise failure("resource_exhausted", f"migration {field} exceed their bound")
    for model in result["models"]:
        item = require_exact_object(model, {"path", "provider", "model"}, "MigrationModel")
        _bounded_source_relative_path(item["path"], "migration model path")
        _bounded_string(item["provider"], "migration provider", MAX_MIGRATION_NAME_BYTES)
        _bounded_string(item["model"], "migration model", MAX_MIGRATION_NAME_BYTES)
    for skill in result["skills"]:
        item = require_exact_object(skill, {"path", "name", "content"}, "MigrationSkill")
        _bounded_source_relative_path(item["path"], "migration skill path")
        _bounded_string(item["name"], "migration skill name", MAX_MIGRATION_NAME_BYTES)
        _bounded_string(item["content"], "migration skill content", MAX_MIGRATION_SKILL_BYTES, allow_empty=True)
    for server in result["mcp_servers"]:
        item = require_exact_object(server, {"path", "name", "command", "args"}, "MigrationMcpServer")
        _bounded_source_relative_path(item["path"], "migration MCP path")
        _bounded_string(item["name"], "migration MCP name", MAX_MIGRATION_NAME_BYTES)
        _bounded_string(item["command"], "migration MCP command", MAX_MIGRATION_COMMAND_BYTES)
        args = item["args"]
        if not isinstance(args, list) or len(args) > MAX_MIGRATION_ITEMS:
            raise failure("resource_exhausted", "migration MCP arguments exceed their bound")
        for argument in args:
            _bounded_string(argument, "migration MCP argument", MAX_MIGRATION_ARGUMENT_BYTES)
    validate_diagnostics(result["diagnostics"])
    return result


def error_response(request_id: Any, name: str) -> dict[str, Any]:
    code, message = ERRORS[name]
    return {"jsonrpc": "2.0", "id": request_id, "error": {"code": code, "message": message}}


def success_response(request_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


class Extension:
    """Single-threaded, read-only migration process."""

    def __init__(self, stdin: BinaryIO, stdout: BinaryIO) -> None:
        self.stdin = stdin
        self.stdout = stdout
        self.max_frame_bytes = MAX_FRAME_BYTES
        self.contract: Optional[dict[str, Any]] = None
        self.initialized = False
        self.stopping = False

    def diagnostic(self, message: str) -> None:
        print(f"octet-import-cline: {message}", file=sys.stderr, flush=True)

    def send(self, value: Any) -> None:
        frame = canonical_bytes(value)
        if len(frame) > self.max_frame_bytes:
            raise failure("resource_exhausted", "outbound frame exceeds max_frame_bytes")
        self.stdout.write(frame + b"\n")
        self.stdout.flush()

    def send_error(self, request_id: Any, error: ProtocolFailure) -> None:
        try:
            self.send(error_response(request_id, error.name))
        except (BrokenPipeError, OSError, ProtocolFailure) as send_error:
            self.diagnostic(f"could not send {error.name}: {send_error}")
            self.stopping = True

    def require_method(self, method: str) -> None:
        if self.contract is None or method not in self.contract["methods"]:
            raise failure("unknown_method", "method is not negotiated")

    def initialize(self, params: Any, request_id: Any) -> None:
        params = require_exact_object(
            params,
            {
                "api_version",
                "octet_version",
                "extension",
                "workspace",
                "capabilities",
                "contributes",
                "flag_values",
                "host",
                "contract",
            },
            "InitializeRequest",
        )
        if params["api_version"] != API_VERSION:
            raise failure("version_mismatch", "the host did not request API 0.3")
        _bounded_string(params["octet_version"], "octet_version", MAX_METHOD_NAME_BYTES)
        _bounded_string(params["workspace"], "workspace", MAX_MIGRATION_PATH_BYTES, allow_empty=True)
        for name in ("extension", "capabilities", "contributes", "host"):
            if params[name] is None:
                raise failure("invalid_params", f"InitializeRequest.{name} must not be null")
            validate_canonical_value(params[name])
        flags = params["flag_values"]
        if not isinstance(flags, list) or len(flags) > MAX_EXTENSION_FLAGS:
            raise failure("resource_exhausted", "InitializeRequest.flag_values exceeds its bound")
        seen_flags: set[str] = set()
        for flag in flags:
            item = require_exact_object(flag, {"name", "value"}, "InitializeFlagValue")
            name = _bounded_string(item["name"], "flag name", MAX_CAPABILITY_NAME_BYTES)
            if name in seen_flags:
                raise failure("invalid_params", "duplicate extension flag")
            seen_flags.add(name)
            if item["value"] is None:
                raise failure("invalid_params", "flag value must not be null")
            validate_canonical_value(item["value"])
        if flags:
            raise failure("capability_mismatch", "Cline adapter declares no extension flags")

        offer = validate_contract_offer(params["contract"])
        selected = select_contract(offer)
        self.contract = {
            "capabilities": set(selected["capabilities"]),
            "methods": set(selected["methods"]),
            "limits": selected["limits"],
        }
        # The negotiated bound applies to the initialize response itself; the
        # host's limit excludes the terminating LF delimiter.
        self.max_frame_bytes = selected["limits"]["max_frame_bytes"]
        response = success_response(
            request_id,
            {
                "api_version": API_VERSION,
                "tools": [],
                "contract": selected,
            },
        )
        self.send(response)
        self.initialized = True

    def _migration_params(self, params: Any, importing: bool) -> tuple[str, list[str]]:
        fields = {"source_root", "config_paths"} if importing else {"source_root"}
        params = require_exact_object(params, fields, "MigrationImportParams" if importing else "MigrationDetectParams")
        source_root = _bounded_string(params["source_root"], "source_root", MAX_MIGRATION_PATH_BYTES)
        if not os.path.isabs(source_root):
            raise failure("invalid_params", "source_root must be absolute")
        config_paths: list[str] = []
        if importing:
            raw_paths = params["config_paths"]
            if not isinstance(raw_paths, list) or len(raw_paths) > MAX_MIGRATION_ITEMS:
                raise failure("resource_exhausted", "config_paths exceeds max_migration_items")
            seen: set[str] = set()
            for path in raw_paths:
                normalized = _bounded_source_relative_path(path, "config path")
                if normalized in seen:
                    raise failure("invalid_params", "config_paths contains a duplicate path")
                seen.add(normalized)
                config_paths.append(normalized)
        return source_root, config_paths

    def migration_detect(self, params: Any, request_id: Any) -> None:
        self.require_method("migration/detect")
        source_root, _ = self._migration_params(params, importing=False)
        try:
            result = detect(source_root)
        except AdapterError as error:
            raise failure("invalid_params", str(error)) from error
        validate_migration_detect_result(result)
        self.send(success_response(request_id, result))

    def migration_import(self, params: Any, request_id: Any) -> None:
        self.require_method("migration/import")
        source_root, config_paths = self._migration_params(params, importing=True)
        try:
            result = import_setup(source_root, config_paths)
        except AdapterError as error:
            raise failure("invalid_params", str(error)) from error
        validate_migration_import_result(result)
        self.send(success_response(request_id, result))

    def cancel(self, params: Any) -> None:
        self.require_method("$/cancelRequest")
        if not isinstance(params, dict):
            raise failure("invalid_params", "CancelRequestParams must be an object")
        unknown = set(params) - {"id", "reason"}
        if unknown:
            raise failure("invalid_params", "CancelRequestParams has unknown fields")
        if "id" not in params:
            raise failure("invalid_params", "CancelRequestParams.id is required")
        validate_rpc_id(params["id"])
        reason = params.get("reason", "request cancelled")
        if not isinstance(reason, str):
            raise failure("invalid_params", "CancelRequestParams.reason must be a string")
        if _utf8_bytes(reason, "cancellation reason") > MAX_REASON_BYTES:
            raise failure("resource_exhausted", "cancellation reason exceeds max_reason_bytes")
        # The adapter has no asynchronous operation to cancel.  The notification
        # is nevertheless consumed as a valid, idempotent foundation method.

    def tool_call(self, params: Any, request_id: Any) -> None:
        self.require_method("tool/call")
        params = require_exact_object(params, {"name", "arguments", "context"}, "ToolCallParams")
        _bounded_string(params["name"], "tool name", MAX_MIGRATION_NAME_BYTES)
        validate_canonical_value(params["arguments"])
        validate_canonical_value(params["context"])
        raise failure("unknown_method", "Cline adapter declares no tools")

    def shutdown(self, params: Any, request_id: Any) -> None:
        self.require_method("shutdown")
        require_exact_object(params, set(), "ShutdownParams")
        self.send(success_response(request_id, {"terminal": "shutdown"}))
        self.stopping = True

    def dispatch(self, request_id: Any, method: str, params: Any, notification: bool) -> None:
        if not self.initialized:
            if method != "initialize":
                raise failure("unknown_method", "only initialize is available before negotiation")
            self.initialize(params, request_id)
            return
        if method == "initialize":
            raise failure("invalid_request", "initialize may only be called once", request_id=request_id)
        if method == "migration/detect":
            self.migration_detect(params, request_id)
        elif method == "migration/import":
            self.migration_import(params, request_id)
        elif method == "$/cancelRequest":
            self.cancel(params)
        elif method == "tool/call":
            self.tool_call(params, request_id)
        elif method == "shutdown":
            self.shutdown(params, request_id)
        else:
            raise failure("unknown_method", "method is not implemented or negotiated", request_id=request_id)

    def run(self) -> None:
        expected_version = os.environ.get("OCTET_EXTENSION_API_VERSION")
        if expected_version is not None and expected_version != API_VERSION:
            self.diagnostic("host requested an unsupported API version")
            return
        while not self.stopping:
            try:
                message = read_frame(self.stdin, self.max_frame_bytes)
            except ProtocolFailure as error:
                # A malformed or noncanonical frame has no trustworthy ID and
                # terminates the stream before dispatch.
                self.diagnostic(error.name)
                return
            except (BrokenPipeError, OSError) as error:
                self.diagnostic(f"I/O failure: {error}")
                return
            if message is None:
                return

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
                if not notification and request_id is not None:
                    self.send_error(request_id, error)
                else:
                    self.diagnostic(error.name)
                if method == "initialize" or not self.initialized:
                    self.stopping = True
            except (BrokenPipeError, OSError) as error:
                self.diagnostic(f"I/O failure: {error}")
                return
            except Exception as error:  # pragma: no cover - process boundary
                self.diagnostic(f"internal error: {type(error).__name__}")
                if not notification and request_id is not None:
                    self.send_error(request_id, failure("internal_error"))
                if method == "initialize":
                    self.stopping = True


def main() -> int:
    Extension(sys.stdin.buffer, sys.stdout.buffer).run()
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
