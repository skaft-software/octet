"""Executable octet extension wiring for the resident MCP bridge."""

from __future__ import annotations

import argparse
from collections.abc import Mapping
import os
from pathlib import Path
import threading
from typing import Any, Callable, Optional

from octet_extension import Extension

from .auth_runtime import RuntimeAuthentication
from .auth_service import AuthService
from .auth_store import PrivateTokenStore, default_store_path
from .oauth_http import OAuthHttp
from .oauth_transport import PinnedOAuthExchange
from .config import (
    STREAMABLE_HTTP_GATE_ERROR,
    BridgeConfig,
    ConfigError,
    load_config,
)
from .manager import BridgeManager


SUPPORTED_FEATURES = (
    "request_cancellation",
    "content_parts",
    "request_progress",
    "artifacts",
    "policy_intents",
    "dynamic_tools",
    "lifecycle_events",
    "approvals",
)
EXPERIMENTAL_STREAMABLE_HTTP_MCP_ARGUMENT = "--experimental-streamable-http-mcp"


class ProtocolReadyExtension(Extension):
    """Expose a fence after the initialize response has actually been flushed."""

    def __init__(self, **kwargs: Any) -> None:
        super().__init__(**kwargs)
        self.protocol_ready = threading.Event()
        self.session_observer: Optional[Callable[[str, Mapping[str, Any]], None]] = None

    def _submit_notification(self, method: str, params: Any) -> None:
        # The legacy SDK queues observations behind tool handlers. Revocation
        # must instead precede subsequent host requests, including when every
        # handler/admission slot is occupied by cancelled remote calls. This
        # observer only updates bounded local fences and schedules socket cleanup.
        if (
            method in {"session/started", "session/settled"}
            and "lifecycle_events" in self.negotiated_features
            and isinstance(params, Mapping)
            and self.session_observer is not None
        ):
            self.session_observer(method, params)
            return
        super()._submit_notification(method, params)

    def _send_result(self, request_id: Any, result: Any) -> None:
        super()._send_result(request_id, result)
        if (
            isinstance(result, Mapping)
            and result.get("api_version") == "0.2"
            and isinstance(result.get("protocol"), Mapping)
        ):
            self.protocol_ready.set()


def build_runtime(
    *,
    config_path: Optional[Path] = None,
    experimental_streamable_http_mcp: bool = False,
    auth_store: Optional[PrivateTokenStore] = None,
    oauth_http: Optional[OAuthHttp] = None,
    client_factory: Optional[Callable[..., Any]] = None,
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
    authentication = RuntimeAuthentication(
        extension, config,
        AuthService(auth_store or PrivateTokenStore(default_store_path()),
                    oauth_http or OAuthHttp(PinnedOAuthExchange()),
                    experimental_streamable_http_mcp=experimental_streamable_http_mcp),
        enabled=experimental_streamable_http_mcp,
    )
    extension.authentication = authentication
    manager = BridgeManager(
        extension,
        config,
        config_error=config_error,
        credential_provider=authentication.provider,
        client_factory=client_factory,
        scratch_directory=Path(
            os.environ.get("OCTET_EXTENSION_SCRATCH", ".octet-mcp-scratch")
        ),
        experimental_streamable_http_mcp=experimental_streamable_http_mcp,
        # The stock SDK has trusted owner/parent-scoped input and confirmation
        # services. Unavailable frontends still decline at action time.
        private_ui=True,
    )

    authentication.manager = manager

    @extension.command(
        name="mcp",
        description="Inspect MCP server state or request a safe lifecycle action",
        usage=(
            "/mcp [status|list|snapshot|show <server>|refresh [server]|"
            "restart <server>|stop <server>|auth <action> <server>]"
        ),
    )
    def mcp_command(arguments: list[str], context: Mapping[str, Any]) -> dict[str, Any]:
        if arguments and arguments[0] == "auth":
            return authentication.execute_command(arguments, context)
        return authentication.manager_command(arguments, context)

    def activate_owner(context: Mapping[str, Any]) -> None:
        try:
            authentication.activate_owner(context)
        except ValueError:
            # Missing/stale ownership denies remote work, not unrelated stdio
            # use or the host's prompt. Never guess an owner from host.session_id.
            pass

    @extension.hook("before_prompt")
    def before_prompt(payload: Mapping[str, Any], context: Mapping[str, Any]) -> dict[str, Any]:
        del payload
        activate_owner(context)
        return {"disposition": {"action": "continue"}}

    @extension.status("status")
    def mcp_status(params: Mapping[str, Any]) -> dict[str, Any]:
        contribution = manager.status_contribution()
        contribution["surface"] = params.get("surface", "status")
        return contribution

    # Register exact API 0.2 subscriptions; ProtocolReadyExtension observes them
    # on the ordered dispatch boundary rather than a possibly saturated pool.
    extension.on_lifecycle("session/started")(lambda event: None)
    extension.on_lifecycle("session/settled")(lambda event: None)
    extension.session_observer = authentication.observe_session

    @extension.on_shutdown
    def shutdown(params: Mapping[str, Any]) -> None:
        del params
        authentication.shutdown()
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
        extension.authentication.shutdown()
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
