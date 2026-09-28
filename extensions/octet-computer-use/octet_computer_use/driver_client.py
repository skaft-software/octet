"""A minimal, bounded MCP stdio client for the Cua Driver.

This speaks the subset of the Model Context Protocol the driver actually uses:
``initialize``, ``notifications/initialized``, ``tools/list``, and
``tools/call``. It is intentionally not a general MCP implementation. The
driver is a fixed, local, single-owner server, and the octet extension host
already owns session lifetime, cancellation, confirmation, and presentation.

Safety properties:

* The direct client path uses ``mcp --direct``; when ``app_daemon`` is selected,
  it connects to the explicitly configured desktop-host socket. It never grants
  an operating-system permission on its own.
* Only reviewed, non-secret desktop session variables are forwarded. Provider
  tokens and arbitrary ambient environment are never inherited.
* Every response is size-bounded, and a tool that does not declare an exact
  ``readOnlyHint: true`` is classified as effectful so the caller can gate it.
"""

from __future__ import annotations

from dataclasses import dataclass
import json
import os
import queue
import subprocess
import threading
from pathlib import Path
from typing import Any, Dict, Iterable, List, Mapping, Optional, Sequence

CLIENT_NAME = "octet-computer-use"
CLIENT_VERSION = "0.8.0"
PROTOCOL_VERSION = "2025-06-18"

# Bounds. A window screenshot can be large, so the ceiling is generous, but it
# is still a ceiling: a hostile or broken driver cannot exhaust host memory.
MAX_MESSAGE_BYTES = 96 * 1024 * 1024
MAX_TOOLS = 256
STARTUP_TIMEOUT_SECONDS = 60.0
CALL_TIMEOUT_SECONDS = 120.0

# Reviewed desktop-session variables, mirroring the manifest declaration. These
# are what let a driver reach the interactive session of the desktop it runs
# on; none of them is a credential. Explicit configuration wins over these.
SESSION_ENVIRONMENT: Sequence[str] = (
    "DISPLAY",
    "WAYLAND_DISPLAY",
    "XDG_RUNTIME_DIR",
    "XDG_SESSION_TYPE",
    "XDG_DATA_HOME",
    "XDG_DATA_DIRS",
    "XDG_CONFIG_HOME",
    "DBUS_SESSION_BUS_ADDRESS",
    "XAUTHORITY",
    "APPDATA",
    "LOCALAPPDATA",
    "USERPROFILE",
    "SYSTEMROOT",
    "WINDIR",
)


def _frame(message: Mapping[str, Any]) -> bytes:
    """One MCP stdio message: UTF-8 JSON terminated by exactly one LF."""

    return (json.dumps(message) + "\n").encode("utf-8")


class McpError(RuntimeError):
    """The driver violated the protocol or the call failed."""


@dataclass(frozen=True)
class ToolInfo:
    """One published driver tool, classified for confirmation."""

    name: str
    description: str
    input_schema: Dict[str, Any]
    read_only: bool

    def as_dict(self) -> Dict[str, Any]:
        return {
            "name": self.name,
            "description": self.description,
            "read_only": self.read_only,
        }


def _child_environment(extra: Optional[Mapping[str, str]] = None) -> Dict[str, str]:
    environment: Dict[str, str] = {}
    for name in SESSION_ENVIRONMENT:
        value = os.environ.get(name)
        if value:
            environment[name] = value
    if extra:
        for name, value in extra.items():
            if isinstance(value, str):
                environment[name] = value
    return environment


def daemon_socket() -> Path:
    """Where the desktop host's daemon listens.

    The driver resolves this itself, but the client needs the exact path to
    address an already-running daemon rather than launch a second runtime.
    """

    override = os.environ.get("OCTET_CUA_DAEMON_SOCKET")
    if override:
        return Path(override)
    return Path.home() / "Library" / "Caches" / "cua-driver" / "cua-driver.sock"


