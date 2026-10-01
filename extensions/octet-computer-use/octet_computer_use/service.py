"""The octet API 0.4 runtime for Cua Driver computer use.

This binds the driver's live tool surface to octet tools and commands. The
runtime owns a single driver client per host owner, provisions the driver on
demand, and applies the configured confirmation policy to effectful driver tools.

The published tool set is intentionally small and stable. The driver exposes 58
tools whose names and schemas are owned by an external project, so the runtime
re-publishes a reviewed subset as octet tools and passes their arguments
through with bounds rather than minting one octet tool per driver tool, which
would make octet's catalog churn with an upstream release.
"""

from __future__ import annotations

import json
import math
from typing import Any, Dict, List, Mapping, Optional, Sequence, Tuple

from octet_computer_use.driver_client import DriverClient, McpError, ToolInfo
from octet_computer_use.jev_use_binding import TOOLS as JEV_USE_TOOLS, PREFIX as JEV_USE_PREFIX


# Driver tools republished as octet tools. Each entry names the driver tool and
# the confirmation policy. Read-only driver tools still require the driver to
# have been started, but never raise a confirmation.
PUBLISHED_TOOLS: Sequence[Tuple[str, str, str]] = (
    ("computer_use_status", "read_driver_health", "Report whether the Cua Driver is provisioned, its version, and OS permission status without changing anything."),
    ("computer_use_setup", "provision", "Provision the MIT-licensed Cua Driver into octet-owned state. Downloads from the configured package index."),
    ("computer_use_installed_apps", "list_apps", "List installed and running applications available to the driver."),
    ("computer_use_windows", "list_windows", "List top-level windows currently known to the desktop session."),
        ("computer_use_window_state", "get_window_state", "Return one window's accessibility tree, with a screenshot when include_screenshot is true. Re-snapshot before each element-indexed action."),
    ("computer_use_desktop_state", "get_desktop_state", "Capture the full desktop in true screen pixels."),
    ("computer_use_click", "click", "Click an element or coordinate in a target window."),
    ("computer_use_type_text", "type_text", "Type text into an element or the focused field of a target window."),
    ("computer_use_press_key", "press_key", "Send a key press to a target window."),
    ("computer_use_hotkey", "hotkey", "Press a key combination in a target window, such as Save As. Follows the driver's confirmation classification and fails closed on a rejected chord."),
    ("computer_use_invoke_menu", "invoke_menu", "Invoke an exact application-menu path through accessibility. Missing, ambiguous, or disabled items fail closed and are never clicked by pixel."),
    ("computer_use_move_cursor", "move_cursor", "Move the visible agent-cursor overlay within the named window. Use coordinates from a fresh computer_use_window_state of that same window; the real OS pointer is never moved."),
    ("computer_use_scroll", "scroll", "Scroll within a target window."),
    ("computer_use_launch_app", "launch_app", "Launch an application by name or bundle identifier."),
    ("computer_use_start_session", "start_session", "Start a named driver session so per-session cursor and cleanup state is released together."),
    ("computer_use_end_session", "end_session", "End a driver session and run its cleanup hooks."),
    ("computer_use_jev_status", "jev_status", "Report whether the optional Jev action chooser is installed and configured. Never returns the API key."),
    ("computer_use_jev_choose", "jev_choose", "Ask Jev to pick one offered candidate id, including reobserve or abstain. Returns a suggestion only; it does not execute or verify an action."),
)

PUBLISHED_TOOLS = tuple(PUBLISHED_TOOLS) + tuple(
    (JEV_USE_PREFIX + operation, "jev_use_" + operation, description)
    for operation, (description, _schema) in JEV_USE_TOOLS.items()
)

# Argument allowlists per published tool. Anything not named is dropped before
# it reaches the driver, so an upstream schema addition cannot silently widen
# what octet forwards.
_ARGUMENTS: Dict[str, Sequence[str]] = {
    "read_driver_health": (),
    "provision": ("version",),
    "list_apps": (),
    "list_windows": ("pid", "on_screen_only"),
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
    "jev_status": (),
    "jev_choose": ("goal", "candidates", "capture_id", "regions", "history", "model"),
    "click": ("pid", "window_id", "element_index", "element_token", "snapshot_id", "capture_id", "x", "y", "button", "count", "delivery_mode"),
    "type_text": ("pid", "window_id", "element_index", "element_token", "text", "delivery_mode"),
    "press_key": ("pid", "window_id", "key", "delivery_mode"),
    "hotkey": (
        "pid",
        "window_id",
        "keys",
        "element_index",
        "element_token",
        "snapshot_id",
        "x",
        "y",
        "delivery_mode",
    ),
    "invoke_menu": ("pid", "window_id", "path"),
    "move_cursor": ("pid", "window_id", "x", "y"),
    "scroll": ("pid", "window_id", "element_index", "element_token", "direction", "amount", "delivery_mode"),
    "launch_app": ("name", "bundle_id", "urls"),
    "start_session": ("session", "capture_scope"),
    "end_session": ("session",),
}

