#!/usr/bin/env python3
"""Bounded, source-only Aider migration adapter for API 0.3.

This executable intentionally uses only the Python standard library.  It reads
only the host-authorized source root, never follows source symlinks, never
executes a configuration command, and emits only the non-secret migration
shapes exposed by API 0.3.
"""
from __future__ import annotations

import json
import os
import re
import stat
import sys
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
MAX_JSON_RPC_ID_BYTES = 256
MAX_MIGRATION_ITEMS = 128
MAX_MIGRATION_PATH_BYTES = 4_096
MAX_MIGRATION_NAME_BYTES = 128
MAX_MIGRATION_SKILL_BYTES = 131_072
MAX_MIGRATION_COMMAND_BYTES = 4_096
MAX_MIGRATION_ARGUMENT_BYTES = 16_384
MAX_MIGRATION_DIAGNOSTIC_BYTES = 4_096
MAX_CONFIG_BYTES = 262_144
MAX_DISCOVERY_ENTRIES = 256
MAX_YAML_LINES = 8_192
MAX_YAML_NODES = 4_096
MAX_YAML_DEPTH = 32
MAX_YAML_SCALAR_BYTES = 16_384
MAX_MCP_ARGUMENTS = 128

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

REQUIRED_CAPABILITIES = ["content_parts", "core", "request_cancellation", "tool_call"]
OPTIONAL_CAPABILITIES = [
    "lifecycle_events", "migration.adapter.v1", "provider_auth",
    "provider_catalog", "provider_stream", "session_lifecycle",
]
ALL_CAPABILITIES = set(REQUIRED_CAPABILITIES + OPTIONAL_CAPABILITIES + ["dynamic_tools"])
REQUIRED_METHODS = ["$/cancelRequest", "initialize", "shutdown", "tool/call"]
ALLOWED_OPTIONAL_METHODS = [
    "hook/run", "migration/detect", "migration/import", "provider/auth/request",
    "provider/auth/revoke", "provider/cancel", "provider/event", "provider/stream",
    "providers/complete", "providers/register", "providers/unregister",
    "providers/update", "session/create", "session/fork", "session/reload",
    "session/switch",
]
ALL_METHODS = set(REQUIRED_METHODS + ALLOWED_OPTIONAL_METHODS + [
    "context/collect", "tools/register", "tools/unregister",
])
METHOD_CAPABILITY = {
    "$/cancelRequest": "request_cancellation", "context/collect": "lifecycle_events",
    "hook/run": "lifecycle_events", "initialize": "core",
    "migration/detect": "migration.adapter.v1", "migration/import": "migration.adapter.v1",
    "provider/auth/request": "provider_auth", "provider/auth/revoke": "provider_auth",
    "provider/cancel": "provider_stream", "provider/event": "provider_stream",
    "provider/stream": "provider_stream", "providers/complete": "provider_catalog",
    "providers/register": "provider_catalog", "providers/unregister": "provider_catalog",
    "providers/update": "provider_catalog", "session/create": "session_lifecycle",
    "session/fork": "session_lifecycle", "session/reload": "session_lifecycle",
    "session/switch": "session_lifecycle", "shutdown": "core", "tool/call": "tool_call",
    "tools/register": "dynamic_tools", "tools/unregister": "dynamic_tools",
}
NOTIFICATION_METHODS = {"$/cancelRequest", "provider/cancel", "provider/event", "providers/complete"}
REQUEST_METHODS = ALL_METHODS - NOTIFICATION_METHODS


class ProtocolFailure(Exception):
    def __init__(self, name: str, detail: str = "", data: Any = None, request_id: Any = None) -> None:
        self.name = name
        self.detail = detail
        self.data = data
        self.request_id = request_id
        super().__init__(detail or name)


def failure(name: str, detail: str = "", data: Any = None, request_id: Any = None) -> ProtocolFailure:
    return ProtocolFailure(name, detail, data, request_id)


def utf8_bytes(value: str, label: str) -> int:
    if any(0xD800 <= ord(c) <= 0xDFFF for c in value):
        raise failure("invalid_params", f"{label} contains an invalid surrogate")
    try:
        return len(value.encode("utf-8"))
    except UnicodeEncodeError as error:
        raise failure("invalid_params", f"{label} must be valid UTF-8") from error


def valid_text(value: Any, label: str, maximum: int, *, newlines: bool = True) -> str:
    if not isinstance(value, str):
        raise failure("invalid_params", f"{label} must be a string")
    if utf8_bytes(value, label) > maximum:
        raise failure("resource_exhausted", f"{label} exceeds its byte bound")
    for c in value:
        code = ord(c)
        if code < 0x20 and (not newlines or c not in "\n\r\t"):
            raise failure("invalid_params", f"{label} contains a control character")
        if 0x7F <= code < 0xA0:
            raise failure("invalid_params", f"{label} contains a control character")
    return value


def validate_json(value: Any, depth: int = 0) -> None:
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
        utf8_bytes(value, "canonical JSON string")
        return
    if isinstance(value, list):
        for item in value:
            validate_json(item, depth + 1)
        return
    if isinstance(value, dict):
        for key, item in value.items():
            if not isinstance(key, str):
                raise failure("invalid_params", "canonical JSON object keys must be strings")
            utf8_bytes(key, "canonical JSON object key")
            validate_json(item, depth + 1)
        return
    raise failure("invalid_params", "canonical JSON value has an unsupported type")


def canonical_bytes(value: Any) -> bytes:
    validate_json(value)
    try:
        return json.dumps(value, ensure_ascii=False, sort_keys=True, separators=(",", ":"), allow_nan=False).encode("utf-8")
    except (TypeError, ValueError, UnicodeEncodeError) as error:
        raise failure("invalid_params", "cannot encode canonical JSON") from error


def no_duplicate_keys(pairs: list[tuple[str, Any]]) -> dict[str, Any]:
    result: dict[str, Any] = {}
    for key, item in pairs:
        if key in result:
            raise ValueError("duplicate object key")
        result[key] = item
    return result


def reject_float(_: str) -> Any:
    raise ValueError("floating-point values are not canonical")


def reject_constant(_: str) -> Any:
    raise ValueError("non-finite values are not canonical")


def read_frame(stream: BinaryIO, limit: int) -> Optional[Any]:
    raw = stream.readline(limit + 2)
    if not raw:
        return None
    if len(raw) > limit + 1 or not raw.endswith(b"\n"):
        raise failure("invalid_request", "frame must be canonical JSON followed by LF")
    payload = raw[:-1]
    if not payload:
        raise failure("parse_error", "empty frame")
    try:
        value = json.loads(
            payload.decode("utf-8"), object_pairs_hook=no_duplicate_keys,
            parse_float=reject_float, parse_constant=reject_constant,
        )
        validate_json(value)
        if canonical_bytes(value) != payload:
            raise ValueError("non-canonical JSON")
        return value
    except ProtocolFailure:
        raise
    except (UnicodeDecodeError, ValueError, json.JSONDecodeError) as error:
        raise failure("parse_error", "invalid canonical JSON frame") from error


def validate_rpc_id(value: Any) -> None:
    if isinstance(value, bool):
        raise failure("invalid_request", "JSON-RPC id is invalid")
    if isinstance(value, int) and 0 <= value <= MAX_PORTABLE_JSON_INTEGER:
        return
    if isinstance(value, str) and value and utf8_bytes(value, "JSON-RPC id") <= MAX_JSON_RPC_ID_BYTES:
        return
    raise failure("invalid_request", "JSON-RPC id must be a bounded string or unsigned integer")


