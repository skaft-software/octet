"""Bind the Cua Driver service to the octet extension API.

Tool and command names are registered from :data:`service.PUBLISHED_TOOLS` so
the manifest and the runtime cannot drift apart. Every effectful driver call
passes through :meth:`ComputerUse._confirm` before it is dispatched, and an
unavailable, declined, or failed confirmation denies the call.
"""

from __future__ import annotations

import os
import threading
import time
from typing import Any, Dict, Mapping, Optional, Tuple

from octet_extension import Extension, text_content, tool_result

from octet_computer_use import driver as driver_module
from octet_computer_use import service
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
from octet_computer_use.service import ArgumentError

# Driver tools that never change the desktop. Everything else is gated.
LOCAL_ONLY_TOOLS = frozenset({"read_driver_health", "provision", "jev_status", "jev_choose"})

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

# Prefix for the driver session that owns the visible agent cursor. A session
# name belongs permanently to the transport that created it: the owner may
# re-take it, but any other transport is refused until it ends. A fixed name
# therefore breaks the second time octet launches, so each transport appends its
# own identifier and ends the session on shutdown.
CURSOR_SESSION_PREFIX = "octet-computer-use"

# Straight-line, instant pointer travel. The theme's per-action animations are
# left alone on purpose: they are how the driver draws the eye to an action, so
# octet only removes the lag, not the signal.
CURSOR_MOTION: Dict[str, Any] = {
    "arc_size": 0.0,
    "turn_radius": 0.0,
    "arc_flow": 0.0,
    "spring": 1.0,
    "glide_duration_ms": 0.0,
    "dwell_after_click_ms": 0.0,
    # Keep the agent cursor on screen for as long as the session is active. The
    # cursor is the always-on indicator that an agent currently has control of
    # this desktop, so it must not fade out between actions. A session that ends
    # removes it. A day-long value is effectively "until the session ends".
    "idle_hide_ms": 86_400_000.0,
}


def cursor_session() -> str:
    """A cursor session name unique to this driver transport.

    Uniqueness is what makes the cursor work on every launch: the driver binds a
    session name to the transport that first claims it, so a shared name would
    be refused after any restart.
    """

    return "%s-%d" % (CURSOR_SESSION_PREFIX, os.getpid())


def _schema_for(driver_tool: str) -> Dict[str, Any]:
    """A permissive-but-bounded input schema for a republished driver tool."""

    allowed = service._ARGUMENTS.get(driver_tool, ())
    properties: Dict[str, Any] = {}
    for name in allowed:
        if name in {
            "pid",
            "window_id",
            "element_index",
            "max_elements",
            "max_depth",
            "max_image_dimension",
            "x",
            "y",
            "count",
            "amount",
        }:
            properties[name] = {"type": "integer"}
        elif name in {"include_screenshot", "include_accessibility_tree"}:
            properties[name] = {"type": "boolean"}
        elif name == "urls":
            properties[name] = {"anyOf": [{"type": "string"}, {"type": "array", "items": {"type": "string"}}]}
        else:
            properties[name] = {"type": "string"}
    return {
        "type": "object",
        "properties": properties,
        "additionalProperties": False,
    }


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


def _output_schema_for(driver_tool: str) -> Optional[Dict[str, Any]]:
    """The declared output shape, or ``None`` for tools that return text only.

    Only a permissive top-level object is declared: the host validates that
    structured content has a schema, not each field. Declaring one where the tool
    returns no structured content would be the opposite error.
    """

    return _OUTPUT_SCHEMAS.get(driver_tool)


