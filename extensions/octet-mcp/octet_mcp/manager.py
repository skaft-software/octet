"""Resident MCP server/catalog owner and octet API 0.2 bridge."""

from __future__ import annotations

from concurrent.futures import Future, ThreadPoolExecutor
from dataclasses import dataclass, field
import os
from pathlib import Path
import random
import threading
import time
from typing import Any, Callable, Mapping, Optional

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
from .streamable_http import CredentialProvider, McpStreamableHttpClient
from .ownership import ResourceOwner


@dataclass
class _ServerState:
    config: ServerConfig
    state: str
    client: Optional[Any] = None
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
    lifecycle_condition: threading.Condition = field(default_factory=threading.Condition)
    active_lifecycle_operations: int = 0
    active_effects: int = 0
    active_effect_threads: dict[int, int] = field(default_factory=dict)
    retirement_pending: bool = False
    retired: bool = False


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
        credential_provider: Optional[CredentialProvider] = None,
        resource_owner: Optional[ResourceOwner] = None,
        client_factory: Optional[Callable[..., Any]] = None,
        random_source: Optional[random.Random] = None,
        experimental_streamable_http_mcp: bool = False,
    ) -> None:
        self.extension = extension
        self.config = config
        self.presentation = presentation or PresentationProducer()
        self.scratch_directory = scratch_directory or Path(
            os.environ.get("OCTET_EXTENSION_SCRATCH", ".octet-mcp-scratch")
        )
        self.config_error = dict(config_error) if config_error is not None else None
        self._remote_owner = resource_owner
        self._credential_provider = credential_provider
        self._client_factory = client_factory or self._default_client_factory
        self._random = random_source or random.SystemRandom()
        self._experimental_streamable_http_mcp = experimental_streamable_http_mcp
        self._servers: dict[str, _ServerState] = {
            server.id: _ServerState(
                config=server,
                state="configured" if server.enabled else "stopped",
            )
            for server in config.servers
        }
        self._host_catalog_revision = 0
        self._started = False
        self._shutting_down = False
        self._lock = threading.RLock()
        self._catalog_lock = threading.Lock()
        self._presentation_publish_lock = threading.Lock()
        self._active_lifecycle_effect_threads: dict[int, int] = {}
        self._calls = threading.BoundedSemaphore(config.limits.max_concurrent_calls)
        # Do not construct workers for inert or rejected configuration. In
        # particular, the denied remote transport must fail before any worker
        # could resolve DNS or ask a credential provider for a secret.
        self._executor: Optional[ThreadPoolExecutor] = None

    def _streamable_http_allowed(self, server: ServerConfig) -> bool:
        return (
            server.transport != "streamable-http"
            or self._experimental_streamable_http_mcp
        )

    def _owner_required(self, state: _ServerState) -> None:
        state.state = "parked"
        self._set_error(
            state,
            "resource_owner_required",
            "Remote MCP connects after an owner-bound action; choose Restart for it in /extensions",
        )

    def bind_owner(self, context: Mapping[str, Any]) -> bool:
        """Bind remote sessions to the host owner of an explicit user action."""

        return self._bind_remote_owner(context)

    def _bind_remote_owner(self, context: Mapping[str, Any]) -> bool:
        owner = ResourceOwner.from_context(context)
        with self._lock:
            if owner is None:
                return False
            if self._remote_owner is not None:
                return owner == self._remote_owner
            self._remote_owner = owner
        return True

    def assert_lifecycle_mutation_allowed(self) -> None:
        """Reject reentrant manager-wide mutations before they alter state."""

        with self._lock:
            if self._active_lifecycle_effect_threads.get(threading.get_ident(), 0):
                raise RuntimeError("MCP lifecycle mutation is not allowed reentrantly from a callback")

    def _begin_cleanup_callback(self) -> None:
        thread_id = threading.get_ident()
        with self._lock:
            self._active_lifecycle_effect_threads[thread_id] = (
                self._active_lifecycle_effect_threads.get(thread_id, 0) + 1
            )

    def _finish_cleanup_callback(self) -> None:
        thread_id = threading.get_ident()
        with self._lock:
            remaining = self._active_lifecycle_effect_threads[thread_id] - 1
            if remaining:
                self._active_lifecycle_effect_threads[thread_id] = remaining
            else:
                del self._active_lifecycle_effect_threads[thread_id]

    def _lifecycle_lease(self, state: _ServerState) -> bool:
        with self._lock:
            if self._shutting_down or self._servers.get(state.config.id) is not state:
                return False
        with state.lifecycle_condition:
            if state.retirement_pending or state.retired:
                return False
            state.active_lifecycle_operations += 1
            return True

    def _begin_lifecycle_effect(self, state: _ServerState) -> bool:
        """Linearize a start/catalog side effect against state retirement.

        The token is acquired under the per-state condition, but the external
        callback runs after that lock is released. Retirement first closes
        admission, then waits for already-admitted effects before publishing the
        retired state.
        """

        thread_id = threading.get_ident()
        with self._lock:
            if self._shutting_down or self._servers.get(state.config.id) is not state:
                return False
            with state.lifecycle_condition:
                if state.retirement_pending or state.retired:
                    return False
                state.active_effects += 1
                state.active_effect_threads[thread_id] = state.active_effect_threads.get(thread_id, 0) + 1
            self._active_lifecycle_effect_threads[thread_id] = (
                self._active_lifecycle_effect_threads.get(thread_id, 0) + 1
            )
            return True

    def _finish_lifecycle_effect(self, state: _ServerState) -> None:
        thread_id = threading.get_ident()
        with state.lifecycle_condition:
            state.active_effects -= 1
            remaining = state.active_effect_threads[thread_id] - 1
            if remaining:
                state.active_effect_threads[thread_id] = remaining
            else:
                del state.active_effect_threads[thread_id]
            if state.active_effects == 0:
                state.lifecycle_condition.notify_all()

        with self._lock:
            remaining_global = self._active_lifecycle_effect_threads[thread_id] - 1
            if remaining_global:
                self._active_lifecycle_effect_threads[thread_id] = remaining_global
            else:
                del self._active_lifecycle_effect_threads[thread_id]

    @staticmethod
    def _release_lifecycle_lease(state: _ServerState) -> None:
        with state.lifecycle_condition:
            state.active_lifecycle_operations -= 1
            if state.active_lifecycle_operations == 0:
                state.lifecycle_condition.notify_all()

    def _retire_state(self, state: _ServerState) -> None:
        thread_id = threading.get_ident()
        with state.lifecycle_condition:
            if state.active_effect_threads.get(thread_id, 0):
                raise RuntimeError("MCP state cannot be retired reentrantly from its active effect")
            state.retirement_pending = True
            while state.active_effects:
                state.lifecycle_condition.wait()
            state.retired = True
            while state.active_lifecycle_operations:
                state.lifecycle_condition.wait()

    def _server_is_live(self, state: _ServerState) -> bool:
        with self._lock:
            if self._shutting_down or self._servers.get(state.config.id) is not state:
                return False
        with state.lifecycle_condition:
            return not state.retirement_pending and not state.retired

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

    def _submit(self, callback: Callable[..., Any], *arguments: Any) -> Future[Any]:
        return self._executor_for_work().submit(callback, *arguments)

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
    ) -> Any:
        if config.transport == "stdio":
            return McpStdioClient(
                config,
                limits,
                on_failure=on_failure,
                on_tools_changed=on_tools_changed,
            )
        if config.transport == "streamable-http":
            assert self._remote_owner is not None
            return McpStreamableHttpClient(
                config,
                limits,
                resource_owner=self._remote_owner,
                credential_provider=self._credential_provider,
                on_failure=on_failure,
                on_tools_changed=on_tools_changed,
            )
        raise ValueError("unsupported MCP server transport")

    def start(self) -> None:
        """Start explicitly configured servers in bounded parallel workers."""

        self.assert_lifecycle_mutation_allowed()
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
                if state.config.transport == "streamable-http" and self._remote_owner is None:
                    self._owner_required(state)
                    continue
                states.append(state)
        for state in states:
            self._submit(self._start_server_for_state, state, False)
        self._presentation_changed()

    def shutdown(self) -> None:
        """Stop admission, timers, and every owned server root within bounds."""

        self.assert_lifecycle_mutation_allowed()
        with self._lock:
            if self._shutting_down:
                return
            self._shutting_down = True
            states = list(self._servers.values())
            executor = self._executor
            self._executor = None
            for state in states:
                if state.timer is not None:
                    state.timer.cancel()
                    state.timer = None
                state.next_retry_at_ms = None
        for state in states:
            self._retire_state(state)
        clients: list[Any] = []
        for state in states:
            with state.operation_lock:
                with self._lock:
                    if state.client is not None:
                        clients.append(state.client)
                    state.client = None
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

    def apply_config(
        self,
        config: BridgeConfig,
        *,
        credential_provider: Optional[CredentialProvider] = None,
    ) -> dict[str, list[str]]:
        """Adopt an edited configuration without restarting the bridge.

        Removed and changed servers are stopped and their tools unpublished;
        added and changed servers start again when enabled. Untouched servers
        keep their sessions. Bridge-wide limits apply to servers started from
        now on.
        """

        self.assert_lifecycle_mutation_allowed()
        with self._lock:
            if self._shutting_down:
                raise RuntimeError("MCP manager is shutting down")
            wanted = {server.id: server for server in config.servers}
            removed = sorted(set(self._servers) - set(wanted))
            added = sorted(set(wanted) - set(self._servers))
            changed = sorted(
                server_id
                for server_id in set(wanted) & set(self._servers)
                if self._servers[server_id].config != wanted[server_id]
            )
            retiring_states = [self._servers[server_id] for server_id in removed + changed]
        for state in retiring_states:
            self._retire_state(state)
        for server_id in removed + changed:
            self.stop_server(server_id)
        with self._lock:
            self.config = config
            self.config_error = None
            if credential_provider is not None:
                self._credential_provider = credential_provider
            for server_id in removed:
                del self._servers[server_id]
            for server_id in added + changed:
                server = wanted[server_id]
                self._servers[server_id] = _ServerState(
                    config=server,
                    state="configured" if server.enabled else "stopped",
                )
            start = [
                self._servers[server_id]
                for server_id in added + changed
                if self._started and wanted[server_id].enabled
            ]
        for state in start:
            self._submit(self._start_server_for_state, state, False)
        self._presentation_changed()
        return {"added": added, "changed": changed, "removed": removed}

    def request_action(self, action: str, server_id: Optional[str] = None) -> Future[Any]:
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
            return self._submit(self._refresh_all)
        callback = {
            "refresh": self.refresh_server,
            "restart": self.restart_server,
            "stop": self.stop_server,
        }[action]
        assert server_id is not None
        if action == "restart":
            with self._lock:
                admitted_state = self._servers[server_id]
            return self._submit(self._restart_server_state, admitted_state)
        return self._submit(callback, server_id)

    def refresh_server(self, server_id: str) -> bool:
        self.assert_lifecycle_mutation_allowed()
        state = self._server(server_id)
        with state.operation_lock:
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
                raw_tools = client.list_tools()
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

    def restart_server(self, server_id: str) -> bool:
        return self._restart_server_state(self._server(server_id))

    def _restart_server_state(self, state: _ServerState) -> bool:
        self.assert_lifecycle_mutation_allowed()
        if not self._lifecycle_lease(state):
            return False
        try:
            return self._restart_server_admitted(state)
        finally:
            self._release_lifecycle_lease(state)

    def _restart_server_admitted(self, state: _ServerState) -> bool:
        server_id = state.config.id
        with state.operation_lock:
            if not self._server_is_live(state):
                return False
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
            self._remove_server_tools(state)
            if client is not None:
                client.close()
            return self._start_server_with_state(state, True)

    def stop_server(self, server_id: str) -> bool:
        self.assert_lifecycle_mutation_allowed()
        state = self._server(server_id)
        with state.operation_lock:
            with self._lock:
                if state.timer is not None:
                    state.timer.cancel()
                    state.timer = None
                state.next_retry_at_ms = None
                client = state.client
                state.client = None
                state.state = "stopped"
            self._remove_server_tools(state)
            if client is not None:
                client.close()
            self._presentation_changed()
            return True

    def domain_snapshot(self) -> dict[str, Any]:
        """Package-internal records, as the options menu reads them."""

        return self._domain_snapshot()

    def _domain_snapshot(self) -> dict[str, Any]:
        """Build package-internal records used to derive one generic snapshot."""

        with self._lock:
            records = [self._server_record(state) for state in self._servers.values()]
            revision = self._host_catalog_revision
            config_error = dict(self.config_error) if self.config_error else None
        return self.presentation.snapshot(
            records,
            host_catalog_revision=revision,
            config_error=config_error,
        )

    def snapshot(self) -> dict[str, Any]:
        """Return the exact generic API 0.2 presentation snapshot."""

        return host_presentation(self._domain_snapshot())

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
            if self._remote_owner is not None:
                publish(self.snapshot(), resource_owner=self._remote_owner.wire())
            else:
                publish(self.snapshot())
        except Exception:
            # An older host may not expose the generic presentation primitive.
            # The status and /mcp fallbacks remain available and no MCP domain
            # state is duplicated in a frontend.
            return

    def execute_command(self, arguments: list[str], context: Optional[Mapping[str, Any]] = None) -> dict[str, Any]:
        """Implement `/mcp` narrow/headless fallback and safe actions."""

        # Never retag remote credentials, catalogs or sessions in place. A new
        # host owner requires a new resident process; tool arguments cannot bind.
        if any(state.config.transport == "streamable-http" for state in self._servers.values()):
            if context and ResourceOwner.from_context(context) is not None:
                if not self._bind_remote_owner(context):
                    return {"text": "Remote MCP owner mismatch; restart the extension for this host owner."}
            elif self._remote_owner is not None:
                return {"text": "Remote MCP requires the matching host resource owner."}
        if not arguments or arguments == ["status"] or arguments == ["list"]:
            text = format_status(self._domain_snapshot())
        elif arguments == ["snapshot"]:
            text = snapshot_json(self.snapshot())
        elif len(arguments) == 2 and arguments[0] == "show":
            text = format_server_detail(self._domain_snapshot(), arguments[1])
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
                self.request_action(action, target)
            except ValueError as error:
                return {"text": f"MCP action rejected: {error}"}
            scope = target or "all connected servers"
            text = f"MCP {action} requested for {scope}. Its state shows in the octet-mcp menu under /extensions."
        else:
            text = self.command_usage()
        return {"text": text, "notifications": [], "context": []}

    @staticmethod
    def command_usage() -> str:
        return (
            "Usage: /mcp [status|list|snapshot|show <server>|refresh [server]|"
            "restart <server>|stop <server>]"
        )

    def _start_server(self, server_id: str, manual: bool) -> bool:
        return self._start_server_for_state(self._server(server_id), manual)

    def _start_server_for_state(self, state: _ServerState, manual: bool) -> bool:
        if not self._lifecycle_lease(state):
            return False
        try:
            return self._start_server_with_state(state, manual)
        finally:
            self._release_lifecycle_lease(state)

    def _start_server_with_state(self, state: _ServerState, manual: bool) -> bool:
        server_id = state.config.id
        with state.operation_lock:
            if not self._streamable_http_allowed(state.config):
                with self._lock:
                    if self._shutting_down:
                        return False
                    self._remote_transport_error(state)
                self._presentation_changed()
                return False
            if not self._server_is_live(state):
                return False
            with self._lock:
                if self._shutting_down or state.retired or self._servers.get(server_id) is not state:
                    return False
                if state.config.transport == "streamable-http" and self._remote_owner is None:
                    self._owner_required(state)
                    return False
                if state.client is not None and state.client.alive:
                    return True
                if not state.config.enabled and not manual:
                    state.state = "stopped"
                    return False
                state.state = "connecting"
                state.next_retry_at_ms = None
                state.last_error = None
            self._presentation_changed()

            client = self._client_factory(
                state.config,
                self.config.limits,
                lambda failed, error: self._on_client_failure(server_id, failed, error),
                lambda changed: self._on_tools_changed(server_id, changed),
            )
            live = self._server_is_live(state)
            with self._lock:
                retired = (
                    not live
                    or self._shutting_down
                    or self._servers.get(server_id) is not state
                )
                if not retired:
                    state.client = client
            if retired or not self._server_is_live(state):
                with self._lock:
                    if state.client is client:
                        state.client = None
                client.close()
                return False
            try:
                if not self._begin_lifecycle_effect(state):
                    raise McpTransportError("stale_connection", "MCP connection was retired")
                try:
                    client.start()
                finally:
                    self._finish_lifecycle_effect(state)
                raw_tools = client.list_tools()
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
                if state.client is not client or self._shutting_down:
                    client.close()
                    return False
                state.state = "ready"
                state.restart_attempt = 0
                state.last_error = None
                state.next_retry_at_ms = None
            self._presentation_changed()
            return True

    def _start_failed(
        self, state: _ServerState, client: Any, error: McpError
    ) -> None:
        with self._lock:
            if state.client is not client:
                return
            state.client = None
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
        self._remove_server_tools(state)
        client.close()
        self._schedule_after_failure(state, error)

    def _schedule_after_failure(self, state: _ServerState, error: McpError) -> None:
        with self._lock:
            if self._shutting_down or state.retired or state.state == "stopped":
                return
            self._set_error(state, error.code, error.safe_summary)
            if error.permanent or state.restart_attempt >= state.config.max_restarts:
                state.state = "parked"
                state.next_retry_at_ms = None
                self._presentation_changed()
                return
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
                lambda: self._submit_restart_after_backoff(state),
            )
            timer.daemon = True
            state.timer = timer
            timer.start()
        self._presentation_changed()

    def _submit_restart_after_backoff(self, state: _ServerState) -> None:
        with self._lock:
            if self._shutting_down or self._servers.get(state.config.id) is not state:
                return
            if (
                state.state != "backoff"
                or not self._streamable_http_allowed(state.config)
            ):
                return
            state.timer = None
        try:
            self._submit(self._start_server_for_state, state, False)
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
            state.refresh_queued = True
        try:
            self._submit(self.refresh_server, server_id)
        except RuntimeError:
            with self._lock:
                state.refresh_queued = False

    def _publish_catalog(
        self,
        state: _ServerState,
        client: Any,
        raw_tools: list[dict[str, Any]],
    ) -> None:
        if not self._lifecycle_lease(state):
            raise McpTransportError("stale_connection", "MCP connection was replaced")
        try:
            self._publish_catalog_with_lease(state, client, raw_tools)
        finally:
            self._release_lifecycle_lease(state)

    def _publish_catalog_with_lease(
        self,
        state: _ServerState,
        client: Any,
        raw_tools: list[dict[str, Any]],
    ) -> None:
        if not self._begin_lifecycle_effect(state):
            raise McpTransportError("stale_connection", "MCP connection was retired")
        try:
            self._publish_catalog_effect(state, client, raw_tools)
        finally:
            self._finish_lifecycle_effect(state)

    def _publish_catalog_effect(
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
        with self._lock:
            other_tools = sum(
                len(other.tools)
                for other in self._servers.values()
                if other is not state
            )
        if other_tools + len(desired) > self.config.limits.max_total_tools:
            raise CatalogError("MCP catalogs exceed the global tool limit")

        with self._catalog_lock:
            with self._lock:
                if (state.client is not client or self._shutting_down or state.retired
                        or self._servers.get(state.config.id) is not state):
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
                        "handler": self._handler(binding, client),
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
                self._begin_cleanup_callback()
                try:
                    response = self.extension.unregister_tools(*names)
                finally:
                    self._finish_cleanup_callback()
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
        self, binding: ToolBinding, client: Any
    ) -> Callable[[Mapping[str, Any], Mapping[str, Any]], dict[str, Any]]:
        def call(arguments: Mapping[str, Any], context: Mapping[str, Any]) -> dict[str, Any]:
            return self._call_tool(binding, client, arguments, context)

        return call

    def _call_tool(
        self,
        binding: ToolBinding,
        client: Any,
        arguments: Mapping[str, Any],
        context: Mapping[str, Any],
    ) -> dict[str, Any]:
        if self._server(binding.server_id).config.transport == "streamable-http":
            owner = ResourceOwner.from_context(context)
            if owner is None or owner != self._remote_owner or owner != client.resource_owner:
                return self._error_result(binding, "Remote MCP call denied: host resource owner mismatch.")
        activity = self.presentation.start_activity(
            binding.server_id, binding.published_name
        )
        self._publish_current_presentation()
        cancellation = getattr(self.extension, "cancellation", None)
        acquired = False
        try:
            arguments = validate_arguments(arguments, binding.input_schema)
            approval_error = self._approve_call(binding, arguments)
            if approval_error is not None:
                self._finish_activity(activity, "failed")
                return self._error_result(binding, approval_error)
            acquired = self._acquire_call_slot(cancellation, binding)
            if not acquired:
                self._finish_activity(activity, "timedOut")
                return self._error_result(binding, "MCP call admission timed out.")
            request_id = getattr(self.extension, "request_id", None)
            self._safe_progress("Calling configured MCP tool", request_id=request_id)
            result = client.call_tool(
                binding.upstream_name,
                arguments,
                cancellation=cancellation,
                progress=lambda event: self._forward_progress(event, request_id),
            )
            lowered = lower_tool_result(
                self.extension,
                binding,
                result,
                scratch_directory=self.scratch_directory,
            )
            outcome = "failed" if lowered.get("is_error") else "succeeded"
            self._finish_activity(activity, outcome)
            self._safe_progress("MCP tool call settled", request_id=request_id)
            return lowered
        except McpCancelled:
            self._finish_activity(activity, "cancelled")
            if cancellation is not None:
                cancellation.raise_if_cancelled()
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
        except (McpError, ToolInputError, ToolResultError, ValueError):
            self._finish_activity(activity, "failed")
            return self._error_result(
                binding, "MCP tool call failed a bounded protocol or result check."
            )
        finally:
            if acquired:
                self._calls.release()

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
                return self._confirm_unannotated_call(binding)
            token = decision.get("approval_token")
            if (
                decision.get("decision") == "ask"
                and isinstance(token, str)
                and "approvals" in features
            ):
                decision = self.extension.evaluate_policy(intent, approval_token=token)
                if decision.get("decision") == "allow":
                    return self._confirm_unannotated_call(binding)
        except Exception:
            return "MCP tool call denied because host policy evaluation failed."
        return "MCP tool call denied by host policy."

    def _confirm_unannotated_call(
        self, binding: ToolBinding
    ) -> Optional[str]:
        """Ask the user before dispatching a call the server did not mark read-only.

        This is cooperative UI layered on top of host policy, not an OS sandbox or
        a replacement for it. A non-interactive or unavailable confirmation
        surface denies rather than assuming approval, and a dropped, expired, or
        declined answer never falls through to dispatch.
        """

        if not self._server(binding.server_id).config.confirm_unknown_tools:
            return None
        confirm = getattr(self.extension, "confirm", None)
        if not callable(confirm):
            return (
                "MCP tool call denied: this server requires user confirmation and the "
                "host confirmation service is unavailable."
            )
        try:
            approved = confirm(
                "Allow this configured MCP tool call?",
                detail=(
                    f"Server: {binding.server_label}. "
                    f"Tool: {binding.published_name}."
                ),
                destructive=binding.approval == "destructive",
                default=False,
            )
        except Exception:
            return "MCP tool call denied because the user confirmation request failed."
        if approved is not True:
            return "MCP tool call denied: the user did not confirm it."
        return None

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
                }
            },
        }

    def _refresh_all(self) -> bool:
        with self._lock:
            identifiers = [
                state.config.id
                for state in self._servers.values()
                if state.client is not None and state.client.alive
            ]
        results = [self.refresh_server(identifier) for identifier in identifiers]
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
