"""Bind the Cua Driver service to the octet extension API.

Tool and command names are registered from :data:`service.PUBLISHED_TOOLS` so
the manifest and the runtime cannot drift apart. Effectful driver calls follow
the configured confirmation policy; failed or declined prompts deny dispatch.
"""

from __future__ import annotations

import base64
import hashlib
import os
import platform
import subprocess
import threading
import time
from pathlib import Path
from typing import Any, Dict, List, Mapping, Optional, Tuple

from octet_extension import Extension, RpcError, image_content, text_content, tool_result

from octet_computer_use import driver as driver_module
from octet_computer_use import cursor_theme, gnome_helper, service, jev_use_binding, jev_use_jobs
from octet_computer_use.driver_client import DriverClient, McpError
from octet_computer_use.jev import (
    Candidate,
    JevContractError,
    JevUnavailable,
    choose_action,
    clear_key,
    store_key,
)
from octet_computer_use.jev import sdk_installed
from octet_computer_use.jev import status as jev_status
from octet_computer_use.screenshots import HostArtifactTransport
from octet_computer_use.service import ArgumentError

# Driver tools that never change the desktop. Everything else is gated.
LOCAL_ONLY_TOOLS = frozenset({"read_driver_health", "provision", "jev_status", "jev_choose"}) | frozenset(
    "jev_use_" + operation for operation in jev_use_binding.TOOLS
)

# Driver tools that address one specific window and therefore need a window_id.
_WINDOW_SCOPED_TOOLS = frozenset(
    {
        "get_window_state",
        "click",
        "type_text",
        "press_key",
        "hotkey",
        "invoke_menu",
        "move_cursor",
        "scroll",
        "double_click",
        "drag",
        "zoom",
        "verify_state",
        "set_window_frame",
    }
)

# Click may be screen-coordinate addressed. The public move_cursor tool always
# requires a named window; desktop scope is never exposed through it.
_COORDINATE_ONLY_TOOLS = frozenset({"click"})

# Driver tools that only observe. They are still gated by the driver's own
# read-only classification, but a missing OS grant must not hide their failure
# behind a permission refusal: the user needs to see the real error.
_READ_ONLY_OVERRIDE = frozenset(
    {"check_permissions", "get_screen_size", "get_cursor_position", "list_apps"}
)

# Bound on text returned to the model, matching octet-browse's result bound.
RESULT_TEXT_LIMIT = 24_000

# How long one macOS permission answer is reused for the effectful-action gate.
_PERMISSION_CACHE_SECONDS = 30.0


def _host_platform() -> str:
    """Lowercased `platform.system()`: `windows`, `darwin`, or `linux`."""
    return platform.system().lower()


def _pending_label(status: Mapping[str, Any]) -> str:
    """Short status-row noun for a not-ready runtime, per host platform.

    One branch point shared with the Linux work (PR #436): macOS names its
    grants, Windows names its target limits because there is no grant to
    chase (see README §Windows).
    """
    if _host_platform() == "windows":
        return "a non-elevated target"
    if status.get("screen_recording") is False:
        return "Screen Recording"
    return "macOS permissions"


def _readiness(status: Mapping[str, Any]) -> Tuple[bool, str]:
    """Whether effectful tools may run, and the short readiness label."""

    granted = (status.get("permissions") == "granted"
               and status.get("runtime") != "unavailable"
               and (status.get("runtime") != "desktop-host" or status.get("cursor_enabled") is True))
    if not status.get("installed"):
        return False, "not set up"
    if granted:
        return True, "ready"
    if status.get("runtime") == "unavailable":
        return False, "host unavailable"
    if status.get("runtime") == "desktop-host" and not status.get("cursor_enabled"):
        return False, "cursor not verified"
    if status.get("platform") == "linux":
        return False, "needs a desktop session"
    return False, "needs %s" % _pending_label(status)


def _not_ready_text() -> str:
    """Denial text when a missing OS grant holds back an effectful action."""
    if _host_platform() == "windows":
        return (
            "Computer use is not ready: the target window may be elevated "
            "(administrator) or on the secure desktop (a UAC prompt or the "
            "lock screen), which Windows does not allow a non-elevated octet "
            "to drive. Switch to a non-elevated window and retry."
        )
    if _host_platform() == "linux":
        return (
            "Computer use is not ready: the driver cannot reach a Linux display "
            "session. Start octet from a terminal inside your graphical session "
            "(X11, or Wayland such as Hyprland) and retry."
        )
    return (
        "Computer use is not ready: macOS has not allowed Accessibility and "
        "Screen Recording for the selected Cua runtime. Grant them to the "
        "selected host and retry."
    )

# Prefix for the driver session that owns the visible agent cursor. A session
# name belongs permanently to the transport that created it: the owner may
# re-take it, but any other transport is refused until it ends. A fixed name
# therefore breaks the second time octet launches, so each transport appends its
# own identifier and ends the session on shutdown.
CURSOR_SESSION_PREFIX = "octet-computer-use"

# Skip Cua's curved turns and spring bounce. A short fixed glide keeps the
# overlay responsive without spending time animating a longer path.
CURSOR_MOTION: Dict[str, Any] = {
    "arc_size": 0.0,
    "turn_radius": 1.0,
    "arc_flow": 0.0,
    "spring": 1.0,
    "glide_duration_ms": 80.0,
    "dwell_after_click_ms": 40.0,
    "idle_hide_ms": 5000.0,
}


def cursor_session() -> str:
    """A cursor session name unique to this driver transport.

    Uniqueness is what makes the cursor work on every launch: the driver binds a
    session name to the transport that first claims it, so a shared name would
    be refused after any restart.
    """

    return "%s-%d" % (CURSOR_SESSION_PREFIX, os.getpid())


def _schema_for(driver_tool: str) -> Dict[str, Any]:
    """A bounded input schema matching the runtime sanitizer."""

    if driver_tool.startswith("jev_use_"):
        return jev_use_binding.TOOLS[driver_tool.removeprefix("jev_use_")][1]
    allowed = service._ARGUMENTS.get(driver_tool, ())
    properties: Dict[str, Any] = {}
    for name in allowed:
        if driver_tool == "jev_choose" and name == "candidates":
            properties[name] = {
                "type": "array", "minItems": 1, "maxItems": service.MAX_CANDIDATES,
                "items": {"type": "object", "additionalProperties": False,
                          "required": ["identifier", "description"],
                          "properties": {"identifier": {"type": "string", "maxLength": 64},
                                         "description": {"type": "string", "maxLength": 1000}}},
            }
        elif driver_tool == "jev_choose" and name == "regions":
            properties[name] = {
                "type": "array", "maxItems": service.MAX_CANDIDATES,
                "items": {"type": "object", "additionalProperties": False,
                          "properties": {"id": {"type": "string", "maxLength": 64},
                                         "role": {"type": "string", "maxLength": 64},
                                         "label": {"type": "string", "maxLength": 200},
                                         "enabled": {"type": "boolean"}}},
            }
        elif driver_tool == "jev_choose" and name == "history":
            properties[name] = {"type": "array", "maxItems": service.MAX_HISTORY,
                                "items": {"type": "string", "maxLength": 400}}
        elif name == "keys":
            properties[name] = {"type": "array", "minItems": 2, "maxItems": service.MAX_HOTKEY_KEYS,
                                "items": {"type": "string", "maxLength": 32}}
        elif name == "path":
            properties[name] = {"type": "array", "minItems": 1, "maxItems": service.MAX_MENU_PATH,
                                "items": {"type": "string", "minLength": 1, "maxLength": 200}}
        elif name == "urls":
            properties[name] = {"anyOf": [{"type": "string", "maxLength": 2048},
                                           {"type": "array", "maxItems": 8,
                                            "items": {"type": "string", "maxLength": 2048}}]}
        elif name in {"pid", "window_id", "element_index", "max_elements", "max_depth",
                      "max_image_dimension", "x", "y", "count", "amount"}:
            limit = (service.MAX_ELEMENTS if name == "max_elements" else
                     1 << 20 if name in {"max_depth", "max_image_dimension", "x", "y"} else
                     service.MAX_INTEGER)
            minimum = 0 if driver_tool == "move_cursor" and name in {"x", "y"} else -limit
            properties[name] = {"type": "integer", "minimum": minimum, "maximum": limit}
        elif name in {"include_screenshot", "include_accessibility_tree"}:
            properties[name] = {"type": "boolean"}
        elif name in {"key", "query", "session", "direction", "button", "delivery_mode",
                      "name", "bundle_id", "capture_scope", "version"}:
            properties[name] = {"type": "string", "maxLength": 256}
        elif name in {"goal"}:
            properties[name] = {"type": "string", "maxLength": 4000}
        elif name in {"capture_id", "model", "snapshot_id", "element_token"}:
            properties[name] = {"type": "string", "maxLength": 128}
        else:
            properties[name] = {"type": "string", "maxLength": 4096}
    schema: Dict[str, Any] = {"type": "object", "properties": properties, "additionalProperties": False}
    required = {
        "get_window_state": ["pid"],
        "hotkey": ["keys"],
        "invoke_menu": ["pid", "window_id", "path"],
        "move_cursor": ["pid", "window_id", "x", "y"],
        "jev_choose": ["goal", "candidates"],
    }.get(driver_tool)
    if required:
        schema["required"] = required
    return schema