def parse_envelope(value: Any) -> tuple[Any, str, Any, bool]:
    if not isinstance(value, dict):
        raise failure("invalid_request", "JSON-RPC envelope must be an object")
    request_id = value.get("id")
    if "id" in value:
        validate_rpc_id(request_id)
    if value.get("jsonrpc") != "2.0":
        raise failure("invalid_request", "JSON-RPC version must be 2.0", request_id=request_id)
    allowed = {"jsonrpc", "method", "params"} | ({"id"} if "id" in value else set())
    if set(value) - allowed or "method" not in value or "params" not in value:
        raise failure("invalid_request", "JSON-RPC envelope has an invalid field set", request_id=request_id)
    if value["params"] is None:
        raise failure("invalid_request", "JSON-RPC params must not be null", request_id=request_id)
    method = value["method"]
    if not isinstance(method, str) or not method:
        raise failure("invalid_request", "JSON-RPC method must be a non-empty string", request_id=request_id)
    if utf8_bytes(method, "JSON-RPC method") > MAX_METHOD_NAME_BYTES:
        raise failure("resource_exhausted", "JSON-RPC method is too long", request_id=request_id)
    notification = "id" not in value
    if method in REQUEST_METHODS and notification:
        raise failure("invalid_request", "request method requires an id", request_id=request_id)
    if method in NOTIFICATION_METHODS and not notification:
        raise failure("invalid_request", "notification method forbids an id", request_id=request_id)
    return request_id, method, value["params"], notification


