"""The octet API 0.4 runtime for Cua Driver computer use.

This binds the driver's live tool surface to octet tools and commands. The
runtime owns a single driver client per host owner, provisions the driver on
demand, and requires explicit user confirmation for any driver tool that is not
declared ``readOnlyHint: true``.

The published tool set is intentionally small and stable. The driver exposes 58
tools whose names and schemas are owned by an external project, so the runtime
re-publishes a reviewed subset as octet tools and passes their arguments
through with bounds rather than minting one octet tool per driver tool, which
would make octet's catalog churn with an upstream release.
"""

from __future__ import annotations

from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from octet_computer_use.driver_client import DriverClient, McpError, ToolInfo


# Driver tools republished as octet tools. Each entry names the driver tool and
# the confirmation policy. Read-only driver tools still require the driver to
# have been started, but never raise a confirmation.
PUBLISHED_TOOLS: Sequence[Tuple[str, str, str]] = (
    ("computer_use_status", "read_driver_health", "Report whether the Cua Driver is provisioned, its version, and OS permission status without changing anything."),
    ("computer_use_setup", "provision", "Provision the MIT-licensed Cua Driver into octet-owned state. Downloads from the configured package index."),
    ("computer_use_installed_apps", "list_apps", "List installed and running applications available to the driver."),
    ("computer_use_windows", "list_windows", "List top-level windows currently known to the desktop session."),
    ("computer_use_window_state", "get_window_state", "Return one window's accessibility tree and screenshot together. Re-snapshot before each element-indexed action."),
    ("computer_use_desktop_state", "get_desktop_state", "Capture the full desktop in true screen pixels."),
    ("computer_use_click", "click", "Click an element or coordinate in a target window."),
    ("computer_use_type_text", "type_text", "Type text into an element or the focused field of a target window."),
    ("computer_use_press_key", "press_key", "Send a key press to a target window."),
    ("computer_use_scroll", "scroll", "Scroll within a target window."),
    ("computer_use_launch_app", "launch_app", "Launch an application by name or bundle identifier."),
    ("computer_use_start_session", "start_session", "Start a named driver session so per-session cursor and cleanup state is released together."),
    ("computer_use_end_session", "end_session", "End a driver session and run its cleanup hooks."),
)

# Argument allowlists per published tool. Anything not named is dropped before
# it reaches the driver, so an upstream schema addition cannot silently widen
# what octet forwards.
_ARGUMENTS: Dict[str, Sequence[str]] = {
    "read_driver_health": (),
    "provision": ("version",),
    "list_apps": (),
    "list_windows": (),
    "get_window_state": (
        "pid",
        "window_id",
        "include_screenshot",
        "include_accessibility_tree",
        "max_elements",
        "max_depth",
        "query",
    ),
    "get_desktop_state": ("max_image_dimension",),
    "click": ("pid", "window_id", "element_index", "element_token", "x", "y", "button", "count", "delivery_mode"),
    "type_text": ("pid", "window_id", "element_index", "element_token", "text", "delivery_mode"),
    "press_key": ("pid", "window_id", "key", "delivery_mode"),
    "scroll": ("pid", "window_id", "element_index", "element_token", "direction", "amount", "delivery_mode"),
    "launch_app": ("name", "bundle_id", "urls"),
    "start_session": ("session", "capture_scope"),
    "end_session": ("session",),
}

# Ceilings applied to forwarded arguments.
MAX_TEXT_BYTES = 4096
MAX_INTEGER = 1 << 31
MAX_ELEMENTS = 2000
MAX_DEPTH = 25


class ArgumentError(ValueError):
    """A published tool received an argument outside its reviewed allowlist."""


def _bounded_int(value: Any, name: str, *, maximum: int = MAX_INTEGER) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise ArgumentError(f"{name} must be an integer")
    if value < -maximum or value > maximum:
        raise ArgumentError(f"{name} is out of range")
    return value


def _bounded_text(value: Any, name: str, *, limit: int = MAX_TEXT_BYTES) -> str:
    if not isinstance(value, str):
        raise ArgumentError(f"{name} must be a string")
    if len(value.encode("utf-8")) > limit:
        raise ArgumentError(f"{name} exceeds {limit} bytes")
    return value


def sanitize(driver_tool: str, values: Mapping[str, Any]) -> Dict[str, Any]:
    """Project caller arguments onto the reviewed subset for one driver tool."""

    allowed = _ARGUMENTS.get(driver_tool)
    if allowed is None:
        raise ArgumentError(f"no reviewed arguments for {driver_tool}")
    forwarded: Dict[str, Any] = {}
    for name in allowed:
        if name not in values or values[name] is None:
            continue
        value = values[name]
        if name in {"pid", "window_id", "element_index", "count", "amount"}:
            forwarded[name] = _bounded_int(value, name)
        elif name in {"max_elements"}:
            forwarded[name] = _bounded_int(value, name, maximum=MAX_ELEMENTS)
        elif name in {"max_depth", "max_image_dimension"}:
            forwarded[name] = _bounded_int(value, name, maximum=1 << 20)
        elif name in {"text", "key", "query", "session", "direction", "button", "delivery_mode", "name", "bundle_id", "capture_scope", "version"}:
            forwarded[name] = _bounded_text(value, name, limit=256 if name in {"key", "query", "session", "direction", "button", "delivery_mode", "bundle_id", "capture_scope", "version", "name"} else MAX_TEXT_BYTES)
        elif name in {"x", "y"}:
            forwarded[name] = _bounded_int(value, name, maximum=1 << 20)
        elif name in {"include_screenshot", "include_accessibility_tree"}:
            if not isinstance(value, bool):
                raise ArgumentError(f"{name} must be a boolean")
            forwarded[name] = value
        elif name in {"urls"}:
            if isinstance(value, list) and all(isinstance(item, str) for item in value):
                forwarded[name] = [_bounded_text(item, name, limit=2048) for item in value[:8]]
            elif isinstance(value, str):
                forwarded[name] = [_bounded_text(value, name, limit=2048)]
            else:
                raise ArgumentError("urls must be a string or an array of strings")
        else:
            forwarded[name] = value
    return forwarded


def summarize_result(result: Mapping[str, Any]) -> Dict[str, Any]:
    """Reduce a driver result to a bounded, presentable summary.

    Full window screenshots and accessibility trees are large. The runtime keeps
    text blocks intact up to a ceiling and reports the presence of images rather
    than inlining megabytes of base64 into a tool result.
    """

    text_parts: List[str] = []
    image_count = 0
    for block in result.get("content", []) or []:
        if not isinstance(block, Mapping):
            continue
        kind = block.get("type")
        if kind == "text":
            text_parts.append(str(block.get("text") or ""))
        elif kind == "image":
            image_count += 1
    text = "\n".join(text_parts)
    truncated = len(text) > 200_000
    if truncated:
        text = text[:200_000] + "\n[truncated]"
    return {
        "is_error": bool(result.get("isError")),
        "text": text,
        "image_count": image_count,
        "truncated": truncated,
    }