# Tools that return structured content must declare an output schema: the host
# rejects a ``structured_content`` payload from a tool that declared none, which
# would fail every status read. The schema is intentionally permissive about
# individual fields - the payload is diagnostic, and the host only needs a shape.
_OUTPUT_SCHEMAS: Dict[str, Dict[str, Any]] = {
    "read_driver_health": {
        "type": "object",
        "properties": {
            "installed": {"type": "boolean"},
            "version": {"type": ["string", "null"]},
            "permissions": {"type": "string"},
            "accessibility": {"type": "boolean"},
            "screen_recording": {"type": "boolean"},
            "doctor_ok": {"type": "boolean"},
            "detail": {"type": "string"},
            "permission_detail": {"type": "string"},
            "runtime": {"type": "string"},
            "runtime_binary": {"type": ["string", "null"]},
            "host_app": {"type": ["string", "null"]},
            "cursor_available": {"type": "boolean"},
            "cursor_enabled": {"type": "boolean"},
            "platform": {"type": "string"},
            "display_server": {"type": ["string", "null"]},
            "x11": {"type": "boolean"},
            "wayland": {"type": "boolean"},
            "atspi": {"type": "boolean"},
            "cursor_themes_installed": {"type": "integer"},
            "cursor_theme": {"type": "string"},
            "cursor_personalized": {"type": "boolean"},
            "cursor_detail": {"type": "string"},
            "gnome_helper": {"type": "string"},
            "gnome_helper_detail": {"type": "string"},
            "provisioned": {"type": "boolean"},
            "jev_note": {"type": "string"},
        },
    },
    "jev_status": {
        "type": "object",
        "properties": {
            "sdk_installed": {"type": "boolean"},
            "api_key_configured": {"type": "boolean"},
            "api_key_source": {"type": ["string", "null"]},
            "usable": {"type": "boolean"},
            "note": {"type": "string"},
        },
    },
    "jev_choose": {
        "type": "object",
        "properties": {
            "jev": {"type": "string"},
            "chosen": {"type": "object"},
            "candidate_ids": {"type": "array", "items": {"type": "string"}},
            "detail": {"type": "string"},
        },
    },
}

# Every republished driver tool now returns the driver's own structured payload,
# so every one of them needs a declared output shape or the host rejects the
# call with "structured_content requires a declared output_schema".
#
# The declared shape is deliberately permissive: it states that the payload is an
# object, without pinning the driver's per-tool fields. The driver's schemas are
# owned by an external project and change between releases, so a precise schema
# here would break on upgrade. The contract that actually matters - that the
# model receives the window ids, app names, and screenshots - is enforced by the
# forwarding code and covered by tests, not by this declaration.
_DRIVER_OUTPUT_SCHEMA: Dict[str, Any] = {
    "type": "object",
    "properties": {
        "text": {"type": "string"},
    },
    "additionalProperties": True,
}


def _output_schema_for(driver_tool: str) -> Optional[Dict[str, Any]]:
    """The declared output shape for a republished tool.

    A tool that returns structured content must declare a schema or the host
    refuses the result. Driver tools all forward the driver's payload, so they
    share one permissive object shape; the local tools declare their own.
    """

    if driver_tool.startswith("jev_use_"):
        return {"type": "object", "additionalProperties": True}
    specific = _OUTPUT_SCHEMAS.get(driver_tool)
    if specific is not None:
        return specific
    if driver_tool in service.PUBLISHED_DRIVER_TOOLS:
        return dict(_DRIVER_OUTPUT_SCHEMA)
    return None


# Whether every effectful action must be individually confirmed.
#
# octet's security model (SECURITY.md) is that full access - the default - runs
# with the user's own permissions and does not ask, and that --safe-mode is the
# mode that asks before every effectful action. Computer use inherits that: under
# full access an agent drives the desktop unattended, and the cursor overlay is
# the standing signal that it has control. Only when the user asks for a gate is
# each effectful action confirmed individually.
#
# The host does not currently pass its access mode to extensions, so this reads
# the environment. It defaults to open, matching the documented default rather
# than inventing a stricter one.
def confirmations_enabled() -> bool:
    """Whether every effectful action must be individually confirmed."""

    raw = os.environ.get("OCTET_CUA_CONFIRM", "").strip().lower()
    if raw in ("0", "false", "no", "off"):
        return False
    if raw in ("1", "true", "yes", "on"):
        return True
    # A host that reports a gated profile turns the gate on unless the user has
    # explicitly disabled it. Absent that signal, full access is assumed.
    gated = os.environ.get("OCTET_EFFECT_POLICY", "").strip().lower()
    return gated in ("safe", "workspace", "sandboxed", "restricted")


