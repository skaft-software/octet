"""Executable octet extension wiring for the resident MCP bridge."""

from __future__ import annotations

import argparse
from collections.abc import Mapping
import os
from pathlib import Path
import threading
from typing import Any, Optional

from octet_extension import Extension

from .config import (
    STREAMABLE_HTTP_GATE_ERROR,
    STATIC_CREDENTIAL_AUTH_TYPE,
    BridgeConfig,
    ConfigError,
    load_config,
)
from .manager import BridgeManager
from .streamable_http import StaticEnvironmentCredentialProvider


SUPPORTED_FEATURES = (
    "request_cancellation",
    "content_parts",
    "request_progress",
    "artifacts",
    "policy_intents",
    "dynamic_tools",
    "approvals",
)
EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT = "--experimental-streamable-http-mcp"


class ProtocolReadyExtension(Extension):
    """Expose a fence after the initialize response has actually been flushed."""

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.protocol_ready = threading.Event()

    def _send_result(self, request_id: Any, result: Any) -> None:
        super()._send_result(request_id, result)
        if (
            isinstance(result, Mapping)
            and result.get("api_version") == "0.2"
            and isinstance(result.get("protocol"), Mapping)
        ):
            self.protocol_ready.set()


def static_credential_provider(
    config: BridgeConfig,
) -> Optional[StaticEnvironmentCredentialProvider]:
    """Compose the bundled static credential source only when it is configured.

    The bridge never inspects ambient environment variables by itself. A
    ``static-bearer`` server descriptor must explicitly name one
    ``OCTET_MCP_*`` variable, and the provider refuses any other name. Every
    other remote descriptor keeps the fail-closed unavailable default until the
    process owner explicitly composes a host credential adapter. OAuth/browser
    authorization stays policy-gated and unimplemented.
    """

    credentials = {
        server.id: server.auth.credential
        for server in config.servers
        if server.enabled
        and server.transport == "streamable-http"
        and server.auth is not None
        and server.auth.type == STATIC_CREDENTIAL_AUTH_TYPE
    }
    return StaticEnvironmentCredentialProvider(credentials) if credentials else None


def build_runtime(
    *,
    config_path: Optional[Path] = None,
    experimental_streamable_http_mcp: bool = False,
) -> tuple[ProtocolReadyExtension, BridgeManager]:
    workspace = os.environ.get("OCTET_WORKSPACE")
    config_error = None
    try:
        config = load_config(
            config_path,
            workspace=workspace,
            experimental_streamable_http_mcp=experimental_streamable_http_mcp,
        )
    except ConfigError as error:
        # Stay inspectable instead of crashing the extension handshake. Only the
        # fixed, process-owner gate diagnostic is safe to expose verbatim; all
        # parser-controlled errors remain bounded and generic.
        config = BridgeConfig.empty(Path(config_path) if config_path else None)
        if str(error) == STREAMABLE_HTTP_GATE_ERROR:
            config_error = {
                "code": "experimental_streamable_http_mcp_required",
                "summary": STREAMABLE_HTTP_GATE_ERROR,
            }
        else:
            config_error = {
                "code": "invalid_config",
                "summary": "MCP configuration failed a bounded trust or schema check",
            }

    extension = ProtocolReadyExtension(
        api_version="0.2",
        max_concurrent_requests=8,
        max_pending_requests=64,
        writer_queue_size=64,
        shutdown_timeout=2.0,
        cancellation_grace=0.25,
        supported_features=SUPPORTED_FEATURES,
    )
    manager = BridgeManager(
        extension,
        config,
        config_error=config_error,
        scratch_directory=Path(
            os.environ.get("OCTET_EXTENSION_SCRATCH", ".octet-mcp-scratch")
        ),
        credential_provider=static_credential_provider(config),
        experimental_streamable_http_mcp=experimental_streamable_http_mcp,
    )

    @extension.command(
        name="mcp",
        description="Inspect MCP server state or request a safe lifecycle action",
        usage=(
            "/mcp [status|list|snapshot|show <server>|refresh [server]|"
            "restart <server>|stop <server>]"
        ),
    )
    def mcp_command(arguments: list[str], context: Mapping[str, Any]) -> dict[str, Any]:
        return manager.execute_command(arguments, context)

    @extension.status("status")
    def mcp_status(params: Mapping[str, Any]) -> dict[str, Any]:
        contribution = manager.status_contribution()
        contribution["surface"] = params.get("surface", "status")
        return contribution

    @extension.on_shutdown
    def shutdown(params: Mapping[str, Any]) -> None:
        del params
        manager.shutdown()

    return extension, manager


def run(
    config_path: Optional[Path] = None,
    *,
    experimental_streamable_http_mcp: bool = False,
) -> None:
    extension, manager = build_runtime(
        config_path=config_path,
        experimental_streamable_http_mcp=experimental_streamable_http_mcp,
    )

    def start_after_handshake() -> None:
        if extension.protocol_ready.wait():
            manager.start()

    threading.Thread(
        target=start_after_handshake,
        name="octet-mcp-bootstrap",
        daemon=True,
    ).start()
    try:
        extension.run()
    finally:
        manager.shutdown()


def main(argv: Optional[list[str]] = None) -> int:
    parser = argparse.ArgumentParser(
        prog="octet-mcp",
        description="octet API 0.2 bridge for explicitly configured stdio and Streamable HTTP MCP servers",
        allow_abbrev=False,
    )
    parser.add_argument(
        "--config",
        type=Path,
        help="explicit user configuration path (normal octet launches use ~/.octet/mcp.json)",
    )
    parser.add_argument(
        EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT,
        action="store_true",
        help=(
            "EXPERIMENTAL: enable Streamable HTTP MCP only when the octet process "
            "owner explicitly supplies this one-shot switch"
        ),
    )
    parser.add_argument(
        "--check-config",
        action="store_true",
        help="validate configuration without launching a server or starting the octet protocol",
    )
    arguments = parser.parse_args(argv)
    if arguments.check_config:
        try:
            config = load_config(
                arguments.config,
                workspace=os.environ.get("OCTET_WORKSPACE"),
                experimental_streamable_http_mcp=arguments.experimental_streamable_http_mcp,
            )
        except ConfigError as error:
            parser.error(str(error))
        print(f"valid MCP configuration: {len(config.servers)} configured servers")
        return 0
    run(
        arguments.config,
        experimental_streamable_http_mcp=arguments.experimental_streamable_http_mcp,
    )
    return 0
