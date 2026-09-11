"""Resident MCP server/catalog owner and octet API 0.2 bridge."""

from __future__ import annotations

from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass, field
import os
from pathlib import Path
import random
import threading
import time
from typing import Any, Callable, Mapping, Optional, Protocol

from .catalog import (
    CatalogError,
    ToolBinding,
    ToolInputError,
    ToolResultError,
    lower_tool_result,
    normalize_catalog_tool,
    validate_arguments,
)
from .config import BridgeConfig, Limits, ServerConfig
from .http_2026 import McpHttp2026Client
from .interactions import make_interaction_handler
from .presentation import (
    PresentationProducer,
    compact_status,
    format_server_detail,
    format_status,
    host_presentation,
    snapshot_json,
)
from .protocol import (
    McpCancelled,
    McpError,
    McpProtocolError,
    McpRemoteError,
    McpStdioClient,
    McpTimeout,
    McpTransportError,
)
from .resources import (
    ResourceBinding,
    ResourceUnsupportedError,
    call_resource,
    resource_bindings,
    supports_resources,
)
from .streamable_http import CredentialProvider, McpStreamableHttpClient


class OwnerCredentialProvider(Protocol):
    """Application broker: resolve a token for the exact host-issued owner triple.

    This is not the transport's already-scoped CredentialProvider. No ambient or
    default owner fallback is permitted, including for background catalog reads.
    """

    def bearer_token(
        self, credential: str, *, server_id: str, resource_owner: Mapping[str, Any],
        deadline: Optional[float] = None, cancel: Callable[[], bool] = lambda: False,
    ) -> Optional[str]:
        """Return an ephemeral token, or None when this owner's secret is unavailable."""


@dataclass(frozen=True)
class ResourceOwner:
    session_id: str
    extension_instance_id: str
    process_generation: int

    @classmethod
    def from_context(cls, context: Mapping[str, Any]) -> "ResourceOwner":
        value = context.get("resource_owner")
        if not isinstance(value, Mapping) or set(value) != {
            "session_id", "extension_instance_id", "process_generation"
        }:
            raise ValueError("remote MCP requires a complete host-derived resource owner")
        for key in ("session_id", "extension_instance_id"):
            identifier = value[key]
            if (
                not isinstance(identifier, str)
                or not identifier
                or len(identifier.encode("utf-8")) > 256
                or any(ord(character) < 32 for character in identifier)
            ):
                raise ValueError("remote MCP resource owner is invalid")
        generation = value["process_generation"]
        if type(generation) is not int or not 1 <= generation <= 2**64 - 1:
            raise ValueError("remote MCP resource owner generation is invalid")
        return cls(value["session_id"], value["extension_instance_id"], generation)

    def as_dict(self) -> dict[str, Any]:
        return {
            "session_id": self.session_id,
            "extension_instance_id": self.extension_instance_id,
            "process_generation": self.process_generation,
        }


@dataclass
class _RemoteScope:
    owner: ResourceOwner
    revoked: threading.Event = field(default_factory=threading.Event)


@dataclass
class _RemoteConnection:
    scope: _RemoteScope
    revoked: threading.Event = field(default_factory=threading.Event)

    @property
    def active(self) -> bool:
        return not self.revoked.is_set() and not self.scope.revoked.is_set()


class _ScopedCredentials:
    """One connection's adapter; never consult a mutable/default current owner."""

    def __init__(
        self, provider: Optional[OwnerCredentialProvider], connection: _RemoteConnection
    ) -> None:
        self._provider = provider
        self._connection = connection

    def bearer_token(
        self, credential: str, *, server_id: str,
        deadline: Optional[float] = None, cancel: Callable[[], bool] = lambda: False,
    ) -> Optional[str]:
        cancelled = lambda: not self._connection.active or cancel()
        if cancelled() or self._provider is None:
            return None
        token = self._provider.bearer_token(
            credential,
            server_id=server_id,
            resource_owner=self._connection.scope.owner.as_dict(),
            deadline=deadline,
            cancel=cancelled,
        )
        # Settlement/cancellation may win while a broker refreshes a grant.
        return token if not cancelled() else None


class _RemoteCancellation:
    def __init__(self, connection: _RemoteConnection, parent: Any) -> None:
        self.connection = connection
        self.parent = parent

    @property
    def cancelled(self) -> bool:
        return not self.connection.active or bool(getattr(self.parent, "cancelled", False))

    @property
    def reason(self) -> str:
        return "remote_owner_or_request_settled"

    def raise_if_cancelled(self) -> None:
        if self.cancelled:
            raise McpCancelled("request_cancelled", "MCP owner or request settled")


@dataclass
class _ServerState:
    config: ServerConfig
    state: str
    client: Optional[Any] = None
    remote_connection: Optional[_RemoteConnection] = None
    catalog_revision: int = 0
    host_catalog_revision: int = 0
    tools: dict[str, ToolBinding] = field(default_factory=dict)
    catalog_client: Optional[Any] = None
    restart_attempt: int = 0
    next_retry_at_ms: Optional[int] = None
    last_error: Optional[dict[str, Any]] = None
    timer: Optional[threading.Timer] = None
    refresh_queued: bool = False
    operation_lock: threading.RLock = field(default_factory=threading.RLock)


