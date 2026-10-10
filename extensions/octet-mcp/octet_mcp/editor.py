"""Guarded edits to the user MCP configuration from the /extensions menu.

Only the user file is ever written, and only after the complete edited
document passes the same trust and schema checks as a launch: it is written to
a private temporary file beside the target, loaded with :func:`load_config`,
and only then renamed over the target. Trusted project files are read-only
here; the user edits them in the project, where their pinned digest lives.
"""

from __future__ import annotations

import copy
import json
import os
from pathlib import Path
import re
import shlex
import tempfile
from typing import Any, Callable, Mapping, Optional

from .config import (
    MAX_CONFIG_BYTES,
    STATIC_CREDENTIAL_AUTH_TYPE,
    STATIC_CREDENTIAL_ENVIRONMENT_PREFIX,
    BridgeConfig,
    ConfigError,
    _read_json_file,
    is_static_credential_environment,
    load_config,
)

SERVER_ID = re.compile(r"^[a-z][a-z0-9-]{0,31}$")
_ENVIRONMENT_NAME = re.compile(r"^[A-Za-z_][A-Za-z0-9_]*$")

# One guided prompt: returns the answer, or ``None`` when it was cancelled.
Ask = Callable[..., Optional[str]]


class EditCancelled(Exception):
    """The person cancelled a guided prompt; nothing was written."""


class ConfigEditor:
    """Read, edit, validate, and atomically replace one user configuration."""

    def __init__(
        self,
        path: Path,
        *,
        workspace: Optional[str],
        experimental_streamable_http_mcp: bool,
    ) -> None:
        self.path = path
        self.workspace = workspace
        self.experimental_streamable_http_mcp = experimental_streamable_http_mcp

    def document(self) -> dict[str, Any]:
        if not self.path.exists():
            return {"version": 1, "servers": {}}
        document, _bytes, _path = _read_json_file(self.path)
        document.setdefault("servers", {})
        if not isinstance(document["servers"], dict):
            raise ConfigError("servers must be an object")
        return document

    def user_servers(self) -> dict[str, Any]:
        return copy.deepcopy(self.document()["servers"])

    def save(self, document: Mapping[str, Any]) -> BridgeConfig:
        """Validate ``document`` exactly as a launch would, then publish it."""

        encoded = (json.dumps(document, indent=2, ensure_ascii=False) + "\n").encode("utf-8")
        if len(encoded) > MAX_CONFIG_BYTES:
            raise ConfigError(f"MCP configuration exceeds the {MAX_CONFIG_BYTES}-byte limit")
        self.path.parent.mkdir(mode=0o700, parents=True, exist_ok=True)
        descriptor, temporary = tempfile.mkstemp(
            prefix=".mcp.", suffix=".json.tmp", dir=self.path.parent
        )
        try:
            try:
                os.fchmod(descriptor, 0o600)
            except (AttributeError, OSError):  # pragma: no cover - Windows ACLs
                pass
            with os.fdopen(descriptor, "wb") as handle:
                handle.write(encoded)
                handle.flush()
                os.fsync(handle.fileno())
            self._load(Path(temporary))
            os.replace(temporary, self.path)
        except BaseException:
            try:
                os.unlink(temporary)
            except OSError:
                pass
            raise
        return self._load(self.path)

    def _load(self, path: Path) -> BridgeConfig:
        return load_config(
            path,
            workspace=self.workspace,
            experimental_streamable_http_mcp=self.experimental_streamable_http_mcp,
        )

    # -- edits ---------------------------------------------------------------

    def set_enabled(self, server_id: str, enabled: bool) -> BridgeConfig:
        document = self.document()
        descriptor = self._user_descriptor(document, server_id)
        descriptor["enabled"] = enabled
        return self.save(document)

    def remove(self, server_id: str) -> BridgeConfig:
        document = self.document()
        self._user_descriptor(document, server_id)
        del document["servers"][server_id]
        return self.save(document)

    def add(self, server_id: str, descriptor: Mapping[str, Any], taken: set[str]) -> BridgeConfig:
        document = self.document()
        if server_id in document["servers"] or server_id in taken:
            raise ConfigError(f"an MCP server named {server_id} already exists")
        document["servers"][server_id] = dict(descriptor)
        return self.save(document)

    def replace(self, server_id: str, descriptor: Mapping[str, Any]) -> BridgeConfig:
        document = self.document()
        self._user_descriptor(document, server_id)
        document["servers"][server_id] = dict(descriptor)
        return self.save(document)

    @staticmethod
    def _user_descriptor(document: Mapping[str, Any], server_id: str) -> dict[str, Any]:
        descriptor = document["servers"].get(server_id)
        if not isinstance(descriptor, dict):
            raise ConfigError(
                f"{server_id} is not in your MCP configuration; a trusted project "
                "file defines it, so edit it there"
            )
        return descriptor


# -- guided forms ------------------------------------------------------------


def _required(ask: Ask, prompt: str, *, secret: bool = False) -> str:
    answer = ask(prompt, secret=secret)
    if answer is None:
        raise EditCancelled()
    return answer.strip()


