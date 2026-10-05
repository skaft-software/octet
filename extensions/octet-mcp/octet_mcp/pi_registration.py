"""Transient Pi registrations over the existing resident BridgeManager.

No config writes, transport, credential resolver or parallel MCP engine lives
here. The host-only context marker is never a command argument.
"""
from __future__ import annotations

from dataclasses import replace
import hashlib
import json
from pathlib import Path
import re
import threading
from typing import Any, Mapping

from .config import BridgeConfig, ConfigError, _parse_servers
from .ownership import ResourceOwner


class PiMcpRegistrations:
    def __init__(self, manager: Any) -> None:
        self.manager = manager
        self._base = manager.config
        self._owner = None
        self._bridge_owner = None
        self._records: list[dict[str, Any]] = []
        self._cwd = Path.cwd()
        self._lock = threading.RLock()

    def command(self, arguments: list[str], context: Mapping[str, Any]) -> dict[str, Any]:
        # Only the coding host constructs this marker. Ordinary /mcp arguments,
        # model tools and extension-controlled JSON cannot select their owner.
        owner = ResourceOwner.from_context({
            "resource_owner": context.get("mcp_registration_owner")
        })
        if owner is None:
            raise ValueError("Pi MCP registration requires a host-issued owner")
        with self._lock:
            if arguments == ["__pi_release"]:
                if self._owner != owner:
                    result = {"changes": {}, "errors": [], "shadowed": []}
                else:
                    result = self._apply([], self._base, self._cwd)
                    self._owner = None
                    self._bridge_owner = None
                    self._records = []
            elif len(arguments) == 2 and arguments[0] == "__pi_replace":
                active = ResourceOwner.from_context(context)
                if active is None or active.session_id != owner.session_id:
                    raise ValueError("Pi MCP registration owner is not active")
                if self._owner is not None and self._owner != owner:
                    raise ValueError("Pi MCP registrations belong to another owner")
                if self.manager.config_error is not None:
                    raise ValueError("MCP persistent configuration is invalid")
                if len(arguments[1].encode("utf-8")) > 262144:
                    raise ValueError("Pi MCP registration snapshot exceeds bounds")
                records = json.loads(arguments[1])
                self._validate_records(records)
                cwd = Path(context["workspace"]).resolve()
                result = self._apply(records, self._base, cwd, bridge_owner=active)
                self._owner = owner
                self._bridge_owner = active
                self._records = records
                self._cwd = cwd
            else:
                raise ValueError("Invalid host Pi MCP registration command")
        return {"text": json.dumps(result, separators=(",", ":")),
                "notifications": [], "context": []}

    def apply_config(self, config: BridgeConfig, *, credential_provider: Any = None) -> None:
        """Keep transient entries out of persisted user edits and preserve them live."""
        with self._lock:
            self._apply(self._records, config, self._cwd,
                        credential_provider=credential_provider,
                        bridge_owner=self._bridge_owner)
            self._base = config

    @staticmethod
    def _validate_records(records: Any) -> None:
        if not isinstance(records, list) or len(records) > 32:
            raise ValueError("Invalid Pi MCP registration snapshot")
        namespaces = set()
        for entry in records:
            if not isinstance(entry, dict) or set(entry) != {"name", "config", "extensionPath"}:
                raise ValueError("Invalid Pi MCP registration record")
            name = entry["name"]
            if not isinstance(name, str) or not re.fullmatch(r"[A-Za-z0-9_-]{1,128}", name):
                raise ValueError("Invalid Pi MCP server name")
            if not isinstance(entry["config"], dict) or not isinstance(entry["extensionPath"], str):
                raise ValueError("Invalid Pi MCP registration record")
            namespace = name.replace("-", "_")
            if namespace in namespaces:
                raise ValueError("Pi MCP registration namespace collision")
            namespaces.add(namespace)

    def _apply(self, records: list[dict[str, Any]], base: BridgeConfig, cwd: Path,
               *, credential_provider: Any = None,
               bridge_owner: Any = None) -> dict[str, Any]:
        configured = {server.id.replace("-", "_") for server in base.servers}
        server_ids = {server.id for server in base.servers}
        transient = []
        errors = []
        shadowed = []
        for entry in records:
            name, config = entry["name"], entry["config"]
            if name.replace("-", "_") in configured:
                shadowed.append(name)
                continue
            code = self._unsupported(config)
            if code is not None:
                errors.append({"name": name, "code": code})
                continue
            server_id = "pi-" + hashlib.sha256(name.encode("utf-8")).hexdigest()[:24]
            if server_id in server_ids:
                errors.append({"name": name, "code": "native_namespace_collision"})
                continue
            descriptor = {"transport": "stdio", "command": config["command"],
                          "args": config.get("args", []), "env": config.get("env", {}),
                          "enabled": config.get("enabled", True),
                          "requestTimeoutMs": 60000}
            if "cwd" in config:
                descriptor["cwd"] = config["cwd"]
            if "timeout" in config:
                seconds = config["timeout"]
                if type(seconds) not in (int, float) or not 0.01 <= seconds <= 120:
                    errors.append({"name": name, "code": "native_timeout_bounds"})
                    continue
                descriptor["requestTimeoutMs"] = int(seconds * 1000)
            # Presentation labels cannot contain credentials/config data.
            descriptor["label"] = name
            try:
                parsed = _parse_servers({server_id: descriptor}, limits=base.limits,
                                        scope="extension", config_dir=cwd, default_cwd=cwd)
            except (ConfigError, ValueError, TypeError):
                # Never leak parser messages containing env/argv/credentials.
                errors.append({"name": name, "code": "native_config_rejected"})
                continue
            if len(base.servers) + len(transient) >= base.limits.max_servers:
                errors.append({"name": name, "code": "native_server_limit"})
                continue
            server_ids.add(server_id)
            transient.extend(replace(server, registration_owner=bridge_owner)
                             for server in parsed)
        changes = self.manager.apply_config(
            replace(base, servers=base.servers + tuple(transient)),
            credential_provider=credential_provider)
        return {"changes": changes, "errors": errors, "shadowed": shadowed}

    @staticmethod
    def _unsupported(config: Mapping[str, Any]) -> Any:
        if (config.get("type") not in (None, "stdio") or "url" in config
                or not isinstance(config.get("command"), str)):
            return "unsupported_transport"
        # Native tool visibility is direct today. Never turn Pi's default
        # codemode/deferred/hidden tools into model-visible declarations.
        if config.get("exposure", "codemode") != "direct":
            return "unsupported_exposure"
        if config.get("toolExposure"):
            return "unsupported_tool_exposure"
        if config.get("description"):
            return "unsupported_server_description"
        if set(config) - {"type", "command", "args", "env", "cwd", "exposure",
                          "toolExposure", "description", "enabled", "timeout"}:
            return "unsupported_config_fields"
        env = config.get("env", {})
        if not isinstance(env, dict) or any(not isinstance(value, str) for value in env.values()):
            return "native_config_rejected"
        if any(value.startswith("!") or "${" in value for value in env.values()):
            return "unsupported_env_expansion"
        args = config.get("args", [])
        if not isinstance(args, list) or any(not isinstance(arg, str) for arg in args):
            return "native_config_rejected"
        paths = [config["command"], config.get("cwd", ""), *args]
        if any(isinstance(value, str) and value.startswith("~/") for value in paths):
            return "unsupported_home_expansion"
        return None