class BridgeManager:
    """The only owner of MCP server sessions, catalog state, and retry policy."""

    def __init__(
        self,
        extension: Any,
        config: BridgeConfig,
        *,
        presentation: Optional[PresentationProducer] = None,
        scratch_directory: Optional[Path] = None,
        config_error: Optional[Mapping[str, Any]] = None,
        credential_provider: Optional[OwnerCredentialProvider] = None,
        client_factory: Optional[Callable[..., Any]] = None,
        random_source: Optional[random.Random] = None,
        experimental_streamable_http_mcp: bool = False,
        private_ui: bool = False,
    ) -> None:
        self.extension = extension
        self.config = config
        self.presentation = presentation or PresentationProducer()
        self.scratch_directory = scratch_directory or Path(
            os.environ.get("OCTET_EXTENSION_SCRATCH", ".octet-mcp-scratch")
        )
        self.config_error = dict(config_error) if config_error is not None else None
        self._credential_provider = credential_provider
        self._client_factory = client_factory or self._default_client_factory
        self._random = random_source or random.SystemRandom()
        self._experimental_streamable_http_mcp = experimental_streamable_http_mcp
        # Host composition only: neither MCP annotations nor model arguments can
        # claim a private input/confirmation surface. Stdio has no reverse-origin
        # correlation and remains deliberately ineligible for elicitation.
        self._private_ui = (
            private_ui is True and getattr(extension, "api_version", None) == "0.2"
            and callable(getattr(extension, "request_input", None))
            and callable(getattr(extension, "confirm", None))
        )
        self._servers: dict[str, _ServerState] = {
            server.id: _ServerState(
                config=server,
                state="configured" if server.enabled else "stopped",
            )
            for server in config.servers
        }
        # API 0.2 tool catalogs are process-global. This bridge therefore supports
        # one remote owner/session per generation, never implicit multi-owner
        # sharing. A new owner or a settled session requires a bridge reload.
        self._remote_session_id: Optional[str] = None
        self._remote_session_settled = False
        self._remote_scope: Optional[_RemoteScope] = None
        self._remote_activated = False
        self._host_catalog_revision = 0
        self._started = False
        self._shutting_down = False
        self._lock = threading.RLock()
        self._catalog_lock = threading.Lock()
        self._presentation_publish_lock = threading.Lock()
        self._calls = threading.BoundedSemaphore(config.limits.max_concurrent_calls)
        # Do not construct workers for inert or rejected configuration. In
        # particular, the denied remote transport must fail before any worker
        # could resolve DNS or ask a credential provider for a secret.
        self._executor: Optional[ThreadPoolExecutor] = None

    def observe_session(self, method: str, event: Mapping[str, Any]) -> None:
        """Record ordered host lifecycle observations without blocking protocol I/O.

        Lifecycle session_id is the host's display/session ID, NOT the durable
        resource_owner.session_id. Join them only using host execution context.
        No session/started notification can resurrect a settled remote scope.
        """
        session_id = event.get("session_id")
        if not isinstance(session_id, str) or not session_id or len(session_id.encode("utf-8")) > 256:
            return
        retired = []
        with self._lock:
            if self._shutting_down:
                return
            if self._remote_session_id is None:
                self._remote_session_id = session_id
            if session_id != self._remote_session_id or method != "session/settled":
                return
            if self._remote_session_settled:
                return
            self._remote_session_settled = True
            if self._remote_scope is not None:
                self._remote_scope.revoked.set()
            for state in self._servers.values():
                if state.config.transport != "streamable-http":
                    continue
                if state.timer is not None:
                    state.timer.cancel()
                    state.timer = None
                state.next_retry_at_ms = None
                state.state = "stopped"
                if state.remote_connection is not None:
                    state.remote_connection.revoked.set()
                if state.client is not None:
                    retired.append((state, state.client))
                    state.client = None
        # At most one retirement per configured remote server per bridge
        # generation. Do not queue socket aborts behind blocked catalog workers.
        for state, client in retired:
            threading.Thread(
                target=self._retire_remote, args=(state, client),
                name="octet-mcp-owner-settled", daemon=True,
            ).start()

    def _retire_remote(self, state: _ServerState, client: Any) -> None:
        client.close()
        with state.operation_lock:
            self._remove_server_tools(state)
        self._presentation_changed()

    def _require_remote_owner(
        self, context: Mapping[str, Any], *, bind: bool = False
    ) -> _RemoteScope:
        owner = ResourceOwner.from_context(context)
        host = context.get("host")
        with self._lock:
            if (
                self._shutting_down
                or "lifecycle_events" not in self.extension.negotiated_features
                or self._remote_session_id is None
                or self._remote_session_settled
                or not isinstance(host, Mapping)
                or host.get("session_id") != self._remote_session_id
            ):
                raise ValueError("remote MCP requires an active host lifecycle session; reload after settlement")
            if self._remote_scope is None and bind:
                self._remote_scope = _RemoteScope(owner)
            scope = self._remote_scope
            if scope is None:
                raise ValueError("remote MCP requires /mcp restart <server> to bind its owner")
            if scope.owner != owner or scope.revoked.is_set():
                raise ValueError("remote MCP is bound to another owner or generation; reload the bridge")
            return scope

    def is_current_owner(self, context: Optional[Mapping[str, Any]]) -> bool:
        """Observe an already-bound active owner; never activate or allocate state."""
        if not isinstance(context, Mapping):
            return False
        try:
            self._require_remote_owner(context)
        except ValueError:
            return False
        return True

    def activate_owner(self, context: Mapping[str, Any]) -> bool:
        """Idempotently start enabled remotes after a real host owner boundary.

        This is explicit runtime activation, not a side effect of snapshot/status
        observation. It cannot rebind an owner or revive a settled session. The
        injected broker must resolve from its owner-bound store, not a reverse
        host request borrowed by a background worker.
        """
        with self._lock:
            states = [
                state for state in self._servers.values()
                if state.config.enabled and state.config.transport == "streamable-http"
                and self._streamable_http_allowed(state.config)
            ]
            if not states or self._shutting_down:
                return False
            scope = self._require_remote_owner(context, bind=True)
            if self._remote_activated:
                return True
            self._remote_activated = True
            if not self._started:
                return True
        for state in states:
            self._submit(self._start_server, state.config.id, False, scope)
        return True

    def _check_remote_scope(
        self, state: _ServerState, scope: Optional[_RemoteScope]
    ) -> None:
        if state.config.transport != "streamable-http":
            return
        with self._lock:
            if not self._streamable_http_allowed(state.config):
                raise ValueError("Streamable HTTP MCP requires the process-owner experimental CLI opt-in")
            if scope is None or scope is not self._remote_scope or scope.revoked.is_set():
                raise ValueError("remote MCP owner is absent or settled")

    def _streamable_http_allowed(self, server: ServerConfig) -> bool:
        return (
            server.transport != "streamable-http"
            or self._experimental_streamable_http_mcp
        )

    def _executor_for_work(self) -> ThreadPoolExecutor:
        with self._lock:
            if self._shutting_down:
                raise RuntimeError("MCP manager is shutting down")
            if self._executor is None:
                self._executor = ThreadPoolExecutor(
                    max_workers=max(2, min(8, max(2, len(self._servers) + 1))),
                    thread_name_prefix="octet-mcp-manager",
                )
            return self._executor

    def _submit(
        self, callback: Callable[..., Any], *arguments: Any, **kwargs: Any
    ) -> Future[Any]:
        return self._executor_for_work().submit(callback, *arguments, **kwargs)

    def _remote_transport_error(self, state: _ServerState) -> None:
        state.state = "parked"
        self._set_error(
            state,
            "experimental_streamable_http_mcp_required",
            "Streamable HTTP MCP requires the process-owner experimental CLI opt-in",
        )

    def _default_client_factory(
        self,
        config: ServerConfig,
        limits: Limits,
        on_failure: Callable[[Any, McpError], None],
        on_tools_changed: Callable[[Any], None],
        *,
        credential_provider: Optional[CredentialProvider] = None,
    ) -> Any:
        if config.transport == "stdio":
            return McpStdioClient(
                config,
                limits,
                on_failure=on_failure,
                on_tools_changed=on_tools_changed,
            )
        if config.transport == "streamable-http":
            client_type = (
                McpHttp2026Client if config.protocol_version == "2026-07-28"
                else McpStreamableHttpClient
            )
            return client_type(
                config,
                limits,
                credential_provider=credential_provider,
                enable_elicitation=self._private_ui,
                on_failure=on_failure,
                on_tools_changed=on_tools_changed,
            )
        raise ValueError("unsupported MCP server transport")

    def start(self) -> None:
        """Start explicitly configured servers in bounded parallel workers."""

        with self._lock:
            if self._started or self._shutting_down:
                return
            self._started = True
            states = []
            for state in self._servers.values():
                if not state.config.enabled:
                    continue
                if not self._streamable_http_allowed(state.config):
                    self._remote_transport_error(state)
                    continue
                if state.config.transport == "streamable-http" and not self._remote_activated:
                    self._set_error(
                        state, "owner_unavailable",
                        "Remote MCP is waiting for an active host-owner prompt or command",
                    )
                    continue
                states.append(state)
        for state in states:
            scope = self._remote_scope if state.config.transport == "streamable-http" else None
            self._submit(self._start_server, state.config.id, False, scope)
        self._presentation_changed()

    def shutdown(self) -> None:
        """Stop admission, timers, and every owned server root within bounds."""

        with self._lock:
            if self._shutting_down:
                return
            self._shutting_down = True
            if self._remote_scope is not None:
                self._remote_scope.revoked.set()
            states = list(self._servers.values())
            executor = self._executor
            self._executor = None
            for state in states:
                if state.timer is not None:
                    state.timer.cancel()
                    state.timer = None
                state.next_retry_at_ms = None
        clients: list[Any] = []
        for state in states:
            # Detach/abort before waiting on operation locks: a catalog or
            # initialize request may itself be waiting for socket cancellation.
            with self._lock:
                if state.client is not None:
                    clients.append(state.client)
                state.client = None
                if state.remote_connection is not None:
                    state.remote_connection.revoked.set()
                state.state = "stopped"
        if clients:
            # The configured server cap is itself bounded; close all roots in
            # parallel so graceful shutdown remains one per-server deadline,
            # not N serial deadlines.
            workers = len(clients)
            with ThreadPoolExecutor(max_workers=workers, thread_name_prefix="octet-mcp-stop") as pool:
                list(pool.map(lambda client: client.close(), clients))
        self._presentation_changed()
        if executor is not None:
            executor.shutdown(wait=False, cancel_futures=True)

    def request_action(
        self, action: str, server_id: Optional[str] = None,
        *, context: Optional[Mapping[str, Any]] = None,
    ) -> Future[Any]:
        """Route one declared safe user action; model tool text never selects it."""

        if action not in {"refresh", "restart", "stop"}:
            raise ValueError("unknown MCP action")
        if server_id is None and action != "refresh":
            raise ValueError(f"{action} requires a server id")
        if server_id is not None and server_id not in self._servers:
            raise ValueError("unknown MCP server")
        if server_id is not None and not self._streamable_http_allowed(
            self._servers[server_id].config
        ):
            raise ValueError(
                "Streamable HTTP MCP requires the process-owner experimental CLI opt-in"
            )
        scope = None
        if server_id is not None and self._servers[server_id].config.transport == "streamable-http":
            scope = self._require_remote_owner(context or {}, bind=action == "restart")
        if action == "refresh" and server_id is None:
            with self._lock:
                all_transports_denied = all(
                    not self._streamable_http_allowed(state.config)
                    for state in self._servers.values()
                )
            if all_transports_denied:
                completed: Future[Any] = Future()
                completed.set_result(True)
                return completed
            with self._lock:
                remote_connected = any(
                    state.config.transport == "streamable-http" and state.client is not None
                    for state in self._servers.values()
                )
            if remote_connected:
                scope = self._require_remote_owner(context or {})
            return self._submit(self._refresh_all, scope)
        callback = {
            "refresh": self.refresh_server,
            "restart": self.restart_server,
            "stop": self.stop_server,
        }[action]
        assert server_id is not None
        return self._submit(callback, server_id, scope=scope)

    def refresh_server(
        self, server_id: str, *, scope: Optional[_RemoteScope] = None
    ) -> bool:
        state = self._server(server_id)
        with state.operation_lock:
            self._check_remote_scope(state, scope)
            with self._lock:
                if self._shutting_down:
                    return False
                client = state.client
                if client is None or not client.alive:
                    self._set_error(state, "not_connected", "MCP server is not connected")
                    return False
                state.state = "refreshing"
                state.refresh_queued = False
            self._presentation_changed()
            try:
                raw_tools = self._list_catalog_tools(client)
                self._publish_catalog(state, client, raw_tools)
            except McpError as error:
                self._disconnect_after_failure(state, client, error)
                return False
            except (CatalogError, RuntimeError, ValueError) as error:
                del error
                with self._lock:
                    if state.client is client:
                        state.state = "degraded"
                        self._set_error(
                            state,
                            "catalog_publish_failed",
                            "MCP catalog could not be published safely",
                        )
                return False
            with self._lock:
                if state.client is client:
                    state.state = "ready"
                    state.last_error = None
            self._presentation_changed()
            return True

    def restart_server(
        self, server_id: str, *, scope: Optional[_RemoteScope] = None
    ) -> bool:
        state = self._server(server_id)
        with state.operation_lock:
            self._check_remote_scope(state, scope)
            with self._lock:
                if self._shutting_down:
                    return False
                if state.timer is not None:
                    state.timer.cancel()
                    state.timer = None
                state.next_retry_at_ms = None
                state.restart_attempt = 0
                client = state.client
                state.client = None
                if state.remote_connection is not None:
                    state.remote_connection.revoked.set()
            self._remove_server_tools(state)
            if client is not None:
                client.close()
            return self._start_server(server_id, True, scope)

    def stop_server(
        self, server_id: str, *, scope: Optional[_RemoteScope] = None
    ) -> bool:
        state = self._server(server_id)
        with state.operation_lock:
            self._check_remote_scope(state, scope)
            with self._lock:
                if state.timer is not None:
                    state.timer.cancel()
                    state.timer = None
                state.next_retry_at_ms = None
                client = state.client
                state.client = None
                if state.remote_connection is not None:
                    state.remote_connection.revoked.set()
                state.state = "stopped"
            self._remove_server_tools(state)
            if client is not None:
                client.close()
            self._presentation_changed()
            return True

    def _domain_snapshot(self, scope: Optional[_RemoteScope] = None) -> dict[str, Any]:
        """Build package-internal records used to derive one generic snapshot."""

        with self._lock:
            records = [self._server_record(state) for state in self._servers.values()]
            revision = self._host_catalog_revision
            config_error = dict(self.config_error) if self.config_error else None
            remote_ids = {
                state.config.id for state in self._servers.values()
                if state.config.transport == "streamable-http"
            }
            include_remote = scope is not None and scope is self._remote_scope and not scope.revoked.is_set()
        if not include_remote:
            for record in records:
                if record["id"] in remote_ids:
                    record["tools"] = []
        snapshot = self.presentation.snapshot(
            records,
            host_catalog_revision=revision,
            config_error=config_error,
        )
        if not include_remote:
            snapshot["activities"] = [
                activity for activity in snapshot["activities"]
                if activity["serverId"] not in remote_ids
            ]
        return snapshot

    def snapshot(self, context: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        """Return the exact generic API 0.2 presentation snapshot."""

        return host_presentation(self._domain_snapshot(self._observation_scope(context)))

    def _observation_scope(self, context: Optional[Mapping[str, Any]]) -> Optional[_RemoteScope]:
        if context is not None:
            try:
                return self._require_remote_owner(context)
            except ValueError:
                pass
        return None

    def status_contribution(self) -> dict[str, Any]:
        return compact_status(self._domain_snapshot())

    def _presentation_changed(self) -> None:
        with self._presentation_publish_lock:
            self.presentation.touch()
            self._publish_current_presentation()

    def _publish_current_presentation(self) -> None:
        publish = getattr(self.extension, "publish_presentation", None)
        if not callable(publish):
            return
        try:
            with self._lock:
                scope = self._remote_scope
            if scope is not None and not scope.revoked.is_set():
                publish(
                    host_presentation(self._domain_snapshot(scope)),
                    resource_owner=scope.owner.as_dict(),
                )
            else:
                publish(self.snapshot())
        except Exception:
            # An older host may not expose the generic presentation primitive.
            # The status and /mcp fallbacks remain available and no MCP domain
            # state is duplicated in a frontend.
            return

    def execute_command(
        self, arguments: list[str], context: Optional[Mapping[str, Any]] = None
    ) -> dict[str, Any]:
        """Implement `/mcp` narrow/headless fallback and safe actions."""

        if not arguments or arguments == ["status"] or arguments == ["list"]:
            text = format_status(self._domain_snapshot(self._observation_scope(context)))
        elif arguments == ["snapshot"]:
            text = snapshot_json(self.snapshot(context))
        elif len(arguments) == 2 and arguments[0] == "show":
            text = format_server_detail(
                self._domain_snapshot(self._observation_scope(context)), arguments[1]
            )
        elif arguments and arguments[0] in {"refresh", "restart", "stop"}:
            action = arguments[0]
            target: Optional[str]
            if action == "refresh" and len(arguments) == 1:
                target = None
            elif len(arguments) == 2:
                target = arguments[1]
            else:
                return {"text": self.command_usage()}
            try:
                self.request_action(action, target, context=context)
            except ValueError as error:
                return {"text": f"MCP action rejected: {error}"}
            scope = target or "all connected servers"
            text = f"MCP {action} requested for {scope}. Use /mcp status to inspect progress."
        else:
            text = self.command_usage()
        return {"text": text, "notifications": [], "context": []}

    @staticmethod
    def command_usage() -> str:
        return (
            "Usage: /mcp [status|list|snapshot|show <server>|refresh [server]|"
            "restart <server>|stop <server>]"
        )

    def _start_server(
        self, server_id: str, manual: bool, scope: Optional[_RemoteScope] = None
    ) -> bool:
        state = self._server(server_id)
        with state.operation_lock:
            if not self._streamable_http_allowed(state.config):
                with self._lock:
                    if self._shutting_down:
                        return False
                    self._remote_transport_error(state)
                self._presentation_changed()
                return False
            with self._lock:
                if self._shutting_down:
                    return False
                if state.client is not None and state.client.alive:
                    return True
                if (not state.config.enabled or state.state == "stopped") and not manual:
                    state.state = "stopped"
                    return False
                try:
                    self._check_remote_scope(state, scope)
                except ValueError:
                    return False
                state.state = "connecting"
                state.refresh_queued = False
                state.next_retry_at_ms = None
                state.last_error = None
            self._presentation_changed()

            connection = _RemoteConnection(scope) if scope is not None else None
            kwargs = {}
            if connection is not None:
                kwargs["credential_provider"] = _ScopedCredentials(self._credential_provider, connection)
            client = self._client_factory(
                state.config,
                self.config.limits,
                lambda failed, error: self._on_client_failure(server_id, failed, error),
                lambda changed: self._on_tools_changed(server_id, changed),
                **kwargs,
            )
            with self._lock:
                stale = self._shutting_down or (connection is not None and not connection.active)
                if not stale:
                    state.client = client
                    state.remote_connection = connection
            if stale:
                client.close()
                return False
            try:
                client.start()
                if connection is not None:
                    _RemoteCancellation(connection, None).raise_if_cancelled()
                raw_tools = self._list_catalog_tools(client)
                self._publish_catalog(state, client, raw_tools)
            except McpError as error:
                self._start_failed(state, client, error)
                return False
            except (CatalogError, RuntimeError, ValueError):
                self._start_failed(
                    state,
                    client,
                    McpProtocolError(
                        "invalid_catalog",
                        "MCP catalog could not be represented safely",
                        permanent=True,
                    ),
                )
                return False
            with self._lock:
                stale = state.client is not client or self._shutting_down
                if not stale:
                    state.state = "ready"
                    state.last_error = None
                    state.next_retry_at_ms = None
            if stale:
                client.close()
                return False
            self._presentation_changed()
            return True

    def _start_failed(
        self, state: _ServerState, client: Any, error: McpError
    ) -> None:
        with self._lock:
            if state.client is not client:
                return
            state.client = None
            if state.remote_connection is not None:
                state.remote_connection.revoked.set()
        client.close()
        self._remove_server_tools(state)
        self._schedule_after_failure(state, error)

    def _disconnect_after_failure(
        self, state: _ServerState, client: Any, error: McpError
    ) -> None:
        with self._lock:
            if state.client is not client:
                return
            state.client = None
            if state.remote_connection is not None:
                state.remote_connection.revoked.set()
        self._remove_server_tools(state)
        client.close()
        self._schedule_after_failure(state, error)

    def _schedule_after_failure(self, state: _ServerState, error: McpError) -> None:
        with self._lock:
            scope = state.remote_connection.scope if state.remote_connection is not None else None
            if self._shutting_down or state.state == "stopped" or (scope is not None and scope.revoked.is_set()):
                return
            self._set_error(state, error.code, error.safe_summary)
            if error.permanent or state.restart_attempt >= state.config.max_restarts:
                state.state = "parked"
                state.next_retry_at_ms = None
            else:
                state.restart_attempt += 1
                ceiling = min(
                    self.config.limits.backoff_max_ms,
                    self.config.limits.backoff_initial_ms * (2 ** (state.restart_attempt - 1)),
                )
                delay_ms = int(self._random.uniform(0, ceiling))
                retry_after_ms = getattr(error, "retry_after_ms", None)
                if isinstance(retry_after_ms, int) and not isinstance(retry_after_ms, bool):
                    delay_ms = max(
                        delay_ms,
                        min(self.config.limits.backoff_max_ms, retry_after_ms),
                    )
                delay_ms = max(1, delay_ms)
                state.state = "backoff"
                state.next_retry_at_ms = int(time.time() * 1000) + delay_ms
                timer = threading.Timer(
                    delay_ms / 1000,
                    lambda: self._submit_restart_after_backoff(state.config.id, scope),
                )
                timer.daemon = True
                state.timer = timer
                timer.start()
        self._presentation_changed()

    def _submit_restart_after_backoff(
        self, server_id: str, scope: Optional[_RemoteScope] = None
    ) -> None:
        with self._lock:
            if self._shutting_down:
                return
            state = self._servers.get(server_id)
            if (
                state is None
                or state.state != "backoff"
                or not self._streamable_http_allowed(state.config)
                or (scope is not None and scope.revoked.is_set())
            ):
                return
            state.timer = None
        try:
            self._submit(self._start_server, server_id, False, scope)
        except RuntimeError:
            return

    def _on_client_failure(
        self, server_id: str, client: Any, error: McpError
    ) -> None:
        try:
            self._submit(self._handle_client_failure, server_id, client, error)
        except RuntimeError:
            return

    def _handle_client_failure(
        self, server_id: str, client: Any, error: McpError
    ) -> None:
        state = self._server(server_id)
        with state.operation_lock:
            self._disconnect_after_failure(state, client, error)

    def _on_tools_changed(self, server_id: str, client: Any) -> None:
        with self._lock:
            state = self._servers.get(server_id)
            if (
                state is None
                or state.client is not client
                or state.refresh_queued
                or self._shutting_down
            ):
                return
            connection = state.remote_connection
            if connection is not None and not connection.active:
                return
            state.refresh_queued = True
        try:
            self._submit(self._refresh_changed_client, state, client, connection)
        except RuntimeError:
            with self._lock:
                state.refresh_queued = False

    def _refresh_changed_client(
        self, state: _ServerState, client: Any, connection: Optional[_RemoteConnection]
    ) -> None:
        # A queued callback from a replaced catalog must not refresh its successor.
        with state.operation_lock:
            with self._lock:
                if state.client is not client or (connection is not None and not connection.active):
                    return
            self.refresh_server(state.config.id, scope=connection.scope if connection else None)

    @staticmethod
    def _list_catalog_tools(client: Any) -> list[dict[str, Any]]:
        # Resources-only MCP servers need not implement tools/list. Do not probe
        # an unadvertised method just to publish the fixed resource operations.
        capabilities = getattr(client, "server_capabilities", None)
        if isinstance(capabilities, Mapping) and "tools" not in capabilities:
            return []
        return client.list_tools()

    def _publish_catalog(
        self,
        state: _ServerState,
        client: Any,
        raw_tools: list[dict[str, Any]],
    ) -> None:
        next_revision = state.catalog_revision + 1
        desired: dict[str, ToolBinding] = {}
        for raw in raw_tools:
            binding = normalize_catalog_tool(
                state.config.id,
                state.config.label,
                raw,
                server_catalog_revision=next_revision,
            )
            if binding.published_name in desired:
                raise CatalogError("normalized MCP tool names collided")
            desired[binding.published_name] = binding
        if supports_resources(client):
            for binding in resource_bindings(
                state.config.id, state.config.label, server_catalog_revision=next_revision
            ):
                if binding.published_name in desired:
                    raise CatalogError("MCP resource and tool names collided")
                desired[binding.published_name] = binding
        if len(desired) > self.config.limits.max_tools_per_server:
            raise CatalogError("MCP catalog exceeds the per-server tool limit")

        with self._catalog_lock:
            with self._lock:
                other_tools = sum(
                    len(other.tools) for other in self._servers.values() if other is not state
                )
                if other_tools + len(desired) > self.config.limits.max_total_tools:
                    raise CatalogError("MCP catalogs exceed the global tool limit")
                if state.client is not client or self._shutting_down:
                    raise McpTransportError("stale_connection", "MCP connection was replaced")
                previous = dict(state.tools)
            unchanged = (
                state.catalog_client is client
                and set(previous) == set(desired)
                and all(
                    previous[name].fingerprint == desired[name].fingerprint
                    for name in desired
                )
            )
            if unchanged:
                return

            accepted_names: Optional[set[str]] = None
            if desired:
                definitions = [
                    {
                        "name": binding.published_name,
                        "description": binding.description,
                        "parameters": binding.input_schema,
                        "output_schema": binding.output_schema,
                        "handler": self._handler(binding, client, state.remote_connection),
                    }
                    for binding in desired.values()
                ]
                response = self.extension.register_tools(definitions)
                accepted_names = self._accept_catalog_response(response)
                with self._lock:
                    # The registration response is the complete authoritative
                    # host catalog. Preserve prior bindings still accepted and
                    # add accepted definitions from this server.
                    state.tools.update(
                        {
                            name: binding
                            for name, binding in desired.items()
                            if name in accepted_names
                        }
                    )
                    self._apply_authoritative_names(accepted_names)
                    state.host_catalog_revision = self._host_catalog_revision

            removed = sorted(set(previous) - set(desired))
            if removed:
                response = self.extension.unregister_tools(*removed)
                accepted_names = self._accept_catalog_response(response)
                with self._lock:
                    self._apply_authoritative_names(accepted_names)
                    state.host_catalog_revision = self._host_catalog_revision

            with self._lock:
                final_accepted = accepted_names if accepted_names is not None else set()
                state.tools = {
                    name: binding
                    for name, binding in desired.items()
                    if name in final_accepted
                }
                state.catalog_revision = next_revision
                state.host_catalog_revision = self._host_catalog_revision
                state.catalog_client = client
        self._presentation_changed()

    def _remove_server_tools(self, state: _ServerState) -> None:
        with self._catalog_lock:
            with self._lock:
                names = sorted(state.tools)
            if not names:
                return
            try:
                response = self.extension.unregister_tools(*names)
                accepted = self._accept_catalog_response(response)
            except Exception:
                with self._lock:
                    state.state = "degraded"
                    self._set_error(
                        state,
                        "catalog_unpublish_failed",
                        "MCP tools could not be unpublished cleanly",
                    )
                self._presentation_changed()
                return
            with self._lock:
                self._apply_authoritative_names(accepted)
                state.tools.clear()
                state.catalog_client = None
                state.catalog_revision += 1
                state.host_catalog_revision = self._host_catalog_revision
        self._presentation_changed()

    def _accept_catalog_response(self, response: Any) -> set[str]:
        if (
            not isinstance(response, Mapping)
            or isinstance(response.get("revision"), bool)
            or not isinstance(response.get("revision"), int)
            or not isinstance(response.get("tools"), list)
            or not all(isinstance(name, str) for name in response["tools"])
        ):
            raise RuntimeError("host returned an invalid dynamic catalog acknowledgement")
        revision = response["revision"]
        with self._lock:
            if revision <= self._host_catalog_revision:
                raise RuntimeError("host catalog revision did not increase")
            self._host_catalog_revision = revision
        return set(response["tools"])

    def _apply_authoritative_names(self, accepted: set[str]) -> None:
        for server in self._servers.values():
            server.tools = {
                name: binding for name, binding in server.tools.items() if name in accepted
            }
            server.host_catalog_revision = self._host_catalog_revision

    def _handler(
        self, binding: ToolBinding, client: Any, connection: Optional[_RemoteConnection] = None
    ) -> Callable[[Mapping[str, Any], Mapping[str, Any]], dict[str, Any]]:
        def call(arguments: Mapping[str, Any], context: Mapping[str, Any]) -> dict[str, Any]:
            return self._call_tool(binding, client, arguments, context, connection)

        return call

    def _call_tool(
        self,
        binding: ToolBinding,
        client: Any,
        arguments: Mapping[str, Any],
        context: Mapping[str, Any],
        connection: Optional[_RemoteConnection] = None,
    ) -> dict[str, Any]:
        if connection is not None:
            try:
                scope = self._require_remote_owner(context)
                if scope is not connection.scope or not connection.active:
                    raise ValueError("remote MCP connection was retired")
            except ValueError:
                return self._error_result(binding, "MCP call denied: remote owner is absent, foreign, or settled.")
        with self._lock:
            if self._shutting_down or self._server(binding.server_id).client is not client:
                return self._error_result(binding, "MCP call denied: its catalog connection was retired.")
        activity = self.presentation.start_activity(
            binding.server_id, binding.published_name
        )
        self._publish_current_presentation()
        parent_cancellation = getattr(self.extension, "cancellation", None)
        cancellation = (
            _RemoteCancellation(connection, parent_cancellation)
            if connection is not None else parent_cancellation
        )
        acquired = False
        call_active = threading.Event()
        call_active.set()
        try:
            if cancellation is not None:
                cancellation.raise_if_cancelled()
            arguments = validate_arguments(arguments, binding.input_schema)
            acquired = self._acquire_call_slot(cancellation, binding)
            if not acquired:
                self._finish_activity(activity, "timedOut")
                return self._error_result(binding, "MCP call admission timed out.")
            if cancellation is not None:
                cancellation.raise_if_cancelled()
            # Admission precedes approval: an approval is never held through a
            # long call-slot wait. Unsafe historical bindings cannot obtain a new
            # approval against a changed catalog, even on the same live client.
            with self._lock:
                approval_epoch = self._host_catalog_revision
                self._check_dispatch(binding, client, approval_epoch)
            approval_error = self._approve_call(binding, arguments)
            if approval_error is not None:
                self._finish_activity(activity, "failed")
                return self._error_result(binding, approval_error)
            request_id = getattr(self.extension, "request_id", None)
            self._safe_progress("Calling configured MCP operation", request_id=request_id)
            progress = lambda event: self._forward_progress(event, request_id)
            timeout_ms = self._server(binding.server_id).config.request_timeout_ms
            with self._lock:
                owner_context = ({
                    "resource_owner": connection.scope.owner.as_dict(),
                    "host": {"session_id": self._remote_session_id},
                } if connection is not None else None)

            def dispatch_guard() -> None:
                # The transport separately checks request cancellation before
                # bytes. Its one bounded cancellation notification must still be
                # allowed for this owner/epoch after the parent call has settled.
                with self._lock:
                    if connection is not None and (
                        not connection.active or not self.is_current_owner(owner_context)
                    ):
                        raise McpCancelled("stale_operation", "MCP owner or operation was retired")
                    self._check_dispatch(binding, client, approval_epoch)

            interaction_handler = None
            eligible = not isinstance(binding, ResourceBinding) or binding.operation == "resources/read"
            if (self._private_ui and eligible and connection is not None
                    and isinstance(client, McpStreamableHttpClient)):
                owner = connection.scope.owner
                def is_active(candidate_owner: Mapping[str, Any], parent_id: int) -> bool:
                    # Never inspect this callback thread's ambient extension
                    # request_id/cancellation or a mutable last-call identity.
                    if (not call_active.is_set() or cancellation.cancelled
                            or parent_id != request_id or candidate_owner != owner.as_dict()):
                        return False
                    dispatch_guard()
                    return True

                interaction_handler = make_interaction_handler(
                    self.extension, owner=owner.as_dict(), parent_request_id=request_id,
                    cancellation=cancellation, deadline=time.monotonic() + timeout_ms / 1000,
                    is_active=is_active, server_label=binding.server_label, private_ui=True,
                    output_schema=binding.output_schema,
                )
            kwargs = {"interaction_handler": interaction_handler} if interaction_handler is not None else {}
            if isinstance(client, (McpStdioClient, McpStreamableHttpClient)):
                # The same closure is checked after transport preparation, before
                # any first MCP bytes, and on each resource/MRTR continuation.
                kwargs["dispatch_guard"] = dispatch_guard
            if cancellation is not None:
                cancellation.raise_if_cancelled()
            # Approval issuance/redemption and private confirmation can race a
            # catalog refresh. Recheck binding + global host epoch immediately at
            # dispatch, without holding a manager lock over network or user input.
            dispatch_guard()
            if isinstance(binding, ResourceBinding):
                lowered = call_resource(
                    binding, client, arguments, limits=self.config.limits,
                    timeout_ms=timeout_ms, cancellation=cancellation, progress=progress, **kwargs,
                )
            else:
                if isinstance(client, McpHttp2026Client):
                    kwargs["input_schema"] = binding.input_schema
                result = client.call_tool(
                    binding.upstream_name, arguments, cancellation=cancellation,
                    progress=progress, **kwargs,
                )
                if cancellation is not None:
                    cancellation.raise_if_cancelled()
                lowered = lower_tool_result(
                    self.extension, binding, result, scratch_directory=self.scratch_directory,
                )
            # Never return results after the owner or captured connection settled,
            # even if an upstream response wins its socket-close race.
            if cancellation is not None:
                cancellation.raise_if_cancelled()
            with self._lock:
                if self._shutting_down or self._server(binding.server_id).client is not client:
                    raise McpCancelled("stale_connection", "MCP catalog connection was retired")
            outcome = "failed" if lowered.get("is_error") else "succeeded"
            self._finish_activity(activity, outcome)
            self._safe_progress("MCP tool call settled", request_id=request_id)
            return lowered
        except McpCancelled:
            self._finish_activity(activity, "cancelled")
            if parent_cancellation is not None:
                parent_cancellation.raise_if_cancelled()
            return self._error_result(
                binding,
                "MCP cancellation was forwarded; the bridge does not claim rollback.",
            )
        except McpTimeout as error:
            self._mark_call_degraded(binding.server_id, client, error)
            self._finish_activity(activity, "timedOut")
            return self._error_result(
                binding,
                "MCP tool call timed out and was not retried because its outcome may be ambiguous.",
            )
        except McpTransportError as error:
            self._mark_call_degraded(binding.server_id, client, error)
            self._finish_activity(activity, "ambiguous")
            return self._error_result(
                binding,
                "MCP transport was lost; the tool call was not replayed.",
            )
        except McpRemoteError as error:
            self._finish_activity(activity, "failed")
            return self._error_result(
                binding, f"MCP server returned JSON-RPC error {error.rpc_code}."
            )
        except ResourceUnsupportedError as error:
            self._finish_activity(activity, "failed")
            return self._error_result(binding, str(error))
        except (McpError, ToolInputError, ToolResultError, ValueError):
            self._finish_activity(activity, "failed")
            return self._error_result(
                binding, "MCP tool call failed a bounded protocol or result check."
            )
        finally:
            call_active.clear()
            if acquired:
                self._calls.release()

    def _check_dispatch(self, binding: ToolBinding, client: Any, approval_epoch: int) -> None:
        """Caller holds _lock. No approval survives a pending/committed mutation."""
        state = self._server(binding.server_id)
        if self._shutting_down or state.client is not client:
            raise McpCancelled("stale_connection", "MCP catalog connection was retired")
        if binding.approval != "readOnly" and (
            self._catalog_lock.locked()
            or self._host_catalog_revision != approval_epoch
            or state.tools.get(binding.published_name) is not binding
            or state.catalog_revision != binding.server_catalog_revision
        ):
            raise ToolInputError("MCP approval was invalidated by a catalog change")

    def _approve_call(
        self, binding: ToolBinding, arguments: Mapping[str, Any]
    ) -> Optional[str]:
        if binding.approval == "readOnly":
            return None
        features = getattr(self.extension, "negotiated_features", frozenset())
        if "policy_intents" not in features:
            return (
                "MCP tool call denied: the tool lacks an explicit read-only annotation "
                "and the host policy service is unavailable."
            )
        intent = {
            "kind": "external_side_effect",
            "operation": "mcp.tool.call",
            "target": {
                "server": binding.server_id,
                "tool": binding.published_name,
                "server_catalog_revision": binding.server_catalog_revision,
                "arguments": dict(arguments),
            },
            "data_classes": ["tool_arguments"],
            "adapter_hints": {
                "read_only": False,
                "destructive": binding.approval == "destructive",
            },
        }
        try:
            decision = self.extension.evaluate_policy(intent)
            if decision.get("decision") == "allow":
                return None
            token = decision.get("approval_token")
            if (
                decision.get("decision") == "ask"
                and isinstance(token, str)
                and "approvals" in features
            ):
                decision = self.extension.evaluate_policy(intent, approval_token=token)
                if decision.get("decision") == "allow":
                    return None
        except Exception:
            return "MCP tool call denied because host policy evaluation failed."
        return "MCP tool call denied by host policy."

    def _acquire_call_slot(self, cancellation: Any, binding: ToolBinding) -> bool:
        deadline = time.monotonic() + self._server(binding.server_id).config.request_timeout_ms / 1000
        while True:
            if cancellation is not None and bool(getattr(cancellation, "cancelled", False)):
                cancellation.raise_if_cancelled()
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return False
            if self._calls.acquire(timeout=min(0.05, remaining)):
                return True

    def _finish_activity(self, identifier: str, outcome: str) -> None:
        self.presentation.finish_activity(identifier, outcome)
        self._publish_current_presentation()

    def _safe_progress(self, message: str, *, request_id: Any) -> None:
        if "request_progress" not in getattr(
            self.extension, "negotiated_features", frozenset()
        ):
            return
        try:
            self.extension.progress(message=message, request_id=request_id)
        except Exception:
            return

    def _forward_progress(self, event: Mapping[str, Any], request_id: Any) -> None:
        current = event.get("progress")
        total = event.get("total")
        kwargs: dict[str, Any] = {
            "message": "MCP tool progress",
            "request_id": request_id,
        }
        if isinstance(current, (int, float)) and not isinstance(current, bool):
            kwargs["current"] = current
        if isinstance(total, (int, float)) and not isinstance(total, bool):
            kwargs["total"] = total
        try:
            self.extension.progress(**kwargs)
        except Exception:
            return

    def _mark_call_degraded(
        self, server_id: str, client: Any, error: McpError
    ) -> None:
        state = self._server(server_id)
        with self._lock:
            if state.client is client and state.state not in {"parked", "stopped"}:
                state.state = "degraded"
                self._set_error(state, error.code, error.safe_summary)
        self._presentation_changed()

    def _error_result(self, binding: ToolBinding, message: str) -> dict[str, Any]:
        return {
            "content": [{"type": "text", "text": message}],
            "is_error": True,
            "metadata": {
                "mcp": {
                    "serverId": binding.server_id,
                    "tool": binding.published_name,
                    "serverCatalogRevision": binding.server_catalog_revision,
                    "approval": binding.approval,
                    "outcome": "failed",
                    **({"operation": binding.operation} if isinstance(binding, ResourceBinding) else {}),
                }
            },
        }

    def _refresh_all(self, scope: Optional[_RemoteScope] = None) -> bool:
        with self._lock:
            identifiers = [
                state.config.id
                for state in self._servers.values()
                if state.client is not None and state.client.alive
                and (state.config.transport != "streamable-http" or scope is not None)
            ]
        results = [self.refresh_server(identifier, scope=scope) for identifier in identifiers]
        return all(results)

    def _server(self, server_id: str) -> _ServerState:
        with self._lock:
            state = self._servers.get(server_id)
        if state is None:
            raise ValueError("unknown MCP server")
        return state

    def _set_error(self, state: _ServerState, code: str, summary: str) -> None:
        state.last_error = {
            "code": code,
            "summary": summary,
            "atMs": int(time.time() * 1000),
        }

    def _server_record(self, state: _ServerState) -> dict[str, Any]:
        client = state.client
        connected = client is not None and client.alive
        actions = [
            {
                "id": "refresh",
                "enabled": connected and state.state in {"ready", "degraded", "refreshing"},
            },
            {
                "id": "restart",
                "enabled": state.state
                in {"ready", "degraded", "backoff", "parked", "stopped"},
            },
            {"id": "stop", "enabled": state.state not in {"configured", "stopped"}},
        ]
        tools = [
            {
                "id": binding.published_name,
                "name": binding.published_name,
                "schemaSummary": binding.schema_summary,
                "approval": binding.approval,
            }
            for binding in sorted(state.tools.values(), key=lambda item: item.published_name)
        ]
        record: dict[str, Any] = {
            "id": state.config.id,
            "label": state.config.label,
            "state": state.state,
            "connected": connected,
            "required": state.config.required,
            "scope": state.config.scope,
            "transport": state.config.transport,
            "catalogRevision": state.catalog_revision,
            "hostCatalogRevision": state.host_catalog_revision,
            "restart": {
                "attempt": state.restart_attempt,
                "maxAttempts": state.config.max_restarts,
                "nextRetryAtMs": state.next_retry_at_ms,
            },
            "actions": actions,
            "tools": tools,
        }
        if state.last_error is not None:
            record["lastError"] = dict(state.last_error)
        return record