def ask_server_id(ask: Ask, taken: set[str]) -> str:
    server_id = _required(
        ask, "Server name (lowercase letters, digits and dashes, for example github):"
    ).lower()
    if not SERVER_ID.fullmatch(server_id):
        raise ConfigError("server names use lowercase letters, digits and dashes, starting with a letter")
    if server_id in taken:
        raise ConfigError(f"an MCP server named {server_id} already exists")
    return server_id


def ask_stdio(ask: Ask, current: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
    """A local command server; ``current`` keeps fields left blank."""

    current = current or {}
    keep = " (Enter keeps it)" if current else ""
    descriptor: dict[str, Any] = {
        key: copy.deepcopy(value)
        for key, value in current.items()
        if key not in {"command", "args", "env", "transport"}
    }
    shown = current.get("command")
    command = _required(
        ask,
        "Command that starts the server, for example npx or /usr/local/bin/server"
        + (f" (now {shown}){keep}" if shown else "")
        + ":",
    )
    descriptor["command"] = command or current.get("command", "")
    if not descriptor["command"]:
        raise ConfigError("the server needs a command")
    shown_args = " ".join(shlex.quote(item) for item in current.get("args", []))
    arguments = _required(
        ask,
        "Arguments, separated by spaces; quote an argument that contains spaces"
        + (f" (now {shown_args}){keep}" if shown_args else " (Enter for none)")
        + ":",
    )
    if arguments:
        try:
            descriptor["args"] = shlex.split(arguments)
        except ValueError as error:
            raise ConfigError(f"the arguments could not be read: {error}") from error
    elif current.get("args"):
        descriptor["args"] = list(current["args"])
    environment = _required(
        ask,
        "Environment variables for the server as NAME=value, separated by spaces. "
        "Hidden as you type; they are stored only in your private MCP configuration"
        + (" (Enter keeps the current ones)" if current.get("env") else " (Enter for none)")
        + ":",
        secret=True,
    )
    if environment:
        descriptor["env"] = parse_environment(environment)
    elif current.get("env"):
        descriptor["env"] = dict(current["env"])
    return descriptor


def ask_http(ask: Ask, current: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
    """A remote Streamable HTTP server; ``current`` keeps fields left blank."""

    current = current or {}
    keep = " (Enter keeps it)" if current else ""
    descriptor: dict[str, Any] = {
        key: copy.deepcopy(value)
        for key, value in current.items()
        if key not in {"url", "auth", "transport"}
    }
    descriptor["transport"] = "streamable-http"
    shown = current.get("url")
    url = _required(
        ask,
        "Server URL (https, or http only on a numeric loopback address)"
        + (f" (now {shown}){keep}" if shown else "")
        + ":",
    )
    descriptor["url"] = url or current.get("url", "")
    if not descriptor["url"]:
        raise ConfigError("the server needs a URL")
    auth = current.get("auth") if isinstance(current.get("auth"), Mapping) else None
    shown_auth = auth.get("environment") if auth else None
    variable = _required(
        ask,
        f"Environment variable that holds its bearer token, starting with "
        f"{STATIC_CREDENTIAL_ENVIRONMENT_PREFIX}"
        + (f" (now {shown_auth}; type none to remove){keep}" if shown_auth else " (Enter for no token)")
        + ":",
    )
    if variable.lower() == "none":
        descriptor.pop("auth", None)
    elif variable:
        if not is_static_credential_environment(variable):
            raise ConfigError(
                f"the token variable must be named {STATIC_CREDENTIAL_ENVIRONMENT_PREFIX}"
                "followed by capital letters, digits or underscores"
            )
        descriptor["auth"] = {"type": STATIC_CREDENTIAL_AUTH_TYPE, "environment": variable}
    elif auth:
        descriptor["auth"] = dict(auth)
    return descriptor


def ask_label(ask: Ask, fallback: str, current: Optional[str] = None) -> str:
    shown = f" (now {current}; Enter keeps it)" if current else f" (Enter uses {fallback})"
    label = _required(ask, "Display name" + shown + ":")
    return label or current or fallback


def parse_environment(text: str) -> dict[str, str]:
    try:
        pairs = shlex.split(text)
    except ValueError as error:
        raise ConfigError("the environment variables could not be read") from error
    environment: dict[str, str] = {}
    for pair in pairs:
        name, separator, value = pair.partition("=")
        if not separator or not _ENVIRONMENT_NAME.fullmatch(name):
            # Never echo the pair: its value may be a secret.
            raise ConfigError("environment variables must be written as NAME=value")
        environment[name] = value
    return environment


def describe(descriptor: Mapping[str, Any]) -> str:
    """A one-line, secret-free summary of a server descriptor."""

    if descriptor.get("transport") == "streamable-http":
        text = f"{descriptor.get('url', '')}"
        auth = descriptor.get("auth")
        if isinstance(auth, Mapping) and auth.get("environment"):
            text += f" · token from {auth['environment']}"
        return text
    parts = [str(descriptor.get("command", ""))]
    parts.extend(shlex.quote(str(item)) for item in descriptor.get("args", []))
    text = " ".join(parts)
    environment = descriptor.get("env")
    if isinstance(environment, Mapping) and environment:
        text += " · env " + ", ".join(sorted(environment))
    return text


__all__ = [
    "ConfigEditor",
    "EditCancelled",
    "ask_http",
    "ask_label",
    "ask_server_id",
    "ask_stdio",
    "describe",
    "parse_environment",
]
