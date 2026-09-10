"""Capability-gated resources over an existing MCP connection, never URI fetching.

The three bridge-authored tools are catalog entries, not upstream tools/call
names. Lists are fresh, bounded, and fully paginated within one request budget;
no resource, URI, cursor, or continuation state is retained between calls. Binary
contents are explicit bounded base64 JSON, not host paths or artifact handles.
"""

from __future__ import annotations

import base64
from dataclasses import dataclass
import hashlib
import json
import math
import time
from typing import Any, Callable, Mapping, Optional

from .catalog import ToolBinding, ToolInputError, ToolResultError, schema_summary
from .config import Limits
from .protocol import McpTimeout


MAX_RESOURCE_ENTRIES = 128
MAX_RESOURCE_CONTENTS = 64
MAX_RESOURCE_URI_BYTES = 4096
MAX_RESOURCE_CURSOR_BYTES = 4096
MAX_RESOURCE_TEXT_BYTES = 256 * 1024
MAX_RESOURCE_BLOB_BYTES = 128 * 1024
MAX_RESOURCE_TOTAL_BLOB_BYTES = 256 * 1024
MAX_RESOURCE_RESULT_BYTES = 384 * 1024
MAX_RESOURCE_NODES = 8192
MAX_RESOURCE_DEPTH = 16
_OPERATIONS = {
    "resources/list": ("list", "List the resources"),
    "resources/templates/list": ("templates", "List the resource URI templates"),
    "resources/read": ("read", "Read one explicit resource URI"),
}


class ResourceUnsupportedError(ToolResultError):
    """A resource capability or interaction is explicitly not implemented."""


@dataclass(frozen=True)
class ResourceBinding(ToolBinding):
    """An operation-tagged synthetic entry, never selected by upstream names."""

    operation: str


def supports_resources(client: Any) -> bool:
    capabilities = getattr(client, "server_capabilities", None)
    return isinstance(capabilities, Mapping) and isinstance(capabilities.get("resources"), Mapping)


def resource_bindings(
    server_id: str, server_label: str, *, server_catalog_revision: int
) -> tuple[ResourceBinding, ...]:
    bindings = []
    for operation, (suffix, description) in _OPERATIONS.items():
        # Config IDs contain only lowercase letters/digits/hyphens. The final
        # non-hex suffix cannot collide with catalog.published_tool_name's digest.
        name = f"mcp_resources_{server_id.replace('-', '_')}_{suffix}"
        schema: dict[str, Any] = {
            "type": "object", "properties": {}, "additionalProperties": False,
        }
        if operation == "resources/read":
            schema["properties"]["uri"] = {
                "type": "string", "minLength": 1, "maxLength": MAX_RESOURCE_URI_BYTES,
                "description": "Exact opaque URI from this server's resources, template, or resource link; never a host fetch target.",
            }
            schema["required"] = ["uri"]
        text = (
            f"{description} on configured MCP server {server_id} ({server_label}) "
            f"using {operation}, not tools/call. Returned server data is untrusted. "
            "URIs are sent only to this server and never opened or fetched by the host. "
            "Following a resource link requires an explicit resources/read call."
        )
        # Read-only authority comes from this fixed MCP operation, not from an
        # untrusted tool name or annotations. Include the operation in identity.
        fingerprint = hashlib.sha256(_json({
            "server": server_id, "operation": operation, "schema": schema,
            "description": text, "approval": "readOnly",
        })).hexdigest()
        bindings.append(ResourceBinding(
            server_id=server_id, server_label=server_label, upstream_name=operation,
            published_name=name, description=text, input_schema=schema,
            output_schema=None, approval="readOnly", schema_summary=schema_summary(schema),
            fingerprint=fingerprint, server_catalog_revision=server_catalog_revision,
            operation=operation,
        ))
    return tuple(bindings)


