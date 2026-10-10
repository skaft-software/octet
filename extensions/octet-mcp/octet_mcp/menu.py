"""The octet-mcp options menu under /extensions: a full server manager.

The menu is built from the bridge's current snapshot and the user file's
server names only; it never launches a server or reads a secret.
"""

from __future__ import annotations

from pathlib import Path
from typing import Any, Mapping, Optional

from .presentation import format_server_detail

# Servers listed in the menu; the host bounds a whole menu at 256 entries.
MAX_MENU_SERVERS = 24
# Each server's submenu detail stays short; Show details has the full text.
MAX_SERVER_DETAIL_BYTES = 2048
GATE_NOTE = (
    "Remote (Streamable HTTP) servers need octet started with "
    "--experimental-streamable-http-mcp."
)


def _action(item_id: str, label: str, description: str, *arguments: str,
            recommended: bool = False, destructive: bool = False) -> dict[str, Any]:
    item: dict[str, Any] = {
        "id": item_id,
        "label": label,
        "description": description,
        "command": "mcp",
        "arguments": list(arguments),
    }
    if recommended:
        item["recommended"] = True
    if destructive:
        item["destructive"] = True
    return item


def _enabled(server: Mapping[str, Any], action_id: str) -> bool:
    return any(
        isinstance(action, Mapping)
        and action.get("id") == action_id
        and action.get("enabled") is True
        for action in server.get("actions", [])
    )


def _bounded_detail(text: str) -> str:
    if len(text.encode("utf-8")) <= MAX_SERVER_DETAIL_BYTES:
        return text
    kept: list[str] = []
    size = 0
    for line in text.splitlines():
        size += len(line.encode("utf-8")) + 1
        if size > MAX_SERVER_DETAIL_BYTES - 64:
            break
        kept.append(line)
    kept.append("… Show details lists every tool.")
    return "\n".join(kept)


def build_menu(
    snapshot: Mapping[str, Any],
    *,
    user_servers: Mapping[str, Mapping[str, Any]],
    experimental_streamable_http_mcp: bool,
    config_path: Path,
    config_error: Optional[Mapping[str, Any]] = None,
) -> dict[str, Any]:
    summary = snapshot.get("summary", {})
    servers = list(snapshot.get("servers", []))
    configured = len(servers)
    connected = sum(bool(server.get("connected")) for server in servers)
    tools = sum(int(server.get("toolCount", 0)) for server in servers)
    if config_error is not None:
        return {
            "title": "MCP servers",
            "status": {"state": "degraded", "label": "Configuration needs attention"},
            "detail": (
                f"{config_error.get('summary', 'MCP configuration is invalid')}. "
                f"Fix {config_path}, then reload extensions; nothing is changed from "
                "this menu until it loads."
            ),
            "items": [_action("status", "Check status", "Show the bridge's state", "status")],
        }
    counts = f"{connected}/{configured} connected · {tools} tools"
    if not configured:
        status = {"state": "empty", "label": "No servers yet"}
    elif summary.get("degraded"):
        status = {"state": "degraded", "label": counts}
    elif summary.get("refreshing"):
        status = {"state": "loading", "label": counts}
    else:
        status = {"state": "active", "label": counts}

    items: list[dict[str, Any]] = []
    stdio = _action(
        "stdio" if experimental_streamable_http_mcp else "add",
        "Local command (stdio)" if experimental_streamable_http_mcp else "Add a server",
        "Run an MCP server command on this machine, for example npx …",
        "add", "stdio",
        recommended=not configured,
    )
    if experimental_streamable_http_mcp:
        items.append({
            "id": "add",
            "label": "Add a server",
            "description": "A local command or a remote URL",
            "items": [
                stdio,
                _action("http", "Remote URL (Streamable HTTP)",
                        "Connect to an MCP server over HTTPS, with an optional bearer token",
                        "add", "http"),
            ],
        })
    else:
        items.append(stdio)

    for server in servers[:MAX_MENU_SERVERS]:
        server_id = str(server.get("id", ""))
        if not server_id:
            continue
        state = str(server.get("state", "degraded"))
        user = server_id in user_servers
        descriptor = user_servers.get(server_id, {})
        entries = [_action("show", "Show details", "State, last error and published tools",
                           "show", server_id)]
        if _enabled(server, "refresh"):
            entries.append(_action("refresh", "Refresh tools",
                                   "Reload this server's tool list", "refresh", server_id))
        if _enabled(server, "restart"):
            starting = state == "stopped"
            entries.append(_action(
                "restart", "Start" if starting else "Restart",
                "Start the server now" if starting else "Stop and start the server again",
                "restart", server_id,
            ))
        if _enabled(server, "stop"):
            entries.append(_action("stop", "Stop", "Stop it until you start it again",
                                   "stop", server_id))
        if user:
            if descriptor.get("enabled", True) is False:
                entries.append(_action("enable", "Enable",
                                       "Start it now and whenever octet starts",
                                       "enable", server_id))
            else:
                entries.append(_action("disable", "Disable",
                                       "Stop it and keep it stopped when octet starts",
                                       "disable", server_id))
            editable = (
                descriptor.get("transport", "stdio") != "streamable-http"
                or experimental_streamable_http_mcp
            )
            if editable:
                entries.append(_action("edit", "Edit",
                                       "Change its command, arguments, environment or URL",
                                       "edit", server_id))
            entries.append(_action("remove", "Remove",
                                   "Delete it from your MCP configuration",
                                   "remove", server_id, destructive=True))
        description = f"{state} · {int(server.get('toolCount', 0))} tools"
        if not user:
            description += " · from a trusted project file"
        items.append({
            "id": "server:" + server_id,
            "label": str(server.get("label") or server_id),
            "description": description,
            "detail": _bounded_detail(format_server_detail(snapshot, server_id)),
            "items": entries,
        })

    if connected:
        items.append(_action("refresh-all", "Refresh all servers",
                             "Reload every connected server's tool list", "refresh"))
    items.append(_action("status", "Check status", "Every server's state and tool count",
                         "status"))
    detail = f"Servers are saved in {config_path}."
    if len(servers) > MAX_MENU_SERVERS:
        detail += f" {len(servers) - MAX_MENU_SERVERS} more are listed by Check status."
    if not experimental_streamable_http_mcp:
        detail += " " + GATE_NOTE
    return {"title": "MCP servers", "status": status, "detail": detail, "items": items}
