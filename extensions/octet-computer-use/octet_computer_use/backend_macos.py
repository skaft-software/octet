"""Safe, explicit macOS observation and input boundary.

The public methods in this module are intentionally synchronous. One backend
instance owns one serialized native stream; every input operation requires a
fresh observation, exact target identity, bounded geometry, and (by default) a
confirmation callback. The native adapter is injectable only so tests can use
fully mocked fixtures without touching a real desktop.
"""

from __future__ import annotations

import re
import threading
from dataclasses import replace
from typing import Any, Callable, Mapping, Optional, Sequence, Tuple

from .macos.model import (
    AccessibilityNode,
    AccessibilityTree,
    ConfirmationRequest,
    MAX_CHILDREN,
    MAX_DEPTH,
    MAX_DESCRIPTION_BYTES,
    MAX_GENERATION,
    MAX_NODE_REF_BYTES,
    MAX_NODES,
    MAX_ACTIONS,
    MAX_SCREENSHOT_BYTES,
    MacOSBackendError,
    Observation,
    PermissionReport,
    Point,
    ResourceOwner,
    TargetIdentity,
    WindowGeometry,
    WindowSnapshot,
)
from .macos.native import MacOSNative
from .macos.policy import (
    MacOSPolicy,
    is_sensitive_node,
    validate_drag_duration,
    validate_key,
    validate_scroll,
    validate_text,
)


_TARGET_KEY = "target"
_DIRECT_OWNER = ("direct",)
_REF_RE = re.compile(r"^ax:(?:root|[0-9]+(?:\.[0-9]+)*)$")