# Local recipe tools are validated by their binding and never reach Driver.
_ARGUMENTS.update({
    "jev_use_" + operation: tuple(schema["properties"])
    for operation, (_description, schema) in JEV_USE_TOOLS.items()
})

# Ceilings applied to forwarded arguments.
MAX_TEXT_BYTES = 4096
MAX_INTEGER = 1 << 31
MAX_ELEMENTS = 2000
MAX_DEPTH = 25
MAX_CANDIDATES = 64
MAX_HISTORY = 12
MAX_HOTKEY_KEYS = 6
MAX_MENU_PATH = 16
# Host ceiling is 256 KiB (crates/octet-agent/src/tool.rs). Stay under it even
# if the host canonicalizes key order differently from this encoder.
STRUCTURED_CONTENT_LIMIT = 256 * 1024
STRUCTURED_CONTENT_BUDGET = STRUCTURED_CONTENT_LIMIT - 2048

# Driver tools that accept a public session label. The extension injects its
# one action-session name; callers cannot address a different session.
SESSION_SCOPED_TOOLS = frozenset(
    {
        "get_window_state",
        "get_desktop_state",
        "click",
        "type_text",
        "press_key",
        "hotkey",
        "invoke_menu",
        "move_cursor",
        "scroll",
        "start_session",
        "end_session",
    }
)


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


def _string_array(value: Any, name: str, *, minimum: int, maximum: int, item_limit: int) -> List[str]:
    if not isinstance(value, list) or not minimum <= len(value) <= maximum:
        raise ArgumentError(f"{name} must be an array containing {minimum} to {maximum} items")
    return [_bounded_text(item, name, limit=item_limit) for item in value]


def _sanitize_candidates(value: Any) -> List[Dict[str, str]]:
    if not isinstance(value, list) or not 1 <= len(value) <= MAX_CANDIDATES:
        raise ArgumentError(f"candidates must be an array containing 1 to {MAX_CANDIDATES} objects")
    candidates: List[Dict[str, str]] = []
    for index, entry in enumerate(value):
        if not isinstance(entry, Mapping):
            raise ArgumentError(f"candidates[{index}] must be an object")
        identifier = entry.get("identifier", entry.get("id"))
        description = entry.get("description", entry.get("label"))
        candidates.append({
            "identifier": _bounded_text(identifier, f"candidates[{index}].identifier", limit=64),
            "description": _bounded_text(description, f"candidates[{index}].description", limit=1000),
        })
    return candidates


def _sanitize_regions(value: Any) -> List[Dict[str, Any]]:
    if not isinstance(value, list) or len(value) > MAX_CANDIDATES:
        raise ArgumentError(f"regions must be an array of at most {MAX_CANDIDATES} objects")
    regions: List[Dict[str, Any]] = []
    for index, entry in enumerate(value):
        if not isinstance(entry, Mapping):
            raise ArgumentError(f"regions[{index}] must be an object")
        clean: Dict[str, Any] = {}
        for key, limit in (("id", 64), ("role", 64), ("label", 200)):
            if key in entry:
                clean[key] = _bounded_text(entry[key], f"regions[{index}].{key}", limit=limit)
        if "enabled" in entry:
            if not isinstance(entry["enabled"], bool):
                raise ArgumentError(f"regions[{index}].enabled must be a boolean")
            clean["enabled"] = entry["enabled"]
        regions.append(clean)
    return regions


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
            coordinate = _bounded_int(value, name, maximum=1 << 20)
            if driver_tool == "move_cursor" and coordinate < 0:
                raise ArgumentError(f"{name} must be a non-negative window-local coordinate")
            forwarded[name] = coordinate
        elif name in {"include_screenshot", "include_accessibility_tree", "on_screen_only"}:
            if not isinstance(value, bool):
                raise ArgumentError(f"{name} must be a boolean")
            forwarded[name] = value
        elif name == "keys":
            forwarded[name] = _string_array(value, name, minimum=2, maximum=MAX_HOTKEY_KEYS, item_limit=32)
        elif name == "path":
            forwarded[name] = _string_array(value, name, minimum=1, maximum=MAX_MENU_PATH, item_limit=200)
            if any(not item.strip() for item in forwarded[name]):
                raise ArgumentError("path entries must not be empty")
        elif name == "candidates":
            forwarded[name] = _sanitize_candidates(value)
        elif name == "regions":
            forwarded[name] = _sanitize_regions(value)
        elif name == "history":
            forwarded[name] = _string_array(value, name, minimum=0, maximum=MAX_HISTORY, item_limit=400)
        elif name == "goal":
            forwarded[name] = _bounded_text(value, name, limit=4000)
        elif name in {"capture_id", "model", "snapshot_id", "element_token"}:
            forwarded[name] = _bounded_text(value, name, limit=128)
        elif name in {"urls"}:
            if isinstance(value, list) and len(value) <= 8 and all(isinstance(item, str) for item in value):
                forwarded[name] = [_bounded_text(item, name, limit=2048) for item in value]
            elif isinstance(value, str):
                forwarded[name] = [_bounded_text(value, name, limit=2048)]
            else:
                raise ArgumentError("urls must be a string or an array of strings")
        else:
            forwarded[name] = value
    return forwarded