def exact_object(value: Any, fields: set[str], label: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != fields:
        raise failure("invalid_params", f"{label} has an invalid field set")
    return value


def names(value: Any, label: str, known: set[str], maximum: int, bytes_limit: int) -> list[str]:
    if not isinstance(value, list) or len(value) > maximum:
        raise failure("resource_exhausted", f"{label} list exceeds its bound")
    result: list[str] = []
    seen: set[str] = set()
    for item in value:
        if not isinstance(item, str) or not item or utf8_bytes(item, label) > bytes_limit:
            raise failure("invalid_params", f"invalid {label} name")
        if item in seen or item not in known:
            raise failure("capability_mismatch", f"unsupported {label} selection")
        seen.add(item)
        result.append(item)
    return result


def limits(value: Any) -> dict[str, int]:
    value = exact_object(value, {"max_frame_bytes", "max_concurrent_requests", "max_tools"}, "limits")
    maxima = {"max_frame_bytes": MAX_FRAME_BYTES, "max_concurrent_requests": MAX_CONCURRENT_REQUESTS, "max_tools": MAX_TOOLS}
    result: dict[str, int] = {}
    for key, maximum in maxima.items():
        item = value[key]
        if isinstance(item, bool) or not isinstance(item, int) or item <= 0:
            raise failure("invalid_params", f"limits.{key} must be positive")
        if item > maximum:
            raise failure("resource_exhausted", f"limits.{key} exceeds API 0.3 maximum")
        result[key] = item
    return result


def validate_offer(value: Any) -> dict[str, Any]:
    value = exact_object(value, {"schema", "encoding", "required_capabilities", "optional_capabilities", "required_methods", "optional_methods", "limits"}, "ContractOffer")
    if value["schema"] != SCHEMA_ID:
        raise failure("version_mismatch", "host selected a different API schema")
    if value["encoding"] != CANONICAL_ENCODING:
        raise failure("invalid_params", "host selected a different canonical encoding")
    required_caps = names(value["required_capabilities"], "capability", ALL_CAPABILITIES, MAX_CAPABILITIES, MAX_CAPABILITY_NAME_BYTES)
    optional_caps = names(value["optional_capabilities"], "capability", ALL_CAPABILITIES, MAX_CAPABILITIES, MAX_CAPABILITY_NAME_BYTES)
    if set(required_caps) != set(REQUIRED_CAPABILITIES) or set(required_caps) & set(optional_caps):
        raise failure("capability_mismatch", "host offer capability sets are incompatible")
    if any(item not in OPTIONAL_CAPABILITIES for item in optional_caps):
        raise failure("capability_mismatch", "host offered an unavailable capability")
    required_methods = names(value["required_methods"], "method", ALL_METHODS, MAX_METHODS, MAX_METHOD_NAME_BYTES)
    optional_methods = names(value["optional_methods"], "method", ALL_METHODS, MAX_METHODS, MAX_METHOD_NAME_BYTES)
    if set(required_methods) != set(REQUIRED_METHODS) or set(required_methods) & set(optional_methods):
        raise failure("capability_mismatch", "host offer method sets are incompatible")
    if any(item not in ALLOWED_OPTIONAL_METHODS for item in optional_methods):
        raise failure("capability_mismatch", "host offered an unavailable method")
    available_caps = set(required_caps) | set(optional_caps)
    available_methods = set(required_methods) | set(optional_methods)
    for method in available_methods:
        if METHOD_CAPABILITY[method] not in available_caps:
            raise failure("capability_mismatch", "offered method lacks its capability")
    if "migration.adapter.v1" not in available_caps or not {"migration/detect", "migration/import"} <= available_methods:
        raise failure("capability_mismatch", "host did not offer the migration contract")
    return {"limits": limits(value["limits"]), "capabilities": available_caps, "methods": available_methods}


def response(request_id: Any, result: Any) -> dict[str, Any]:
    return {"jsonrpc": "2.0", "id": request_id, "result": result}


def error_response(request_id: Any, name: str, data: Any = None) -> dict[str, Any]:
    code, message = ERRORS[name]
    error: dict[str, Any] = {"code": code, "message": message}
    if data is not None:
        error["data"] = data
    return {"jsonrpc": "2.0", "id": request_id, "error": error}


# ---------------------------------------------------------------------------
# Source boundary and bounded discovery

KNOWN_FILES = {
    ".aider.conf.yml", ".aider.conf.yaml", ".aider.yml", ".aider.yaml",
    ".aider.model.settings.yml", ".aider.model.settings.yaml", ".aider.md",
    ".aider.chat.history.md", ".aider.input.history", ".aiderignore",
}
HISTORY_FILES = {".aider.chat.history.md", ".aider.input.history"}
METADATA_FILES = {".aider.model.metadata.json", ".aider.tags.cache.v3", ".aider.tags.cache.v4"}


def disallowed_migration_scalar(value: str, *, allow_text_controls: bool = False) -> bool:
    """Match the host's visible/path scalar safety policy."""
    for character in value:
        code = ord(character)
        if (
            (code < 0x20 and not (allow_text_controls and character in "\n\r\t"))
            or 0x7F <= code < 0xA0
            or code in {0x061C, 0x00AD, 0x034F, 0x115F, 0x1160, 0x3164, 0xFEFF, 0xFFA0}
            or 0x17B4 <= code <= 0x17B5
            or 0x180B <= code <= 0x180F
            or 0x200B <= code <= 0x200F
            or 0x2028 <= code <= 0x202E
            or 0x2060 <= code <= 0x206F
            or 0xFE00 <= code <= 0xFE0F
            or 0xFFF0 <= code <= 0xFFF8
            or 0x1BCA0 <= code <= 0x1BCA3
            or 0x1D173 <= code <= 0x1D17A
            or 0xE0000 <= code <= 0xE0FFF
        ):
            return True
    return False


def safe_relative(path: Any) -> bool:
    if not isinstance(path, str) or not path or "\x00" in path or "\\" in path or path.startswith("/"):
        return False
    if path.strip() != path or not any(not character.isspace() for character in path):
        return False
    if disallowed_migration_scalar(path):
        return False
    try:
        if utf8_bytes(path, "source path") > MAX_MIGRATION_PATH_BYTES:
            return False
    except ProtocolFailure:
        return False
    parts = path.split("/")
    return len(parts) == 1 and parts[0] not in {"", ".", ".."}


def metadata_file(path: str) -> bool:
    return path in METADATA_FILES or ".tags.cache" in path or ".metadata." in path


def allowed_file(path: str) -> bool:
    return (
        path in KNOWN_FILES
        or (path.startswith(".aider") and path.endswith((".yml", ".yaml")) and not metadata_file(path))
    )


def validate_source_root_details(value: Any) -> tuple[str, tuple[int, int]]:
    if not isinstance(value, str) or not value:
        raise failure("invalid_params", "source_root must be a non-empty path")
    valid_text(value, "source_root", MAX_MIGRATION_PATH_BYTES, newlines=False)
    if disallowed_migration_scalar(value):
        raise failure("invalid_params", "source_root contains unsafe formatting characters")
    if not os.path.isabs(value):
        raise failure("invalid_params", "source_root must be an absolute bounded path")
    try:
        root_stat = os.lstat(value)
    except OSError as error:
        raise failure("invalid_params", "source_root is unavailable") from error
    if stat.S_ISLNK(root_stat.st_mode) or not stat.S_ISDIR(root_stat.st_mode):
        raise failure("invalid_params", "source_root must be a non-symlink directory")
    try:
        absolute = os.path.abspath(value)
        resolved = os.path.realpath(absolute)
        resolved_stat = os.lstat(resolved)
        if (
            resolved != absolute
            or not stat.S_ISDIR(resolved_stat.st_mode)
            or (resolved_stat.st_dev, resolved_stat.st_ino) != (root_stat.st_dev, root_stat.st_ino)
        ):
            raise OSError("source root contains a symlink or is not a directory")
    except OSError as error:
        raise failure("invalid_params", "source_root is unavailable") from error
    return resolved, (root_stat.st_dev, root_stat.st_ino)


def validate_source_root(value: Any) -> str:
    return validate_source_root_details(value)[0]


def descriptor_relative_supported() -> bool:
    supports_dir_fd = getattr(os, "supports_dir_fd", ())
    supports_follow_symlinks = getattr(os, "supports_follow_symlinks", ())
    return (
        os.open in supports_dir_fd
        and os.stat in supports_dir_fd
        and os.stat in supports_follow_symlinks
    )


def open_source_root(root: str, expected: Optional[tuple[int, int]] = None) -> int:
    if not descriptor_relative_supported():
        raise failure("invalid_params", "source_root descriptor access is unavailable")
    if expected is None:
        root, expected = validate_source_root_details(root)
    flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_DIRECTORY", 0) | getattr(os, "O_NOFOLLOW", 0)
    descriptor: Optional[int] = None
    try:
        descriptor = os.open(root, flags)
        opened = os.fstat(descriptor)
        if (
            not stat.S_ISDIR(opened.st_mode)
            or (opened.st_dev, opened.st_ino) != expected
        ):
            raise OSError("source root changed during validation")
        return descriptor
    except OSError as error:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass
        raise failure("invalid_params", "source_root changed or is unavailable") from error


def read_regular(
    root: str,
    relative: str,
    maximum: int,
    root_fd: Optional[int] = None,
) -> tuple[Optional[bytes], str]:
    if not safe_relative(relative):
        return None, "unsafe"
    path = relative if root_fd is not None else os.path.join(root, relative)
    try:
        if root_fd is None:
            before = os.lstat(path)
        else:
            before = os.stat(relative, dir_fd=root_fd, follow_symlinks=False)
    except FileNotFoundError:
        return None, "missing"
    except (NotImplementedError, TypeError, ValueError):
        return None, "unavailable"
    except OSError:
        return None, "unavailable"
    if stat.S_ISLNK(before.st_mode):
        return None, "symlink"
    if not stat.S_ISREG(before.st_mode):
        return None, "not_regular"
    if before.st_size > maximum:
        return None, "too_large"
    descriptor: Optional[int] = None
    try:
        flags = os.O_RDONLY | getattr(os, "O_CLOEXEC", 0) | getattr(os, "O_NOFOLLOW", 0)
        if root_fd is None:
            descriptor = os.open(path, flags)
        else:
            descriptor = os.open(path, flags, dir_fd=root_fd)
        opened = os.fstat(descriptor)
        if (opened.st_dev, opened.st_ino) != (before.st_dev, before.st_ino):
            return None, "symlink"
        if not stat.S_ISREG(opened.st_mode):
            return None, "not_regular"
        if opened.st_size > maximum:
            return None, "too_large"
        chunks: list[bytes] = []
        total = 0
        while total <= maximum:
            chunk = os.read(descriptor, min(65_536, maximum + 1 - total))
            if not chunk:
                break
            chunks.append(chunk)
            total += len(chunk)
            if total > maximum:
                return None, "too_large"
        return b"".join(chunks), "ok"
    except FileNotFoundError:
        return None, "missing"
    except (NotImplementedError, TypeError, ValueError):
        return None, "unavailable"
    except OSError:
        return None, "unavailable"
    finally:
        if descriptor is not None:
            try:
                os.close(descriptor)
            except OSError:
                pass


def add_diag(diags: list[dict[str, str]], path: str, severity: str, reason: str) -> None:
    if len(diags) >= MAX_MIGRATION_ITEMS:
        return
    if path != "$" and not safe_relative(path):
        path = "$"
    diags.append({"path": path, "severity": severity, "reason": reason})


def file_limit(path: str) -> int:
    return MAX_MIGRATION_SKILL_BYTES if path in HISTORY_FILES or path == ".aider.md" else MAX_CONFIG_BYTES


def inspect_candidate(
    root: str,
    path: str,
    diags: list[dict[str, str]],
    root_fd: Optional[int] = None,
) -> bool:
    _, status = read_regular(root, path, file_limit(path), root_fd)
    if status == "ok":
        return True
    if status == "missing":
        return False
    reasons = {
        "symlink": "Aider source symlinks are not imported.",
        "not_regular": "Aider source entries must be regular files.",
        "too_large": "Aider source file exceeds the adapter bound and was skipped.",
        "unavailable": "Aider source file could not be read.",
        "unsafe": "Aider source path is unsafe and was not imported.",
    }
    add_diag(diags, path, "warning", reasons.get(status, "Aider source file was skipped."))
    return False


def discover(
    root: str,
    expected: Optional[tuple[int, int]] = None,
) -> tuple[list[str], list[dict[str, str]]]:
    diags: list[dict[str, str]] = []
    found: set[str] = set()
    root_fd = open_source_root(root, expected)
    try:
        for path in sorted(KNOWN_FILES):
            if inspect_candidate(root, path, diags, root_fd):
                found.add(path)
        try:
            names: list[str] = []
            truncated = False
            with os.scandir(root_fd) as entries:
                for index, entry in enumerate(entries):
                    if index >= MAX_DISCOVERY_ENTRIES:
                        truncated = True
                        break
                    names.append(entry.name)
            if truncated:
                add_diag(diags, "$", "warning", "Aider source discovery was truncated at its entry bound.")
            for name in sorted(names):
                if name in KNOWN_FILES:
                    continue
                if metadata_file(name):
                    add_diag(diags, name if safe_relative(name) else "$", "warning", "Aider metadata was not imported.")
                    continue
                if len(found) >= MAX_MIGRATION_ITEMS:
                    add_diag(diags, "$", "warning", "Aider configuration discovery reached its item bound.")
                    break
                if name.startswith(".aider") and name.endswith((".yml", ".yaml")):
                    if not safe_relative(name):
                        add_diag(diags, "$", "warning", "An Aider configuration path is unsafe and was not imported.")
                        continue
                    if allowed_file(name) and inspect_candidate(root, name, diags, root_fd):
                        found.add(name)
        except (NotImplementedError, TypeError, ValueError, OSError):
            add_diag(diags, "$", "error", "The Aider source directory could not be enumerated.")
        return sorted(found), diags
    finally:
        try:
            os.close(root_fd)
        except OSError:
            pass


def validate_paths(value: Any) -> list[str]:
    if not isinstance(value, list) or len(value) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "config_paths exceeds its item bound")
    result: list[str] = []
    seen: set[str] = set()
    for path in value:
        if not safe_relative(path) or not allowed_file(path) or path in seen:
            raise failure("invalid_params", "config_paths contains an unsafe, duplicate, or unsupported path")
        seen.add(path)
        result.append(path)
    return result

class YamlError(Exception):
    pass