class MacOSBackend:
    """Native macOS backend with explicit opt-in and fail-closed boundaries."""

    def __init__(
        self,
        *,
        opt_in: bool = False,
        enabled: Optional[bool] = None,
        allow_input: bool = False,
        allowed_bundle_ids: Optional[Sequence[str]] = None,
        require_confirmation: bool = True,
        require_foreground: bool = True,
        max_nodes: int = MAX_NODES,
        max_depth: int = MAX_DEPTH,
        max_text_bytes: int = 4_096,
        max_screenshot_bytes: int = MAX_SCREENSHOT_BYTES,
        max_scroll_delta: int = 2_000,
        max_drag_seconds: float = 5.0,
        native: Any = None,
        policy: Optional[MacOSPolicy] = None,
    ) -> None:
        if enabled is not None:
            if type(enabled) is not bool:
                raise ValueError("enabled must be a boolean")
            opt_in = enabled
        if policy is not None:
            self.policy = policy
        else:
            self.policy = MacOSPolicy(
                opt_in=opt_in,
                allow_input=allow_input,
                allowed_bundle_ids=allowed_bundle_ids,
                require_confirmation=require_confirmation,
                require_foreground=require_foreground,
                max_nodes=max_nodes,
                max_depth=max_depth,
                max_text_bytes=max_text_bytes,
                max_screenshot_bytes=max_screenshot_bytes,
                max_scroll_delta=max_scroll_delta,
                max_drag_seconds=max_drag_seconds,
            )
        self.native = native if native is not None else MacOSNative()
        self._lock = threading.RLock()
        self._stop_requested = threading.Event()
        self._generation = 0
        self._observations: dict[Tuple[Any, Tuple[str, int, int, Optional[str]]], Observation] = {}
        self._selected: dict[Any, TargetIdentity] = {}

    @property
    def enabled(self) -> bool:
        return self.policy.opt_in

    def status(self) -> dict[str, Any]:
        """Return bounded capability and permission state without prompting."""

        with self._lock:
            report = self._permission_report()
            return {
                "schema": "octet.macos.backend-status.v1",
                "backend": "macos-native",
                "enabled": bool(self.policy.opt_in),
                "opt_in": bool(self.policy.opt_in),
                "ready": bool(self.policy.opt_in and report.ready and not self._stop_requested.is_set()),
                "stopped": self._stop_requested.is_set(),
                "input_ready": bool(self.policy.opt_in and self.policy.allow_input and report.ready and not self._stop_requested.is_set()),
                "permissions": report.as_dict(),
                "policy": self.policy.as_dict(),
                "native_available": self._native_available(),
                "permission_requests": "never",
            }

    def permission_report(self) -> PermissionReport:
        with self._lock:
            return self._permission_report()

    def permissions(self) -> dict[str, Any]:
        return self.permission_report().as_dict()

    def windows(self, *, owner: Any = None) -> list[dict[str, Any]]:
        """Enumerate only bounded on-screen window identities from the adapter."""

        with self._lock:
            self._require_ready("observe")
            owner_key = self._owner_key(owner)
            del owner_key  # Enumeration is not mutable, but validates the owner shape.
            method = getattr(self.native, "list_windows", None)
            if not callable(method):
                raise MacOSBackendError("native_unavailable", "The native adapter cannot enumerate windows.")
            try:
                values = method()
            except MacOSBackendError:
                raise
            except Exception as error:
                raise MacOSBackendError("native_failure", "The native window enumeration failed safely.") from error
            result: list[dict[str, Any]] = []
            try:
                iterator = iter(values or ())
                for index, value in enumerate(iterator):
                    if index >= MAX_NODES:
                        break
                    try:
                        window = self._coerce_window(value, None)
                    except MacOSBackendError:
                        continue
                    if self.policy.allows_target(window.identity):
                        result.append(window.as_dict())
            except MacOSBackendError:
                raise
            except Exception as error:
                raise MacOSBackendError(
                    "native_malformed", "The native window enumeration was malformed."
                ) from error
            return result[:MAX_NODES]

    list_windows = windows

    def select_target(self, target: Any, *, owner: Any = None) -> TargetIdentity:
        """Validate and pin one exact process/window identity for an owner."""

        with self._lock:
            self._require_ready("observe")
            owner_key = self._owner_key(owner)
            requested = self._coerce_target(target)
            current = self._inspect(requested)
            self._selected[owner_key] = current.identity
            return current.identity

    select = select_target

    def selected_window(self, *, owner: Any = None) -> Optional[dict[str, Any]]:
        with self._lock:
            owner_key = self._owner_key(owner)
            selected = self._selected.get(owner_key)
            if selected is None:
                return None
            self._require_ready("observe")
            return self._inspect(selected).as_dict()

    def observe(
        self,
        target: Any = None,
        *,
        owner: Any = None,
        include_screenshot: bool = False,
    ) -> dict[str, Any]:
        """Observe exact target identity, geometry, and value-free AX nodes."""

        with self._lock:
            self._require_ready("observe")
            owner_key = self._owner_key(owner)
            requested = self._target_for(target, owner_key)
            observation = self._observe_locked(requested, owner_key)
            result = observation.as_dict()
            if include_screenshot:
                result["screenshot_png"] = self._capture_locked(observation.target, observation, owner_key)
            return result

    def click(
        self,
        target: Any,
        ref: str,
        *,
        observation: Any,
        owner: Any = None,
        confirmation: Any = None,
        cancellation: Any = None,
        authorization: Any = None,
    ) -> dict[str, Any]:
        """Press one observed AX node or its observed bounded center."""

        with self._lock:
            context = self._prepare_action(
                target,
                observation,
                owner,
                ref=ref,
                operation="click",
                cancellation=cancellation,
            )
            node = context[2]
            point = self._node_point(node, context[1].geometry)
            self._confirm(confirmation, "click", context[0].target, node.ref)
            self._revalidate_after_confirmation(context)
            self._check_cancel(cancellation)
            try:
                method = getattr(self.native, "click", None)
                if not callable(method):
                    raise MacOSBackendError("native_unavailable", "The native adapter cannot click.")
                if authorization is not None:
                    authorization()
                method(
                    context[0].target,
                    node.path,
                    point,
                    expected_role=node.role,
                    expected_title=node.title,
                    expected_bounds=node.bounds,
                    cancellation=self._native_cancellation(cancellation),
                )
                result = self._after_action_locked(
                    context[0].target, context[0], "click", owner, cancellation
                )
                return result
            except MacOSBackendError:
                self._safe_release_all()
                raise
            except Exception as error:
                self._safe_release_all()
                raise MacOSBackendError("native_failure", "The native click failed safely.") from error
            finally:
                if not self._safe_release_all():
                    self._stop_requested.set()
                    raise MacOSBackendError("input_release_unverified", "Native input release was not acknowledged; runtime stopped.")

    def type_text(
        self,
        target: Any,
        ref: str,
        text: str,
        *,
        observation: Any,
        owner: Any = None,
        confirmation: Any = None,
        cancellation: Any = None,
        authorization: Any = None,
    ) -> dict[str, Any]:
        """Set one non-sensitive observed editable value without returning it."""

        with self._lock:
            context = self._prepare_action(
                target,
                observation,
                owner,
                ref=ref,
                operation="type_text",
                cancellation=cancellation,
            )
            node = context[2]
            if not node.editable:
                raise MacOSBackendError("node_not_editable", "The observed node is not an editable control.")
            if is_sensitive_node(node):
                raise MacOSBackendError(
                    "sensitive_input_refused",
                    "Text input into credential or protected controls is refused.",
                )
            bounded_text = validate_text(text, self.policy.max_text_bytes)
            point = self._node_point(node, context[1].geometry)
            self._confirm(confirmation, "type_text", context[0].target, node.ref)
            self._revalidate_after_confirmation(context)
            self._check_cancel(cancellation)
            try:
                method = getattr(self.native, "set_text", None)
                if not callable(method):
                    method = getattr(self.native, "type_text", None)
                if not callable(method):
                    raise MacOSBackendError("native_unavailable", "The native adapter cannot type text.")
                if authorization is not None:
                    authorization()
                method(
                    context[0].target,
                    node.path,
                    bounded_text,
                    point,
                    expected_role=node.role,
                    expected_title=node.title,
                    expected_bounds=node.bounds,
                    cancellation=self._native_cancellation(cancellation),
                )
                return self._after_action_locked(
                    context[0].target, context[0], "type_text", owner, cancellation
                )
            except MacOSBackendError:
                self._safe_release_all()
                raise
            except Exception as error:
                self._safe_release_all()
                raise MacOSBackendError("native_failure", "The native text input failed safely.") from error
            finally:
                if not self._safe_release_all():
                    self._stop_requested.set()
                    raise MacOSBackendError("input_release_unverified", "Native input release was not acknowledged; runtime stopped.")

    def press(
        self,
        target: Any,
        key: str,
        *,
        observation: Any,
        ref: Optional[str] = None,
        owner: Any = None,
        confirmation: Any = None,
        cancellation: Any = None,
        authorization: Any = None,
    ) -> dict[str, Any]:
        """Press one allowlisted key only when focus is observed in the target."""

        with self._lock:
            context = self._prepare_action(
                target,
                observation,
                owner,
                ref=ref,
                operation="press",
                cancellation=cancellation,
                require_node=False,
            )
            bounded_key = validate_key(key)
            node = context[2]
            if node is None:
                focused = [item for item in context[0].accessibility.nodes if item.focused]
                if len(focused) != 1:
                    raise MacOSBackendError(
                        "focus_unverified",
                        "A key action requires exactly one observed focused control.",
                    )
                node = focused[0]
            elif not node.focused:
                raise MacOSBackendError(
                    "focus_unverified",
                    "The requested key target was not observed focused; observe again.",
                )
            if not node.enabled:
                raise MacOSBackendError("node_disabled", "The observed focused control is disabled.")
            if is_sensitive_node(node):
                raise MacOSBackendError(
                    "sensitive_input_refused",
                    "Keyboard input to credential or protected controls is refused.",
                )
            self._confirm(confirmation, "press", context[0].target, node.ref)
            self._revalidate_after_confirmation(context)
            self._check_cancel(cancellation)
            try:
                method = getattr(self.native, "press_key", None)
                if not callable(method):
                    raise MacOSBackendError("native_unavailable", "The native adapter cannot press keys.")
                if authorization is not None:
                    authorization()
                method(bounded_key, cancellation=self._native_cancellation(cancellation))
                return self._after_action_locked(
                    context[0].target, context[0], "press", owner, cancellation
                )
            except MacOSBackendError:
                self._safe_release_all()
                raise
            except Exception as error:
                self._safe_release_all()
                raise MacOSBackendError("native_failure", "The native key action failed safely.") from error
            finally:
                if not self._safe_release_all():
                    self._stop_requested.set()
                    raise MacOSBackendError("input_release_unverified", "Native input release was not acknowledged; runtime stopped.")

    press_key = press

    def scroll(
        self,
        target: Any,
        delta_x: int,
        delta_y: int,
        *,
        observation: Any,
        owner: Any = None,
        confirmation: Any = None,
        cancellation: Any = None,
        authorization: Any = None,
    ) -> dict[str, Any]:
        """Scroll a bounded amount at the exact observed window."""

        with self._lock:
            context = self._prepare_action(
                target,
                observation,
                owner,
                ref=None,
                operation="scroll",
                cancellation=cancellation,
                require_node=False,
            )
            bounded_x, bounded_y = validate_scroll(
                delta_x, delta_y, self.policy.max_scroll_delta
            )
            self._confirm(confirmation, "scroll", context[0].target, None)
            self._revalidate_after_confirmation(context)
            self._check_cancel(cancellation)
            try:
                method = getattr(self.native, "scroll", None)
                if not callable(method):
                    raise MacOSBackendError("native_unavailable", "The native adapter cannot scroll.")
                if authorization is not None:
                    authorization()
                method(
                    context[0].target,
                    bounded_x,
                    bounded_y,
                    cancellation=self._native_cancellation(cancellation),
                )
                return self._after_action_locked(
                    context[0].target, context[0], "scroll", owner, cancellation
                )
            except MacOSBackendError:
                self._safe_release_all()
                raise
            except Exception as error:
                self._safe_release_all()
                raise MacOSBackendError("native_failure", "The native scroll failed safely.") from error
            finally:
                if not self._safe_release_all():
                    self._stop_requested.set()
                    raise MacOSBackendError("input_release_unverified", "Native input release was not acknowledged; runtime stopped.")

    def drag(
        self,
        target: Any,
        ref: str,
        to: Any,
        *,
        observation: Any,
        duration: float = 0.25,
        owner: Any = None,
        confirmation: Any = None,
        cancellation: Any = None,
        authorization: Any = None,
    ) -> dict[str, Any]:
        """Drag from an observed node center to a bounded point in its window."""

        with self._lock:
            context = self._prepare_action(
                target,
                observation,
                owner,
                ref=ref,
                operation="drag",
                cancellation=cancellation,
            )
            node = context[2]
            start = self._node_point(node, context[1].geometry)
            try:
                end = Point.from_value(to, label="drag destination")
            except MacOSBackendError:
                raise
            if not context[1].geometry.contains(end):
                raise MacOSBackendError(
                    "invalid_geometry",
                    "The drag destination must remain inside the selected window.",
                )
            bounded_duration = validate_drag_duration(duration, self.policy.max_drag_seconds)
            self._confirm(confirmation, "drag", context[0].target, node.ref)
            self._revalidate_after_confirmation(context)
            self._check_cancel(cancellation)
            try:
                method = getattr(self.native, "drag", None)
                if not callable(method):
                    raise MacOSBackendError("native_unavailable", "The native adapter cannot drag.")
                if authorization is not None:
                    authorization()
                method(
                    context[0].target,
                    start,
                    end,
                    bounded_duration,
                    cancellation=self._native_cancellation(cancellation),
                )
                return self._after_action_locked(
                    context[0].target, context[0], "drag", owner, cancellation
                )
            except MacOSBackendError:
                self._safe_release_all()
                raise
            except Exception as error:
                self._safe_release_all()
                raise MacOSBackendError("native_failure", "The native drag failed safely.") from error
            finally:
                if not self._safe_release_all():
                    self._stop_requested.set()
                    raise MacOSBackendError("input_release_unverified", "Native input release was not acknowledged; runtime stopped.")

    def screenshot(
        self,
        target: Any = None,
        *,
        observation: Any = None,
        owner: Any = None,
    ) -> bytes:
        """Capture only the exact selected window as bounded PNG bytes."""

        with self._lock:
            self._require_ready("screenshot")
            owner_key = self._owner_key(owner)
            requested = self._target_for(target, owner_key)
            stored: Optional[Observation] = None
            if observation is not None:
                stored = self._find_observation(observation, requested, owner_key)
                requested = stored.target
            current = self._inspect(requested)
            if stored is not None:
                self._assert_window_unchanged(stored.window, current)
            return self._capture_locked(current.identity, stored, owner_key)

    capture_window = screenshot

    def perform(
        self,
        action: Mapping[str, Any],
        *,
        owner: Any = None,
        confirmation: Any = None,
        cancellation: Any = None,
        authorization: Any = None,
    ) -> Any:
        """Dispatch the stable typed API without accepting native code strings."""

        if not isinstance(action, Mapping):
            raise MacOSBackendError("invalid_action", "Native actions must be a mapping.")
        operation = action.get("operation", action.get("type"))
        if not isinstance(operation, str):
            raise MacOSBackendError("invalid_action", "Native actions require a bounded operation name.")
        if operation == "stop":
            return self.stop(owner=owner)
        target = action.get(_TARGET_KEY)
        observation = action.get("observation")
        if operation == "click":
            return self.click(target, action.get("ref"), observation=observation, owner=owner, confirmation=confirmation, cancellation=cancellation, authorization=authorization)
        if operation == "type_text":
            return self.type_text(target, action.get("ref"), action.get("text"), observation=observation, owner=owner, confirmation=confirmation, cancellation=cancellation, authorization=authorization)
        if operation in {"press", "press_key"}:
            return self.press(target, action.get("key"), observation=observation, ref=action.get("ref"), owner=owner, confirmation=confirmation, cancellation=cancellation, authorization=authorization)
        if operation == "scroll":
            return self.scroll(target, action.get("delta_x", 0), action.get("delta_y", 0), observation=observation, owner=owner, confirmation=confirmation, cancellation=cancellation, authorization=authorization)
        if operation == "drag":
            return self.drag(target, action.get("ref"), action.get("to"), observation=observation, duration=action.get("duration", 0.25), owner=owner, confirmation=confirmation, cancellation=cancellation, authorization=authorization)
        if operation in {"screenshot", "capture_window"}:
            return self.screenshot(target, observation=observation, owner=owner)
        raise MacOSBackendError("invalid_action", "The native operation is not in the bounded action allowlist.")

    def stop(self, *, owner: Any = None) -> dict[str, Any]:
        """Trusted stop: cancel, release every held input, and discard evidence."""

        # Set the event before waiting for the serialization lock so a drag or
        # Unicode loop can observe cancellation while another thread requests
        # the trusted stop.
        self._owner_key(owner)
        self._stop_requested.set()
        with self._lock:
            released = self._safe_release_all()
            self._observations.clear()
            self._selected.clear()
            return {
                "schema": "octet.macos.stop-result.v1",
                "stopped": True,
                "input_released": released,
            }

    def takeover(self, *, owner: Any = None) -> dict[str, Any]:
        """Alias used by hosts when ownership is revoked or replaced."""

        return self.stop(owner=owner)

    close = stop
    release = stop

    def _require_ready(self, operation: str) -> None:
        if self._stop_requested.is_set():
            raise MacOSBackendError("stopped", "A stopped backend requires a new owner runtime.")
        if not self.policy.opt_in:
            raise MacOSBackendError(
                "native_opt_in_required",
                "Native macOS automation is disabled until explicit opt_in=True is configured.",
            )
        if not self._native_available():
            raise MacOSBackendError(
                "native_unavailable",
                "The dependency-free macOS native interfaces are unavailable on this host.",
            )
        report = self._permission_report()
        required = ("accessibility", "screen_recording")
        if operation in {"input", "click", "type_text", "press", "scroll", "drag"}:
            if not self.policy.allow_input:
                raise MacOSBackendError(
                    "input_opt_in_required",
                    "Input actions require the separate allow_input=True opt-in.",
                )
            required = required + ("synthetic_input",)
        missing = [name for name in required if getattr(report, name) != "granted"]
        if missing:
            readable = ", ".join(missing)
            raise MacOSBackendError(
                "permission_denied",
                f"Required macOS permission is {readable}; operation {operation!r} is fail-closed.",
            )

    def _permission_report(self) -> PermissionReport:
        if not self.policy.opt_in:
            supported = self._native_available()
            return PermissionReport(
                supported=supported,
                accessibility="not_opted_in",
                screen_recording="not_opted_in",
                synthetic_input="not_opted_in",
                detail="Permission preflights are not queried until native opt-in is explicit.",
            )
        method = getattr(self.native, "permission_status", None)
        if not callable(method):
            return PermissionReport(
                supported=self._native_available(),
                accessibility="unknown",
                screen_recording="unknown",
                synthetic_input="unknown",
                detail="The native adapter did not provide permission preflights.",
            )
        try:
            value = method()
        except Exception:
            return PermissionReport(
                supported=self._native_available(),
                accessibility="unknown",
                screen_recording="unknown",
                synthetic_input="unknown",
                detail="A permission preflight failed; access is denied closed.",
            )
        if isinstance(value, PermissionReport):
            return value
        if not isinstance(value, Mapping):
            return PermissionReport(
                supported=self._native_available(),
                accessibility="unknown",
                screen_recording="unknown",
                synthetic_input="unknown",
                detail="The native permission report was malformed; access is denied closed.",
            )
        supported_value = value.get("supported", self._native_available())
        supported = supported_value if type(supported_value) is bool else False
        raw_detail = value.get("detail")
        if not isinstance(raw_detail, str):
            detail = ""
        else:
            try:
                detail = raw_detail if len(raw_detail.encode("utf-8")) <= MAX_DESCRIPTION_BYTES else ""
            except UnicodeError:
                detail = ""
        return PermissionReport(
            supported=supported,
            accessibility=self._normalize_permission(value.get("accessibility")),
            screen_recording=self._normalize_permission(value.get("screen_recording")),
            synthetic_input=self._normalize_permission(value.get("synthetic_input", value.get("input"))),
            detail=detail,
        )

    @staticmethod
    def _normalize_permission(value: Any) -> str:
        """Normalize only trusted boolean preflight results.

        Native permission APIs are expected to return exact booleans at this
        boundary. Text such as ``"granted"`` is not accepted from an injected
        or serialized report because it can turn malformed status data into an
        input grant.
        """
        if type(value) is bool:
            return "granted" if value else "denied"
        return "unknown"

    def _native_available(self) -> bool:
        value = getattr(self.native, "available", True)
        try:
            result = value() if callable(value) else value
        except Exception:
            return False
        return type(result) is bool and result

    @staticmethod
    def _owner_key(owner: Any) -> Any:
        if owner is None:
            return _DIRECT_OWNER
        if isinstance(owner, ResourceOwner):
            return owner.key
        return ResourceOwner.from_value(owner).key

    @staticmethod
    def _coerce_target(value: Any) -> TargetIdentity:
        try:
            return TargetIdentity.from_value(value)
        except MacOSBackendError:
            raise
        except (TypeError, ValueError) as error:
            raise MacOSBackendError("invalid_target", "The target identity is malformed.") from error

    def _target_for(self, value: Any, owner_key: Any) -> TargetIdentity:
        if value is None:
            value = self._selected.get(owner_key)
            if value is None:
                raise MacOSBackendError(
                    "target_required",
                    "An exact target is required; no target was selected for this owner.",
                )
        target = self._coerce_target(value)
        if not self.policy.allows_target(target):
            raise MacOSBackendError(
                "target_not_allowed",
                "The selected bundle identifier is outside the configured native policy.",
            )
        return target

    def _inspect(self, target: TargetIdentity) -> WindowSnapshot:
        if not self.policy.allows_target(target):
            raise MacOSBackendError(
                "target_not_allowed",
                "The selected bundle identifier is outside the configured native policy.",
            )
        method = getattr(self.native, "inspect_target", None)
        if not callable(method):
            raise MacOSBackendError("native_unavailable", "The native adapter cannot inspect exact windows.")
        try:
            value = method(target)
        except MacOSBackendError:
            raise
        except Exception as error:
            raise MacOSBackendError("native_failure", "The native target inspection failed safely.") from error
        current = self._coerce_window(value, target)
        identity = current.identity
        if identity.bundle_id != target.bundle_id or identity.pid != target.pid or identity.window_id != target.window_id:
            raise MacOSBackendError(
                "target_changed",
                "The native response did not match the requested exact target identity.",
            )
        if target.process_start_token is not None and identity.process_start_token != target.process_start_token:
            raise MacOSBackendError(
                "target_replaced",
                "The selected process identity changed; select and observe the replacement explicitly.",
            )
        return current

    def _coerce_window(self, value: Any, requested: Optional[TargetIdentity]) -> WindowSnapshot:
        if isinstance(value, WindowSnapshot):
            return value
        if not isinstance(value, Mapping):
            raise MacOSBackendError("native_malformed", "The native window response was malformed.")
        identity_value = value.get("identity", value)
        try:
            identity = TargetIdentity.from_value(identity_value)
            geometry = WindowGeometry.from_value(
                value.get("geometry", value.get("bounds")), label="window geometry"
            )
            title = value.get("title", "")
            owner_name = value.get("owner_name", "")
            frontmost = value.get("frontmost", False)
            if title is None:
                title = ""
            if owner_name is None:
                owner_name = ""
            if not isinstance(title, str) or not isinstance(owner_name, str) or type(frontmost) is not bool:
                raise ValueError("native window text or focus fields are malformed")
            return WindowSnapshot(
                identity=identity,
                title=title,
                geometry=geometry,
                owner_name=owner_name,
                frontmost=frontmost,
            )
        except MacOSBackendError:
            raise
        except (TypeError, ValueError) as error:
            raise MacOSBackendError("native_malformed", "The native window response was malformed.") from error

    def _observe_locked(self, requested: TargetIdentity, owner_key: Any) -> Observation:
        current = self._inspect(requested)
        tree_method = getattr(self.native, "accessibility_tree", None)
        if not callable(tree_method):
            raise MacOSBackendError("native_unavailable", "The native adapter cannot inspect Accessibility nodes.")
        try:
            raw_tree = tree_method(
                current.identity,
                max_nodes=self.policy.max_nodes,
                max_depth=self.policy.max_depth,
            )
        except MacOSBackendError:
            raise
        except Exception as error:
            raise MacOSBackendError("native_failure", "The native accessibility observation failed safely.") from error
        tree = self._coerce_tree(raw_tree)
        tree = self._validate_tree(tree, current)
        if not tree.nodes:
            raise MacOSBackendError(
                "accessibility_unavailable",
                "The selected window returned no bounded Accessibility nodes.",
            )
        if self._generation >= MAX_GENERATION:
            raise MacOSBackendError(
                "observation_generation_exhausted",
                "The bounded observation generation space is exhausted; restart the backend safely.",
            )
        self._generation += 1
        observation = Observation(
            generation=self._generation,
            target=current.identity,
            window=current,
            accessibility=tree,
        )
        self._observations[(owner_key, observation.target.key)] = observation
        return observation

    def _coerce_tree(self, value: Any) -> AccessibilityTree:
        if isinstance(value, AccessibilityTree):
            return value
        if isinstance(value, Mapping):
            raw_nodes = value.get("nodes", ())
            truncated = value.get("truncated", False)
            if type(truncated) is not bool:
                raise MacOSBackendError("native_malformed", "The native accessibility truncation flag was malformed.")
        elif isinstance(value, Sequence) and not isinstance(value, (str, bytes, bytearray)):
            raw_nodes = value
            truncated = False
        else:
            raise MacOSBackendError("native_malformed", "The native accessibility response was malformed.")
        if raw_nodes is None:
            raw_nodes = ()
        if not isinstance(raw_nodes, Sequence) or isinstance(raw_nodes, (str, bytes, bytearray)):
            raise MacOSBackendError("native_malformed", "The native accessibility nodes were malformed.")
        nodes: list[AccessibilityNode] = []
        try:
            for index, raw in enumerate(raw_nodes):
                if index >= self.policy.max_nodes:
                    truncated = True
                    break
                try:
                    nodes.append(self._coerce_node(raw))
                except MacOSBackendError:
                    raise
                except Exception as error:
                    raise MacOSBackendError(
                        "native_malformed", "The native accessibility node was malformed."
                    ) from error
        except MacOSBackendError:
            raise
        except Exception as error:
            raise MacOSBackendError("native_malformed", "The native accessibility response was malformed.") from error
        try:
            return AccessibilityTree(tuple(nodes), truncated=truncated)
        except (TypeError, ValueError) as error:
            raise MacOSBackendError(
                "native_malformed", "The native accessibility tree was malformed."
            ) from error

    @staticmethod
    def _path_from_ref(ref: Any) -> Tuple[int, ...]:
        if not isinstance(ref, str) or not _REF_RE.fullmatch(ref):
            raise MacOSBackendError("native_malformed", "The native accessibility reference was malformed.")
        if ref == "ax:root":
            return ()
        try:
            parts = ref[3:].split(".")
            if len(ref.encode("utf-8")) > MAX_NODE_REF_BYTES or len(parts) > MAX_DEPTH:
                raise ValueError("reference is outside the bounded range")
            path = tuple(int(part) for part in parts)
            if any(index > 65_535 for index in path):
                raise ValueError("reference index is outside the bounded range")
            return path
        except (UnicodeError, TypeError, ValueError, OverflowError) as error:
            raise MacOSBackendError("native_malformed", "The native accessibility reference was malformed.") from error

    def _coerce_node(self, value: Any) -> AccessibilityNode:
        if isinstance(value, AccessibilityNode):
            return value
        if not isinstance(value, Mapping):
            raise MacOSBackendError("native_malformed", "The native accessibility node was malformed.")
        path_value = value.get("path")
        ref_value = value.get("ref")
        if path_value is None:
            path = self._path_from_ref(ref_value)
        else:
            if not isinstance(path_value, Sequence) or isinstance(path_value, (str, bytes, bytearray)):
                raise MacOSBackendError("native_malformed", "The native accessibility path was malformed.")
            try:
                path_parts = []
                for index in path_value:
                    if len(path_parts) >= MAX_DEPTH:
                        raise ValueError("path is too deep")
                    if type(index) is not int or not 0 <= index <= 65_535:
                        raise ValueError("path index is malformed")
                    path_parts.append(index)
                path = tuple(path_parts)
            except (TypeError, ValueError, OverflowError) as error:
                raise MacOSBackendError("native_malformed", "The native accessibility path was malformed.") from error
            if ref_value is not None and self._path_from_ref(ref_value) != path:
                raise MacOSBackendError("native_malformed", "The native accessibility reference did not match its path.")
        bounds_value = value.get("bounds", value.get("geometry"))
        bounds = None if bounds_value is None else WindowGeometry.from_value(bounds_value, label="node bounds")
        actions = value.get("actions", ())
        children = value.get("children", ())
        if not isinstance(actions, Sequence) or isinstance(actions, (str, bytes, bytearray)):
            raise MacOSBackendError("native_malformed", "The native accessibility actions were malformed.")
        if not isinstance(children, Sequence) or isinstance(children, (str, bytes, bytearray)):
            raise MacOSBackendError("native_malformed", "The native accessibility children were malformed.")
        try:
            action_values = []
            for index, item in enumerate(actions):
                if index >= MAX_ACTIONS or not isinstance(item, str):
                    raise ValueError("node action is malformed or exceeds its bounded limit")
                action_values.append(item)
            child_values = []
            for index, item in enumerate(children):
                if index >= MAX_CHILDREN or not isinstance(item, str):
                    raise ValueError("node child reference is malformed or exceeds its bounded limit")
                child_values.append(item)
            action_values_tuple = tuple(action_values)
            child_values_tuple = tuple(child_values)
            role = value.get("role", "AXUnknown")
            title = value.get("title", value.get("name", ""))
            description = value.get("description", "")
            if role is None:
                role = "AXUnknown"
            if title is None:
                title = ""
            if description is None:
                description = ""
            boolean_values = {
                "enabled": value.get("enabled", True),
                "focused": value.get("focused", False),
                "selected": value.get("selected", False),
                "editable": value.get("editable", False),
                "sensitive": value.get("sensitive", False),
            }
            if any(type(item) is not bool for item in boolean_values.values()):
                raise ValueError("node boolean field is malformed")
            return AccessibilityNode(
                path=path,
                role=role,
                title=title,
                description=description,
                bounds=bounds,
                actions=action_values_tuple,
                children=child_values_tuple,
                **boolean_values,
            )
        except (TypeError, ValueError) as error:
            raise MacOSBackendError("native_malformed", "The native accessibility node was malformed.") from error

    def _validate_tree(self, tree: AccessibilityTree, window: WindowSnapshot) -> AccessibilityTree:
        seen: set[str] = set()
        nodes: list[AccessibilityNode] = []
        for node in tree.nodes[: self.policy.max_nodes]:
            if node.ref in seen:
                raise MacOSBackendError("native_malformed", "The native accessibility tree contained duplicate references.")
            seen.add(node.ref)
            bounds = node.bounds
            if bounds is not None and not window.geometry.contains(bounds.center(), margin=4.0):
                # Keep the semantic node visible but remove geometry that is
                # not safe to use for input.
                bounds = None
            sensitive = is_sensitive_node(node)
            if bounds is not node.bounds or sensitive != node.sensitive:
                node = replace(node, bounds=bounds, sensitive=sensitive)
            nodes.append(node)
        return AccessibilityTree(tuple(nodes), truncated=tree.truncated or len(tree.nodes) > len(nodes))

    def _find_observation(
        self,
        value: Any,
        target: TargetIdentity,
        owner_key: Any,
    ) -> Observation:
        if isinstance(value, Observation):
            generation = value.generation
            observed_target = value.target
        elif isinstance(value, Mapping):
            generation = value.get("generation")
            try:
                observed_target = TargetIdentity.from_value(value.get("target"))
            except (MacOSBackendError, TypeError, ValueError) as error:
                raise MacOSBackendError("observation_invalid", "The observation target is malformed.") from error
        else:
            raise MacOSBackendError("observation_required", "Every native input action requires a fresh observation.")
        if (
            type(generation) is not int
            or not 1 <= generation <= MAX_GENERATION
        ):
            raise MacOSBackendError("observation_invalid", "The observation generation is malformed.")
        for (candidate_owner, _), observation in self._observations.items():
            if candidate_owner != owner_key or observation.generation != generation:
                continue
            if self._identities_compatible(observation.target, observed_target) and self._identities_compatible(observation.target, target):
                return observation
        raise MacOSBackendError(
            "observation_stale",
            "The observation is unknown, owned by another caller, or no longer current; observe again.",
        )

    @staticmethod
    def _identities_compatible(left: TargetIdentity, right: TargetIdentity) -> bool:
        if (
            left.bundle_id != right.bundle_id
            or left.pid != right.pid
            or left.window_id != right.window_id
        ):
            return False
        # ``right`` is the caller-supplied constraint.  A missing constraint is
        # compatible with a stronger stored observation, but a supplied token
        # must never be accepted when the stored observation cannot prove it.
        return right.process_start_token is None or left.process_start_token == right.process_start_token

    def _prepare_action(
        self,
        target: Any,
        observation: Any,
        owner: Any,
        *,
        ref: Optional[str],
        operation: str,
        cancellation: Any,
        require_node: bool = True,
    ) -> Tuple[Observation, WindowSnapshot, Optional[AccessibilityNode]]:
        self._require_ready(operation)
        owner_key = self._owner_key(owner)
        requested = self._target_for(target, owner_key)
        stored = self._find_observation(observation, requested, owner_key)
        current = self._inspect(stored.target)
        self._assert_window_unchanged(stored.window, current)
        if self.policy.require_foreground and not current.frontmost:
            raise MacOSBackendError(
                "foreground_required",
                "The exact target is not frontmost; foreground operation requires explicit host focus management.",
            )
        self._check_cancel(cancellation)
        node: Optional[AccessibilityNode] = None
        if ref is not None:
            if not isinstance(ref, str) or not _REF_RE.fullmatch(ref):
                raise MacOSBackendError("invalid_node", "The accessibility reference is malformed.")
            try:
                self._path_from_ref(ref)
            except MacOSBackendError as error:
                raise MacOSBackendError("invalid_node", "The accessibility reference is malformed.") from error
            node = stored.accessibility.by_ref(ref)
            if node is None:
                raise MacOSBackendError("stale_node", "The observed accessibility node is not present; observe again.")
            if not node.enabled:
                raise MacOSBackendError("node_disabled", "The observed accessibility node is disabled.")
            if is_sensitive_node(node):
                raise MacOSBackendError(
                    "sensitive_input_refused",
                    "Input actions on credential or protected controls are refused.",
                )
        elif require_node:
            raise MacOSBackendError("node_required", "This native action requires an observed accessibility node.")
        return stored, current, node

    def _revalidate_after_confirmation(self, context: Any) -> None:
        self._require_ready("input")
        current = self._inspect(context[0].target)
        self._assert_window_unchanged(context[1], current)
        if self.policy.require_foreground and not current.frontmost:
            raise MacOSBackendError("foreground_required", "The selected target lost foreground.")

        method = getattr(self.native, "accessibility_tree", None)
        if not callable(method):
            raise MacOSBackendError("native_unavailable", "Fresh Accessibility evidence is unavailable.")
        try:
            tree = self._coerce_tree(method(current.identity, max_nodes=self.policy.max_nodes,
                                           max_depth=self.policy.max_depth))
            tree = self._validate_tree(tree, current)
        except Exception as error:
            raise MacOSBackendError("observation_stale", "Accessibility revalidation failed.") from error
        if tree != context[0].accessibility:
            raise MacOSBackendError("observation_stale", "Accessibility focus or contents changed during approval.")

    @staticmethod
    def _assert_window_unchanged(observed: WindowSnapshot, current: WindowSnapshot) -> None:
        if observed.identity.bundle_id != current.identity.bundle_id or observed.identity.pid != current.identity.pid or observed.identity.window_id != current.identity.window_id:
            raise MacOSBackendError("target_replaced", "The observed application process or window was replaced.")
        if (
            observed.identity.process_start_token != current.identity.process_start_token
            and (
                observed.identity.process_start_token is not None
                or current.identity.process_start_token is not None
            )
        ):
            raise MacOSBackendError("target_replaced", "The observed process identity changed; observe again.")
        if not observed.geometry.approximately_equals(current.geometry, tolerance=0.5):
            raise MacOSBackendError("window_changed", "The observed window geometry changed; observe again before input.")

    @staticmethod
    def _node_point(node: AccessibilityNode, window_geometry: WindowGeometry) -> Point:
        if node.bounds is None:
            raise MacOSBackendError(
                "geometry_unavailable",
                "The observed node has no safe bounded geometry for input.",
            )
        point = node.bounds.center()
        if not window_geometry.contains(point, margin=0.5):
            raise MacOSBackendError(
                "geometry_outside_target",
                "The observed node center is outside the exact selected window.",
            )
        return point

    def _confirm(
        self,
        confirmation: Any,
        operation: str,
        target: TargetIdentity,
        ref: Optional[str],
    ) -> None:
        if not self.policy.require_confirmation:
            return
        request = ConfirmationRequest(operation=operation, target=target, ref=ref)
        if confirmation is True:
            return
        if not callable(confirmation):
            raise MacOSBackendError(
                "confirmation_required",
                "Every native input action requires an explicit confirmation callback.",
            )
        try:
            accepted = confirmation(request)
        except Exception as error:
            raise MacOSBackendError(
                "confirmation_denied",
                "The confirmation callback did not approve the native action.",
            ) from error
        if type(accepted) is not bool or not accepted:
            raise MacOSBackendError(
                "confirmation_denied",
                "The confirmation callback denied the native action.",
            )

    def _after_action_locked(
        self,
        target: TargetIdentity,
        previous: Observation,
        operation: str,
        owner: Any,
        cancellation: Any,
    ) -> dict[str, Any]:
        self._check_cancel(cancellation)
        owner_key = self._owner_key(owner)
        try:
            fresh = self._observe_locked(target, owner_key)
        except MacOSBackendError as error:
            self._safe_release_all()
            raise MacOSBackendError(
                "action_unverified",
                "The input was dispatched but fresh native observation failed; input was stopped.",
            ) from error
        return {
            "schema": "octet.macos.action-result.v1",
            "operation": operation,
            "state": "dispatched_and_reobserved",
            "target": fresh.target.as_dict(),
            "previous_generation": previous.generation,
            "observation": fresh.as_dict(),
        }

    def _capture_locked(
        self,
        target: TargetIdentity,
        observation: Optional[Observation],
        owner_key: Any,
    ) -> bytes:
        del owner_key
        current = self._inspect(target)
        if observation is not None:
            self._assert_window_unchanged(observation.window, current)
        method = getattr(self.native, "capture_window", None)
        if not callable(method):
            method = getattr(self.native, "screenshot", None)
        if not callable(method):
            raise MacOSBackendError("native_unavailable", "The native adapter cannot capture windows.")
        try:
            value = method(target, max_bytes=self.policy.max_screenshot_bytes)
        except MacOSBackendError:
            raise
        except Exception as error:
            raise MacOSBackendError("native_failure", "The native screenshot failed safely.") from error
        if not isinstance(value, (bytes, bytearray)) or not value:
            raise MacOSBackendError("capture_malformed", "The native screenshot response was malformed.")
        if len(value) > self.policy.max_screenshot_bytes:
            raise MacOSBackendError("capture_too_large", "The native screenshot exceeded the bounded byte limit.")
        return bytes(value)

    def _native_cancellation(self, cancellation: Any) -> Callable[[], bool]:
        """Return a native-loop callback that also observes trusted stop state."""

        def is_cancelled() -> bool:
            if self._stop_requested.is_set():
                return True
            if cancellation is None:
                return False
            if callable(cancellation):
                value = cancellation()
            elif callable(getattr(cancellation, "is_cancelled", None)):
                value = cancellation.is_cancelled()
            else:
                value = getattr(cancellation, "cancelled", False)
            if type(value) is not bool:
                raise ValueError("cancellation state is not a boolean")
            return value

        return is_cancelled

    def _check_cancel(self, cancellation: Any) -> None:
        if self._stop_requested.is_set():
            self._safe_release_all()
            raise MacOSBackendError("cancelled", "The native operation was cancelled safely.")
        if cancellation is None:
            return
        try:
            if callable(cancellation):
                cancelled = cancellation()
            elif callable(getattr(cancellation, "is_cancelled", None)):
                cancelled = cancellation.is_cancelled()
            else:
                cancelled = getattr(cancellation, "cancelled", False)
            if type(cancelled) is not bool:
                raise ValueError("cancellation state is not a boolean")
        except Exception as error:
            self._safe_release_all()
            raise MacOSBackendError("cancelled", "The cancellation state could not be trusted; input was stopped.") from error
        if cancelled:
            self._safe_release_all()
            raise MacOSBackendError("cancelled", "The native operation was cancelled safely.")

    def _safe_release_all(self) -> bool:
        method = getattr(self.native, "release_all", None)
        if not callable(method):
            method = getattr(self.native, "stop", None)
        if callable(method):
            try:
                return method() is True
            except Exception:
                # Releasing is best effort here; the public operation remains
                # failed closed and the native adapter owns its final cleanup.
                return False
        return False


# Public compatibility names are aliases, not separate implementations.
MacOSAutomationBackend = MacOSBackend
NativeMacOSBackend = MacOSBackend

__all__ = [
    "MacOSAutomationBackend",
    "MacOSBackend",
    "MacOSBackendError",
    "NativeMacOSBackend",
    "AccessibilityNode",
    "AccessibilityTree",
    "ConfirmationRequest",
    "MacOSPolicy",
    "Observation",
    "PermissionReport",
    "Point",
    "ResourceOwner",
    "TargetIdentity",
    "WindowGeometry",
    "WindowSnapshot",
]
