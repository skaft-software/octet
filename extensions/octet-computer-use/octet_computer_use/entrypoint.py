"""Bind the Cua Driver service to the octet extension API.

Tool and command names are registered from :data:`service.PUBLISHED_TOOLS` so
the manifest and the runtime cannot drift apart. Every effectful driver call
passes through :meth:`ComputerUse._confirm` before it is dispatched, and an
unavailable, declined, or failed confirmation denies the call.
"""

from __future__ import annotations

import threading
from typing import Any, Dict, Mapping, Optional, Tuple

from octet_extension import Extension, text_content, tool_result

from octet_computer_use import driver as driver_module
from octet_computer_use import service
from octet_computer_use.driver_client import DriverClient, McpError
from octet_computer_use.service import ArgumentError

# Driver tools that never change the desktop. Everything else is gated.
LOCAL_ONLY_TOOLS = frozenset({"read_driver_health", "provision"})

# Bound on text returned to the model, matching octet-browse's result bound.
RESULT_TEXT_LIMIT = 24_000


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


class ComputerUse:
    """Owns the driver client and enforces the confirmation boundary."""

    def __init__(self, extension: Extension, *, home: Optional[Any] = None) -> None:
        self._extension = extension
        self._paths = driver_module.DriverPaths.for_home(home)
        self._client: Optional[DriverClient] = None
        self._lock = threading.Lock()

    # -- driver lifecycle --------------------------------------------------

    def client(self) -> DriverClient:
        with self._lock:
            if self._client is not None and self._client.started:
                return self._client
            binary = driver_module.installed_binary(self._paths)
            if binary is None:
                raise McpError(
                    "Cua Driver is not installed. Run computer_use_setup to provision it."
                )
            client = DriverClient(binary)
            client.start()
            self._client = client
            return client

    def shutdown(self) -> None:
        with self._lock:
            if self._client is not None:
                self._client.close()
                self._client = None

    # -- dispatch ----------------------------------------------------------

    def call(self, driver_tool: str, values: Mapping[str, Any]) -> Dict[str, Any]:
        """Run one driver tool, gating it when it is not read-only."""

        if driver_tool in LOCAL_ONLY_TOOLS:
            raise ArgumentError(f"{driver_tool} is handled locally")
        arguments = service.sanitize(driver_tool, values)
        client = self.client()
        if client.requires_confirmation(driver_tool):
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

    def status(self) -> Dict[str, Any]:
        return driver_module.health(self._paths).as_dict()

    def provision(self, version: str = "") -> Dict[str, Any]:
        binary = driver_module.provision(self._paths, version=version)
        return {
            "provisioned": True,
            "binary": str(binary),
            "version": driver_module.driver_version(binary),
        }


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
        )(lambda args, ctx, _n=name: handler(_n, args, ctx))

    def setup_command(arguments: Any, context: Mapping[str, Any]) -> Dict[str, Any]:
        result = computer_use.provision()
        return tool_result(text_content(_render_status(computer_use.status())), structured_content=result)

    extension.command(
        name="computer-use",
        description="Provision and report the Cua Driver used for native computer use.",
    )(setup_command)

    extension.on_shutdown(computer_use.shutdown)
    return extension, computer_use


_DRIVER_TOOLS = {name: driver_tool for name, driver_tool, _ in service.PUBLISHED_TOOLS}


def _render_status(status: Mapping[str, Any]) -> str:
    if not status.get("installed"):
        return (
            "Cua Driver is not installed. Run the /computer-use command or the "
            "computer_use_setup tool to provision it from the package index."
        )
    lines = [
        f"Cua Driver {status.get('version') or 'unknown'} is installed.",
        f"driver self-check: {'ok' if status.get('doctor_ok') else 'needs attention'}",
        f"OS permissions: {status.get('permissions')}",
    ]
    if status.get("permissions") not in {"granted", "ok", "authorized"}:
        lines.append(
            "Grant Accessibility and Screen Recording to the driver before acting. "
            "The bundle never grants an operating-system permission for you."
        )
    return "\n".join(lines)


def main() -> None:
    extension, _ = create_extension()
    extension.run()