class SafeYaml:
    """A deliberately small, bounded YAML subset with no object constructors."""

    def __init__(self, text: str) -> None:
        if utf8_bytes(text, "YAML text") > MAX_CONFIG_BYTES or "\x00" in text:
            raise YamlError("configuration exceeds the input bound")
        for character in text:
            code = ord(character)
            if (code < 0x20 and character not in "\r\n") or 0x7F <= code < 0xA0:
                raise YamlError("YAML contains a control character")
        raw_lines = text.splitlines()
        if len(raw_lines) > MAX_YAML_LINES:
            raise YamlError("configuration has too many lines")
        self.lines: list[tuple[int, str, int]] = []
        self.nodes = 0
        for number, raw in enumerate(raw_lines, 1):
            if any((ord(character) < 0x20 and character not in "\r\n") or 0x7F <= ord(character) < 0xA0 for character in raw):
                raise YamlError("YAML contains a control character")
            indent = len(raw) - len(raw.lstrip(" "))
            if "\t" in raw or indent > MAX_YAML_DEPTH * 8:
                raise YamlError("invalid YAML indentation")
            content = self.strip_comment(raw[indent:]).strip()
            if not content:
                continue
            if content == "---" or content == "..." or content.startswith("--- ") or content.startswith("%"):
                raise YamlError("YAML documents and directives are not supported")
            self.lines.append((indent, content, number))

    @staticmethod
    def strip_comment(value: str) -> str:
        quote: Optional[str] = None
        escaped = False
        for index, character in enumerate(value):
            if quote == '"' and escaped:
                escaped = False
                continue
            if quote == '"' and character == "\\":
                escaped = True
                continue
            if character in "'\"":
                if quote is None:
                    quote = character
                elif quote == character:
                    quote = None
            elif character == "#" and quote is None and (index == 0 or value[index - 1].isspace()):
                return value[:index].rstrip()
        if quote is not None:
            raise YamlError("unterminated YAML quote")
        return value

    def node(self) -> None:
        self.nodes += 1
        if self.nodes > MAX_YAML_NODES:
            raise YamlError("configuration has too many YAML nodes")

    @staticmethod
    def is_sequence(content: str) -> bool:
        return content == "-" or content.startswith("- ")

    @staticmethod
    def split_pair(content: str, *, flow: bool = False) -> Optional[tuple[str, str]]:
        quote: Optional[str] = None
        escaped = False
        square = 0
        curly = 0
        for index, character in enumerate(content):
            if quote == '"' and escaped:
                escaped = False
                continue
            if quote == '"' and character == "\\":
                escaped = True
                continue
            if character in "'\"":
                if quote is None:
                    quote = character
                elif quote == character:
                    quote = None
                continue
            if quote is not None:
                continue
            if character == "[":
                square += 1
            elif character == "]":
                square -= 1
            elif character == "{":
                curly += 1
            elif character == "}":
                curly -= 1
            elif character == ":" and square == 0 and curly == 0:
                if flow or index + 1 == len(content) or content[index + 1].isspace():
                    return content[:index].strip(), content[index + 1:].strip()
        if quote is not None or square != 0 or curly != 0:
            raise YamlError("malformed YAML mapping")
        return None

    @staticmethod
    def split_flow(value: str) -> list[str]:
        parts: list[str] = []
        start = 0
        quote: Optional[str] = None
        escaped = False
        square = 0
        curly = 0
        for index, character in enumerate(value):
            if quote == '"' and escaped:
                escaped = False
                continue
            if quote == '"' and character == "\\":
                escaped = True
                continue
            if character in "'\"":
                if quote is None:
                    quote = character
                elif quote == character:
                    quote = None
                continue
            if quote is not None:
                continue
            if character == "[":
                square += 1
            elif character == "]":
                square -= 1
            elif character == "{":
                curly += 1
            elif character == "}":
                curly -= 1
            elif character == "," and square == 0 and curly == 0:
                parts.append(value[start:index].strip())
                start = index + 1
        if quote is not None or square != 0 or curly != 0:
            raise YamlError("malformed YAML flow value")
        tail = value[start:].strip()
        if tail:
            parts.append(tail)
        return parts

    @staticmethod
    def has_constructor_marker(value: str) -> bool:
        quote: Optional[str] = None
        escaped = False
        for index, character in enumerate(value):
            if quote == '"' and escaped:
                escaped = False
                continue
            if quote == '"' and character == "\\":
                escaped = True
                continue
            if character in "'\"":
                if quote is None:
                    quote = character
                elif quote == character:
                    quote = None
                continue
            if quote is None and character in "&*!" and (
                index == 0 or value[index - 1].isspace() or value[index - 1] in "[{,"
            ):
                return True
        return False

    def scalar(self, value: str, depth: int = 0) -> Any:
        if depth > MAX_YAML_DEPTH:
            raise YamlError("YAML nesting exceeds the depth bound")
        value = value.strip()
        if not value:
            return None
        if utf8_bytes(value, "YAML scalar") > MAX_YAML_SCALAR_BYTES:
            raise YamlError("YAML scalar is too large")
        if value == "<<" or value.startswith("<<:") or self.has_constructor_marker(value):
            raise YamlError("YAML tags, aliases, anchors, and merge keys are not supported")
        self.node()
        if value.startswith("[") or value.startswith("{"):
            closing = "]" if value.startswith("[") else "}"
            if not value.endswith(closing):
                raise YamlError("malformed YAML flow value")
            inner = value[1:-1].strip()
            fields = [] if not inner else self.split_flow(inner)
            if len(fields) > MAX_MIGRATION_ITEMS:
                raise YamlError("YAML flow value is too large")
            if value.startswith("["):
                return [self.scalar(item, depth + 1) for item in fields]
            result: dict[str, Any] = {}
            for field in fields:
                pair = self.split_pair(field, flow=True)
                if pair is None or not pair[0]:
                    raise YamlError("malformed YAML flow map")
                key = self.scalar(pair[0], depth + 1)
                if isinstance(key, str) and key == "<<":
                    raise YamlError("YAML merge keys are not supported")
                if not isinstance(key, str) or key in result:
                    raise YamlError("YAML map keys must be unique strings")
                result[key] = self.scalar(pair[1], depth + 1)
            return result
        if value.startswith("'"):
            if len(value) < 2 or not value.endswith("'"):
                raise YamlError("unterminated YAML string")
            result = value[1:-1].replace("''", "'")
            try:
                valid_text(result, "YAML string", MAX_YAML_SCALAR_BYTES, newlines=False)
            except ProtocolFailure as error:
                raise YamlError("YAML string contains invalid text") from error
            return result
        if value.startswith('"'):
            if len(value) < 2 or not value.endswith('"'):
                raise YamlError("unterminated YAML string")
            try:
                result = json.loads(value)
            except (ValueError, json.JSONDecodeError) as error:
                raise YamlError("invalid YAML string") from error
            if not isinstance(result, str):
                raise YamlError("invalid YAML string")
            try:
                valid_text(result, "YAML string", MAX_YAML_SCALAR_BYTES, newlines=False)
            except ProtocolFailure as error:
                raise YamlError("YAML string contains invalid text") from error
            return result
        lowered = value.lower()
        if lowered in {"true", "false"}:
            return lowered == "true"
        if lowered in {"null", "~"}:
            return None
        if re.fullmatch(r"[-+]?[0-9]+", value):
            if len(value.lstrip("+-")) > 18:
                raise YamlError("YAML integer is too large")
            number = int(value)
            if abs(number) > MAX_PORTABLE_JSON_INTEGER:
                raise YamlError("YAML integer is outside the portable range")
            return number
        # Floats are retained as opaque strings.  They are never emitted as
        # migration values, but accepting them lets unrelated Aider settings
        # produce useful unmapped-setting diagnostics.
        if re.fullmatch(r"[-+]?(?:[0-9]*\.[0-9]+|[0-9]+e[-+]?[0-9]+)", lowered):
            return value
        if any(ord(c) < 0x20 or 0x7F <= ord(c) < 0xA0 for c in value):
            raise YamlError("YAML scalar contains a control character")
        return value

    def block_scalar(self, index: int, parent_indent: int, folded: bool) -> tuple[str, int]:
        self.node()
        values: list[str] = []
        child_indent: Optional[int] = None
        while index < len(self.lines) and self.lines[index][0] > parent_indent:
            indent, content, _ = self.lines[index]
            if child_indent is None:
                child_indent = indent
            if indent < child_indent:
                break
            values.append(content)
            index += 1
        if folded:
            result = " ".join(values) + ("\n" if values else "")
        else:
            result = "\n".join(values) + ("\n" if values else "")
        try:
            size = utf8_bytes(result, "YAML block scalar")
        except ProtocolFailure as error:
            raise YamlError("YAML block scalar is invalid UTF-8") from error
        if size > MAX_YAML_SCALAR_BYTES:
            raise YamlError("YAML block scalar is too large")
        return result, index

    def at(self, index: int, indent: int, depth: int = 0) -> tuple[Any, int]:
        if depth > MAX_YAML_DEPTH:
            raise YamlError("YAML nesting exceeds the depth bound")
        if index >= len(self.lines) or self.lines[index][0] != indent:
            raise YamlError("invalid YAML indentation")
        if self.is_sequence(self.lines[index][1]):
            return self.sequence(index, indent, depth)
        return self.mapping(index, indent, depth)

    def mapping(self, index: int, indent: int, depth: int = 0) -> tuple[dict[str, Any], int]:
        if depth > MAX_YAML_DEPTH:
            raise YamlError("YAML nesting exceeds the depth bound")
        result: dict[str, Any] = {}
        while index < len(self.lines) and self.lines[index][0] == indent:
            content = self.lines[index][1]
            if self.is_sequence(content):
                break
            pair = self.split_pair(content)
            if pair is None or not pair[0]:
                raise YamlError("YAML mapping entry is missing a colon")
            key = self.scalar(pair[0], depth + 1)
            if not isinstance(key, str) or key in {"<<"} or key in result:
                raise YamlError("YAML map keys must be unique non-merge strings")
            rest = pair[1]
            index += 1
            if rest in {"|", ">"}:
                value, index = self.block_scalar(index, indent, rest == ">")
            elif rest:
                value = self.scalar(rest, depth + 1)
            elif index < len(self.lines) and self.lines[index][0] > indent:
                value, index = self.at(index, self.lines[index][0], depth + 1)
            else:
                value = None
            result[key] = value
        if index < len(self.lines) and self.lines[index][0] > indent:
            raise YamlError("unexpected YAML indentation")
        return result, index

    def sequence(self, index: int, indent: int, depth: int = 0) -> tuple[list[Any], int]:
        if depth > MAX_YAML_DEPTH:
            raise YamlError("YAML nesting exceeds the depth bound")
        result: list[Any] = []
        while index < len(self.lines) and self.lines[index][0] == indent:
            content = self.lines[index][1]
            if not self.is_sequence(content):
                break
            rest = content[1:].strip()
            index += 1
            if not rest:
                if index < len(self.lines) and self.lines[index][0] > indent:
                    item, index = self.at(index, self.lines[index][0], depth + 1)
                else:
                    item = None
                result.append(item)
                continue
            pair = self.split_pair(rest)
            if pair is None:
                result.append(self.scalar(rest, depth + 1))
                continue
            key = self.scalar(pair[0], depth + 1)
            if not isinstance(key, str) or not key or key == "<<":
                raise YamlError("YAML sequence map key is invalid")
            item: dict[str, Any] = {}
            first_rest = pair[1]
            if first_rest in {"|", ">"}:
                value, index = self.block_scalar(index, indent, first_rest == ">")
            elif first_rest:
                value = self.scalar(first_rest, depth + 1)
            elif index < len(self.lines) and self.lines[index][0] > indent:
                value, index = self.at(index, self.lines[index][0], depth + 1)
            else:
                value = None
            item[key] = value
            if index < len(self.lines) and self.lines[index][0] > indent:
                continuation, index = self.mapping(index, self.lines[index][0], depth + 1)
                for continuation_key, continuation_value in continuation.items():
                    if continuation_key in item:
                        raise YamlError("duplicate YAML sequence map key")
                    item[continuation_key] = continuation_value
            result.append(item)
            if len(result) > MAX_MIGRATION_ITEMS:
                raise YamlError("YAML sequence is too large")
        if index < len(self.lines) and self.lines[index][0] > indent:
            raise YamlError("unexpected YAML indentation")
        return result, index

    def parse(self) -> Any:
        if not self.lines:
            return {}
        value, index = self.at(0, self.lines[0][0])
        if index != len(self.lines):
            raise YamlError("trailing YAML content")
        return value




