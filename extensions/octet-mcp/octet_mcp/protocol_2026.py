"""Narrow MCP 2026-07-28 wire helpers, not a full protocol implementation.

Pinned evidence: modelcontextprotocol/modelcontextprotocol
 aa8ce049f089f92618340190d4ece141f663310d, specification/2026-07-28:
 basic/index, basic/versioning, server/discover, basic/transports/streamable-http.
Codex comparison: openai/codex 6baa076eb692b73ede097b4870dc45692c7556d9,
 rmcp-client/src/protocol_mode.rs and tests/mcp_2026_discovery.rs.

Only discovery, bounded tools, and explicit resource requests are implemented.
The HTTP adapter additionally drives bounded MRTR and advertises elicitation only
with an enabled, host-bound per-operation handler. No subscriptions, sampling,
roots, tasks, or other extension capabilities are offered.
"""

from __future__ import annotations

import base64
from dataclasses import dataclass
import re
from typing import Any, Mapping

from .catalog import CatalogError, MAX_SCHEMA_DEPTH, MAX_SCHEMA_NODES, normalize_schema
from .protocol import CLIENT_NAME, CLIENT_VERSION, McpError, McpProtocolError, McpRemoteError


MCP_PROTOCOL_VERSION_2026 = "2026-07-28"
META_PROTOCOL_VERSION = "io.modelcontextprotocol/protocolVersion"
META_CLIENT_INFO = "io.modelcontextprotocol/clientInfo"
META_CLIENT_CAPABILITIES = "io.modelcontextprotocol/clientCapabilities"
META_SERVER_INFO = "io.modelcontextprotocol/serverInfo"
MAX_MIRRORED_HEADERS = 64
MAX_MIRRORED_HEADER_BYTES = 16 * 1024
MAX_SAFE_INTEGER = (1 << 53) - 1
_HEADER_TOKEN = re.compile(r"[!#$%&'*+.^_`|~0-9A-Za-z-]+\Z")
_VERSION = re.compile(r"[0-9]{4}-[0-9]{2}-[0-9]{2}\Z")


def request_metadata() -> dict[str, Any]:
    return {
        META_PROTOCOL_VERSION: MCP_PROTOCOL_VERSION_2026,
        META_CLIENT_INFO: {"name": CLIENT_NAME, "version": CLIENT_VERSION},
        META_CLIENT_CAPABILITIES: {},
    }


def supported_versions(value: Any) -> tuple[str, ...]:
    if (not isinstance(value, list) or not 1 <= len(value) <= 16
            or any(not isinstance(item, str) or not _VERSION.fullmatch(item) for item in value)
            or len(set(value)) != len(value)):
        raise McpProtocolError(
            "invalid_protocol_versions", "MCP supported protocol versions were malformed", permanent=True
        )
    return tuple(value)


class McpModernRemoteError(McpRemoteError):
    """Recognized modern errors expose codes, not untrusted error text/data."""

    def __init__(self, code: int, *, versions: tuple[str, ...] = ()) -> None:
        names = {
            -32020: ("header_mismatch", "MCP HTTP metadata was rejected; refresh the catalog explicitly; the call was not replayed"),
            -32021: ("unsupported_client_capability", "MCP server requires a client capability this bridge does not implement"),
            -32022: ("unsupported_protocol", "MCP server rejected the explicitly selected 2026-07-28 protocol; no downgrade or call replay was attempted"),
        }
        name, summary = names[code]
        McpError.__init__(self, name, summary, permanent=True)
        self.rpc_code = code
        self.supported_versions = versions


def modern_remote_error(error: Mapping[str, Any]) -> McpRemoteError:
    code = error.get("code")
    if (isinstance(code, bool) or not isinstance(code, int)
            or not isinstance(error.get("message"), str)):
        raise McpProtocolError("invalid_response", "MCP JSON-RPC error was malformed", permanent=True)
    if code not in {-32020, -32021, -32022}:
        return McpRemoteError(code)
    versions: tuple[str, ...] = ()
    if code == -32022:
        data = error.get("data")
        if not isinstance(data, Mapping):
            raise McpProtocolError("invalid_response", "MCP version rejection omitted supported versions", permanent=True)
        versions = supported_versions(data.get("supported"))
        if data.get("requested", MCP_PROTOCOL_VERSION_2026) != MCP_PROTOCOL_VERSION_2026:
            raise McpProtocolError("invalid_response", "MCP version rejection did not match the request", permanent=True)
    return McpModernRemoteError(code, versions=versions)


@dataclass(frozen=True)
class HeaderParameter:
    path: tuple[str, ...]
    name: str
    primitive_type: str