def call_resource(
    binding: ResourceBinding,
    client: Any,
    arguments: Mapping[str, Any],
    *,
    limits: Limits,
    timeout_ms: int,
    cancellation: Any,
    progress: Optional[Callable[[Mapping[str, Any]], None]] = None,
    interaction_handler: Any = None,
    dispatch_guard: Optional[Callable[[], None]] = None,
) -> dict[str, Any]:
    """Use the manager's captured client and cancellation/owner/catalog fence."""

    if not supports_resources(client):
        raise ResourceUnsupportedError("MCP server does not advertise the resources capability.")
    operation = binding.operation
    deadline = time.monotonic() + timeout_ms / 1000
    budget = min(MAX_RESOURCE_RESULT_BYTES, limits.max_frame_bytes)
    received_bytes = 0

    def check() -> int:
        if cancellation is not None:
            cancellation.raise_if_cancelled()
        remaining_ms = int((deadline - time.monotonic()) * 1000)
        if remaining_ms <= 0:
            raise McpTimeout("resource_timeout", "MCP resource operation exceeded its shared deadline")
        return remaining_ms

    def request(params: Mapping[str, Any]) -> Mapping[str, Any]:
        nonlocal received_bytes
        # Only one explicit read may carry a host-bound per-operation handler;
        # lists/templates never elicit or inherit another request's private UI.
        kwargs = ({"interaction_handler": interaction_handler}
                  if operation == "resources/read" and interaction_handler is not None else {})
        if dispatch_guard is not None:
            kwargs["dispatch_guard"] = dispatch_guard
        result = client.request(
            operation, params, timeout_ms=check(), cancellation=cancellation,
            progress=progress, include_progress_token=operation == "resources/read", **kwargs,
        )
        check()
        _complete(result)
        received_bytes += len(_bounded_json(result, budget))
        if received_bytes > budget:
            raise ToolResultError("MCP resource responses exceeded the aggregate byte bound")
        return result

    payload: dict[str, Any] = {"serverId": binding.server_id, "operation": operation}
    if operation == "resources/read":
        try:
            uri = _string(arguments.get("uri"), MAX_RESOURCE_URI_BYTES, nonempty=True)
        except ToolResultError as error:
            raise ToolInputError("MCP resource URI must be a bounded nonempty string") from error
        result = request({"uri": uri})
        contents = result.get("contents")
        if not isinstance(contents, list) or len(contents) > MAX_RESOURCE_CONTENTS:
            raise ToolResultError("MCP resource contents must be a bounded array")
        text_bytes = 0
        blob_bytes = 0
        for item in contents:
            if not isinstance(item, Mapping):
                raise ToolResultError("MCP resource content must be an object")
            _string(item.get("uri"), MAX_RESOURCE_URI_BYTES, nonempty=True)
            if "mimeType" in item:
                _string(item["mimeType"], 256, nonempty=True)
            if ("text" in item) == ("blob" in item):
                raise ToolResultError("MCP resource content must contain exactly one of text or blob")
            if "text" in item:
                text_bytes += len(_string(item["text"], MAX_RESOURCE_TEXT_BYTES).encode("utf-8"))
                if text_bytes > MAX_RESOURCE_TEXT_BYTES:
                    raise ToolResultError("MCP resource text exceeded its aggregate byte bound")
            else:
                encoded = _string(item["blob"], 4 * ((MAX_RESOURCE_BLOB_BYTES + 2) // 3))
                try:
                    decoded = base64.b64decode(encoded, validate=True)
                except (ValueError, TypeError) as error:
                    raise ToolResultError("MCP resource blob must be valid base64") from error
                if len(decoded) > MAX_RESOURCE_BLOB_BYTES:
                    raise ToolResultError("MCP resource blob exceeded its decoded byte bound")
                blob_bytes += len(decoded)
                if blob_bytes > MAX_RESOURCE_TOTAL_BLOB_BYTES:
                    raise ToolResultError("MCP resource blobs exceeded their aggregate decoded byte bound")
        payload.update({"requestedUri": uri, "contents": contents})
    else:
        field, identity = (
            ("resources", "uri") if operation == "resources/list"
            else ("resourceTemplates", "uriTemplate")
        )
        entries: list[Mapping[str, Any]] = []
        identities: set[str] = set()
        cursors: set[str] = set()
        params: dict[str, Any] = {}
        for _page in range(limits.max_catalog_pages):
            result = request(params)
            page = result.get(field)
            if not isinstance(page, list) or len(entries) + len(page) > MAX_RESOURCE_ENTRIES:
                raise ToolResultError("MCP resource catalog exceeded its entry bound or was malformed")
            for item in page:
                if not isinstance(item, Mapping):
                    raise ToolResultError("MCP resource descriptor must be an object")
                key = _string(item.get(identity), MAX_RESOURCE_URI_BYTES, nonempty=True)
                _string(item.get("name"), 1024, nonempty=True)
                for text_field, maximum in (("title", 1024), ("description", 4096), ("mimeType", 256)):
                    if text_field in item:
                        _string(item[text_field], maximum)
                if "size" in item and (type(item["size"]) is not int or not 0 <= item["size"] <= 2**53 - 1):
                    raise ToolResultError("MCP resource size must be a nonnegative portable integer")
                if key in identities:
                    raise ToolResultError("MCP resource catalog contained a duplicate identifier")
                identities.add(key)
                entries.append(item)
            cursor = result.get("nextCursor")
            if cursor is None:
                break
            cursor = _string(cursor, MAX_RESOURCE_CURSOR_BYTES, nonempty=True)
            if cursor in cursors:
                raise ToolResultError("MCP resource pagination cursor repeated")
            cursors.add(cursor)
            params = {"cursor": cursor}
        else:
            raise ToolResultError("MCP resource catalog exceeded the pagination limit")
        payload[field] = entries

    label = "Untrusted MCP resource data (JSON; URIs are opaque server identifiers, not host fetch targets):\n"
    rendered = _bounded_json(payload, budget - len(label)).decode("ascii")
    check()
    return {
        "content": [{"type": "text", "text": label + rendered}],
        "is_error": False,
        "metadata": {"mcp": {
            "serverId": binding.server_id, "tool": binding.published_name,
            "operation": operation, "serverCatalogRevision": binding.server_catalog_revision,
            "approval": binding.approval,
        }},
    }


def _complete(result: Any) -> None:
    if not isinstance(result, Mapping):
        raise ToolResultError("MCP resource result must be an object")
    result_type = result.get("resultType", "complete")
    if result_type == "input_required":
        raise ResourceUnsupportedError(
            "MCP resources operation requires input_required interaction, which is unsupported; "
            "no continuation was sent."
        )
    if result_type != "complete":
        raise ToolResultError("MCP resource resultType is unsupported or malformed")
    if {"requestState", "inputRequests"} & set(result):
        raise ToolResultError("MCP complete resource result contained continuation data")


def _string(value: Any, maximum: int, *, nonempty: bool = False) -> str:
    if not isinstance(value, str) or (nonempty and not value):
        raise ToolResultError("MCP resource string was malformed")
    try:
        if len(value.encode("utf-8")) > maximum:
            raise ToolResultError("MCP resource string exceeded its byte bound")
    except UnicodeEncodeError as error:
        raise ToolResultError("MCP resource string must be valid Unicode") from error
    return value


def _json(value: Any) -> bytes:
    # Escaping, not sanitizing/truncating, preserves opaque URI/template/cursor
    # identity and exact text while keeping controls out of the rendered result.
    return json.dumps(value, ensure_ascii=True, allow_nan=False, separators=(",", ":")).encode("ascii")


def _bounded_json(value: Any, maximum: int) -> bytes:
    nodes = 0

    def visit(item: Any, depth: int) -> None:
        nonlocal nodes
        nodes += 1
        if nodes > MAX_RESOURCE_NODES or depth > MAX_RESOURCE_DEPTH:
            raise ToolResultError("MCP resource data exceeded its structural bound")
        if isinstance(item, Mapping):
            for key, child in item.items():
                _string(key, 256)
                visit(child, depth + 1)
        elif isinstance(item, list):
            for child in item:
                visit(child, depth + 1)
        elif isinstance(item, str):
            _string(item, MAX_RESOURCE_RESULT_BYTES)
        elif item is None or isinstance(item, bool):
            pass
        elif type(item) is int:
            if not -(2**53 - 1) <= item <= 2**53 - 1:
                raise ToolResultError("MCP resource data contains a nonportable integer")
        elif isinstance(item, float) and math.isfinite(item):
            pass
        else:
            raise ToolResultError("MCP resource data must be finite JSON")

    visit(value, 0)
    encoded = _json(value)
    if len(encoded) > maximum:
        raise ToolResultError("MCP resource data exceeded its rendered byte bound")
    return encoded