def key_name(value: Any) -> str:
    if not isinstance(value, str):
        return ""
    return re.sub(r"-+", "-", value.strip().lower().replace("_", "-"))


SECRET_KEY_PARTS = {
    "api-key", "apikey", "api-token", "token", "access-token", "refresh-token",
    "password", "passwd", "secret", "client-secret", "private-key", "authorization",
    "credentials", "credential", "auth", "headers", "env", "environment",
}
MCP_UNSAFE_ARGUMENT_KEYS = {
    "api-key", "apikey", "api-token", "token", "access-token", "password", "passwd",
    "secret", "client-secret", "private-key", "authorization", "credential", "credentials",
    "auth", "header", "headers", "env", "environment",
}
SECRET_VALUE_RE = re.compile(
    r"(?i)(?:sk-[a-z0-9_-]{12,}|(?:bearer|basic)\s+[a-z0-9._~+/=-]{12,}|(?:api[_-]?key|token|password|secret)\s*[:=]\s*\S+)"
)


def secret_key(value: str) -> bool:
    normalized = key_name(value)
    return normalized in SECRET_KEY_PARTS or any(
        part in normalized for part in ("api-key", "token", "password", "secret", "credential")
    )


def secret_text(value: str) -> bool:
    return bool(SECRET_VALUE_RE.search(value))


def text_value(value: Any, label: str, maximum: int) -> Optional[str]:
    if not isinstance(value, str) or not value:
        return None
    try:
        valid_text(value, label, maximum, newlines=False)
    except ProtocolFailure:
        return None
    if disallowed_migration_scalar(value):
        return None
    return value


def inferred_provider(model: str, configured_provider: Optional[str]) -> tuple[Optional[str], str]:
    if "/" in model:
        prefix, remainder = model.split("/", 1)
        if prefix and remainder and re.fullmatch(r"[A-Za-z0-9_.-]+", prefix):
            return prefix, remainder
    if configured_provider:
        return configured_provider, model
    lowered = model.lower()
    hints = (
        ("anthropic", ("claude",)),
        ("google", ("gemini", "gemma")),
        ("deepseek", ("deepseek",)),
        ("mistral", ("mistral", "mixtral")),
        ("xai", ("grok",)),
        ("cohere", ("command-r", "command_")),
        ("openai", ("gpt-", "o1", "o3", "o4-mini", "chatgpt")),
        ("ollama", ("llama", "qwen", "phi", "deepseek-r")),
    )
    for provider, prefixes in hints:
        if any(lowered.startswith(prefix) for prefix in prefixes):
            return provider, model
    return None, model


def valid_migration_text(value: Any, label: str) -> Optional[str]:
    if not isinstance(value, str) or not value or secret_text(value) or disallowed_migration_scalar(value):
        return None
    try:
        valid_text(value, label, MAX_MIGRATION_NAME_BYTES, newlines=False)
    except ProtocolFailure:
        return None
    return value


def validate_migration_text(value: Any, label: str, maximum: int, *, newlines: bool = True) -> str:
    text = valid_text(value, label, maximum, newlines=newlines)
    if disallowed_migration_scalar(text, allow_text_controls=newlines):
        raise failure("invalid_params", f"{label} contains unsafe formatting characters")
    return text