class ComputerUse:
    """Owns the driver client and enforces the confirmation boundary."""

    def __init__(self, extension: Extension, *, home: Optional[Any] = None) -> None:
        self._extension = extension
        self._paths = driver_module.DriverPaths.for_home(home)
        self._client: Optional[DriverClient] = None
        self._lock = threading.Lock()
        # (expires_at_monotonic, blocked) for the effectful-action gate.
        self._permission_cache: Optional[Tuple[float, bool]] = None
        self._app_daemon = False
        self._cursor_session: Optional[str] = None

    # -- driver lifecycle --------------------------------------------------

    #: Select a desktop host automatically. Set to 0 to force the direct runtime.
    _USE_DESKTOP_HOST = 1

    @classmethod
    def use_desktop_host(cls) -> bool:
        override = os.environ.get("OCTET_CUA_DESKTOP_HOST")
        if override is not None:
            return override.strip() not in ("0", "false", "no", "off")
        return bool(cls._USE_DESKTOP_HOST)

    def client(self) -> DriverClient:
        with self._lock:
            if self._client is not None and self._client.started:
                return self._client
            # Prefer an installed desktop host: it owns the OS permission
            # identity and the GUI main thread, which is what enables the agent
            # cursor. Fall back to the direct runtime when no host is present.
            #
            # Presence is not enough. On some macOS releases the host installs
            # and launches correctly but its Accessibility/Screen Recording
            # grant is never persisted, so every launch re-prompts and every
            # tool call returns ``permissions_pending``. In that state the host
            # is strictly worse than no host: the direct runtime inherits the
            # *calling host's* grants and works immediately, at the cost of the
            # cursor overlay. So only adopt the host when its permissions are
            # actually live, and treat an unusable host as absent.
            app_binary = driver_module.desktop_app_binary() if self.use_desktop_host() else None
            app_daemon = app_binary is not None
            if app_daemon and not driver_module.desktop_app_usable(app_binary):
                app_binary = None
                app_daemon = False
            binary = app_binary or driver_module.installed_binary(self._paths)
            if binary is None:
                raise McpError(
                    "Cua Driver is not installed. Run computer_use_setup to provision it."
                )
            client = DriverClient(binary, app_daemon=app_daemon)
            client.start()
            self._client = client
            self._app_daemon = app_daemon
            if app_daemon:
                self._show_cursor(client)
            return client

    def _show_cursor(self, client: DriverClient) -> None:
        """Start this transport's session and give it a fast, low-latency cursor.

        The session name is unique to this transport, because the driver binds a
        name permanently to whoever claims it first and refuses every other
        transport. Motion is deliberately flat and instant: the default theme's
        per-action animations stay in place, so the twirl that draws the eye is
        preserved, while the pointer travels in a straight line with no settle
        pause. Best-effort throughout - the cursor must never block a tool call.
        """

        session = cursor_session()
        self._cursor_session = session
        try:
            client.call("start_session", {"session": session})
            client.call(
                "set_agent_cursor_motion",
                {"session": session, **CURSOR_MOTION},
            )
            client.call(
                "set_agent_cursor_enabled",
                {"session": session, "enabled": True},
            )
        except Exception:
            return

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
        if client is not None:
            # End the cursor session so the name is released. The driver binds a
            # name to its first owner, so a session left open would refuse the
            # next octet launch outright.
            if session:
                try:
                    client.call("end_session", {"session": session})
                except Exception:
                    pass
            client.close()

    # -- dispatch ----------------------------------------------------------

    def call(self, driver_tool: str, values: Mapping[str, Any]) -> Dict[str, Any]:
        """Run one driver tool, gating it when it is not read-only."""

        if driver_tool in LOCAL_ONLY_TOOLS:
            raise ArgumentError(f"{driver_tool} is handled locally")
        arguments = service.sanitize(driver_tool, values)
        client = self.client()
        if client.requires_confirmation(driver_tool):
            # Actuation needs the OS grants. Without them the driver would fail
            # deep inside a capture or click, so refuse here with the fix, and
            # do not spend the user's confirmation on an action that cannot
            # run.
            if self._permissions_block(driver_tool):
                return tool_result(
                    text_content(
                        "Computer use is not ready: macOS has not allowed "
                        "Accessibility and Screen Recording for octet. Run "
                        "/computer-use setup and choose Allow when macOS asks, "
                        "then retry."
                    ),
                    is_error=True,
                )
            description = next(
                (info.description for info in client.tools() if info.name == driver_tool),
                "",
            )
            try:
                approved = self._extension.confirm(
                    "Allow this computer-use action?",
                    detail=f"Driver tool: {driver_tool}. {description[:400]}",
                    destructive=driver_tool in _DESTRUCTIVE,
                    default=False,
                )
            except Exception:
                # A frontend with no confirmation surface, or a request that
                # failed, must deny rather than assume approval.
                return tool_result(
                    text_content(
                        f"Denied: the user confirmation request for {driver_tool} "
                        "could not be completed."
                    ),
                    is_error=True,
                )
            if approved is not True:
                return tool_result(
                    text_content(
                        f"Denied: the user did not confirm the Cua Driver tool {driver_tool}."
                    ),
                    is_error=True,
                )
        result = client.call(driver_tool, arguments)
        summary = service.summarize_result(result)
        text = summary["text"][:RESULT_TEXT_LIMIT]
        return tool_result(
            text_content(text or "(no text content)"),
            is_error=summary["is_error"],
            metadata={
                "driver_tool": driver_tool,
                "image_count": summary["image_count"],
                "truncated": summary["truncated"],
            },
        )

    def status(self, *, prompt: bool = False) -> Dict[str, Any]:
        report = driver_module.health(self._paths).as_dict()
        if not report.get("installed"):
            return report
        # The CLI probe only answers from a CuaDriver daemon, which the
        # pip-provisioned path never installs, so it would report `unknown` no
        # matter what the host actually holds. Ask the live direct session.
        try:
            probe = driver_module.permission_state(self.client(), prompt=prompt)
        except Exception:
            return report
        # A fresh probe supersedes any cached answer.
        self._permission_cache = None
        report.update(
            {
                "permissions": probe["permissions"],
                "accessibility": probe["accessibility"],
                "screen_recording": probe["screen_recording"],
                "permission_detail": probe["detail"],
            }
        )
        return report

    def provision(self, version: str = "") -> Dict[str, Any]:
        binary = driver_module.provision(self._paths, version=version)
        # Provisioning replaces the runtime, so a client that was already
        # running now points at a replaced binary. Drop it; the next call
        # starts a fresh session against the new one.
        with self._lock:
            if self._client is not None:
                self._client.close()
                self._client = None
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
        granted = status.get("permissions") == "granted"
        if not status.get("installed"):
            label = "computer use · not set up"
        elif granted:
            label = "computer use · ready"
        else:
            label = "computer use · needs %s" % (
                "Screen Recording"
                if status.get("screen_recording") is False
                else "macOS permissions"
            )
        try:
            self._extension.set_status(
                {"state": "active" if granted else "pending", "label": label}
            )
        except Exception:
            # A host without the status surface still gets the report; the
            # status row is an aid, never authority for actuation.
            pass
        return status