class DriverClient:
    """A synchronous, single-owner MCP stdio client for one driver process."""

    def __init__(
        self,
        binary,
        *,
        cwd=None,
        environment: Optional[Mapping[str, str]] = None,
        app_daemon: bool = False,
    ) -> None:
        self._binary = str(binary)
        self._cwd = str(cwd) if cwd is not None else None
        self._environment = environment or {}
        # A selected CuaDriver.app host owns the runtime so macOS attributes
        # Accessibility/Screen Recording to the app and the app can draw the
        # agent cursor. Direct mode is explicit and has no cursor overlay.
        self._app_daemon = bool(app_daemon)
        self._process: Optional[subprocess.Popen] = None
        self._next_id = 0
        self._lock = threading.Lock()
        self._tools: Dict[str, ToolInfo] = {}
        self._started = False
        self._reader_queue: "queue.Queue[Optional[str]]" = queue.Queue()
        self._reader_thread: Optional[threading.Thread] = None

    # -- lifecycle ---------------------------------------------------------

    def start(self, timeout: float = STARTUP_TIMEOUT_SECONDS) -> None:
        if self._started:
            return
        if self._app_daemon:
            # The desktop host owns the runtime; address its daemon explicitly.
            # Bare `mcp` assumes an app-daemon proxy that is only reachable
            # from a launched GUI app, so it exits immediately when run as a
            # plain child process.
            arguments = ["mcp", "--socket", str(daemon_socket())]
        else:
            arguments = ["mcp", "--direct"]
        try:
            # Binary pipes with explicit UTF-8 framing: text mode would use the
            # ANSI code page on Windows, where a window title with an em dash
            # or curly quote fails to decode and reads as a closed stream, and
            # would write CRLF line endings.
            self._process = subprocess.Popen(
                [self._binary, *arguments],
                cwd=self._cwd,
                env=_child_environment(self._environment),
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
            )
        except OSError as error:
            raise McpError(f"failed to start the driver: {error}") from error

        # One reader owns stdout for the process lifetime. A reader per request
        # would race: the first thread would keep consuming lines and later
        # requests would block forever.
        self._reader_queue: "queue.Queue[Optional[str]]" = queue.Queue()
        process = self._process
        assert process.stdout is not None
        stream = process.stdout
        self._reader_thread = threading.Thread(
            target=self._read_lines, args=(stream, self._reader_queue), daemon=True
        )
        self._reader_thread.start()

        try:
            self._request(
                "initialize",
                {
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": {"name": CLIENT_NAME, "version": CLIENT_VERSION},
                },
                timeout=timeout,
            )
            self._notify("notifications/initialized")
            self.refresh_tools(timeout=timeout)
        except Exception:
            self.close()
            raise
        self._started = True

    def close(self) -> None:
        process = self._process
        self._process = None
        self._started = False
        if process is None:
            return
        # Terminate first so the reader thread sees EOF and exits on its own;
        # closing stdout underneath it would raise inside the thread.
        try:
            process.terminate()
            process.wait(timeout=5)
        except Exception:
            try:
                process.kill()
                process.wait(timeout=5)
            except Exception:
                pass
        for stream in (process.stdin, process.stdout):
            try:
                if stream is not None:
                    stream.close()
            except Exception:
                pass

    @staticmethod
    def _read_lines(stream, sink: "queue.Queue[Optional[str]]") -> None:
        try:
            while True:
                line = stream.readline()
                if not line:
                    sink.put(None)
                    return
                # MCP stdio is UTF-8. A malformed line becomes non-JSON and is
                # skipped by the caller instead of ending the stream.
                sink.put(line.decode("utf-8", "replace"))
        except (ValueError, OSError):
            sink.put(None)

    def __enter__(self) -> "DriverClient":
        self.start()
        return self

    def __exit__(self, *exc: Any) -> None:
        self.close()

    @property
    def started(self) -> bool:
        return self._started

    # -- catalog -----------------------------------------------------------

    def refresh_tools(self, timeout: float = STARTUP_TIMEOUT_SECONDS) -> List[ToolInfo]:
        payload = self._request("tools/list", {}, timeout=timeout)
        tools = payload.get("tools")
        if not isinstance(tools, list):
            raise McpError("tools/list did not return an array")
        if len(tools) > MAX_TOOLS:
            raise McpError(f"driver published more than {MAX_TOOLS} tools")
        catalog: Dict[str, ToolInfo] = {}
        for entry in tools:
            if not isinstance(entry, Mapping):
                continue
            name = entry.get("name")
            if not isinstance(name, str) or not name:
                continue
            annotations = entry.get("annotations")
            read_only = (
                isinstance(annotations, Mapping) and annotations.get("readOnlyHint") is True
            )
            schema = entry.get("inputSchema")
            catalog[name] = ToolInfo(
                name=name,
                description=str(entry.get("description") or ""),
                input_schema=dict(schema) if isinstance(schema, Mapping) else {},
                read_only=bool(read_only),
            )
        self._tools = catalog
        return list(catalog.values())

    def tools(self) -> List[ToolInfo]:
        return list(self._tools.values())

    def read_only_tools(self) -> List[str]:
        return [name for name, info in self._tools.items() if info.read_only]

    def effectful_tools(self) -> List[str]:
        return [name for name, info in self._tools.items() if not info.read_only]

    def requires_confirmation(self, tool: str) -> bool:
        """A tool is gated unless it is present and explicitly read-only.

        An unknown tool is treated as effectful. That is the fail-closed
        default: a tool the driver has not described cannot be assumed safe.
        """

        info = self._tools.get(tool)
        return info is None or not info.read_only

    # -- calls -------------------------------------------------------------

    def call(
        self, tool: str, arguments: Optional[Mapping[str, Any]] = None, *, timeout: float = CALL_TIMEOUT_SECONDS
    ) -> Dict[str, Any]:
        if not self._started:
            raise McpError("driver is not started")
        payload = self._request(
            "tools/call",
            {"name": tool, "arguments": dict(arguments or {})},
            timeout=timeout,
        )
        if not isinstance(payload, Mapping):
            raise McpError("tools/call returned a non-object result")
        return dict(payload)

    # -- transport ---------------------------------------------------------

    def _notify(self, method: str, params: Optional[Mapping[str, Any]] = None) -> None:
        process = self._process
        if process is None or process.stdin is None:
            raise McpError("driver is not running")
        message = {"jsonrpc": "2.0", "method": method, "params": dict(params or {})}
        try:
            process.stdin.write(_frame(message))
            process.stdin.flush()
        except (BrokenPipeError, ValueError) as error:
            raise McpError("driver stdin closed") from error

    def _request(
        self, method: str, params: Mapping[str, Any], *, timeout: float
    ) -> Dict[str, Any]:
        process = self._process
        if process is None or process.stdin is None or process.stdout is None:
            raise McpError("driver is not running")
        with self._lock:
            self._next_id += 1
            request_id = self._next_id
            message = {
                "jsonrpc": "2.0",
                "id": request_id,
                "method": method,
                "params": dict(params),
            }
            try:
                process.stdin.write(_frame(message))
                process.stdin.flush()
            except (BrokenPipeError, ValueError) as error:
                raise McpError("driver stdin closed") from error

            while True:
                try:
                    line = self._reader_queue.get(timeout=timeout)
                except queue.Empty:
                    raise McpError(f"driver did not answer {method} within {timeout}s")
                if line is None:
                    raise McpError("driver closed its output stream")
                if len(line.encode("utf-8", "replace")) > MAX_MESSAGE_BYTES:
                    raise McpError("driver message exceeded the size ceiling")
                try:
                    payload = json.loads(line)
                except json.JSONDecodeError:
                    # Ignore a non-JSON line rather than desynchronizing.
                    continue
                if not isinstance(payload, Mapping):
                    continue
                if payload.get("id") != request_id:
                    continue
                if "error" in payload:
                    error = payload.get("error")
                    message_text = ""
                    if isinstance(error, Mapping):
                        message_text = str(error.get("message") or "")
                    raise McpError(f"driver rejected {method}: {message_text or 'unknown error'}")
                value = payload.get("result")
                if not isinstance(value, Mapping):
                    raise McpError(f"{method} returned a non-object result")
                return dict(value)