def model_parts(value: Any, configured_provider: Optional[str]) -> Optional[tuple[str, str]]:
    if isinstance(value, str):
        model = value.strip()
        if not model or secret_text(model):
            return None
        provider, model_name = inferred_provider(model, configured_provider)
    elif isinstance(value, dict):
        provider_value = value.get("provider")
        provider = provider_value if isinstance(provider_value, str) else configured_provider
        model_value = value.get("model", value.get("id", value.get("name")))
        if not isinstance(model_value, str):
            return None
        model_name = model_value.strip()
        provider, model_name = inferred_provider(model_name, provider)
    else:
        return None
    provider = valid_migration_text(provider, "model provider")
    model_name = valid_migration_text(model_name, "model identifier")
    if provider is None or model_name is None:
        return None
    return provider, model_name


MODEL_KEYS = {
    "model", "main-model", "default-model", "selected-model", "weak-model",
    "weak-model-name", "editor-model", "editor-model-name", "architect-model",
    "reasoning-model", "models", "model-settings", "model-settings-file",
}


def add_model(
    value: Any,
    path: str,
    configured_provider: Optional[str],
    models: list[dict[str, str]],
    seen: set[tuple[str, str, str]],
    diagnostics: list[dict[str, str]],
) -> None:
    if len(models) >= MAX_MIGRATION_ITEMS:
        return
    if isinstance(value, list):
        for item in value:
            add_model(item, path, configured_provider, models, seen, diagnostics)
        return
    parts = model_parts(value, configured_provider)
    if parts is None:
        # A mapping is also allowed to be a named model-settings collection.
        if isinstance(value, dict) and not any(key_name(key) in {"model", "id", "name"} for key in value):
            for item in value.values():
                add_model(item, path, configured_provider, models, seen, diagnostics)
        elif value is not None:
            add_diag(diagnostics, path, "warning", "An Aider model setting could not be mapped without importing credentials.")
        return
    provider, model = parts
    identity = (path, provider, model)
    if identity not in seen:
        seen.add(identity)
        models.append({"path": path, "provider": provider, "model": model})


def provider_hint(document: Any) -> Optional[str]:
    if not isinstance(document, dict):
        return None
    explicit = document.get("provider")
    if isinstance(explicit, str) and explicit and not secret_text(explicit):
        return explicit
    for key in document:
        normalized = key_name(key)
        if secret_key(normalized):
            for provider in ("openai", "anthropic", "google", "mistral", "cohere", "xai", "deepseek", "azure", "bedrock"):
                if provider in normalized:
                    return provider
    return None


def collect_models(
    document: Any,
    path: str,
    models: list[dict[str, str]],
    seen: set[tuple[str, str, str]],
    diagnostics: list[dict[str, str]],
) -> None:
    if isinstance(document, list):
        for item in document:
            if isinstance(item, dict):
                collect_models(item, path, models, seen, diagnostics)
            else:
                add_model(item, path, None, models, seen, diagnostics)
        return
    if not isinstance(document, dict):
        add_diag(diagnostics, path, "error", "The Aider configuration root must be a mapping or sequence.")
        return
    configured_provider = provider_hint(document)
    for raw_key, value in document.items():
        normalized = key_name(raw_key)
        if secret_key(normalized):
            # Do not inspect or reflect the value.  Credentials are intentionally
            # not represented in API 0.3 migration output.
            continue
        if normalized in MODEL_KEYS:
            add_model(value, path, configured_provider, models, seen, diagnostics)
            continue
        if normalized in {"provider", "api-type", "edit-format", "edit-format-name"}:
            continue
        if normalized in {"mcp-servers", "mcp-server", "mcp"}:
            continue
        # Aider's model settings file commonly contains records with a `name`
        # and `model` field; recurse only through bounded mapping/list values so
        # arbitrary source files are never opened.
        if isinstance(value, (dict, list)) and normalized in {"settings", "profiles", "providers", "model-profiles"}:
            collect_models(value, path, models, seen, diagnostics)


def mcp_records(value: Any) -> list[tuple[Optional[str], Any]]:
    if isinstance(value, list):
        return [(None, item) for item in value]
    if isinstance(value, dict):
        if any(key_name(key) in {"command", "cmd", "url", "endpoint", "transport"} for key in value):
            return [(None, value)]
        return [(key if isinstance(key, str) else None, item) for key, item in value.items()]
    return []


def unsafe_mcp_argument(value: str) -> bool:
    if secret_text(value):
        return True
    option = value.strip().lstrip("-")
    option = re.split(r"[=:]", option, maxsplit=1)[0]
    return key_name(option) in MCP_UNSAFE_ARGUMENT_KEYS


def add_mcp(
    value: Any,
    path: str,
    servers: list[dict[str, Any]],
    seen: set[tuple[str, str, tuple[str, ...]]],
    diagnostics: list[dict[str, str]],
) -> None:
    if len(servers) >= MAX_MIGRATION_ITEMS:
        return
    records = mcp_records(value)
    if not records:
        add_diag(diagnostics, path, "warning", "An Aider MCP declaration was not a local stdio mapping.")
        return
    for map_name, record in records:
        if len(servers) >= MAX_MIGRATION_ITEMS:
            return
        if not isinstance(record, dict):
            add_diag(diagnostics, path, "warning", "An Aider MCP declaration was not a mapping.")
            continue
        normalized = {key_name(key): item for key, item in record.items() if isinstance(key, str)}
        name = text_value(normalized.get("name"), "MCP server name", MAX_MIGRATION_NAME_BYTES)
        if name is None:
            name = text_value(map_name, "MCP server name", MAX_MIGRATION_NAME_BYTES)
        command = text_value(normalized.get("command"), "MCP command", MAX_MIGRATION_COMMAND_BYTES) or text_value(normalized.get("cmd"), "MCP command", MAX_MIGRATION_COMMAND_BYTES)
        remote_keys = {
            "url", "endpoint", "headers", "transport-url", "server-url", "server-endpoint",
            "base-url", "http-url", "sse-url", "websocket-url", "remote",
        }
        remote = any(key in normalized for key in remote_keys)
        transport = normalized.get("transport")
        if "transport" in normalized and (not isinstance(transport, str) or key_name(transport) not in {"stdio", "local"}):
            remote = True
        transport_type = normalized.get("type")
        if isinstance(transport_type, str) and key_name(transport_type) not in {"stdio", "local", "command", "exec"}:
            remote = True
        if remote:
            add_diag(diagnostics, path, "warning", "Aider remote MCP transports are not imported.")
            continue
        if name is None or command is None or secret_text(name) or secret_text(command):
            add_diag(diagnostics, path, "warning", "An Aider MCP declaration lacks a safe local name or command.")
            continue
        args_value = normalized.get("args", normalized.get("arguments", []))
        if not isinstance(args_value, list) or len(args_value) > MAX_MCP_ARGUMENTS:
            add_diag(diagnostics, path, "warning", "An Aider MCP argument list exceeds the migration bound.")
            continue
        args: list[str] = []
        valid = True
        for argument in args_value:
            if not isinstance(argument, str) or unsafe_mcp_argument(argument):
                valid = False
                break
            try:
                valid_text(argument, "MCP argument", MAX_MIGRATION_ARGUMENT_BYTES, newlines=False)
            except ProtocolFailure:
                valid = False
                break
            if disallowed_migration_scalar(argument):
                valid = False
                break
            args.append(argument)
        if not valid:
            add_diag(diagnostics, path, "warning", "An Aider MCP argument was omitted because it is unsafe or too large.")
            continue
        try:
            valid_text(name, "MCP server name", MAX_MIGRATION_NAME_BYTES, newlines=False)
            valid_text(command, "MCP command", MAX_MIGRATION_COMMAND_BYTES, newlines=False)
        except ProtocolFailure:
            add_diag(diagnostics, path, "warning", "An Aider MCP declaration exceeds the migration bound.")
            continue
        identity = (name, command, tuple(args))
        if identity in seen:
            continue
        seen.add(identity)
        servers.append({"path": path, "name": name, "command": command, "args": args})
        if any(
            key_name(key) in {
                "env", "environment", "headers", "cwd", "working-directory", "auth", "token",
                "api-key", "apikey", "password", "secret", "credentials", "credential",
                "authorization", "access-token", "client-secret", "private-key",
            }
            for key in record
            if isinstance(key, str)
        ):
            add_diag(diagnostics, path, "warning", "Secret-bearing or execution-context MCP fields were not imported.")