# Driver tools that end or destroy user state and are labelled destructive in
# the confirmation prompt.
_DESTRUCTIVE = frozenset({"kill_app", "end_session", "stop_recording", "replay_trajectory"})


def create_extension(*, home: Optional[Any] = None) -> Tuple[Extension, ComputerUse]:
    extension = Extension(
        api_version="0.4",
        max_concurrent_requests=4,
        max_pending_requests=16,
        supported_features=("request_cancellation", "content_parts"),
    )
    computer_use = ComputerUse(extension, home=home)

    def handler(name: str, arguments: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
        values = dict(arguments) if isinstance(arguments, Mapping) else {}
        if name == "computer_use_status":
            result = computer_use.status()
            return tool_result(
                text_content(_render_status(result)),
                structured_content=result,
            )
        if name == "computer_use_setup":
            result = computer_use.provision(values.get("version", "") or "")
            return tool_result(text_content(_render_status(computer_use.status())), structured_content=result)
        if name == "computer_use_jev_status":
            report = jev_status()
            if report["usable"]:
                source = report.get("api_key_source")
                detail = "Jev is usable"
                detail += " (key from environment)" if source == "environment" else " (key stored)"
                detail += "."
            elif not report["sdk_installed"]:
                detail = "Jev is optional; its SDK is not installed. Run /computer-use jev."
            elif not report["api_key_configured"]:
                detail = "Jev is optional; no API key yet. Run /computer-use jev to add one."
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
        parts = [str(part) for part in (arguments or [])]
        action = parts[0] if parts else "status"
        if action == "setup":
            result = computer_use.provision()
            # The user asked for setup, so ask macOS now rather than making
            # them re-run a second command. The dialog is attributed to octet
            # because the driver runs in the host's responsibility chain.
            result.update(computer_use.publish_status(prompt=True))
            jev_outcome = _setup_jev(extension, computer_use)
            result.update(jev_outcome)
            result["jev_note"] = _render_jev_setup(jev_outcome)
        elif action == "jev":
            outcome = _setup_jev(extension, computer_use, dedicated=True)
            return tool_result(
                text_content(_render_jev_setup(outcome)),
                structured_content=outcome,
            )
        elif action == "status":
            result = computer_use.publish_status()
        else:
            return tool_result(
                text_content("Usage: /computer-use [status|setup|jev]"),
                is_error=True,
            )
        return tool_result(
            text_content(_render_status(result)),
            structured_content=result,
        )

    extension.command(
        name="computer-use",
        description=(
            "Provision and report the Cua Driver used for native computer use, "
            "and optionally set up Jev."
        ),
    )(setup_command)

    extension.on_shutdown(computer_use.shutdown)
    return extension, computer_use


_DRIVER_TOOLS = {name: driver_tool for name, driver_tool, _ in service.PUBLISHED_TOOLS}


def _setup_jev(
    extension: Any,
    computer_use: "ComputerUse",
    *,
    dedicated: bool = False,
) -> Dict[str, Any]:
    """Offer the optional Jev setup, once, without ever blocking setup on it.

    Jev is optional. The user is asked a single yes/no question; a "no", a
    cancelled prompt, or a frontend with no input surface all leave computer use
    exactly as it was. The API key is requested as a secret, so it is never
    echoed, logged, or placed in a command argument, and it is stored 0600 under
    octet's own state.
    """

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
    try:
        installed = driver_module.provision_jev(computer_use._paths)  # noqa: SLF001
    except Exception:  # noqa: BLE001 - JEV must never break setup
        installed = False
    if not installed:
        return {"jev_setup": "sdk_unavailable", "jev": jev_status()}

    # Use a key already present in the environment, or ask for one.
    if os.environ.get("TYPESAFE_API_KEY", "").strip():
        return {"jev_setup": "environment", "jev": jev_status()}

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
    if state == "environment":
        return "Jev is using the TYPESAFE_API_KEY from your environment."
    if state == "sdk_only":
        return "Jev SDK installed, but no API key was set. Run /computer-use jev to add one."
    if state == "sdk_unavailable":
        return "Jev SDK could not be installed. Computer use is unaffected."
    if state == "store_failed":
        return "Could not store the Jev API key. Computer use is unaffected."
    if state == "declined":
        return "Jev setup declined. Computer use is unaffected."
    return "Jev setup skipped. Computer use is unaffected."


def _handle_jev_choose(values: Mapping[str, Any]) -> Dict[str, Any]:
    """Offer Jev a bounded candidate set and return the chosen id, or fail closed.

    This never dispatches a driver tool. It only decides which offered candidate
    the caller should take next; the caller remains responsible for resolving the
    identifier, executing it, and verifying the result.
    """

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
    if not status.get("installed"):
        return (
            "Cua Driver is not installed. Run the /computer-use setup command or "
            "the computer_use_setup tool to provision it from the package index."
        )
    lines = [
        f"Cua Driver {status.get('version') or 'unknown'} is installed.",
        f"driver self-check: {'ok' if status.get('doctor_ok') else 'needs attention'}",
    ]
    # Say plainly which runtime is live, because the permission fix is different
    # for each and a user chasing the wrong one will never succeed.
    if status.get("runtime") == "desktop-host":
        lines.append("runtime: desktop host (agent cursor enabled)")
    else:
        lines.append("runtime: direct (inherits your terminal's permissions)")
    permissions = status.get("permissions")
    if permissions == "granted":
        lines.append("macOS permissions: Accessibility and Screen Recording allowed.")
        return "\n".join(lines)
    detail = status.get("permission_detail")
    lines.append("macOS permissions: %s" % (detail or permissions or "unknown"))
    lines.append(
        "Grant Accessibility and Screen Recording to the app you run octet from "
        "(your terminal or editor), then restart octet. The direct runtime uses "
        "that app's grants. octet cannot grant a system permission for you."
    )
    return "\n".join(lines)


def main() -> None:
    extension, _ = create_extension()
    extension.run()