# Driver tools that are republished through `ComputerUse.call` and therefore
# return the driver's own payload, so each needs a declared output schema.
PUBLISHED_DRIVER_TOOLS = frozenset(
    (
        "read_driver_health",
        "provision",
        "list_apps",
        "list_windows",
        "get_window_state",
        "get_desktop_state",
        "click",
        "type_text",
        "press_key",
        "hotkey",
        "invoke_menu",
        "move_cursor",
        "scroll",
        "launch_app",
        "start_session",
        "end_session",
        "jev_status",
        "jev_choose",
    )
)


def resolve_window_id(
    client: Any,
    pid: Any,
    window_id: Any = None,
) -> Optional[int]:
    """Pick the window a caller meant when it named only a process.

    The driver requires a ``window_id`` for every window-scoped call, but an
    agent almost always knows the *app* it wants, not a WindowServer handle.
    Resolving it here keeps that translation out of the model: without it a
    correct next step ("read this app's window") fails with a schema complaint
    that gives the agent nothing to act on.

    An explicit ``window_id`` always wins. Otherwise the process's own window is
    chosen, preferring an on-screen one and then the frontmost by ``z_index``.
    Returning ``None`` means the caller should report "that app has no single
    window" rather than act on a guess.
    """

    if isinstance(window_id, int) and not isinstance(window_id, bool):
        return window_id
    if not isinstance(pid, int) or isinstance(pid, bool):
        return None
    try:
        listed = client.call("list_windows", {"pid": pid})
    except Exception:
        return None
    structured = (listed or {}).get("structuredContent")
    windows = structured.get("windows") if isinstance(structured, dict) else None
    if not isinstance(windows, list) or not windows:
        return None

    def rank(entry: Mapping[str, Any]) -> tuple[int, int]:
        z = entry.get("z_index")
        front = z if isinstance(z, int) and not isinstance(z, bool) else -1
        on_screen = 1 if entry.get("is_on_screen") else 0
        return (on_screen, front)

    candidates = [
        entry
        for entry in windows
        if isinstance(entry, dict) and isinstance(entry.get("window_id"), int)
    ]
    if not candidates:
        return None
    chosen = max(candidates, key=rank).get("window_id")
    return chosen if isinstance(chosen, int) and not isinstance(chosen, bool) else None