def collect_mcp(document: Any, path: str, servers: list[dict[str, Any]], seen: set[tuple[str, str, tuple[str, ...]]], diagnostics: list[dict[str, str]]) -> None:
    if not isinstance(document, dict):
        return
    for raw_key, value in document.items():
        if key_name(raw_key) in {"mcp-servers", "mcp-server", "mcp"}:
            add_mcp(value, path, servers, seen, diagnostics)


def parse_config(
    data: bytes,
    path: str,
    models: list[dict[str, str]],
    model_seen: set[tuple[str, str, str]],
    servers: list[dict[str, Any]],
    server_seen: set[tuple[str, str, tuple[str, ...]]],
    diagnostics: list[dict[str, str]],
) -> None:
    try:
        text = data.decode("utf-8")
        document = SafeYaml(text).parse()
    except (UnicodeDecodeError, YamlError):
        add_diag(diagnostics, path, "error", "The Aider YAML configuration was not valid in the supported safe subset.")
        return
    collect_models(document, path, models, model_seen, diagnostics)
    collect_mcp(document, path, servers, server_seen, diagnostics)


def likely_secret_skill(content: str) -> bool:
    return bool(SECRET_VALUE_RE.search(content)) or bool(re.search(r"(?im)^\s*(?:export\s+)?(?:api[_-]?key|token|password|secret)\s*[:=]", content))


def import_skill(
    root: str,
    path: str,
    skills: list[dict[str, str]],
    diagnostics: list[dict[str, str]],
    root_fd: Optional[int] = None,
) -> None:
    data, status = read_regular(root, path, MAX_MIGRATION_SKILL_BYTES, root_fd)
    if status == "missing":
        return
    if status != "ok" or data is None:
        add_diag(diagnostics, path, "warning", "The Aider instruction file was not imported.")
        return
    try:
        content = data.decode("utf-8")
    except UnicodeDecodeError:
        add_diag(diagnostics, path, "warning", "The Aider instruction file is not UTF-8 and was not imported.")
        return
    if likely_secret_skill(content):
        add_diag(diagnostics, path, "warning", "The Aider instruction file appears to contain secret-bearing material and was not imported.")
        return
    try:
        valid_text(content, "skill content", MAX_MIGRATION_SKILL_BYTES)
    except ProtocolFailure:
        add_diag(diagnostics, path, "warning", "The Aider instruction file exceeds the migration bound.")
        return
    if disallowed_migration_scalar(content, allow_text_controls=True):
        add_diag(diagnostics, path, "warning", "The Aider instruction file contains unsafe formatting characters and was not imported.")
        return
    skills.append({"path": path, "name": "aider", "content": content})


def import_history(
    root: str,
    path: str,
    diagnostics: list[dict[str, str]],
    root_fd: Optional[int] = None,
) -> None:
    if not safe_relative(path):
        add_diag(diagnostics, "$", "warning", "The Aider history path is unsafe and was not imported.")
        return
    try:
        if root_fd is None:
            present = os.lstat(os.path.join(root, path))
        else:
            present = os.stat(path, dir_fd=root_fd, follow_symlinks=False)
    except FileNotFoundError:
        return
    except OSError:
        add_diag(diagnostics, path, "warning", "The Aider history file could not be inspected.")
        return
    if stat.S_ISLNK(present.st_mode):
        add_diag(diagnostics, path, "warning", "The Aider history symlink was not imported.")
    elif stat.S_ISREG(present.st_mode):
        add_diag(diagnostics, path, "warning", "Aider history is detected but has no API 0.3 migration destination.")
    else:
        add_diag(diagnostics, path, "warning", "The Aider history entry was not a regular file.")


def validate_diagnostics(value: Any) -> None:
    if not isinstance(value, list) or len(value) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "diagnostics exceeds its item bound")
    for diagnostic in value:
        item = exact_object(diagnostic, {"path", "severity", "reason"}, "MigrationDiagnostic")
        path = item["path"]
        if path != "$" and not safe_relative(path):
            raise failure("invalid_params", "MigrationDiagnostic.path is unsafe")
        if item["severity"] not in {"warning", "error"}:
            raise failure("invalid_params", "MigrationDiagnostic.severity is invalid")
        validate_migration_text(item["reason"], "diagnostic reason", MAX_MIGRATION_DIAGNOSTIC_BYTES, newlines=False)


def validate_migration_result(value: Any) -> None:
    result = exact_object(value, {"models", "skills", "mcp_servers", "diagnostics"}, "MigrationImportResult")
    models = result["models"]
    if not isinstance(models, list) or len(models) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "models exceeds its item bound")
    for model in models:
        item = exact_object(model, {"path", "provider", "model"}, "MigrationModel")
        if not safe_relative(item["path"]):
            raise failure("invalid_params", "MigrationModel.path is unsafe")
        validate_migration_text(item["provider"], "model provider", MAX_MIGRATION_NAME_BYTES, newlines=False)
        validate_migration_text(item["model"], "model identifier", MAX_MIGRATION_NAME_BYTES, newlines=False)
    skills = result["skills"]
    if not isinstance(skills, list) or len(skills) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "skills exceeds its item bound")
    for skill in skills:
        item = exact_object(skill, {"path", "name", "content"}, "MigrationSkill")
        if not safe_relative(item["path"]):
            raise failure("invalid_params", "MigrationSkill.path is unsafe")
        validate_migration_text(item["name"], "skill name", MAX_MIGRATION_NAME_BYTES, newlines=False)
        validate_migration_text(item["content"], "skill content", MAX_MIGRATION_SKILL_BYTES)
    servers = result["mcp_servers"]
    if not isinstance(servers, list) or len(servers) > MAX_MIGRATION_ITEMS:
        raise failure("resource_exhausted", "mcp_servers exceeds its item bound")
    for server in servers:
        item = exact_object(server, {"path", "name", "command", "args"}, "MigrationMcpServer")
        if not safe_relative(item["path"]):
            raise failure("invalid_params", "MigrationMcpServer.path is unsafe")
        validate_migration_text(item["name"], "MCP server name", MAX_MIGRATION_NAME_BYTES, newlines=False)
        validate_migration_text(item["command"], "MCP command", MAX_MIGRATION_COMMAND_BYTES, newlines=False)
        if not isinstance(item["args"], list) or len(item["args"]) > MAX_MCP_ARGUMENTS:
            raise failure("resource_exhausted", "MCP arguments exceeds its item bound")
        for argument in item["args"]:
            validate_migration_text(argument, "MCP argument", MAX_MIGRATION_ARGUMENT_BYTES, newlines=False)
    validate_diagnostics(result["diagnostics"])


def validate_detect_result(value: Any) -> None:
    result = exact_object(value, {"detected", "config_paths", "diagnostics"}, "MigrationDetectResult")
    if not isinstance(result["detected"], bool):
        raise failure("invalid_params", "detected must be boolean")
    validate_paths(result["config_paths"])
    validate_diagnostics(result["diagnostics"])


def read_diagnostic(diagnostics: list[dict[str, str]], path: str, status: str) -> None:
    reasons = {
        "symlink": "Aider source symlinks are not imported.",
        "not_regular": "Aider source entries must be regular files.",
        "too_large": "Aider source file exceeds the adapter bound and was skipped.",
        "unavailable": "Aider source file could not be read.",
        "unsafe": "Aider source path is unsafe and was not imported.",
    }
    add_diag(diagnostics, path, "warning", reasons.get(status, "Aider source file was skipped."))