def header_parameters(schema: Any) -> tuple[HeaderParameter, ...]:
    """Validate the raw schema before $ref expansion can erase reachability.

    Invalid definitions are excluded by the HTTP client, with a generic warning.
    The catalog's schema subset is intentionally narrower than full JSON Schema.
    """

    normalize_schema(schema, require_object=True)
    headers: list[HeaderParameter] = []
    names: set[str] = set()
    nodes = 0

    def visit(node: Mapping[str, Any], path: tuple[str, ...], reachable: bool, depth: int) -> None:
        nonlocal nodes
        nodes += 1
        if nodes > MAX_SCHEMA_NODES or depth > MAX_SCHEMA_DEPTH:
            raise CatalogError("MCP header schema exceeds structural bounds")
        if "x-mcp-header" in node:
            name = node["x-mcp-header"]
            kind = node.get("type")
            if (not reachable or not path or not isinstance(name, str)
                    or not _HEADER_TOKEN.fullmatch(name) or len(name) > 128
                    or name.lower() in names or kind not in ("string", "integer", "boolean")):
                raise CatalogError("MCP tool has an invalid x-mcp-header annotation")
            names.add(name.lower())
            headers.append(HeaderParameter(path, "Mcp-Param-" + name, kind))
            if len(headers) > MAX_MIRRORED_HEADERS - 3:
                raise CatalogError("MCP tool has too many mirrored header annotations")
        for key, item in node.items():
            if key == "properties":
                for name, child in item.items():
                    visit(child, (*path, name), reachable, depth + 1)
            elif key == "$defs":
                for child in item.values():
                    visit(child, path, False, depth + 1)
            elif key in {"allOf", "anyOf", "oneOf"}:
                for child in item:
                    visit(child, path, False, depth + 1)
            elif key == "items" or (key == "additionalProperties" and isinstance(item, Mapping)):
                visit(item, path, False, depth + 1)

    visit(schema, (), True, 0)
    return tuple(headers)


def encode_header_value(value: str) -> str:
    """MCP's exact UTF-8 Base64 sentinel encoding, including sentinel escaping."""

    try:
        raw = value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise McpProtocolError("invalid_header_value", "MCP header value was not valid Unicode") from error
    if len(raw) > MAX_MIRRORED_HEADER_BYTES:
        raise McpProtocolError("headers_too_large", "MCP request metadata exceeds the HTTP header bound")
    plain = (value == value.strip(" \t")
             and all(char == "\t" or 0x20 <= ord(char) <= 0x7E for char in value)
             and not (value.startswith("=?base64?") and value.endswith("?=")))
    return value if plain else "=?base64?" + base64.b64encode(raw).decode("ascii") + "?="


def request_headers(
    method: str, params: Mapping[str, Any], parameters: tuple[HeaderParameter, ...] = ()
) -> dict[str, str]:
    headers = {"MCP-Protocol-Version": MCP_PROTOCOL_VERSION_2026, "Mcp-Method": method}
    if method == "resources/read":
        uri = params.get("uri")
        if not isinstance(uri, str) or not uri:
            raise McpProtocolError("invalid_outbound", "MCP resource URI was malformed")
        # This is an opaque MCP identifier at the configured endpoint, never a
        # destination to fetch through the host's HTTP or filesystem tools.
        headers["Mcp-Name"] = encode_header_value(uri)
    if method == "tools/call":
        name = params.get("name")
        arguments = params.get("arguments", {})
        if not isinstance(name, str) or not name or not isinstance(arguments, Mapping):
            raise McpProtocolError("invalid_outbound", "MCP tool call name or arguments were malformed")
        headers["Mcp-Name"] = encode_header_value(name)
        for parameter in parameters:
            value: Any = arguments
            for segment in parameter.path:
                if not isinstance(value, Mapping) or segment not in value:
                    value = None
                    break
                value = value[segment]
            if value is None:
                continue
            if parameter.primitive_type == "string" and isinstance(value, str):
                text = value
            elif parameter.primitive_type == "boolean" and isinstance(value, bool):
                text = "true" if value else "false"
            elif (parameter.primitive_type == "integer" and not isinstance(value, bool)
                  and isinstance(value, (int, float)) and -MAX_SAFE_INTEGER <= value <= MAX_SAFE_INTEGER
                  and int(value) == value):
                text = str(int(value))
            else:
                raise McpProtocolError("invalid_header_value", "MCP mirrored argument has an invalid primitive type or integer range")
            headers[parameter.name] = encode_header_value(text)
    size = sum(len(key) + len(value) + 4 for key, value in headers.items())
    if len(headers) > MAX_MIRRORED_HEADERS or size > MAX_MIRRORED_HEADER_BYTES:
        raise McpProtocolError("headers_too_large", "MCP request metadata exceeds the HTTP header bound")
    return headers