def _json_size(value: Any) -> int:
    try:
        return len(json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8"))
    except (TypeError, ValueError, RecursionError):
        return STRUCTURED_CONTENT_LIMIT


def _bounded_identifier(value: Any) -> Any:
    if isinstance(value, str) and len(value) <= 512:
        return value
    if isinstance(value, bool):
        return value
    if isinstance(value, int) and -(1 << 63) <= value < (1 << 64):
        return value
    if isinstance(value, float) and math.isfinite(value) and abs(value) <= 1e100:
        return value
    return None


def _project_element(element: Mapping[str, Any]) -> Dict[str, Any]:
    projected: Dict[str, Any] = {}
    for key in ("element_index", "element_token", "role", "parent_index", "depth", "enabled", "selected"):
        if key not in element:
            continue
        item = element[key]
        if isinstance(item, str) and len(item) <= (256 if key == "element_token" else 64):
            projected[key] = item
        elif isinstance(item, bool):
            projected[key] = item
        elif isinstance(item, int) and -(1 << 63) <= item < (1 << 64):
            projected[key] = item
    label = element.get("label")
    if isinstance(label, str):
        projected["label"] = label[:180]
    frame = element.get("frame")
    if isinstance(frame, Mapping):
        projected["frame"] = {
            key: frame[key]
            for key in ("x", "y", "w", "h", "width", "height")
            if isinstance(frame.get(key), (int, float)) and not isinstance(frame.get(key), bool)
        }
    value = element.get("value")
    if isinstance(value, str) and len(value) <= 160:
        projected["value"] = value
    actions = element.get("actions")
    if isinstance(actions, list):
        projected["actions"] = [item[:40] for item in actions[:8] if isinstance(item, str)]
    return projected


def bound_structured_content(value: Mapping[str, Any]) -> Dict[str, Any]:
    """Keep structured driver output below octet's 256 KiB ceiling.

    Small payloads remain intact. Oversized snapshots are projected to bounded
    targeting metadata and element handles; markdown duplication and bulk AX
    values are omitted before elements are trimmed from the tail.
    """

    original = dict(value)
    if _json_size(original) <= STRUCTURED_CONTENT_BUDGET:
        return original

    projected: Dict[str, Any] = {}
    # These fields identify the exact surface/snapshot and are required to
    # target or verify a subsequent action.
    root_fields = (
        "snapshot_id", "capture_id", "pid", "window_id", "owner_pid",
        "app_name", "window_title", "title", "window_bounds", "bounds",
        "screenshot_width", "screenshot_height", "screenshot_scale",
        "screenshot_scales", "screenshot_frame_valid", "degraded",
        "degraded_reason", "truncated", "truncation_reasons", "element_count",
        "total_element_count", "returned_element_count", "filtered_element_count",
        "elements_complete", "screenshot_widths", "screenshot_heights",
    )
    for key in root_fields:
        if key in original:
            projected[key] = original[key]

    elements = original.get("elements")
    if isinstance(elements, list):
        projected["elements"] = [_project_element(item) for item in elements[:500] if isinstance(item, Mapping)]

    # Keep bounded window/app records with the exact identifiers needed to
    # continue targeting them.
    for collection, keys in (("windows", ("window_id", "pid", "app_name", "title", "bounds", "is_on_screen", "z_index")),
                             ("apps", ("pid", "name", "bundle_id", "running", "active", "launch_path"))):
        records = original.get(collection)
        if isinstance(records, list):
            projected[collection] = [
                {key: row[key] for key in keys if key in row}
                for row in records[:500] if isinstance(row, Mapping)
            ]

    projected["truncated"] = True
    projected["truncation_reason"] = "structured_content_bounded"
    projected["returned_element_count"] = len(projected.get("elements", []))
    while _json_size(projected) > STRUCTURED_CONTENT_BUDGET and projected.get("elements"):
        projected["elements"].pop()
        projected["returned_element_count"] = len(projected["elements"])
    while _json_size(projected) > STRUCTURED_CONTENT_BUDGET and projected.get("windows"):
        projected["windows"].pop()
    while _json_size(projected) > STRUCTURED_CONTENT_BUDGET and projected.get("apps"):
        projected["apps"].pop()
    if _json_size(projected) > STRUCTURED_CONTENT_BUDGET:
        # Last-resort scalar projection preserves valid target identifiers and
        # geometry, but drops pathological strings and non-scalar metadata.
        scalar_fields = (
            "snapshot_id", "capture_id", "pid", "window_id", "owner_pid",
            "app_name", "window_title", "title", "screenshot_width",
            "screenshot_height", "screenshot_scale", "degraded", "truncated",
        )
        minimal = {
            key: safe_value
            for key in scalar_fields
            if (safe_value := _bounded_identifier(projected.get(key))) is not None
        }
        for key in ("window_bounds", "bounds"):
            geometry = projected.get(key)
            if isinstance(geometry, Mapping):
                safe_geometry = {
                    field: number
                    for field, number in geometry.items()
                    if field in ("x", "y", "w", "h", "width", "height")
                    and isinstance(number, (int, float))
                    and not isinstance(number, bool)
                    and (not isinstance(number, float) or math.isfinite(number))
                    and abs(number) <= 1e100
                }
                if safe_geometry:
                    minimal[key] = safe_geometry
        minimal["truncated"] = True
        minimal["truncation_reason"] = "structured_content_bounded"
        projected = minimal
    return projected


def summarize_result(result: Mapping[str, Any]) -> Dict[str, Any]:
    """Reduce a driver result to a bounded, presentable summary.

    Text blocks are kept intact up to a ceiling. Image blocks are *not* inlined
    here: they are megabytes of base64 and the extension publishes them as host
    artifacts instead, so the model actually receives the pixels. The summary
    only reports how many images the driver returned and preserves the raw
    content blocks so the caller can publish them.
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
        # The driver's own structured payload carries the fields an agent needs
        # to target something: window_id, app_name, title, bounds, and the
        # per-app records list_apps returns. Dropping it left callers with a
        # bare count and no way to act on what they were shown.
        "structured": bound_structured_content(result["structuredContent"])
        if isinstance(result.get("structuredContent"), dict)
        else None,
        # Raw image blocks, kept so the caller can publish them as artifacts.
        "images": [
            dict(block)
            for block in (result.get("content") or [])
            if isinstance(block, Mapping) and block.get("type") == "image"
        ],
    }