class ComputerUse:
    """Owns the driver client, action session, and confirmation boundary."""

    _direct_cursor = False
    _cursor_failure: Optional[str] = None

    def __init__(self, extension: Extension, *, home: Optional[Any] = None) -> None:
        self._extension = extension
        self._paths = driver_module.DriverPaths.for_home(home)
        self._client: Optional[DriverClient] = None
        self._lock = threading.Lock()
        self._permission_cache: Optional[Tuple[float, bool]] = None
        self._app_daemon = False
        # Linux's direct runtime draws its own X11/Wayland overlay. Unlike the
        # macOS host it was never the gate for computer use, so its cursor is
        # best-effort: a failure is reported, and actions still proceed.
        self._direct_cursor = False
        self._cursor_failure: Optional[str] = None
        self._binary: Optional[Path] = None
        self._cursor_session: Optional[str] = None
        self._cursor_ready = False
        self._theme_lab = "unknown"
        self._selected_theme = "cua.default"
        self._theme_ids = {"cua.default"}
        # The last full status probe, and whether a driver was found, for the
        # options menu, which must answer without starting the driver.
        self._last_status: Optional[Dict[str, Any]] = None
        self._installed_hint: Optional[bool] = None

    def select_model(self, context: Mapping[str, Any]) -> None:
        lab = cursor_theme.theme_for_host(context)
        with self._lock:
            if lab != self._theme_lab:
                self._theme_lab = lab
                self._cursor_ready = False
                self._cursor_failure = None

    @property
    def _cursor_runtime(self) -> bool:
        return self._app_daemon or self._direct_cursor

    def _ensure_cursor(self, client: DriverClient, session: str) -> None:
        """Bring the cursor up to date before an action, per runtime policy."""

        if self._app_daemon:
            self._show_cursor(client, session)
        elif self._direct_cursor and self._cursor_failure is None:
            try:
                self._show_cursor(client, session)
            except Exception as error:
                self._cursor_failure = str(error)[:300]

    _USE_DESKTOP_HOST = 1

    @classmethod
    def use_desktop_host(cls) -> bool:
        override = os.environ.get("OCTET_CUA_DESKTOP_HOST")
        if override is not None:
            return override.strip().lower() not in ("0", "false", "no", "off")
        return bool(cls._USE_DESKTOP_HOST)

    def client(self) -> DriverClient:
        with self._lock:
            if self._client is not None and self._client.started:
                if self._cursor_runtime and not self._cursor_ready:
                    self._ensure_cursor(self._client, self._cursor_session or cursor_session())
                return self._client

            use_host = self.use_desktop_host()
            app_binary = driver_module.desktop_app_binary() if use_host else None
            host_usable = bool(app_binary and driver_module.desktop_app_usable(app_binary))
            if driver_module.cursor_host_required() and not host_usable:
                raise McpError(
                    "Computer use is unavailable: the signed Cua Driver app at "
                    "/Applications/CuaDriver.app is unavailable or lacks live permissions; "
                    "refusing to fall back to a cursorless direct runtime."
                )
            app_daemon = bool(app_binary and host_usable)
            binary = app_binary if app_daemon else driver_module.installed_binary(self._paths)
            if binary is None:
                raise McpError("Cua Driver is not installed. Run computer_use_setup to provision it.")

            client = DriverClient(binary, app_daemon=app_daemon)
            client.start()
            self._client = client
            self._binary = Path(binary)
            self._app_daemon = app_daemon
            self._direct_cursor = not app_daemon and driver_module.host_platform() == "linux"
            self._cursor_failure = None
            if self._direct_cursor:
                self._theme_ids = self._linux_theme_ids(Path(binary))
                self._ensure_cursor(client, self._cursor_session or cursor_session())
            if app_daemon:
                try:
                    self._theme_ids = cursor_theme.installed_theme_ids(binary)
                except (OSError, ValueError, subprocess.SubprocessError):
                    self._theme_ids = {"cua.default"}
                try:
                    self._show_cursor(client, self._cursor_session or cursor_session())
                except Exception:
                    self._client = None
                    self._app_daemon = False
                    self._cursor_ready = False
                    failed_session, self._cursor_session = self._cursor_session, None
                    if failed_session:
                        try:
                            client.call("end_session", {"session": failed_session})
                        except Exception:
                            pass
                    client.close()
                    raise
            return client

    @staticmethod
    def _checked_call(client: DriverClient, tool: str, arguments: Mapping[str, Any]) -> Dict[str, Any]:
        result = client.call(tool, arguments)
        if result.get("isError") or result.get("is_error"):
            content = result.get("content") or []
            detail = next((str(item.get("text", "")) for item in content if isinstance(item, Mapping)), "")
            raise McpError(f"{tool} failed: {detail or 'driver rejected the request'}")
        return result

    @staticmethod
    def _payload(result: Mapping[str, Any]) -> Mapping[str, Any]:
        payload = result.get("structuredContent", result.get("structured_content"))
        return payload if isinstance(payload, Mapping) else result

    def _show_cursor(self, client: DriverClient, session: str) -> None:
        self._checked_call(client, "start_session", {"session": session})
        self._cursor_session = session
        self._configure_cursor(client, session)

    @staticmethod
    def _linux_theme_ids(binary: Path) -> set:
        """Installed theme IDs, installing the bundled ones first if missing.

        The Linux wheel has no theme compiler, so the bundled artifacts go
        straight into the driver's own store. Doing it here, not only in
        computer-use setup, is what makes model colors work as soon as the
        driver is provisioned by any route. Only reviewed bundled files are
        written, and an up-to-date store is left untouched.
        """

        try:
            ids = cursor_theme.installed_theme_ids(binary)
        except (OSError, ValueError, subprocess.SubprocessError):
            ids = {"cua.default"}
        wanted = {entry["id"] for entry in cursor_theme.PALETTE.values()}
        if not wanted <= ids:
            try:
                cursor_theme.install_bundled_themes(binary)
                ids = cursor_theme.installed_theme_ids(binary)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
                pass
        return ids

    def _configure_cursor(self, client: DriverClient, session: str) -> None:
        """Enable and verify the overlay on the same session used by actions."""

        self._cursor_ready = False
        try:
            motion_ack = self._payload(self._checked_call(
                client, "set_agent_cursor_motion", {"session": session, **CURSOR_MOTION}))
        except Exception as error:
            raise McpError(f"agent cursor motion configuration failed: {error}") from error
        desired = cursor_theme.theme_id(self._theme_lab)
        selected = desired if desired in self._theme_ids else "cua.default"
        try:
            theme_ack = self._payload(self._checked_call(client, "set_agent_cursor_theme", {
                "session": session, "theme_id": selected, "reduced_motion": "auto"}))
        except Exception as error:
            if not self._direct_cursor or selected == "cua.default":
                raise McpError(f"agent cursor theme selection failed: {error}") from error
            # A driver newer than the bundled artifacts may reject them; keep
            # the Linux cursor on Cua's default rather than losing it.
            selected = "cua.default"
            try:
                theme_ack = self._payload(self._checked_call(client, "set_agent_cursor_theme", {
                    "session": session, "theme_id": selected, "reduced_motion": "auto"}))
            except Exception as fallback:
                raise McpError(f"agent cursor theme selection failed: {fallback}") from fallback
        try:
            enabled_ack = self._payload(self._checked_call(
                client, "set_agent_cursor_enabled", {"session": session, "enabled": True}))
        except Exception as error:
            raise McpError(f"agent cursor enablement failed: {error}") from error
        # The overlay applies configuration asynchronously: its first read-back
        # can still contain the previous motion even after the setter succeeds.
        for attempt in range(4):
            try:
                state = self._payload(self._checked_call(client, "get_agent_cursor_state", {"session": session}))
            except Exception as error:
                raise McpError(f"agent cursor state verification failed: {error}") from error
            motion = state.get("motion")
            theme = state.get("theme")
            # Do not report readiness until the quick, straight motion is active.
            if (state.get("enabled") is True
                    and isinstance(motion, Mapping)
                    and all(motion.get(key) == value for key, value in CURSOR_MOTION.items())
                    and isinstance(theme, Mapping) and theme.get("id") == selected):
                break
            if attempt < 3:
                time.sleep(0.05)
        else:
            # Cua Driver 0.30 answers get_agent_cursor_state from its X11
            # overlay's state, so on native Wayland it reads back defaults even
            # though the layer-shell overlay renders the configured cursor. On
            # Linux, accept the setters' own acknowledgements of exactly what
            # was requested; macOS keeps requiring the read-back.
            acknowledged = (
                self._direct_cursor
                and enabled_ack.get("enabled") is True
                and isinstance(motion_ack.get("motion"), Mapping)
                and all(motion_ack["motion"].get(key) == value for key, value in CURSOR_MOTION.items())
                and isinstance(theme_ack.get("theme"), Mapping)
                and theme_ack["theme"].get("id") == selected)
            if not acknowledged:
                raise McpError("agent cursor is not enabled with configured motion; refusing to report readiness")
        self._cursor_session = session
        self._selected_theme = selected
        self._cursor_ready = True
        if self._direct_cursor and gnome_helper.is_gnome_wayland():
            # GNOME draws the cursor in its Shell helper, which colors it per
            # session; pin it to the same model color the theme carries.
            color = cursor_theme.PALETTE.get(self._theme_lab, cursor_theme.PALETTE["unknown"])["color"]
            gnome_helper.set_theme_color(color)

    def _permissions_block(self, driver_tool: str) -> bool:
        """Whether a missing OS grant must hold back this effectful action.

        Read-only observation is allowed to try and report its own failure, so
        the user can see what is missing. Only actuation is held, and only when
        the host actually denies a grant. An ``unknown`` probe is not treated as
        a denial: the driver may simply not be able to read TCC state, and
        blocking on that would disable a working install.

        The answer is cached for a short window. macOS permission state cannot
        flip within a conversation turn without a restart or a System Settings
        visit, and re-probing on every click would add a round trip to each
        action and pollute the driver's own dispatch sequence.
        """

        if driver_tool in _READ_ONLY_OVERRIDE:
            return False
        now = time.monotonic()
        cached = self._permission_cache
        if cached is not None and cached[0] > now:
            return cached[1]
        try:
            probe = driver_module.permission_state(self.client())
        except Exception:
            self._permission_cache = (now + _PERMISSION_CACHE_SECONDS, False)
            return False
        blocked = probe.get("permissions") == "denied"
        self._permission_cache = (now + _PERMISSION_CACHE_SECONDS, blocked)
        return blocked

    def shutdown(self) -> None:
        with self._lock:
            client, self._client = self._client, None
            session, self._cursor_session = self._cursor_session, None
            self._cursor_ready = False
        if client is not None:
            if session:
                try:
                    # Bounded so a hung driver cannot stall TUI exit: the
                    # session record is best-effort on this path.
                    client.call("end_session", {"session": session}, timeout=3.0)
                except Exception:
                    pass
            client.close()

    # -- dispatch ----------------------------------------------------------

    def call(self, driver_tool: str, values: Mapping[str, Any]) -> Dict[str, Any]:
        """Run one reviewed driver tool with the owned session and safety gate."""

        if driver_tool in LOCAL_ONLY_TOOLS:
            raise ArgumentError(f"{driver_tool} is handled locally")
        arguments = service.sanitize(driver_tool, values)
        client = self.client()

        # A window-state read usually needs the element tree, not its pixels.
        # The driver attaches a full screenshot by default, which made each
        # read publish megabytes even when the caller only wanted elements.
        # Only this tool accepts include_screenshot: desktop_state is itself a
        # screenshot, while list/health tools have no such argument.
        if driver_tool == "get_window_state" and "include_screenshot" not in arguments:
            arguments["include_screenshot"] = False

        # A public end may only end the session subsequent actions are attached
        # to. Refuse unrelated names rather than leaving the action binding stale.
        if driver_tool == "end_session":
            active = self._cursor_session
            requested = arguments.get("session")
            if not active or (requested is not None and requested != active):
                return tool_result(text_content("No matching active computer-use session to end."), is_error=True)

        # Window-scoped tools resolve a PID to a unique window when possible.
        by_coordinates = driver_tool in _COORDINATE_ONLY_TOOLS and "x" in arguments and "y" in arguments
        if driver_tool in _WINDOW_SCOPED_TOOLS and "window_id" not in arguments and not by_coordinates:
            resolved = service.resolve_window_id(client, arguments.get("pid"), arguments.get("window_id"))
            if resolved is None:
                return tool_result(
                    text_content(
                        f"{driver_tool} needs an exact window. Name its pid and call "
                        "computer_use_windows to find the window_id."
                    ),
                    is_error=True,
                )
            arguments["window_id"] = resolved

        if driver_tool == "invoke_menu" and "pid" not in arguments:
            return tool_result(text_content("invoke_menu requires the target pid and window_id."), is_error=True)
        if driver_tool == "move_cursor":
            if "pid" not in arguments or "window_id" not in arguments:
                return tool_result(text_content("move_cursor requires the exact pid and window_id."), is_error=True)
            # Driver move_cursor's window target is expressed as an exact target
            # object. Never forward desktop scope through the public tool.
            arguments["target"] = {"kind": "window", "pid": arguments.pop("pid"),
                                   "window_id": arguments.pop("window_id")}
            arguments["scope"] = "window"

        if client.requires_confirmation(driver_tool) and confirmations_enabled():
            if self._permissions_block(driver_tool):
                return tool_result(
                    text_content(_not_ready_text()),
                    is_error=True,
                )
            description = next((info.description for info in client.tools() if info.name == driver_tool), "")
            try:
                approved = self._extension.confirm(
                    "Allow this computer-use action?",
                    detail=f"Driver tool: {driver_tool}. {description[:400]}",
                    destructive=driver_tool in _DESTRUCTIVE,
                    default=False,
                )
            except Exception:
                return tool_result(text_content(f"Denied: confirmation for {driver_tool} could not be completed."), is_error=True)
            if approved is not True:
                return tool_result(text_content(f"Denied: the user did not confirm the Cua Driver tool {driver_tool}."), is_error=True)

        if driver_tool == "start_session":
            requested = arguments.get("session") or cursor_session()
            previous = self._cursor_session
            if previous and previous != requested:
                try:
                    self._checked_call(client, "end_session", {"session": previous})
                finally:
                    self._cursor_session = None
                    self._cursor_ready = False
            start_args = {key: value for key, value in arguments.items() if key in {"capture_scope"}}
            start_args["session"] = requested
            result = self._checked_call(client, "start_session", start_args)
            self._cursor_session = requested
            self._cursor_ready = False
            if self._direct_cursor and self._cursor_failure is None:
                try:
                    self._configure_cursor(client, requested)
                except Exception as error:
                    self._cursor_failure = str(error)[:300]
            if self._app_daemon:
                try:
                    self._configure_cursor(client, requested)
                except Exception:
                    try:
                        client.call("end_session", {"session": requested})
                    except Exception:
                        pass
                    self._cursor_session = None
                    self._cursor_ready = False
                    raise
            return self._format_result(driver_tool, result)

        if driver_tool == "end_session":
            session = self._cursor_session
            try:
                result = self._checked_call(client, "end_session", {"session": session})
            finally:
                # A dispatched end can have an uncertain acknowledgement. The
                # next action must explicitly start/configure a session again.
                self._cursor_session = None
                self._cursor_ready = False
            return self._format_result(driver_tool, result)

        if self._cursor_runtime and not self._cursor_ready:
            self._ensure_cursor(client, self._cursor_session or cursor_session())
        session = self._cursor_session
        if driver_tool in service.SESSION_SCOPED_TOOLS and session:
            arguments["session"] = session

        result = client.call(driver_tool, arguments)
        return self._format_result(driver_tool, result, query=arguments.get("query"))

    def _format_result(self, driver_tool: str, result: Mapping[str, Any], *, query: Optional[str] = None) -> Dict[str, Any]:
        summary = service.summarize_result(result)
        text = summary["text"][:RESULT_TEXT_LIMIT]
        parts: List[Mapping[str, Any]] = [text_content(text or "(no text content)")]
        structured = summary.get("structured")
        if isinstance(structured, dict):
            visible = _visible_targets(driver_tool, structured, query=query)
            if visible:
                if len(visible) > RESULT_TEXT_LIMIT:
                    visible = visible[:RESULT_TEXT_LIMIT - 50] + "\n[truncated; narrow the observation]"
                parts.insert(0, text_content(visible))
        transport = HostArtifactTransport(self._extension)
        for block in summary.get("images") or ():
            data = block.get("data")
            mime = str(block.get("mimeType") or block.get("mime_type") or "image/png")
            if not isinstance(data, str) or not data:
                continue
            try:
                raw = base64.b64decode(data, validate=True)
                artifact = transport.publish(
                    mime_type=mime, data=raw, size=len(raw), sha256=hashlib.sha256(raw).hexdigest(),
                )
            except Exception as error:
                cause = error.__cause__ or error
                code = getattr(cause, "code", type(cause).__name__)
                message = str(getattr(cause, "message", cause)).replace("\n", " ")[:200]
                parts.append(text_content(
                    "[octet] the driver returned a screenshot but it could not be delivered "
                    f"({code}: {message}). Treat this capture as unavailable; the tree may still be usable."
                ))
                continue
            parts.append(image_content(artifact, mime))
        return tool_result(
            *parts,
            structured_content=structured or {"text": text or ""},
            is_error=summary["is_error"],
            metadata={"driver_tool": driver_tool, "image_count": summary["image_count"],
                      "truncated": summary["truncated"]},
        )

    def status(self, *, prompt: bool = False) -> Dict[str, Any]:
        report = driver_module.health(self._paths).as_dict()
        if not report.get("installed"):
            return report
        # The CLI probe only answers from a CuaDriver daemon, which the
        # matter what the selected host runtime actually holds. Ask the live
        # session instead so status reports the permissions for dispatch.
        try:
            client = self.client()
            if self._app_daemon:
                report.update({"runtime": "desktop-host", "cursor_available": True,
                               "cursor_enabled": self._cursor_ready,
                               "cursor_theme": self._selected_theme,
                               "cursor_personalized": self._selected_theme != "cua.default"})
            elif self._direct_cursor:
                report.update({"cursor_available": True,
                               "cursor_enabled": self._cursor_ready,
                               "cursor_theme": self._selected_theme,
                               "cursor_personalized": self._selected_theme != "cua.default"})
                if self._cursor_failure:
                    report["cursor_detail"] = self._cursor_failure
                if gnome_helper.is_gnome_wayland():
                    report.update(gnome_helper.status())
            else:
                report["cursor_enabled"] = False
            probe = driver_module.permission_state(client, prompt=prompt)
        except Exception:
            if report.get("runtime") == "desktop-host":
                report.update({"permissions": "unknown", "doctor_ok": False,
                               "cursor_enabled": False,
                               "detail": "The selected desktop host's cursor session could not be verified."})
            return report
        # A fresh probe supersedes any cached answer.
        self._permission_cache = None
        # Only report what the probe actually answered: macOS names two grants,
        # Linux names its display session, and an unanswered field stays absent
        # rather than becoming a null the declared output shape does not allow.
        report["permissions"] = probe["permissions"]
        report["permission_detail"] = probe["detail"]
        for key in ("accessibility", "screen_recording", "display_server", "x11", "wayland", "atspi"):
            value = probe.get(key)
            if value is not None:
                report[key] = value
        return report

    def provision(self, version: str = "", *, progress: Optional[Any] = None) -> Dict[str, Any]:
        self.shutdown()
        binary = driver_module.provision(self._paths, version=version, progress=progress)
        with self._lock:
            self._installed_hint = True
            self._last_status = None
        # The next call starts a fresh session against the provisioned runtime.
        return {
            "provisioned": True,
            "binary": str(binary),
            "version": driver_module.driver_version(binary),
        }

    def publish_status(self, *, prompt: bool = False) -> Dict[str, Any]:
        """Report state and mirror it into octet's status row, then arm or hold.

        This runs on load and after setup, so an installed bundle reports its
        own readiness the way web-search does - no command required in the
        normal case. Effectful tools stay unavailable until both grants are
        present, so a missing permission is visible before a click fails
        opaquely.
        """

        status = self.status(prompt=prompt)
        granted, readiness = _readiness(status)
        with self._lock:
            self._last_status = dict(status)
            self._installed_hint = bool(status.get("installed"))
        try:
            self._extension.set_status(
                {"state": "active" if granted else "pending",
                 "label": "computer use · " + readiness}
            )
        except Exception:
            # A host without the status surface still gets the report; the
            # status row is an aid, never authority for actuation.
            pass
        return status

    def menu_state(self) -> Tuple[bool, Optional[Dict[str, Any]]]:
        """Whether a driver is installed, and the last full status probe.

        Cheap enough for the options menu: it never starts the driver, and
        only looks for an installed driver once per provisioning.
        """

        with self._lock:
            last = dict(self._last_status) if self._last_status is not None else None
            installed = self._installed_hint
        if last is not None:
            return bool(last.get("installed")), last
        if installed is None:
            try:
                installed = (driver_module.installed_binary(self._paths) is not None
                             or driver_module.desktop_app() is not None)
            except Exception:  # noqa: BLE001 - the menu reports, never fails
                installed = False
            with self._lock:
                self._installed_hint = installed
        return installed, None