def import_source(
    root: str,
    paths: list[str],
    expected: Optional[tuple[int, int]] = None,
) -> dict[str, Any]:
    paths = validate_paths(paths)
    models: list[dict[str, str]] = []
    skills: list[dict[str, str]] = []
    servers: list[dict[str, Any]] = []
    diagnostics: list[dict[str, str]] = []
    model_seen: set[tuple[str, str, str]] = set()
    server_seen: set[tuple[str, str, tuple[str, ...]]] = set()
    root_fd = open_source_root(root, expected)
    try:
        for path in sorted(paths):
            if path == ".aider.md":
                import_skill(root, path, skills, diagnostics, root_fd)
                continue
            if path in HISTORY_FILES:
                import_history(root, path, diagnostics, root_fd)
                continue
            if path == ".aiderignore":
                add_diag(diagnostics, path, "warning", "Aider ignore rules have no API 0.3 migration destination.")
                continue
            data, status = read_regular(root, path, file_limit(path), root_fd)
            if status != "ok" or data is None:
                read_diagnostic(diagnostics, path, status)
                continue
            parse_config(data, path, models, model_seen, servers, server_seen, diagnostics)
        result = {"models": models, "skills": skills, "mcp_servers": servers, "diagnostics": diagnostics}
        validate_migration_result(result)
        return result
    finally:
        try:
            os.close(root_fd)
        except OSError:
            pass


class Adapter:
    def __init__(self, stdin: BinaryIO, stdout: BinaryIO) -> None:
        self.stdin = stdin
        self.stdout = stdout
        self.max_frame_bytes = MAX_FRAME_BYTES
        self.contract: Optional[dict[str, Any]] = None
        self.initialized = False
        self.stopped = False

    def send(self, value: Any) -> None:
        frame = canonical_bytes(value)
        if len(frame) > self.max_frame_bytes:
            raise failure("resource_exhausted", "outbound frame exceeds max_frame_bytes")
        self.stdout.write(frame + b"\n")
        self.stdout.flush()

    def send_error(self, request_id: Any, error: ProtocolFailure) -> None:
        try:
            self.send(error_response(request_id, error.name))
        except (BrokenPipeError, OSError, ProtocolFailure):
            self.stopped = True

    def require_method(self, method: str) -> None:
        if self.contract is None or method not in self.contract["methods"]:
            raise failure("unknown_method")

    def initialize(self, params: Any, request_id: Any) -> None:
        fields = {
            "api_version", "octet_version", "extension", "workspace",
            "capabilities", "contributes", "flag_values", "host", "contract",
        }
        if not isinstance(params, dict) or "api_version" not in params:
            raise failure("invalid_params")
        if not isinstance(params["api_version"], str):
            raise failure("invalid_params")
        if params["api_version"] != API_VERSION:
            raise failure("version_mismatch")
        params = exact_object(params, fields, "InitializeRequest")
        for name in ("api_version", "octet_version", "workspace"):
            if not isinstance(params[name], str):
                raise failure("invalid_params")
            utf8_bytes(params[name], f"InitializeRequest.{name}")
        for name in ("extension", "capabilities", "contributes", "host"):
            if params[name] is None:
                raise failure("invalid_params")
            validate_json(params[name])
        flags = params["flag_values"]
        if not isinstance(flags, list):
            raise failure("invalid_params")
        if len(flags) > MAX_EXTENSION_FLAGS:
            raise failure("resource_exhausted")
        for flag in flags:
            item = exact_object(flag, {"name", "value"}, "InitializeFlagValue")
            valid_text(item["name"], "flag name", MAX_METHOD_NAME_BYTES)
            if item["value"] is None:
                raise failure("invalid_params")
            validate_json(item["value"])
        offer = validate_offer(params["contract"])
        capabilities = [name for name in REQUIRED_CAPABILITIES if name in offer["capabilities"]]
        capabilities.append("migration.adapter.v1")
        methods = list(REQUIRED_METHODS) + ["migration/detect", "migration/import"]
        self.contract = {
            "capabilities": set(capabilities),
            "methods": set(methods),
            "limits": dict(offer["limits"]),
        }
        result = {
            "api_version": API_VERSION,
            "tools": [],
            "contract": {
                "schema": SCHEMA_ID,
                "encoding": CANONICAL_ENCODING,
                "capabilities": capabilities,
                "methods": methods,
                "limits": dict(offer["limits"]),
            },
        }
        self.send(response(request_id, result))
        self.max_frame_bytes = offer["limits"]["max_frame_bytes"]
        self.initialized = True

    def migration_detect(self, params: Any, request_id: Any) -> None:
        self.require_method("migration/detect")
        params = exact_object(params, {"source_root"}, "MigrationDetectParams")
        root, identity = validate_source_root_details(params["source_root"])
        paths, diagnostics = discover(root, identity)
        result = {"detected": bool(paths), "config_paths": paths, "diagnostics": diagnostics}
        validate_detect_result(result)
        self.send(response(request_id, result))

    def migration_import(self, params: Any, request_id: Any) -> None:
        self.require_method("migration/import")
        params = exact_object(params, {"source_root", "config_paths"}, "MigrationImportParams")
        root, identity = validate_source_root_details(params["source_root"])
        paths = validate_paths(params["config_paths"])
        self.send(response(request_id, import_source(root, paths, identity)))

    def cancel(self, params: Any) -> None:
        self.require_method("$/cancelRequest")
        if not isinstance(params, dict) or set(params) - {"id", "reason"} or "id" not in params:
            raise failure("invalid_params")
        validate_rpc_id(params["id"])
        if "reason" in params:
            valid_text(params["reason"], "cancellation reason", MAX_REASON_BYTES)

    def shutdown(self, params: Any, request_id: Any) -> None:
        self.require_method("shutdown")
        exact_object(params, set(), "ShutdownParams")
        self.send(response(request_id, {"terminal": "shutdown"}))
        self.stopped = True

    def tool_call(self, params: Any) -> None:
        self.require_method("tool/call")
        params = exact_object(params, {"name", "arguments", "context"}, "ToolCallParams")
        valid_text(params["name"], "tool name", MAX_METHOD_NAME_BYTES, newlines=False)
        if params["arguments"] is None or params["context"] is None:
            raise failure("invalid_params")
        validate_json(params["arguments"])
        validate_json(params["context"])
        raise failure("invalid_params")

    def dispatch(self, request_id: Any, method: str, params: Any, notification: bool) -> None:
        if not self.initialized:
            if method != "initialize":
                raise failure("unknown_method")
            self.initialize(params, request_id)
            return
        if method == "initialize":
            raise failure("invalid_request")
        if method == "$/cancelRequest":
            if not notification:
                raise failure("invalid_request")
            self.cancel(params)
            return
        if method == "migration/detect":
            if notification:
                raise failure("invalid_request")
            self.migration_detect(params, request_id)
            return
        if method == "migration/import":
            if notification:
                raise failure("invalid_request")
            self.migration_import(params, request_id)
            return
        if method == "shutdown":
            if notification:
                raise failure("invalid_request")
            self.shutdown(params, request_id)
            return
        if method == "tool/call":
            if notification:
                raise failure("invalid_request")
            self.tool_call(params)
            return
        raise failure("unknown_method")

    def run(self) -> None:
        while not self.stopped:
            try:
                message = read_frame(self.stdin, self.max_frame_bytes)
            except ProtocolFailure:
                break
            if message is None:
                break
            request_id: Any = None
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
                if not self.initialized:
                    self.stopped = True
            except (BrokenPipeError, OSError):
                self.stopped = True
            except Exception:
                if not notification and request_id is not None:
                    self.send_error(request_id, failure("internal_error"))
                if not self.initialized:
                    self.stopped = True


def main() -> None:
    Adapter(sys.stdin.buffer, sys.stdout.buffer).run()


if __name__ == "__main__":
    main()
