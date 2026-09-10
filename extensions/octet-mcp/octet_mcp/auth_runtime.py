"""User-command-only authentication wiring for one isolated host generation.

The manager owns MCP connections and their _RemoteScope fences. This adapter
uses only its public owner observation/activation/action APIs; it never infers an owner from
initialize, command arguments, an MCP response, or a default/ambient identity.
"""

from __future__ import annotations

from concurrent.futures import TimeoutError as FutureTimeout
import threading
import time
from typing import Any, Mapping, Optional

from .auth import AuthError, AuthOwner, check_operation
from .auth_service import AuthService, COMMAND_OPERATION_SECONDS, OwnerCredentialProvider
from .config import BridgeConfig, ServerConfig


AUTH_ACTIONS = frozenset({"login", "login-manual", "poll", "complete-manual", "cancel", "status", "logout"})
AUTH_USAGE = ("Usage: /mcp auth [login|login-manual|poll|complete-manual|cancel|status|logout] <server>. "
              "Never put tokens or callback URLs in command arguments.")


class RuntimeAuthentication:
    def __init__(self, extension: Any, config: BridgeConfig, service: AuthService, *, enabled: bool) -> None:
        self.extension = extension
        self.service = service
        self.config = config
        self.enabled = enabled
        self._context: Optional[dict[str, Any]] = None
        self._lock = threading.RLock()
        self._retired = threading.Event()
        self._blocked: set[str] = set()
        self._command_epochs = {server.id: 0 for server in config.servers}
        self.provider = OwnerCredentialProvider(service, config.servers, is_current=self._credential_current)
        # build_runtime assigns the manager before any handler or worker starts.
        self.manager: Any = None

    def activate_owner(self, context: Mapping[str, Any]) -> bool:
        # The public manager operation checks negotiated lifecycle, display-ID
        # mapping, durable owner, generation, settlement and process shutdown.
        # Hold this short lock so startup's credential lookup cannot race the
        # installation of its exact host context. No host request occurs here.
        with self._lock:
            active = self.manager.activate_owner(context)
            active = active or self.manager.is_current_owner(context)
            if active:
                self._remember_owner(context)
            return active

    def _remember_owner(self, context: Mapping[str, Any]) -> None:
        AuthOwner.from_context(context)
        frozen = {"resource_owner": dict(context["resource_owner"]),
                  "host": {"session_id": context["host"]["session_id"]}}
        if self._context is None:
            self._context = frozen
        elif self._context != frozen:
            raise AuthError("authentication_denied")

    def manager_command(self, arguments: list[str], context: Mapping[str, Any]) -> dict[str, Any]:
        # Explicit restart may bind a previously unbound/disabled remote. Its
        # worker must see the host context before it can consult credentials.
        with self._lock:
            result = self.manager.execute_command(arguments, context)
            if self.manager.is_current_owner(context):
                self._remember_owner(context)
            return result

    def _owner_current(self, owner: Mapping[str, Any], server: ServerConfig) -> bool:
        with self._lock:
            context = self._context
            if (not self.enabled or self._retired.is_set() or context is None
                    or owner != context["resource_owner"] or server not in self.manager.config.servers):
                return False
        try:
            # Passive public fence: credential use can never activate a scope
            # or restart a stopped connection. The manager remains authority.
            return self.manager.is_current_owner(context)
        except (ValueError, RuntimeError):
            return False

    def _credential_current(self, owner: Mapping[str, Any], server: ServerConfig) -> bool:
        with self._lock:
            if server.id in self._blocked:
                return False
        return self._owner_current(owner, server)

    def observe_session(self, method: str, event: Mapping[str, Any]) -> None:
        self.manager.observe_session(method, event)
        with self._lock:
            context = self._context
            settles = (method == "session/settled" and context is not None
                       and event.get("session_id") == context["host"]["session_id"])
            if settles:
                self._retired.set()
        if settles:
            # Ordered reader boundary: do not wait for private input, network,
            # callback workers or the general command-admission lock.
            self.service.retire_owner(context, wait=False)

    def _stop_connection(self, server: ServerConfig, context: Mapping[str, Any], deadline: float, cancel) -> bool:
        # Revoke provider use before a queued manager action or slow catalog
        # unregister can complete. The manager's own connection fence closes the
        # socket and rejects captured handlers on stop; no call is replayed.
        with self._lock:
            self._blocked.add(server.id)
        future = self.manager.request_action("stop", server.id, context=context)
        stop_deadline = min(deadline, time.monotonic() + 5.0)
        while True:
            check_operation(stop_deadline, cancel)
            try:
                return bool(future.result(timeout=0.05))
            except FutureTimeout:
                pass

    def execute_command(self, arguments: list[str], context: Mapping[str, Any]) -> dict[str, Any]:
        # Only the declared command dispatcher calls this method. It is not a
        # model tool, hook, notification, or MCP reverse-request handler.
        if (len(arguments) != 3 or arguments[0] != "auth"
                or not all(isinstance(item, str) for item in arguments)
                or arguments[1] not in AUTH_ACTIONS):
            return {"text": AUTH_USAGE, "notifications": [], "context": []}
        action, server_id = arguments[1:]
        try:
            if not self.enabled:
                raise AuthError("authentication_gate")
            server = next((item for item in self.config.servers if item.id == server_id), None)
            if server is None or server.transport != "streamable-http" or server.auth is None:
                raise AuthError("authentication_unavailable")
            if action in {"login", "login-manual"}:
                if not self.activate_owner(context):
                    raise AuthError("authentication_denied")
            else:
                # Status/poll/cancel/logout never allocate or launch a remote.
                # A completed setup or explicit restart supplies its binding.
                with self._lock:
                    if not self.manager.is_current_owner(context):
                        raise AuthError("authentication_denied")
                    self._remember_owner(context)
            owner = dict(context["resource_owner"])
            with self._lock:
                if action in {"cancel", "logout"}:
                    self._command_epochs[server.id] += 1
                epoch = self._command_epochs[server.id]
            def is_current() -> bool:
                with self._lock:
                    same_command_epoch = self._command_epochs[server.id] == epoch
                return same_command_epoch and self._owner_current(owner, server)
            token = self.extension.cancellation  # Capture this command, not a worker's ambient scope.
            cancelled = lambda: bool(token and token.cancelled) or not is_current()
            deadline = time.monotonic() + COMMAND_OPERATION_SECONDS
            check_operation(deadline, cancelled)

            def present(url: str, *, issuer: str, resource: str) -> bool:
                check_operation(deadline, cancelled)
                # Authorization state belongs only in ephemeral private input,
                # not ordinary confirmation details or public frontend events.
                prompt = ("Manual MCP authorization\n"
                          "Credentials will be stored as owner-private plaintext (not encrypted).\n"
                          "Copy/open this URL manually. Type continue to proceed, or cancel to stop. "
                          "Finish authentication after this command returns, then /mcp auth poll <server>.\n"
                          "Issuer: " + issuer + "\nResource: " + resource + "\nAuthorization URL: " + url)
                if len(prompt.encode("utf-8")) > 16 * 1024:
                    raise AuthError("authentication_metadata")
                # This first, secret-free consent also fails before private
                # presentation when confirmation support was not negotiated.
                if not self.extension.confirm(
                        "Review this manual MCP login?",
                        detail="After manual authorization, credentials will be stored as owner-private plaintext (not encrypted).",
                        default=False):
                    return False
                check_operation(deadline, cancelled)
                answer = self.extension.request_input(prompt, secret=True)
                check_operation(deadline, cancelled)
                return answer == "continue"

            # A credential replacement must not leave a previous MCP session
            # alive under a newly authorized identity. Explicit restart is the
            # separate connection action after successful login.
            if action in {"login", "login-manual"}:
                if not self._stop_connection(server, context, deadline, cancelled):
                    raise AuthError("authentication_unavailable")
            if action == "logout":
                with self._lock:
                    self._blocked.add(server.id)
            result = self.service.execute_command(
                action, server, {"resource_owner": owner}, is_current=is_current,
                trusted_user_command=True, request_input=self.extension.request_input,
                present_authorization=present, cancel=cancelled, deadline=deadline)
            if action in {"login", "login-manual", "poll", "complete-manual"} and result["auth"]["state"] == "active":
                check_operation(deadline, cancelled)
                with self._lock:
                    self._blocked.discard(server.id)
                result["text"] += " Use /mcp restart <server> to connect; no call was replayed."
            if action == "logout":
                try:
                    self._stop_connection(server, context, deadline, cancelled)
                except Exception:
                    # Local removal result must remain truthful even if remote
                    # shutdown is still pending. Credentials stay blocked here.
                    result["text"] += " MCP connection shutdown is pending; reload if it does not settle."
            return {"text": result["text"], "notifications": [], "context": []}
        except AuthError as error:
            return {"text": error.safe_message, "notifications": [], "context": []}
        except Exception:
            # SDK generic handler exceptions include exception strings in logs.
            # Catch private UI/provider failures here, before that boundary.
            return {"text": AuthError().safe_message, "notifications": [], "context": []}

    def shutdown(self) -> None:
        self._retired.set()
        self.service.shutdown()