# Driver tools that end or destroy user state and are labelled destructive in
# the confirmation prompt.
_DESTRUCTIVE = frozenset({"kill_app", "end_session", "stop_recording", "replay_trajectory"})


def create_extension(*, home: Optional[Any] = None) -> Tuple[Extension, ComputerUse]:
    extension = Extension(
        api_version="0.4",
        max_concurrent_requests=4,
        max_pending_requests=16,
        supported_features=(
            "request_cancellation", "content_parts", "artifacts", "lifecycle_events",
            "request_progress",
        ),
    )
    computer_use = ComputerUse(extension, home=home)
    jev_jobs = jev_use_jobs.Jobs(computer_use, extension, home=home)
    extension.on_lifecycle("session/settled")(jev_jobs.session_settled)

    def handler(name: str, arguments: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
        computer_use.select_model(context)
        values = dict(arguments) if isinstance(arguments, Mapping) else {}
        if name.startswith(jev_use_binding.PREFIX):
            return jev_jobs.handle(
                name.removeprefix(jev_use_binding.PREFIX), values, context,
                gated=confirmations_enabled(),
            )
        if name == "computer_use_status":
            result = computer_use.status()
            return tool_result(
                text_content(_render_status(result)),
                structured_content=result,
            )
        if name == "computer_use_setup":
            try:
                result = computer_use.provision(
                    values.get("version", "") or "",
                    progress=lambda message: _progress(extension, message),
                )
            except (driver_module.ProvisionError, OSError) as error:
                return _setup_failed(error)
            return tool_result(text_content(_render_status(computer_use.status())), structured_content=result)
        if name == "computer_use_jev_status":
            report = jev_status()
            if report["usable"]:
                source = report.get("api_key_source")
                detail = "Jev is usable"
                detail += " (key from environment)" if source == "environment" else " (key stored)"
                detail += "."
            elif not report["sdk_installed"]:
                detail = f"Jev is optional; its SDK is not installed. To add it, {JEV_HINT}."
            elif not report["api_key_configured"]:
                detail = f"Jev is optional; no API key yet. To add one, {JEV_HINT}."
            else:
                detail = "Jev is not usable."
            return tool_result(text_content(detail), structured_content=report)
        if name == "computer_use_jev_choose":
            return _handle_jev_choose(values)
        driver_tool = _DRIVER_TOOLS.get(name)
        if driver_tool is None:
            return tool_result(text_content(f"unknown tool: {name}"), is_error=True)
        try:
            return computer_use.call(driver_tool, values)
        except ArgumentError as error:
            return tool_result(text_content(f"invalid arguments: {error}"), is_error=True)
        except McpError as error:
            return tool_result(text_content(f"driver error: {error}"), is_error=True)

    for name, driver_tool, description in service.PUBLISHED_TOOLS:
        extension.tool(
            name=name,
            description=description,
            parameters=_schema_for(driver_tool),
            output_schema=_output_schema_for(driver_tool),
        )(lambda args, ctx, _n=name: handler(_n, args, ctx))

    def setup_command(arguments: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
        computer_use.select_model(context)
        parts = [str(part) for part in (arguments or [])]
        action = parts[0] if parts else "status"
        try:
            return run_command(action, parts, context)
        except RpcError:
            # Cancellation and protocol errors keep their own meaning.
            raise
        except Exception as error:  # noqa: BLE001 - name the failure, never an opaque -32603
            return _menu_result(tool_result(
                text_content(f"computer-use {action} failed: {error}"), is_error=True
            ))

    def run_command(action: str, parts: List[str], context: Mapping[str, Any]) -> Dict[str, Any]:
        if action == "jev-use":
            try:
                operation, options = jev_use_binding.command_options(parts[1:])
            except ValueError as error:
                return _menu_result(tool_result(text_content(str(error)), is_error=True))
            return _menu_result(jev_jobs.handle(operation, options, context, gated=confirmations_enabled()))
        if action == "setup" and len(parts) == 1:
            return _run_setup(extension, computer_use)
        if action == "jev" and len(parts) <= 2:
            return _run_jev(extension, computer_use, parts[1] if len(parts) == 2 else "setup")
        # A bare `computer-use` is a status check, as its default action says.
        if action == "status" and len(parts) <= 1:
            _progress(extension, "Running the driver self-check and permission probe…")
            result = computer_use.publish_status()
            return _menu_result(tool_result(text_content(_render_status(result)), structured_content=result))
        return _menu_result(tool_result(
            text_content(f"Unknown computer-use action. {driver_module.SETUP_HINT[0].upper()}"
                         f"{driver_module.SETUP_HINT[1:]}."),
            is_error=True,
        ))

    extension.command(
        name="computer-use",
        description=(
            "Set up, check, and configure native computer use (Cua Driver, "
            "optional Jev and the jev-use recipe)."
        ),
    )(setup_command)

    @extension.menu
    def options_menu(_request: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
        return _options_menu(computer_use, jev_jobs, context)

    def shutdown() -> None:
        jev_jobs.shutdown()
        computer_use.shutdown()

    extension.on_shutdown(shutdown)
    return extension, computer_use


_DRIVER_TOOLS = {name: driver_tool for name, driver_tool, _ in service.PUBLISHED_TOOLS}


def _setup_failed(error: BaseException) -> Dict[str, Any]:
    """Report a provisioning failure as a tool error, not a JSON-RPC one.

    An escaped exception reaches the host only as "internal error", which hides
    actionable causes such as an interpreter older than the driver supports.
    """

    return tool_result(text_content(f"Cua Driver setup failed: {error}"), is_error=True)


def _menu_result(value: Any) -> Dict[str, Any]:
    """Shape a menu-command outcome for the host's result document.

    The `command/execute` channel renders the ``text`` field; a tool-shaped
    ``{"content": [...]}`` dict has none, so the host would show only
    "<label> finished" and the report would be invisible. Every
    options-menu return path goes through here. Tool handlers keep the tool
    shape untouched.
    """

    if isinstance(value, Mapping):
        if isinstance(value.get("text"), str):
            return {"text": value["text"]}
        content = value.get("content")
        if isinstance(content, list):
            for part in content:
                if isinstance(part, Mapping) and part.get("type") == "text":
                    text = part.get("text")
                    if isinstance(text, str) and text:
                        return {"text": text}
    text = value if isinstance(value, str) else ""
    return {"text": text}


#: Where a person configures the optional Jev integration.
JEV_HINT = "open /extensions, choose octet-computer-use, then Jev"


def _progress(extension: Any, message: str) -> None:
    """Show one setup step live; without request progress it is skipped."""

    try:
        extension.progress(message=message)
    except Exception:  # noqa: BLE001 - progress is an aid, never a failure
        pass


def _run_setup(extension: Any, computer_use: "ComputerUse") -> Dict[str, Any]:
    """Everything computer use needs, in order, reporting each step live."""

    step = lambda message: _progress(extension, message)  # noqa: E731
    try:
        result = computer_use.provision(progress=step)
    except (driver_module.ProvisionError, OSError) as error:
        return _menu_result(_setup_failed(error))
    step("Installing the model-colored cursor themes…")
    try:
        result["cursor_themes_installed"] = cursor_theme.install_bundled_themes(
            Path(result["binary"]))
        computer_use._theme_ids = {entry["id"] for entry in cursor_theme.PALETTE.values()}
    except (OSError, RuntimeError, subprocess.SubprocessError) as error:
        return _menu_result(tool_result(
            text_content(f"Cua Driver was provisioned, but cursor theme setup failed: {error}"),
            is_error=True,
        ))
    if gnome_helper.is_gnome_wayland():
        # GNOME's compositor exposes window geometry, activation, and the
        # cursor only to its own Shell extensions.
        step("Installing the GNOME Shell helper…")
        result.update(gnome_helper.install())
    # The user asked for setup, so ask macOS now rather than making them run a
    # second step. The dialog is attributed to octet because the driver runs in
    # the host's responsibility chain. On Linux this only reads the session.
    step("Checking permissions; macOS may ask you to allow access…"
         if _host_platform() == "darwin" else "Checking the desktop session…")
    result.update(computer_use.publish_status(prompt=True))
    jev_outcome = _setup_jev(extension, computer_use)
    result.update(jev_outcome)
    result["jev_note"] = _render_jev_setup(jev_outcome)
    return _menu_result(tool_result(
        text_content(_render_status(result) + "\n\n" + result["jev_note"]),
        structured_content=result,
    ))


def _run_jev(extension: Any, computer_use: "ComputerUse", action: str) -> Dict[str, Any]:
    """Set up Jev, replace its stored key, or forget it."""

    if action == "setup":
        outcome = _setup_jev(extension, computer_use, dedicated=True)
    elif action == "key":
        outcome = _ask_for_jev_key(extension)
    elif action == "forget":
        clear_key()
        outcome = {"jev_setup": "forgotten", "jev": jev_status()}
    else:
        return _menu_result(tool_result(text_content(f"Unknown Jev action. To configure Jev, {JEV_HINT}."),
                           is_error=True))
    failed = outcome.get("jev_setup") in {"sdk_unavailable", "store_failed"}
    return _menu_result(tool_result(text_content(_render_jev_setup(outcome)),
                       structured_content=outcome, is_error=failed))


def _setup_jev(
    extension: Any,
    computer_use: "ComputerUse",
    *,
    dedicated: bool = False,
) -> Dict[str, Any]:
    """Offer the optional Jev setup, once, without ever blocking setup on it.

    Jev is optional. Within computer-use setup the user is asked a single
    yes/no question first; choosing Jev setup itself (``dedicated``) skips it.
    A "no", a cancelled prompt, or a frontend with no input surface all leave
    computer use exactly as it was. The API key is requested as a secret, so it
    is never echoed, logged, or placed in a command argument, and it is stored
    0600 under octet's own state.
    """

    if not dedicated:
        if jev_status()["usable"]:
            return {"jev_setup": "already", "jev": jev_status()}
        try:
            offer = extension.confirm(
                "Set up Jev for computer use?",
                detail=(
                    "Jev (TypeSafe) can choose which offered action to take next, "
                    "instead of relying only on the model. It is optional and needs "
                    "a TypeSafe API key."
                ),
                destructive=False,
                default=False,
            )
        except Exception:
            # No confirmation surface, or the request failed: stay optional.
            return {"jev_setup": "skipped"}

        if offer is not True:
            return {"jev_setup": "declined"}

    # Install the optional SDK into the same octet-owned venv as the driver.
    _progress(extension, "Installing the optional Jev SDK…")
    try:
        installed = driver_module.provision_jev(computer_use._paths)  # noqa: SLF001
    except Exception:  # noqa: BLE001 - JEV must never break setup
        installed = False
    if not installed:
        return {"jev_setup": "sdk_unavailable", "jev": jev_status()}

    # Use a key already present in the environment, or ask for one.
    if os.environ.get("TYPESAFE_API_KEY", "").strip():
        return {"jev_setup": "environment", "jev": jev_status()}
    return _ask_for_jev_key(extension)


def _ask_for_jev_key(extension: Any) -> Dict[str, Any]:
    """Ask for a TypeSafe key as a secret and store it privately."""

    try:
        entered = extension.request_input(
            "TypeSafe API key for Jev (stored privately, never logged). "
            "Leave blank to skip.",
            secret=True,
        )
    except Exception:
        return {"jev_setup": "sdk_only", "jev": jev_status()}

    if not entered or not entered.strip():
        return {"jev_setup": "sdk_only", "jev": jev_status()}

    stored = store_key(entered)
    # The entered value is deliberately not referenced again after this point.
    del entered
    if stored is None:
        return {"jev_setup": "store_failed", "jev": jev_status()}
    return {"jev_setup": "configured", "jev": jev_status()}


def _render_jev_setup(outcome: Mapping[str, Any]) -> str:
    """A short, key-free line describing what Jev setup did."""

    state = outcome.get("jev_setup")
    if state == "configured":
        return "Jev is configured (API key stored)."
    if state == "already":
        return "Jev is already configured."
    if state == "forgotten":
        if (outcome.get("jev") or {}).get("api_key_source") == "environment":
            return "Forgot the stored Jev API key; TYPESAFE_API_KEY from your environment is still used."
        return "Forgot the stored Jev API key."
    if state == "environment":
        return "Jev is using the TYPESAFE_API_KEY from your environment."
    if state == "sdk_only":
        return f"Jev SDK installed, but no API key was set. To add one, {JEV_HINT}."
    if state == "sdk_unavailable":
        return "Jev SDK could not be installed. Computer use is unaffected."
    if state == "store_failed":
        return "Could not store the Jev API key. Computer use is unaffected."
    if state == "declined":
        return "Jev setup declined. Computer use is unaffected."
    return "Jev setup skipped. Computer use is unaffected."


def _action(item_id: str, label: str, description: str, *arguments: str,
            recommended: bool = False, destructive: bool = False) -> Dict[str, Any]:
    item: Dict[str, Any] = {
        "id": item_id,
        "label": label,
        "description": description,
        "command": "computer-use",
        "arguments": list(arguments),
    }
    if recommended:
        item["recommended"] = True
    if destructive:
        item["destructive"] = True
    return item


def _options_menu(computer_use: "ComputerUse", jobs: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
    """Computer use's options under /extensions, from cached state only."""

    installed, last = computer_use.menu_state()
    if last is not None:
        ready, readiness = _readiness(last)
        status = {"state": "active" if ready else "pending",
                  "label": readiness[:1].upper() + readiness[1:]}
        detail = _render_status(last)
    elif installed:
        ready = False
        status = {"state": "pending", "label": "Installed"}
        detail = "Check status runs the driver self-check and reads what your desktop allows."
    else:
        ready = False
        status = {"state": "pending", "label": "Not set up"}
        detail = ("Set up installs Cua Driver into octet's private environment, adds the "
                  "cursor themes and desktop helpers, and checks what your desktop needs.")
    items = [
        _action(
            "setup",
            "Set up again" if installed else "Set up computer use",
            ("Update Cua Driver if needed, reinstall the helpers, and re-check permissions"
             if installed else
             "Install Cua Driver, cursor themes and desktop helpers, then check permissions"),
            "setup",
            recommended=not installed or (last is not None and not ready),
        ),
        _action("status", "Check status", "Run the driver self-check and permission probe",
                "status", recommended=installed and last is None),
        _jev_menu(),
        _jev_use_menu(jobs, context),
    ]
    return {"title": "Computer use", "status": status, "detail": detail, "items": items}


def _jev_menu() -> Dict[str, Any]:
    report = jev_status()
    if report["usable"]:
        state = ("Configured (key from your environment)"
                 if report.get("api_key_source") == "environment" else "Configured")
    elif report["sdk_installed"]:
        state = "SDK installed, no API key"
    else:
        state = "Not set up"
    items = []
    if not report["usable"]:
        items.append(_action("setup", "Set up Jev",
                             "Install the Jev SDK and enter your TypeSafe API key",
                             "jev", recommended=True))
    if report["sdk_installed"]:
        items.append(_action("key", "Replace API key" if report["api_key_configured"] else "Enter API key",
                             "Store a TypeSafe API key privately; it is never logged", "jev", "key"))
    if report.get("api_key_source") == "stored" or (
            report.get("api_key_source") == "environment" and _stored_jev_key()):
        items.append(_action("forget", "Forget stored API key",
                             "Delete the key octet stored for Jev", "jev", "forget",
                             destructive=True))
    return {
        "id": "jev",
        "label": "Jev (optional)",
        "description": state,
        "detail": ("Jev (TypeSafe) can choose which offered action computer use takes next, "
                   "instead of relying only on the model. It is optional and needs a TypeSafe "
                   "API key.\n\nStatus: " + state),
        "items": items,
    }


def _stored_jev_key() -> bool:
    try:
        from octet_computer_use.jev import key_path  # noqa: PLC0415
        return key_path().is_file()
    except OSError:
        return False


def _jev_use_menu(jobs: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
    items = [
        _action("status", "Check readiness", "Report whether the pinned recipe is installed",
                "jev-use", "status"),
        _action("setup", "Install the recipe",
                "Download the pinned Cua source and its locked Python dependencies",
                "jev-use", "setup"),
        _action("setup-typescript", "Install with TypeScript support",
                "Also install the recipe's TypeScript dependencies",
                "jev-use", "setup", "--typescript"),
        _action("run-mock", "Run the demo with mock Jev",
                "Drive the local form fixture in an isolated browser without calling TypeSafe",
                "jev-use", "run"),
        _action("run-live", "Run the demo with live Jev",
                "Sends compact synthetic state to TypeSafe and may incur charges",
                "jev-use", "run", "--live", destructive=True),
        {
            "id": "more",
            "label": "More run options",
            "description": "The TypeScript runner and the visual path",
            "items": [
                _action("typescript", "Run with the TypeScript runner",
                        "Needs the recipe installed with TypeScript support",
                        "jev-use", "run", "--typescript"),
                _action("typescript-live", "Run with the TypeScript runner and live Jev",
                        "Sends compact synthetic state to TypeSafe and may incur charges",
                        "jev-use", "run", "--typescript", "--live", destructive=True),
                _action("visual", "Run the visual path",
                        "Needs the separately installed Cua perception extension",
                        "jev-use", "run", "--visual-fixture", "--require-visual-path"),
            ],
        },
    ]
    for job_id, operation, state in jobs.owned(context)[:8]:
        job_items = [_action("check", "Check job", "Show this job's state or result",
                             "jev-use", "status", job_id)]
        if state == "running":
            job_items.append(_action("cancel", "Cancel job",
                                     "Signal the job to stop; this is not a rollback",
                                     "jev-use", "cancel", job_id, destructive=True))
        items.append({
            "id": "job:" + job_id,
            "label": f"Job {job_id[:8]} · {operation} · {state}",
            "items": job_items,
        })
    return {
        "id": "jev-use",
        "label": "jev-use recipe (advanced)",
        "description": "Upstream Cua demo where Jev fills a local test form",
        "detail": ("The pinned upstream jev-use recipe runs as a background job in its own "
                   "isolated browser against a local form fixture. It cannot ask before each "
                   "action, so runs are refused while per-action confirmation is on."),
        "items": items,
    }


# Keep model-visible target lists compact; structured details are not sent to the model.
_TARGETING_HINT_ROWS = 20
_LIST_ROWS = 100


def _visible_targets(driver_tool: str, structured: Mapping[str, Any], *, query: Optional[str] = None) -> str:
    """Project driver handles into text the model can actually read."""

    if driver_tool in {"list_windows", "list_apps"}:
        collection = "windows" if driver_tool == "list_windows" else "apps"
        records = structured.get(collection)
        if not isinstance(records, list):
            return ""
        lines = []
        for record in records[:_LIST_ROWS]:
            if not isinstance(record, Mapping):
                continue
            fields = ("pid", "window_id", "app_name", "title") if collection == "windows" else (
                "pid", "name", "bundle_id",
            )
            lines.append("  " + " ".join(
                f"{field}={str(record[field])[:100]!r}" if isinstance(record[field], str)
                else f"{field}={record[field]}"
                for field in fields if record.get(field) is not None
            ))
        shown = len(lines)
        if len(records) > _LIST_ROWS:
            guidance = "narrow the window list by pid" if collection == "windows" else "inspect a specific app by name"
            lines.append(f"  ... {len(records) - _LIST_ROWS} more; {guidance}.")
        return f"{collection}: {shown} shown of {len(records)} returned\n" + "\n".join(lines)
    if driver_tool == "get_window_state":
        return _targeting_hint(structured, query=query)
    return ""


def _targeting_hint(structured: Mapping[str, Any], *, query: Optional[str] = None) -> str:
    elements = structured.get("elements")
    if not isinstance(elements, list) or not elements:
        if query:
            return "No matching accessibility elements. Try another query or omit query."
        if structured.get("element_count", 0) or structured.get("returned_element_count", 0):
            return "[octet] Snapshot reported elements but has no usable tree; retry the observation."
        return ""
    lines = [
        "Addressing this window: pass element_token (preferred) or element_index "
        "together with snapshot_id. Tokens are stable only for this snapshot."
    ]
    snapshot = structured.get("snapshot_id")
    if isinstance(snapshot, str) and snapshot:
        lines.append(f"snapshot_id: {snapshot}")
    addressable = [
        element for element in elements
        if isinstance(element, dict)
        and (element.get("element_token") is not None or element.get("element_index") is not None)
    ]
    for element in addressable[:_TARGETING_HINT_ROWS]:
        lines.append(
            f"  token={element.get('element_token')} index={element.get('element_index')} "
            f"role={element.get('role')} label={str(element.get('label'))[:100]}"
        )
    if len(addressable) > _TARGETING_HINT_ROWS:
        lines.append(
            f"  ... {len(addressable) - _TARGETING_HINT_ROWS} more in this snapshot; "
            "use an index from the tree above with snapshot_id, or narrow with query."
        )
    return "\n".join(lines)


def _handle_jev_choose(values: Mapping[str, Any]) -> Dict[str, Any]:
    """Offer Jev a bounded candidate set and return the chosen id, or fail closed.

    This never dispatches a driver tool. It only decides which offered candidate
    the caller should take next; the caller remains responsible for resolving the
    identifier, executing it, and verifying the result.
    """

    try:
        values = service.sanitize("jev_choose", values)
    except ArgumentError as error:
        return tool_result(text_content(f"invalid arguments: {error}"), is_error=True)

    raw_candidates = values.get("candidates")
    if not isinstance(raw_candidates, (list, tuple)) or not raw_candidates:
        return tool_result(
            text_content("jev_choose requires a non-empty 'candidates' list."),
            is_error=True,
        )
    candidates: list[Candidate] = []
    for entry in raw_candidates:
        if isinstance(entry, Mapping):
            identifier = entry.get("identifier") or entry.get("id")
            description = entry.get("description") or entry.get("label")
            if isinstance(identifier, str) and isinstance(description, str):
                try:
                    candidates.append(Candidate(identifier, description))
                except ValueError:
                    # A malformed candidate is dropped, not fatal: a smaller valid
                    # set is still a bounded, safe offer.
                    continue
    if not candidates:
        return tool_result(
            text_content("jev_choose received no usable candidate actions."),
            is_error=True,
        )

    goal = values.get("goal")
    if not isinstance(goal, str) or not goal.strip():
        return tool_result(
            text_content("jev_choose requires a non-empty 'goal' string."),
            is_error=True,
        )

    regions = values.get("regions") if isinstance(values.get("regions"), list) else None
    history = values.get("history") if isinstance(values.get("history"), list) else None
    capture_id = values.get("capture_id")
    model = values.get("model") if isinstance(values.get("model"), str) else None

    try:
        choice = choose_action(
            goal=goal,
            candidates=candidates,
            capture_id=capture_id if isinstance(capture_id, str) else None,
            regions=regions,
            history=history,
            model=model,
        )
    except JevUnavailable as error:
        return tool_result(
            text_content(f"Jev is unavailable: {error}. Octet will choose without it."),
            structured_content={"jev": "unavailable", "detail": str(error)},
        )
    except JevContractError as error:
        return tool_result(
            text_content(
                f"Jev returned an out-of-contract answer ({error}); no action was taken."
            ),
            structured_content={"jev": "contract_error", "detail": str(error)},
            is_error=True,
        )

    summary = {
        "jev": "ok",
        "chosen": choice.as_dict(),
        "candidate_ids": [c.identifier for c in candidates],
    }
    return tool_result(
        text_content(
            f"Jev chose '{choice.identifier}' (confidence {choice.confidence:.2f}). "
            "This is a suggestion only; octet has not acted on it."
        ),
        structured_content=summary,
    )



def _render_status(status: Mapping[str, Any]) -> str:
    windows = _host_platform() == "windows"
    if status.get("runtime") == "unavailable":
        if windows:
            grant = ("Pick a non-elevated target on the interactive desktop; "
                     "elevated windows and the secure desktop are off-limits. ")
        else:
            grant = "Grant Accessibility and Screen Recording to that host; "
        return (
            "runtime: unavailable (the required Cua Driver desktop host is not ready). "
            f"{status.get('detail') or 'Install and authorize the selected desktop host.'} "
            f"{grant}"
            "OCTET_CUA_DESKTOP_HOST=0 explicitly selects cursorless direct mode."
        )
    if not status.get("installed"):
        return (
            f"Cua Driver is not installed. To install it, {driver_module.SETUP_HINT} "
            "(or ask for the computer_use_setup tool)."
        )
    check = "ok" if status.get("doctor_ok") else "needs attention"
    if not status.get("doctor_ok") and status.get("detail"):
        check += f" ({status['detail']})"
    lines = [
        f"Cua Driver {status.get('version') or 'unknown'} is installed.",
        f"driver self-check: {check}",
    ]
    # Say plainly which runtime is live, because the permission fix is different
    # for each and a user chasing the wrong one will never succeed.
    if status.get("runtime") == "desktop-host":
        cursor_state = "enabled" if status.get("cursor_enabled") else "not verified"
        lines.append(f"runtime: desktop host (agent cursor {cursor_state})")
        if status.get("cursor_enabled"):
            lines.append(f"cursor theme: {status.get('cursor_theme', 'cua.default')}")
            if not status.get("cursor_personalized"):
                lines.append(f"To install the bundled model-color themes, {driver_module.SETUP_HINT}.")
    elif status.get("platform") == "linux":
        lines.append("runtime: direct (drives the desktop session octet runs in)")
        _render_linux_session(status, lines)
        return "\n".join(lines)
    else:
        lines.append("runtime: direct (inherits your terminal's permissions)")
    permissions = status.get("permissions")
    if permissions == "granted":
        if windows:
            lines.append(
                "Windows: no separate grant is needed; octet cannot drive "
                "elevated windows or the secure desktop (UAC prompts, lock screen)."
            )
        else:
            lines.append("macOS permissions: Accessibility and Screen Recording allowed.")
        return "\n".join(lines)
    detail = status.get("permission_detail")
    if windows:
        lines.append("Windows limits: %s" % (detail or permissions or "unknown"))
        lines.append(
            "Drive a non-elevated window on the interactive desktop. Elevated "
            "windows and the secure desktop are off-limits to a non-elevated "
            "octet; see README §Windows."
        )
        return "\n".join(lines)
    lines.append("macOS permissions: %s" % (detail or permissions or "unknown"))
    if status.get("runtime") == "desktop-host":
        lines.append(
            "Grant Accessibility and Screen Recording to the selected Cua Driver "
            "desktop host, then retry. octet cannot grant a system permission for you."
        )
    else:
        lines.append(
            "Grant Accessibility and Screen Recording to the app you run octet from "
            "(your terminal or editor), then restart octet. The direct runtime uses "
            "that app's grants. octet cannot grant a system permission for you."
        )
    return "\n".join(lines)


def _render_linux_session(status: Mapping[str, Any], lines: List[str]) -> None:
    """Linux has no system grant to name; say which session is reachable."""

    permissions = status.get("permissions")
    detail = status.get("permission_detail")
    lines.append("Linux desktop session: %s" % (detail or permissions or "unknown"))
    if permissions != "granted":
        lines.append(
            "Start octet from a terminal inside your graphical session so DISPLAY or "
            "WAYLAND_DISPLAY, XDG_RUNTIME_DIR, and DBUS_SESSION_BUS_ADDRESS reach the "
            "driver. There is no system permission to grant on Linux."
        )
    elif status.get("atspi") is False:
        lines.append(
            "For element trees, install at-spi2-core (Arch/Omarchy: "
            "`sudo pacman -S at-spi2-core`) and log in again; pixel actions work without it."
        )
    if status.get("cursor_available"):
        if status.get("cursor_enabled"):
            lines.append(f"agent cursor: on (theme {status.get('cursor_theme', 'cua.default')})")
        else:
            lines.append("agent cursor: not shown%s" % (
                f" ({status['cursor_detail']})" if status.get("cursor_detail") else ""))
    helper = status.get("gnome_helper")
    if helper and helper != "active":
        lines.append("GNOME Shell helper: %s" % (
            status.get("gnome_helper_detail") or {
                "missing": "not installed; set up computer use from /extensions, then log out and back in",
                "upstream": "an older copy is loaded; set up computer use from /extensions, then log out and back in",
                "restart-required": "installed; log out and back in once to load it",
            }.get(helper, helper)))


def main() -> None:
    extension, _ = create_extension()
    extension.run()
