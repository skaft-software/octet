"""Owner-fenced, bounded computer-use lifecycle primitives.

This module intentionally contains no browser, desktop, subprocess, or network
implementation.  A host-owned adapter is injected at the boundary.  The
lifecycle is the part that makes an adapter safe to call repeatedly: it owns
nothing outside one host resource owner, requires a fresh observation before
an input action, serializes input, and records an ambiguous effect instead of
retrying it.

The public objects are deliberately dependency-free so that they can be used
by both a typed extension transport and the contained model-code transport.
The native/browser siblings provide the adapter and the host policy; they do
not get to bypass these checks.
"""

from __future__ import annotations

import hashlib
import inspect
import json
import math
import queue
import threading
import time
import uuid
from collections import OrderedDict
from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Callable, Dict, List, Mapping, Optional, Protocol, Sequence, Tuple, Union


# These are protocol limits, not suggestions.  A host can choose smaller
# values, but may not construct an unbounded lifecycle through this module.
MAX_ID_CHARS = 256
MAX_OBSERVATION_ID_CHARS = 256
MAX_ACTION_ID_CHARS = 256
MAX_GROUP_ID_CHARS = 256
MAX_ACTIONS_PER_GROUP = 8
MAX_TOTAL_ACTIONS = 256
MAX_HISTORY = 32
MAX_OUTPUT_CHARS = 64 * 1024
MAX_CAPTURE_BYTES = 32 * 1024 * 1024
MAX_STRUCTURED_ITEMS = 128
MAX_STRUCTURED_DEPTH = 6
DEFAULT_OPERATION_SECONDS = 10.0
DEFAULT_STOP_SECONDS = 2.0
MAX_DIMENSION = 100_000
MAX_SCALE = 16.0
MAX_COORDINATE = 100_000.0


class LifecycleError(RuntimeError):
    """A bounded, model-safe lifecycle error.

    ``owner`` is optional only while parsing a malformed request.  Every
    operation result produced after owner parsing includes the host owner in
    its envelope, including errors.
    """

    def __init__(
        self,
        code: str,
        message: str,
        *,
        owner: Optional["OwnerIdentity"] = None,
        details: Optional[Mapping[str, Any]] = None,
    ) -> None:
        self.code = bounded_text(code, 96)
        self.message = bounded_text(message, 2048)
        self.owner = owner
        self.details = _bounded_value(details or {}, max_depth=3, max_items=24)
        super().__init__(self.message)

    def as_dict(self, owner: Optional["OwnerIdentity"] = None) -> Dict[str, Any]:
        selected = owner or self.owner
        result: Dict[str, Any] = {
            "code": self.code,
            "message": self.message,
        }
        if selected is not None:
            result["owner"] = selected.as_dict()
        if self.details:
            result["details"] = dict(self.details)
        return result


class AdapterActionError(LifecycleError):
    """An adapter failure with an explicit acknowledgement boundary."""

    def __init__(
        self,
        code: str,
        message: str,
        *,
        may_have_effect: bool = True,
        owner: Optional["OwnerIdentity"] = None,
    ) -> None:
        self.may_have_effect = bool(may_have_effect)
        super().__init__(code, message, owner=owner)


class BudgetExceeded(LifecycleError):
    """A persistent finite lifecycle budget was exhausted."""


class InputBusy(LifecycleError):
    """Another owner currently holds the single input lease."""


class _BoundedCallInterrupted(Exception):
    def __init__(self, reason: str) -> None:
        self.reason = reason
        super().__init__(reason)


class SessionState(str, Enum):
    ACTIVE = "active"
    PAUSED = "paused"
    AWAITING_APPROVAL = "awaiting-approval"
    STOPPING = "stopping"
    DEGRADED = "degraded"


# A shorter name is useful to integrations which call this a lifecycle state.
LifecycleState = SessionState


class ActionOutcome(str, Enum):
    COMMITTED = "committed"
    DENIED = "denied"
    APPROVAL_REQUIRED = "approval_required"
    FAILED = "failed"
    CANCELLED = "cancelled"
    UNKNOWN_EFFECT = "unknown_effect"
    STALE_OBSERVATION = "stale_observation"
    TARGET_CHANGED = "target_changed"
    BUDGET_EXHAUSTED = "budget_exhausted"
    INPUT_BUSY = "input_busy"


class EffectState(str, Enum):
    NONE = "none"
    COMMITTED = "committed"
    UNKNOWN = "unknown"


# ---------------------------------------------------------------------------
# Boundary values


def bounded_text(value: Any, maximum: int) -> str:
    """Return at most ``maximum`` UTF-8 bytes of text, without splitting text."""

    if value is None:
        return ""
    text = value if isinstance(value, str) else str(value)
    if len(text.encode("utf-8", errors="replace")) <= maximum:
        return text
    result = text
    while result and len(result.encode("utf-8", errors="replace")) > maximum:
        result = result[:-1]
    return result


def _require_id(value: Any, field_name: str, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise LifecycleError("invalid_" + field_name, field_name + " must be text.")
    if not allow_empty and not value:
        raise LifecycleError("invalid_" + field_name, field_name + " must not be empty.")
    if len(value.encode("utf-8")) > MAX_ID_CHARS:
        raise LifecycleError("invalid_" + field_name, field_name + " is too long.")
    if any(ord(character) < 32 or ord(character) == 127 for character in value):
        raise LifecycleError("invalid_" + field_name, field_name + " contains a control character.")
    return value


def _require_generation(value: Any, field_name: str = "generation") -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 0 or value > (2**64 - 1):
        raise LifecycleError("invalid_" + field_name, field_name + " must be a bounded non-negative integer.")
    return value


def _finite_number(value: Any, field_name: str, *, minimum: float, maximum: float) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise LifecycleError("invalid_" + field_name, field_name + " must be a number.")
    result = float(value)
    if not math.isfinite(result) or result < minimum or result > maximum:
        raise LifecycleError("invalid_" + field_name, field_name + " is outside its finite bound.")
    return result


def _bounded_value(value: Any, *, max_depth: int = MAX_STRUCTURED_DEPTH, max_items: int = MAX_STRUCTURED_ITEMS) -> Any:
    """Copy a result into a bounded JSON-like value.

    Raw image bytes, file objects, handles, and arbitrary host objects are not
    values in the lifecycle protocol.  They are represented by an opaque
    screenshot reference or rejected at the adapter boundary.
    """

    if max_depth < 0:
        return "[depth limit]"
    if value is None or isinstance(value, (str, bool, int, float)):
        if isinstance(value, float) and not math.isfinite(value):
            return "[non-finite]"
        if isinstance(value, str):
            return bounded_text(value, MAX_OUTPUT_CHARS)
        return value
    if isinstance(value, (bytes, bytearray, memoryview)):
        raise LifecycleError("raw_binary_result", "Binary results must use a durable screenshot reference.")
    if isinstance(value, Mapping):
        result: Dict[str, Any] = {}
        for index, (key, item) in enumerate(value.items()):
            if index >= max_items:
                result["[items truncated]"] = True
                break
            safe_key = bounded_text(key, 128)
            result[safe_key] = _bounded_value(item, max_depth=max_depth - 1, max_items=max_items)
        return result
    if isinstance(value, (list, tuple)):
        return [
            _bounded_value(item, max_depth=max_depth - 1, max_items=max_items)
            for item in list(value)[:max_items]
        ]
    raise LifecycleError("host_object_result", "Adapter results may not contain host objects.")


def _stable_digest(value: Any) -> str:
    encoded = json.dumps(
        _bounded_value(value, max_depth=MAX_STRUCTURED_DEPTH, max_items=MAX_STRUCTURED_ITEMS),
        sort_keys=True,
        separators=(",", ":"),
        ensure_ascii=False,
    ).encode("utf-8")
    return hashlib.sha256(encoded).hexdigest()


def _digest(value: Any, field_name: str = "digest") -> str:
    result = _require_id(value, field_name)
    if len(result) > 256:
        raise LifecycleError("invalid_" + field_name, field_name + " is too long.")
    return result


def _callable_kwargs(
    function: Callable[..., Any],
    kwargs: Mapping[str, Any],
    *,
    positional: Sequence[Any] = (),
) -> Dict[str, Any]:
    """Pass only supported keyword arguments to simple fixture adapters.

    Native adapters are expected to accept the full cancellation/timeout
    contract.  Filtering is only a compatibility aid for deterministic
    fixtures and never changes positional arguments.  When a parameter is
    already occupied by a positional argument it is removed from the keyword
    set; this matters for adapters with ``**kwargs`` and avoids Python's
    duplicate-argument error at the boundary.
    """

    try:
        signature = inspect.signature(function)
    except (TypeError, ValueError):
        return dict(kwargs)
    parameters = list(signature.parameters.values())
    positional_names = [
        parameter.name
        for parameter in parameters
        if parameter.kind in (inspect.Parameter.POSITIONAL_ONLY, inspect.Parameter.POSITIONAL_OR_KEYWORD)
    ]
    occupied = set(positional_names[: len(positional)])
    accepts_var_keyword = any(parameter.kind == inspect.Parameter.VAR_KEYWORD for parameter in parameters)
    if accepts_var_keyword:
        return {key: value for key, value in kwargs.items() if key not in occupied}
    accepted = {
        parameter.name
        for parameter in parameters
        if parameter.kind in (inspect.Parameter.POSITIONAL_OR_KEYWORD, inspect.Parameter.KEYWORD_ONLY)
    }
    return {
        key: value
        for key, value in kwargs.items()
        if key in accepted and key not in occupied
    }


def _call_with_contract(
    function: Callable[..., Any],
    positional: Sequence[Any],
    keyword: Mapping[str, Any],
) -> Any:
    return function(*positional, **_callable_kwargs(function, keyword, positional=positional))


def bounded_call(
    function: Callable[[], Any],
    *,
    token: "CancellationToken",
    timeout: Optional[float],
    abort: Optional[Callable[[], Any]] = None,
    operation: str = "helper operation",
) -> Any:
    """Run one adapter call with a cancellation/deadline observation.

    The adapter contract remains cooperative: its methods receive the token
    and timeout.  The watcher prevents a non-cooperative fixture or helper
    from blocking the trusted lifecycle forever.  If the call was entered but
    did not acknowledge completion, callers must record an unknown effect and
    must not replay it.  The orphaned watcher is daemonized and a healthy
    native helper is still required to implement ``abort``/``interrupt``.
    """

    token.throw_if_cancelled()
    remaining = token.remaining()
    requested = DEFAULT_OPERATION_SECONDS if timeout is None else float(timeout)
    if not math.isfinite(requested) or requested <= 0:
        raise LifecycleError("invalid_timeout", operation + " timeout must be finite and positive.")
    if remaining is not None:
        requested = min(requested, remaining)
    if requested <= 0:
        token.cancel("deadline_exceeded")
        raise _BoundedCallInterrupted("deadline_exceeded")

    completed = threading.Event()
    values: "queue.Queue[Tuple[str, Any]]" = queue.Queue(maxsize=1)

    def invoke() -> None:
        try:
            values.put(("ok", function()), block=False)
        except BaseException as error:  # delivered to the owning lifecycle below
            try:
                values.put(("error", error), block=False)
            except queue.Full:
                pass
        finally:
            completed.set()

    worker = threading.Thread(target=invoke, name="octet-computer-use-call", daemon=True)
    worker.start()
    deadline = token.clock() + requested
    while True:
        now = token.clock()
        if completed.wait(timeout=min(0.025, max(0.0, deadline - now))):
            kind, value = values.get()
            if kind == "error":
                raise value
            return value
        if token.cancelled:
            if abort is not None:
                _best_effort_abort(abort)
            raise _BoundedCallInterrupted(token.reason or "cancelled")
        if token.clock() >= deadline:
            token.cancel("deadline_exceeded")
            if abort is not None:
                _best_effort_abort(abort)
            raise _BoundedCallInterrupted("deadline_exceeded")


def _best_effort_abort(function: Callable[[], Any]) -> None:
    def invoke() -> None:
        try:
            function()
        except BaseException:
            pass

    threading.Thread(target=invoke, name="octet-computer-use-abort", daemon=True).start()


class CancellationToken:
    """A host-cancellable token with a monotonic deadline and parent token."""

    def __init__(
        self,
        *,
        deadline: Optional[float] = None,
        parent: Optional["CancellationToken"] = None,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if deadline is not None and (not isinstance(deadline, (int, float)) or not math.isfinite(float(deadline))):
            raise ValueError("deadline must be finite")
        self.deadline = float(deadline) if deadline is not None else None
        self.parent = parent
        self.clock = clock
        self._event = threading.Event()
        self._lock = threading.Lock()
        self._reason: Optional[str] = None

    @property
    def cancelled(self) -> bool:
        if self._event.is_set():
            return True
        if self.parent is not None and self.parent.cancelled:
            return True
        if self.deadline is not None and self.clock() >= self.deadline:
            self.cancel("deadline_exceeded")
            return True
        return False

    @property
    def is_cancelled(self) -> bool:
        return self.cancelled

    @property
    def reason(self) -> Optional[str]:
        if self._reason:
            return self._reason
        if self.parent is not None:
            return self.parent.reason
        if self.cancelled and self._reason:
            return self._reason
        return None

    def cancel(self, reason: str = "cancelled") -> None:
        reason = bounded_text(reason or "cancelled", 96)
        with self._lock:
            if self._reason is None:
                self._reason = reason
            self._event.set()

    def remaining(self) -> Optional[float]:
        if self.cancelled:
            return 0.0
        candidates: List[float] = []
        if self.deadline is not None:
            candidates.append(max(0.0, self.deadline - self.clock()))
        if self.parent is not None:
            parent_remaining = self.parent.remaining()
            if parent_remaining is not None:
                candidates.append(parent_remaining)
        return min(candidates) if candidates else None

    def throw_if_cancelled(self) -> None:
        if self.cancelled:
            code = "deadline_exceeded" if self.reason == "deadline_exceeded" else "cancelled"
            raise LifecycleError(code, "The computer-use operation was cancelled.")

    def wait(self, timeout: Optional[float] = None) -> bool:
        end = None if timeout is None else time.monotonic() + max(0.0, float(timeout))
        while not self.cancelled:
            remaining = self.remaining()
            wait_for = 0.025
            if remaining is not None:
                wait_for = min(wait_for, remaining)
            if end is not None:
                wait_for = min(wait_for, max(0.0, end - time.monotonic()))
            if wait_for <= 0:
                break
            if self._event.wait(wait_for):
                break
            if end is not None and time.monotonic() >= end:
                break
        return self.cancelled

    def child(self, timeout: Optional[float] = None) -> "CancellationToken":
        deadline = None
        if timeout is not None:
            requested = float(timeout)
            if not math.isfinite(requested) or requested <= 0:
                raise ValueError("child timeout must be finite and positive")
            deadline = self.clock() + requested
        if self.deadline is not None:
            deadline = self.deadline if deadline is None else min(deadline, self.deadline)
        return CancellationToken(deadline=deadline, parent=self, clock=self.clock)


@dataclass(frozen=True)
class OwnerIdentity:
    """The host-derived owner and extension process-generation fence."""

    session_id: str
    extension_instance_id: str
    process_generation: int

    def __post_init__(self) -> None:
        _require_id(self.session_id, "session_id")
        _require_id(self.extension_instance_id, "extension_instance_id")
        _require_generation(self.process_generation, "process_generation")
        if self.process_generation < 1:
            raise LifecycleError("invalid_process_generation", "process_generation must be at least one.")

    @property
    def extension_generation(self) -> int:
        return self.process_generation

    @property
    def key(self) -> Tuple[str, str, int]:
        return (self.session_id, self.extension_instance_id, self.process_generation)

    @classmethod
    def from_context(cls, context: Mapping[str, Any]) -> "OwnerIdentity":
        if isinstance(context, OwnerIdentity):
            return context
        value: Any = context.get("resource_owner") if isinstance(context, Mapping) else None
        if value is None:
            value = context
        if not isinstance(value, Mapping):
            raise LifecycleError("owner_unavailable", "A host-derived resource owner is required.")
        try:
            return cls(
                session_id=value.get("session_id"),
                extension_instance_id=value.get("extension_instance_id"),
                process_generation=value.get("process_generation", value.get("extension_generation")),
            )
        except LifecycleError as error:
            raise LifecycleError("owner_unavailable", "The host resource owner is invalid.") from error
        except (TypeError, ValueError) as error:
            raise LifecycleError("owner_unavailable", "The host resource owner is invalid.") from error

    @classmethod
    def from_value(cls, value: Any) -> "OwnerIdentity":
        if isinstance(value, OwnerIdentity):
            return value
        if isinstance(value, Mapping):
            return cls.from_context(value)
        raise LifecycleError("owner_unavailable", "A host-derived resource owner is required.")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "session_id": self.session_id,
            "extension_instance_id": self.extension_instance_id,
            "process_generation": self.process_generation,
        }


# Names used by the adjacent extension code and by host handoff documents.
ResourceOwner = OwnerIdentity
OwnerFence = OwnerIdentity


@dataclass(frozen=True)
class TargetIdentity:
    """Opaque app/window/tab/frame handles selected by the host."""

    app_id: str
    window_id: str
    tab_id: Optional[str] = None
    frame_generation: int = 0
    target_id: Optional[str] = None

    def __post_init__(self) -> None:
        _require_id(self.app_id, "app_id")
        _require_id(self.window_id, "window_id")
        if self.tab_id is not None:
            _require_id(self.tab_id, "tab_id")
        if self.target_id is not None:
            _require_id(self.target_id, "target_id")
        _require_generation(self.frame_generation, "frame_generation")

    @classmethod
    def from_value(cls, value: Any) -> "TargetIdentity":
        if isinstance(value, TargetIdentity):
            return value
        if not isinstance(value, Mapping):
            raise LifecycleError("invalid_target", "A target must include opaque app and window handles.")
        try:
            return cls(
                app_id=value.get("app_id", value.get("app")),
                window_id=value.get("window_id", value.get("window")),
                tab_id=value.get("tab_id", value.get("tab")),
                frame_generation=value.get("frame_generation", value.get("frame", 0)),
                target_id=value.get("target_id", value.get("target")),
            )
        except LifecycleError:
            raise
        except (TypeError, ValueError) as error:
            raise LifecycleError("invalid_target", "The target identity is invalid.") from error

    def same_surface(self, other: "TargetIdentity") -> bool:
        return (
            self.app_id == other.app_id
            and self.window_id == other.window_id
            and self.tab_id == other.tab_id
            and self.target_id == other.target_id
        )

    def as_dict(self) -> Dict[str, Any]:
        return {
            "app_id": self.app_id,
            "window_id": self.window_id,
            "tab_id": self.tab_id,
            "target_id": self.target_id,
            "frame_generation": self.frame_generation,
        }


TargetBinding = TargetIdentity


def _normalise_transform(value: Any) -> Tuple[float, float, float, float, float, float]:
    if isinstance(value, Mapping):
        values = [value.get(key) for key in ("a", "b", "c", "d", "e", "f")]
    elif isinstance(value, (list, tuple)) and len(value) == 6:
        values = list(value)
    else:
        raise LifecycleError(
            "invalid_transform",
            "An observation must include a six-value viewport transform.",
        )
    return tuple(
        _finite_number(item, "transform", minimum=-1_000_000.0, maximum=1_000_000.0)
        for item in values
    )  # type: ignore[return-value]


@dataclass(frozen=True)
class ScreenshotReference:
    """A durable host artifact reference; image bytes never enter this module."""

    artifact_id: str
    owner: OwnerIdentity
    digest: str
    width: int
    height: int
    size_bytes: int = 0

    def __post_init__(self) -> None:
        _require_id(self.artifact_id, "artifact_id")
        _digest(self.digest)
        if not isinstance(self.width, int) or isinstance(self.width, bool) or not 1 <= self.width <= MAX_DIMENSION:
            raise LifecycleError("invalid_screenshot", "Screenshot width is outside its bound.")
        if not isinstance(self.height, int) or isinstance(self.height, bool) or not 1 <= self.height <= MAX_DIMENSION:
            raise LifecycleError("invalid_screenshot", "Screenshot height is outside its bound.")
        if not isinstance(self.size_bytes, int) or isinstance(self.size_bytes, bool) or not 0 <= self.size_bytes <= MAX_CAPTURE_BYTES:
            raise LifecycleError("invalid_screenshot", "Screenshot size is outside its bound.")

    def as_dict(self) -> Dict[str, Any]:
        return {
            "artifact_id": self.artifact_id,
            "owner": self.owner.as_dict(),
            "digest": self.digest,
            "width": self.width,
            "height": self.height,
            "size_bytes": self.size_bytes,
        }


@dataclass(frozen=True)
class TargetObservation:
    """One fresh selected-target capture and its geometry fence."""

    owner: OwnerIdentity
    target: TargetIdentity
    observation_id: str
    sequence: int
    width: int
    height: int
    scale: float
    transform: Tuple[float, float, float, float, float, float]
    digest: str
    capture_kind: str = "structured"
    screenshot: Optional[ScreenshotReference] = None
    structured: Optional[Mapping[str, Any]] = None
    capture_bytes: int = 0
    observed_at: float = 0.0

    def __post_init__(self) -> None:
        _require_id(self.observation_id, "observation_id")
        if not isinstance(self.sequence, int) or isinstance(self.sequence, bool) or self.sequence < 1:
            raise LifecycleError("invalid_observation", "Observation sequence must be positive.")
        if not isinstance(self.width, int) or isinstance(self.width, bool) or not 1 <= self.width <= MAX_DIMENSION:
            raise LifecycleError("invalid_dimensions", "Observation width is outside its bound.")
        if not isinstance(self.height, int) or isinstance(self.height, bool) or not 1 <= self.height <= MAX_DIMENSION:
            raise LifecycleError("invalid_dimensions", "Observation height is outside its bound.")
        _finite_number(self.scale, "scale", minimum=0.0001, maximum=MAX_SCALE)
        if len(self.transform) != 6:
            raise LifecycleError("invalid_transform", "Observation transform must have six values.")
        _digest(self.digest)
        if self.capture_kind not in {"screenshot", "structured", "screenshot+structured"}:
            raise LifecycleError("invalid_capture", "Observation capture kind is unsupported.")
        if self.screenshot is not None and self.screenshot.owner != self.owner:
            raise LifecycleError("owner_mismatch", "Screenshot owner does not match the observation owner.")
        if self.structured is not None:
            _bounded_value(self.structured)
        if not isinstance(self.capture_bytes, int) or isinstance(self.capture_bytes, bool) or not 0 <= self.capture_bytes <= MAX_CAPTURE_BYTES:
            raise LifecycleError("invalid_capture", "Observation capture size is outside its bound.")

    @property
    def dimensions(self) -> Tuple[int, int]:
        return (self.width, self.height)

    def as_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "owner": self.owner.as_dict(),
            "target": self.target.as_dict(),
            "observation_id": self.observation_id,
            "sequence": self.sequence,
            "dimensions": {"width": self.width, "height": self.height},
            "scale": self.scale,
            "transform": list(self.transform),
            "digest": self.digest,
            "capture_kind": self.capture_kind,
            "fresh": True,
        }
        if self.screenshot is not None:
            result["screenshot"] = self.screenshot.as_dict()
        if self.structured is not None:
            result["structured"] = dict(_bounded_value(self.structured))
        return result


Observation = TargetObservation


def _screenshot_from_value(value: Any, owner: OwnerIdentity) -> Optional[ScreenshotReference]:
    if value is None:
        return None
    if isinstance(value, ScreenshotReference):
        if value.owner != owner:
            raise LifecycleError("owner_mismatch", "Screenshot reference has a different owner.")
        return value
    if not isinstance(value, Mapping):
        raise LifecycleError("invalid_screenshot", "Screenshot must be a durable artifact reference.")
    supplied_owner = value.get("owner")
    if supplied_owner is not None and OwnerIdentity.from_value(supplied_owner) != owner:
        raise LifecycleError("owner_mismatch", "Screenshot reference has a different owner.")
    return ScreenshotReference(
        artifact_id=value.get("artifact_id", value.get("id")),
        owner=owner,
        digest=value.get("digest"),
        width=value.get("width"),
        height=value.get("height"),
        size_bytes=value.get("size_bytes", value.get("bytes", 0)),
    )


# ---------------------------------------------------------------------------
# Typed actions and policy

TYPED_ACTIONS = frozenset({"click", "double_click", "drag", "move", "press", "scroll", "type", "wait"})
_FORBIDDEN_ACTIONS = frozenset(
    {"evaluate", "run_code", "execute_code", "shell", "subprocess", "script", "javascript", "python"}
)


@dataclass(frozen=True)
class ActionSpec:
    """A bounded typed action bound to one previously returned observation."""

    kind: str
    target: TargetIdentity
    observation_id: str
    parameters: Mapping[str, Any] = field(default_factory=dict)
    action_id: str = ""
    expected_digest: Optional[str] = None

    def __post_init__(self) -> None:
        if not isinstance(self.kind, str):
            raise LifecycleError("invalid_action", "Action kind must be text.")
        kind = self.kind.strip().lower()
        if kind in _FORBIDDEN_ACTIONS or kind not in TYPED_ACTIONS:
            raise LifecycleError("unsupported_action", "Only bounded typed computer-use actions are available.")
        object.__setattr__(self, "kind", kind)
        if not isinstance(self.target, TargetIdentity):
            object.__setattr__(self, "target", TargetIdentity.from_value(self.target))
        _require_id(self.observation_id, "observation_id")
        action_id = self.action_id or "action-" + uuid.uuid4().hex
        _require_id(action_id, "action_id")
        object.__setattr__(self, "action_id", action_id)
        safe_parameters = _bounded_value(self.parameters, max_depth=4, max_items=24)
        if not isinstance(safe_parameters, Mapping):
            raise LifecycleError("invalid_action", "Action parameters must be an object.")
        object.__setattr__(self, "parameters", dict(safe_parameters))
        if self.expected_digest is not None:
            _digest(self.expected_digest, "expected_digest")
        _validate_action_parameters(kind, self.parameters)

    @classmethod
    def from_value(cls, value: Any) -> "ActionSpec":
        if isinstance(value, ActionSpec):
            return value
        if not isinstance(value, Mapping):
            raise LifecycleError("invalid_action", "Each action must be an object.")
        kind = value.get("kind", value.get("type"))
        target = value.get("target")
        observation_id = value.get("observation_id", value.get("observation"))
        parameters = value.get("parameters")
        if parameters is None:
            direct_keys = {
                "button",
                "coordinates",
                "duration_ms",
                "key",
                "text",
                "x",
                "y",
                "delta_x",
                "delta_y",
            }
            parameters = {key: value[key] for key in direct_keys if key in value}
        allowed = {
            "kind",
            "type",
            "target",
            "observation_id",
            "observation",
            "parameters",
            "action_id",
            "expected_digest",
        }
        allowed.update({
            "button",
            "coordinates",
            "duration_ms",
            "key",
            "text",
            "x",
            "y",
            "delta_x",
            "delta_y",
        })
        unknown = set(value) - allowed
        if unknown:
            raise LifecycleError("invalid_action", "Unknown action fields are not accepted.")
        return cls(
            kind=kind,
            target=TargetIdentity.from_value(target),
            observation_id=observation_id,
            parameters=parameters,
            action_id=value.get("action_id", ""),
            expected_digest=value.get("expected_digest"),
        )

    def as_dict(self) -> Dict[str, Any]:
        return {
            "action_id": self.action_id,
            "kind": self.kind,
            "target": self.target.as_dict(),
            "observation_id": self.observation_id,
            "expected_digest": self.expected_digest,
            "parameters": dict(_bounded_value(self.parameters)),
        }


def _validate_action_parameters(kind: str, parameters: Mapping[str, Any]) -> None:
    allowed_by_kind = {
        "click": {"button", "coordinates", "x", "y"},
        "double_click": {"button", "coordinates", "x", "y"},
        "drag": {"button", "coordinates", "x", "y", "duration_ms"},
        "move": {"button", "coordinates", "x", "y"},
        "press": {"key"},
        "scroll": {"delta_x", "delta_y"},
        "type": {"text"},
        "wait": {"duration_ms"},
    }
    unknown = set(parameters) - allowed_by_kind[kind]
    if unknown:
        raise LifecycleError("invalid_action", "Action parameters contain fields outside the typed allowlist.")
    if kind in {"click", "double_click", "drag", "move"}:
        button = parameters.get("button", "left")
        if button not in {"left", "middle", "right"}:
            raise LifecycleError("invalid_action", "Mouse button is not in the typed allowlist.")
    if kind in {"click", "double_click", "move", "drag"}:
        coordinates = parameters.get("coordinates")
        if coordinates is None and "x" in parameters and "y" in parameters:
            coordinates = {"x": parameters["x"], "y": parameters["y"]}
        if coordinates is not None:
            if not isinstance(coordinates, Mapping) or set(coordinates) - {"x", "y"}:
                raise LifecycleError("invalid_action", "Coordinates must be a bounded x/y object.")
            _finite_number(coordinates.get("x"), "x", minimum=-MAX_COORDINATE, maximum=MAX_COORDINATE)
            _finite_number(coordinates.get("y"), "y", minimum=-MAX_COORDINATE, maximum=MAX_COORDINATE)
        if kind == "drag" and "duration_ms" in parameters:
            duration = parameters["duration_ms"]
            if not isinstance(duration, int) or isinstance(duration, bool) or not 0 <= duration <= 10_000:
                raise LifecycleError("invalid_action", "Drag duration is outside its bound.")
    if kind == "type":
        text = parameters.get("text")
        if not isinstance(text, str) or len(text.encode("utf-8")) > 16_384:
            raise LifecycleError("invalid_action", "Typed text must be bounded text.")
    if kind == "press":
        key = parameters.get("key")
        if key not in {
            "ArrowUp",
            "ArrowDown",
            "ArrowLeft",
            "ArrowRight",
            "Enter",
            "Escape",
            "Tab",
            "Backspace",
            "Home",
            "End",
            "PageUp",
            "PageDown",
        }:
            raise LifecycleError("invalid_action", "Key is not in the typed navigation allowlist.")
    if kind == "scroll":
        for name in ("delta_x", "delta_y"):
            value = parameters.get(name, 0)
            if not isinstance(value, int) or isinstance(value, bool) or not -10_000 <= value <= 10_000:
                raise LifecycleError("invalid_action", "Scroll distance is outside its bound.")
    if kind == "wait":
        value = parameters.get("duration_ms", 0)
        if not isinstance(value, int) or isinstance(value, bool) or not 0 <= value <= 10_000:
            raise LifecycleError("invalid_action", "Wait duration is outside its bound.")


@dataclass(frozen=True)
class ActionGroup:
    actions: Tuple[ActionSpec, ...]
    group_id: str = ""

    def __post_init__(self) -> None:
        values = tuple(ActionSpec.from_value(item) for item in self.actions)
        if not values:
            raise LifecycleError("invalid_action_group", "An action group must contain one action.")
        if len(values) > MAX_ACTIONS_PER_GROUP:
            raise LifecycleError("action_group_too_large", "Action groups are bounded and ordered.")
        object.__setattr__(self, "actions", values)
        group_id = self.group_id or "group-" + uuid.uuid4().hex
        _require_id(group_id, "group_id")
        object.__setattr__(self, "group_id", group_id)

    @classmethod
    def from_value(cls, value: Any) -> "ActionGroup":
        if isinstance(value, ActionGroup):
            return value
        if isinstance(value, Mapping):
            actions = value.get("actions")
            group_id = value.get("group_id", "")
        else:
            actions = value
            group_id = ""
        if not isinstance(actions, (list, tuple)):
            raise LifecycleError("invalid_action_group", "An action group must contain an action array.")
        return cls(tuple(actions), group_id=group_id)

    def as_dict(self) -> Dict[str, Any]:
        return {"group_id": self.group_id, "actions": [item.as_dict() for item in self.actions]}


@dataclass(frozen=True)
class PolicyDecision:
    """Host policy output; model input cannot mark an action as authorized."""

    decision: str
    reason: str = ""
    prompt: str = ""
    detail: str = ""

    def __post_init__(self) -> None:
        if self.decision not in {"allow", "deny", "approval"}:
            raise LifecycleError("invalid_policy_decision", "Policy returned an unsupported decision.")

    @classmethod
    def allow(cls, reason: str = "") -> "PolicyDecision":
        return cls("allow", reason=bounded_text(reason, 1024))

    @classmethod
    def deny(cls, reason: str = "") -> "PolicyDecision":
        return cls("deny", reason=bounded_text(reason, 1024))

    @classmethod
    def require_approval(cls, prompt: str, detail: str = "") -> "PolicyDecision":
        return cls("approval", prompt=bounded_text(prompt, 1024), detail=bounded_text(detail, 2048))

    @classmethod
    def from_value(cls, value: Any) -> "PolicyDecision":
        if isinstance(value, PolicyDecision):
            return value
        if isinstance(value, bool):
            return cls.allow() if value else cls.deny()
        if isinstance(value, str):
            return cls(value)
        if isinstance(value, Mapping):
            decision = value.get("decision", value.get("outcome"))
            return cls(
                decision=decision,
                reason=bounded_text(value.get("reason", ""), 1024),
                prompt=bounded_text(value.get("prompt", ""), 1024),
                detail=bounded_text(value.get("detail", ""), 2048),
            )
        raise LifecycleError("invalid_policy_decision", "Policy did not return a decision.")

    def as_dict(self) -> Dict[str, Any]:
        result = {"decision": self.decision, "reason": self.reason}
        if self.prompt:
            result["prompt"] = self.prompt
        if self.detail:
            result["detail"] = self.detail
        return result


class DenyByDefaultPolicy:
    """Safe default when the host has not supplied #383 policy."""

    def decide(self, owner: OwnerIdentity, action: ActionSpec, observation: TargetObservation, **_: Any) -> PolicyDecision:
        _ = (owner, action, observation)
        return PolicyDecision.deny("No host policy decision was supplied.")


# ---------------------------------------------------------------------------
# Receipts and budgets


@dataclass(frozen=True)
class ActionExecution:
    outcome: str = "committed"
    effect: str = "committed"
    acknowledged: bool = True
    detail: str = ""
    result: Optional[Mapping[str, Any]] = None
    output_chars: int = 0
    capture_bytes: int = 0

    def __post_init__(self) -> None:
        if self.outcome in {"ok", "success", "succeeded"}:
            object.__setattr__(self, "outcome", "committed")
        if self.outcome not in {"committed", "failed", "cancelled", "unknown_effect"}:
            raise LifecycleError("invalid_adapter_result", "Adapter returned an unsupported action outcome.")
        if self.effect not in {"none", "committed", "unknown"}:
            raise LifecycleError("invalid_adapter_result", "Adapter returned an unsupported effect state.")
        if self.outcome == "committed" and (not self.acknowledged or self.effect != "committed"):
            object.__setattr__(self, "outcome", "unknown_effect")
            object.__setattr__(self, "effect", "unknown")
        elif self.outcome in {"failed", "cancelled"} and self.effect != "none":
            # A failure that reports any possible effect is ambiguous.  It is
            # never safe for a caller to retry it automatically.
            object.__setattr__(self, "outcome", "unknown_effect")
            object.__setattr__(self, "effect", "unknown")
        if self.outcome == "unknown_effect":
            object.__setattr__(self, "effect", "unknown")
        if self.result is not None:
            safe_result = _bounded_value(self.result, max_depth=4, max_items=MAX_STRUCTURED_ITEMS)
            if not isinstance(safe_result, Mapping):
                raise LifecycleError("invalid_adapter_result", "Action result must be an object.")
            object.__setattr__(self, "result", dict(safe_result))
        if not isinstance(self.output_chars, int) or isinstance(self.output_chars, bool) or self.output_chars < 0:
            raise LifecycleError("invalid_adapter_result", "Action output size is invalid.")
        if not isinstance(self.capture_bytes, int) or isinstance(self.capture_bytes, bool) or self.capture_bytes < 0:
            raise LifecycleError("invalid_adapter_result", "Action capture size is invalid.")

    @classmethod
    def from_value(cls, value: Any) -> "ActionExecution":
        if isinstance(value, ActionExecution):
            return value
        if not isinstance(value, Mapping):
            raise LifecycleError("invalid_adapter_result", "The adapter did not return an action acknowledgement.")
        raw_outcome = value.get("outcome", value.get("status"))
        if raw_outcome in {"ok", "success", "succeeded", True}:
            raw_outcome = "committed"
        if raw_outcome is None:
            raw_outcome = "committed" if value.get("ok", True) else "failed"
        may_have_effect = value.get("may_have_effect", False)
        if raw_outcome in {"failed", "cancelled"} and may_have_effect:
            raw_outcome = "unknown_effect"
        effect = value.get("effect")
        if effect is None:
            effect = "committed" if raw_outcome == "committed" else ("unknown" if raw_outcome == "unknown_effect" else "none")
        return cls(
            outcome=raw_outcome,
            effect=effect,
            acknowledged=bool(value.get("acknowledged", raw_outcome == "committed")),
            detail=bounded_text(value.get("detail", value.get("message", "")), 2048),
            result=value.get("result") if isinstance(value.get("result"), Mapping) else None,
            output_chars=int(value.get("output_chars", 0)) if isinstance(value.get("output_chars", 0), int) else 0,
            capture_bytes=int(value.get("capture_bytes", 0)) if isinstance(value.get("capture_bytes", 0), int) else 0,
        )


@dataclass(frozen=True)
class VerificationResult:
    owner: OwnerIdentity
    target: TargetIdentity
    observation_id: str
    digest: str
    trusted: bool
    screenshot: Optional[ScreenshotReference] = None
    structured: Optional[Mapping[str, Any]] = None
    detail: str = ""

    def __post_init__(self) -> None:
        _require_id(self.observation_id, "observation_id")
        _digest(self.digest)
        if self.screenshot is not None and self.screenshot.owner != self.owner:
            raise LifecycleError("owner_mismatch", "Verification screenshot has a different owner.")
        if self.structured is not None:
            _bounded_value(self.structured)

    @classmethod
    def from_observation(
        cls, observation: TargetObservation, *, trusted: bool = True, detail: str = ""
    ) -> "VerificationResult":
        return cls(
            owner=observation.owner,
            target=observation.target,
            observation_id=observation.observation_id,
            digest=observation.digest,
            trusted=trusted,
            screenshot=observation.screenshot,
            structured=observation.structured,
            detail=bounded_text(detail, 2048),
        )

    def as_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "owner": self.owner.as_dict(),
            "target": self.target.as_dict(),
            "observation_id": self.observation_id,
            "digest": self.digest,
            "trusted": self.trusted,
            "detail": self.detail,
        }
        if self.screenshot is not None:
            result["screenshot"] = self.screenshot.as_dict()
        if self.structured is not None:
            result["structured"] = dict(_bounded_value(self.structured))
        return result


@dataclass(frozen=True)
class ActionReceipt:
    owner: OwnerIdentity
    lifecycle_epoch: int
    group_id: str
    action_id: str
    index: int
    target: TargetIdentity
    observation_id: Optional[str]
    outcome: str
    effect: str
    acknowledged: bool
    detail: str = ""
    result: Optional[Mapping[str, Any]] = None
    policy: Optional[PolicyDecision] = None
    model_epoch: int = 0
    recorded_at: float = 0.0

    def as_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "owner": self.owner.as_dict(),
            "lifecycle_epoch": self.lifecycle_epoch,
            "model_epoch": self.model_epoch,
            "group_id": self.group_id,
            "action_id": self.action_id,
            "index": self.index,
            "target": self.target.as_dict(),
            "observation_id": self.observation_id,
            "outcome": self.outcome,
            "effect": self.effect,
            "acknowledged": self.acknowledged,
            "detail": bounded_text(self.detail, 2048),
            "recorded_at": self.recorded_at,
        }
        if self.result is not None:
            result["result"] = dict(_bounded_value(self.result))
        if self.policy is not None:
            result["policy"] = self.policy.as_dict()
        return result


@dataclass(frozen=True)
class ActionGroupResult:
    owner: OwnerIdentity
    lifecycle_epoch: int
    model_epoch: int
    group_id: str
    status: str
    receipts: Tuple[ActionReceipt, ...]
    committed_prefix: Tuple[str, ...]
    unknown_effects: Tuple[Mapping[str, Any], ...]
    final_observation: Optional[TargetObservation] = None
    verification: Optional[VerificationResult] = None
    cleanup: Optional[Mapping[str, Any]] = None
    state: str = SessionState.PAUSED.value

    @property
    def ok(self) -> bool:
        return self.status == "succeeded" and not self.unknown_effects

    def as_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "owner": self.owner.as_dict(),
            "lifecycle_epoch": self.lifecycle_epoch,
            "model_epoch": self.model_epoch,
            "group_id": self.group_id,
            "status": self.status,
            "state": self.state,
            "receipts": [receipt.as_dict() for receipt in self.receipts],
            "committed_prefix": list(self.committed_prefix),
            "unknown_effects": [dict(_bounded_value(item)) for item in self.unknown_effects],
        }
        if self.final_observation is not None:
            result["final_observation"] = self.final_observation.as_dict()
        if self.verification is not None:
            result["verification"] = self.verification.as_dict()
        if self.cleanup is not None:
            result["cleanup"] = dict(_bounded_value(self.cleanup))
        return result


@dataclass(frozen=True)
class LifecycleBudget:
    """Finite persistent budgets for a lifecycle session."""

    max_actions: int = MAX_TOTAL_ACTIONS
    max_actions_per_group: int = MAX_ACTIONS_PER_GROUP
    max_wall_seconds: float = 300.0
    max_action_seconds: float = DEFAULT_OPERATION_SECONDS
    max_capture_bytes: int = MAX_CAPTURE_BYTES
    max_output_chars: int = MAX_OUTPUT_CHARS
    max_screenshots: int = 32
    max_cpu_seconds: float = 120.0
    max_memory_bytes: int = 512 * 1024 * 1024
    max_in_flight: int = 1

    def __post_init__(self) -> None:
        for name in (
            "max_actions",
            "max_actions_per_group",
            "max_capture_bytes",
            "max_output_chars",
            "max_screenshots",
            "max_memory_bytes",
            "max_in_flight",
        ):
            value = getattr(self, name)
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise ValueError(name + " must be a positive integer")
        for name in ("max_wall_seconds", "max_action_seconds", "max_cpu_seconds"):
            value = getattr(self, name)
            if not isinstance(value, (int, float)) or isinstance(value, bool) or not math.isfinite(float(value)) or value <= 0:
                raise ValueError(name + " must be finite and positive")
        if self.max_actions_per_group > MAX_ACTIONS_PER_GROUP:
            raise ValueError("max_actions_per_group exceeds protocol bound")
        if self.max_actions > MAX_TOTAL_ACTIONS:
            raise ValueError("max_actions exceeds protocol bound")
        if self.max_capture_bytes > MAX_CAPTURE_BYTES or self.max_output_chars > MAX_OUTPUT_CHARS:
            raise ValueError("budget exceeds protocol bound")


Budget = LifecycleBudget


class BudgetLedger:
    def __init__(self, budget: LifecycleBudget, *, clock: Callable[[], float]) -> None:
        self.budget = budget
        self.clock = clock
        self.started_at = clock()
        self.actions_started = 0
        self.capture_bytes = 0
        self.output_chars = 0
        self.screenshots = 0
        self.cpu_seconds = 0.0
        self.peak_memory_bytes = 0
        self._in_flight = 0
        self._condition = threading.Condition()

    @property
    def deadline(self) -> float:
        return self.started_at + float(self.budget.max_wall_seconds)

    def remaining_wall(self) -> float:
        return max(0.0, self.deadline - self.clock())

    def enter(self, token: CancellationToken) -> None:
        with self._condition:
            while self._in_flight >= self.budget.max_in_flight:
                token.throw_if_cancelled()
                remaining = token.remaining()
                wait_for = 0.025 if remaining is None else min(0.025, remaining)
                if wait_for <= 0:
                    raise BudgetExceeded("backpressure_timeout", "The lifecycle remained backpressured until its deadline.")
                self._condition.wait(wait_for)
            if self.remaining_wall() <= 0:
                raise BudgetExceeded("wall_budget_exhausted", "The persistent lifecycle wall-time budget is exhausted.")
            self._in_flight += 1

    def leave(self) -> None:
        with self._condition:
            self._in_flight = max(0, self._in_flight - 1)
            self._condition.notify_all()

    def charge_action(self) -> None:
        with self._condition:
            if self.actions_started >= self.budget.max_actions:
                raise BudgetExceeded("action_budget_exhausted", "The persistent action budget is exhausted.")
            self.actions_started += 1

    def charge_capture(self, amount: int) -> None:
        if not isinstance(amount, int) or isinstance(amount, bool) or amount < 0:
            raise BudgetExceeded("capture_budget_exhausted", "The adapter returned an invalid capture size.")
        with self._condition:
            if self.capture_bytes + amount > self.budget.max_capture_bytes:
                raise BudgetExceeded("capture_budget_exhausted", "The persistent capture budget is exhausted.")
            self.capture_bytes += amount

    def charge_output(self, amount: int) -> None:
        if not isinstance(amount, int) or isinstance(amount, bool) or amount < 0:
            raise BudgetExceeded("output_budget_exhausted", "The adapter returned an invalid output size.")
        with self._condition:
            if self.output_chars + amount > self.budget.max_output_chars:
                raise BudgetExceeded("output_budget_exhausted", "The persistent output budget is exhausted.")
            self.output_chars += amount

    def charge_screenshot(self) -> None:
        with self._condition:
            if self.screenshots >= self.budget.max_screenshots:
                raise BudgetExceeded("screenshot_budget_exhausted", "The persistent screenshot budget is exhausted.")
            self.screenshots += 1

    def charge_metrics(self, *, cpu_seconds: float = 0.0, memory_bytes: int = 0) -> None:
        if not isinstance(cpu_seconds, (int, float)) or not math.isfinite(float(cpu_seconds)) or cpu_seconds < 0:
            raise BudgetExceeded("cpu_budget_exhausted", "The adapter returned invalid CPU usage.")
        if not isinstance(memory_bytes, int) or isinstance(memory_bytes, bool) or memory_bytes < 0:
            raise BudgetExceeded("memory_budget_exhausted", "The adapter returned invalid memory usage.")
        with self._condition:
            if self.cpu_seconds + float(cpu_seconds) > self.budget.max_cpu_seconds:
                raise BudgetExceeded("cpu_budget_exhausted", "The persistent CPU budget is exhausted.")
            if memory_bytes > self.budget.max_memory_bytes:
                raise BudgetExceeded("memory_budget_exhausted", "The persistent memory budget is exhausted.")
            self.cpu_seconds += float(cpu_seconds)
            self.peak_memory_bytes = max(self.peak_memory_bytes, memory_bytes)

    def as_dict(self) -> Dict[str, Any]:
        return {
            "max_actions": self.budget.max_actions,
            "actions_started": self.actions_started,
            "max_capture_bytes": self.budget.max_capture_bytes,
            "capture_bytes": self.capture_bytes,
            "max_output_chars": self.budget.max_output_chars,
            "output_chars": self.output_chars,
            "max_screenshots": self.budget.max_screenshots,
            "screenshots": self.screenshots,
            "max_cpu_seconds": self.budget.max_cpu_seconds,
            "cpu_seconds": self.cpu_seconds,
            "max_memory_bytes": self.budget.max_memory_bytes,
            "peak_memory_bytes": self.peak_memory_bytes,
            "remaining_wall_seconds": self.remaining_wall(),
            "in_flight": self._in_flight,
        }


@dataclass(frozen=True)
class InputLease:
    owner: OwnerIdentity
    lease_id: str

    def as_dict(self) -> Dict[str, Any]:
        return {"owner": self.owner.as_dict(), "lease_id": self.lease_id}


class InputOwnership:
    """One process-wide input owner, with no model-triggered takeover."""

    def __init__(self) -> None:
        self._condition = threading.Condition()
        self._lease: Optional[InputLease] = None

    @property
    def current(self) -> Optional[InputLease]:
        with self._condition:
            return self._lease

    def acquire(self, owner: OwnerIdentity, *, token: Optional[CancellationToken] = None) -> InputLease:
        selected_token = token or CancellationToken()
        with self._condition:
            if self._lease is not None:
                raise InputBusy(
                    "input_busy",
                    "Another owner holds the single computer-input lease.",
                    owner=owner,
                    details={"current_owner": self._lease.owner.as_dict()},
                )
            selected_token.throw_if_cancelled()
            lease = InputLease(owner=owner, lease_id="lease-" + uuid.uuid4().hex)
            self._lease = lease
            return lease

    def release(self, lease: InputLease) -> bool:
        with self._condition:
            if self._lease != lease:
                return False
            self._lease = None
            self._condition.notify_all()
            return True

    def release_owner(self, owner: OwnerIdentity) -> bool:
        with self._condition:
            if self._lease is None or self._lease.owner != owner:
                return False
            self._lease = None
            self._condition.notify_all()
            return True

    def revoke_for_host(self, owner: OwnerIdentity) -> Optional[InputLease]:
        """Revoke a lease only when called by a host takeover path."""

        with self._condition:
            previous = self._lease
            if previous is not None and previous.owner != owner:
                self._lease = None
                self._condition.notify_all()
            return previous


# ---------------------------------------------------------------------------
# Adapter contract and session implementation


class ComputerUseAdapter(Protocol):
    """Host/native adapter contract.

    All methods are expected to honor ``cancellation`` and ``timeout``.  The
    lifecycle also watches them and calls ``abort``/``release_input`` on a
    timeout.  An action method must return an acknowledgement with an explicit
    effect; returning after an unacknowledged click is an unknown effect.
    """

    def observe(
        self,
        owner: OwnerIdentity,
        target: TargetIdentity,
        *,
        cancellation: CancellationToken,
        timeout: float,
    ) -> Union[TargetObservation, Mapping[str, Any]]:
        ...

    def perform_action(
        self,
        owner: OwnerIdentity,
        target: TargetIdentity,
        action: ActionSpec,
        *,
        cancellation: CancellationToken,
        timeout: float,
    ) -> Union[ActionExecution, Mapping[str, Any]]:
        ...


class LifecycleSession:
    """Persistent state for exactly one owner and lifecycle epoch."""

    def __init__(
        self,
        owner: OwnerIdentity,
        target: TargetIdentity,
        adapter: Any,
        *,
        policy: Optional[Any] = None,
        approval: Optional[Any] = None,
        budget: Optional[LifecycleBudget] = None,
        input_ownership: Optional[InputOwnership] = None,
        clock: Callable[[], float] = time.monotonic,
        lifecycle_epoch: int = 1,
        on_settle: Optional[Callable[[OwnerIdentity, str], Any]] = None,
    ) -> None:
        self.owner = OwnerIdentity.from_value(owner)
        self._selected_target = TargetIdentity.from_value(target)
        self.adapter = adapter
        self.policy = policy or DenyByDefaultPolicy()
        self.approval = approval
        self.budget = BudgetLedger(budget or LifecycleBudget(), clock=clock)
        self.input_ownership = input_ownership or InputOwnership()
        self.clock = clock
        self.lifecycle_epoch = lifecycle_epoch
        self.model_epoch = 0
        self._on_settle = on_settle
        self._session_token = CancellationToken(deadline=self.budget.deadline, clock=clock)
        self._state = SessionState.PAUSED
        self._state_reason = "observation_required"
        self._last_observation: Optional[TargetObservation] = None
        self._history: "OrderedDict[str, TargetObservation]" = OrderedDict()
        self._observation_sequence = 0
        self._operation_condition = threading.Condition()
        self._operations = 0
        self._settled = False
        self._settlement_reason: Optional[str] = None
        self._cleanup_degraded = False
        self._safe_fallback_required = False
        self._cleanup_callbacks: List[Callable[[OwnerIdentity, str], Any]] = []

    @property
    def state(self) -> SessionState:
        return self._state

    @property
    def selected_target(self) -> TargetIdentity:
        return self._selected_target

    @property
    def last_observation(self) -> Optional[TargetObservation]:
        return self._last_observation

    @property
    def settled(self) -> bool:
        return self._settled

    def register_cleanup(self, callback: Callable[[OwnerIdentity, str], Any]) -> None:
        if not callable(callback):
            raise TypeError("cleanup callback must be callable")
        self._cleanup_callbacks.append(callback)

    def _check_owner(self, owner: Any) -> OwnerIdentity:
        selected = OwnerIdentity.from_value(owner)
        if selected != self.owner:
            raise LifecycleError(
                "owner_mismatch",
                "The request owner does not match the persistent lifecycle owner.",
                owner=selected,
                details={"active_owner": self.owner.as_dict()},
            )
        if self._settled:
            raise LifecycleError(
                "owner_settled",
                "The owner lifecycle has been settled and cannot resume.",
                owner=selected,
                details={"reason": self._settlement_reason or "settled"},
            )
        return selected

    def _enter_operation(self, token: CancellationToken) -> None:
        with self._operation_condition:
            while self._operations >= self.budget.budget.max_in_flight:
                token.throw_if_cancelled()
                remaining = token.remaining()
                wait_for = 0.025 if remaining is None else min(0.025, remaining)
                if wait_for <= 0:
                    raise BudgetExceeded("backpressure_timeout", "The lifecycle remained backpressured until its deadline.")
                self._operation_condition.wait(wait_for)
            token.throw_if_cancelled()
            if self.budget.remaining_wall() <= 0:
                raise BudgetExceeded("wall_budget_exhausted", "The persistent lifecycle wall-time budget is exhausted.")
            self._operations += 1

    def _leave_operation(self) -> None:
        with self._operation_condition:
            self._operations = max(0, self._operations - 1)
            self._operation_condition.notify_all()

    def _operation_token(self, cancellation: Optional[CancellationToken], timeout: Optional[float]) -> CancellationToken:
        if timeout is not None:
            if isinstance(timeout, bool) or not isinstance(timeout, (int, float)) or not math.isfinite(float(timeout)) or timeout <= 0:
                raise LifecycleError("invalid_timeout", "The operation timeout must be finite and positive.", owner=self.owner)
        parent: CancellationToken = self._session_token
        if cancellation is not None:
            # Keep host cancellation separate from session cancellation.  A
            # cancelled request must not silently cancel future requests for
            # the same owner.
            parent = _CombinedCancellationToken(parent, cancellation, clock=self.clock)
        return parent.child(
            self.budget.budget.max_action_seconds if timeout is None else timeout
        )

    def _call_adapter(
        self,
        names: Sequence[str],
        positional: Sequence[Any],
        *,
        token: CancellationToken,
        timeout: Optional[float],
        abort_names: Sequence[str] = ("abort", "cancel", "interrupt"),
        keyword: Optional[Mapping[str, Any]] = None,
    ) -> Any:
        function = None
        for name in names:
            candidate = getattr(self.adapter, name, None)
            if callable(candidate):
                function = candidate
                break
        if function is None:
            raise LifecycleError("adapter_unavailable", "The selected computer-use adapter is unavailable.", owner=self.owner)
        kwargs = {
            "cancellation": token,
            "timeout": self.budget.budget.max_action_seconds if timeout is None else timeout,
        }
        if keyword:
            kwargs.update(keyword)
        abort_function: Optional[Callable[[], Any]] = None
        for name in abort_names:
            candidate = getattr(self.adapter, name, None)
            if callable(candidate):
                abort_function = lambda candidate=candidate: _call_with_contract(
                    candidate,
                    (self.owner,),
                    {"cancellation": token, "timeout": DEFAULT_STOP_SECONDS},
                )
                break
        return bounded_call(
            lambda: _call_with_contract(function, positional, kwargs),
            token=token,
            timeout=self.budget.budget.max_action_seconds if timeout is None else timeout,
            abort=abort_function,
            operation=names[0],
        )

    def select_target(self, owner: Any, target: Any) -> Dict[str, Any]:
        selected = self._check_owner(owner)
        new_target = TargetIdentity.from_value(target)
        with self._operation_condition:
            if self._operations:
                raise LifecycleError("lifecycle_busy", "Target selection cannot race an active operation.", owner=selected)
            self._selected_target = new_target
            self._last_observation = None
            self._history.clear()
            self._state = SessionState.PAUSED
            self._state_reason = "observation_required_after_reselection"
            self._safe_fallback_required = False
        return self.status()

    def _normalize_observation(self, value: Any, requested: TargetIdentity) -> TargetObservation:
        if isinstance(value, TargetObservation):
            observation = value
            if observation.owner != self.owner:
                raise LifecycleError("owner_mismatch", "Adapter observation has a different owner.", owner=self.owner)
            return observation
        if not isinstance(value, Mapping):
            raise LifecycleError("invalid_observation", "Adapter observation must be a bounded object.", owner=self.owner)
        supplied_owner = value.get("owner")
        if supplied_owner is not None and OwnerIdentity.from_value(supplied_owner) != self.owner:
            raise LifecycleError("owner_mismatch", "Adapter observation has a different owner.", owner=self.owner)
        raw_target = value.get("target", requested)
        target = TargetIdentity.from_value(raw_target)
        dimensions = value.get("dimensions")
        if isinstance(dimensions, Mapping):
            width = dimensions.get("width")
            height = dimensions.get("height")
        elif isinstance(dimensions, (list, tuple)) and len(dimensions) == 2:
            width, height = dimensions
        else:
            width, height = value.get("width"), value.get("height")
        screenshot = _screenshot_from_value(value.get("screenshot", value.get("screenshot_ref")), self.owner)
        if width is None and screenshot is not None:
            width = screenshot.width
        if height is None and screenshot is not None:
            height = screenshot.height
        scale = value.get("scale")
        transform = value.get("transform")
        if scale is None or transform is None:
            raise LifecycleError(
                "incomplete_observation",
                "Observation must include dimensions, scale, transform, and digest.",
                owner=self.owner,
            )
        structured_value = value.get("structured", value.get("structured_result"))
        structured: Optional[Mapping[str, Any]]
        if structured_value is None:
            structured = None
        else:
            bounded_structured = _bounded_value(structured_value)
            if not isinstance(bounded_structured, Mapping):
                raise LifecycleError("invalid_observation", "Structured observation must be an object.", owner=self.owner)
            structured = dict(bounded_structured)
        digest = value.get("digest")
        if digest is None:
            digest = _stable_digest({"target": target.as_dict(), "structured": structured, "dimensions": [width, height]})
        self._observation_sequence += 1
        observation_id = value.get("observation_id")
        if observation_id is None:
            observation_id = "obs-" + str(self._observation_sequence)
        capture_kind = value.get("capture_kind")
        if capture_kind is None:
            capture_kind = "screenshot+structured" if screenshot is not None and structured is not None else (
                "screenshot" if screenshot is not None else "structured"
            )
        capture_bytes = value.get("capture_bytes", value.get("bytes", 0))
        if not isinstance(capture_bytes, int) or isinstance(capture_bytes, bool):
            raise LifecycleError("invalid_capture", "Capture size must be a bounded integer.", owner=self.owner)
        if capture_bytes == 0 and screenshot is not None:
            capture_bytes = screenshot.size_bytes
        return TargetObservation(
            owner=self.owner,
            target=target,
            observation_id=observation_id,
            sequence=self._observation_sequence,
            width=width,
            height=height,
            scale=scale,
            transform=_normalise_transform(transform),
            digest=digest,
            capture_kind=capture_kind,
            screenshot=screenshot,
            structured=structured,
            capture_bytes=capture_bytes,
            observed_at=self.clock(),
        )

    def _observe_internal(
        self,
        requested: TargetIdentity,
        *,
        token: CancellationToken,
        timeout: Optional[float],
    ) -> TargetObservation:
        if self._state in {SessionState.STOPPING} or self._settled:
            raise LifecycleError("lifecycle_stopping", "The lifecycle is stopping.", owner=self.owner)
        if not requested.same_surface(self._selected_target):
            self._state = SessionState.DEGRADED
            self._state_reason = "target_reselection_required"
            raise LifecycleError(
                "target_reselection_required",
                "The requested target is not the selected app/window/tab; explicitly select it first.",
                owner=self.owner,
                details={"selected_target": self._selected_target.as_dict(), "requested_target": requested.as_dict()},
            )
        raw = self._call_adapter(
            ("observe", "capture", "snapshot"),
            (self.owner, requested),
            token=token,
            timeout=timeout,
        )
        observation = self._normalize_observation(raw, requested)
        if not observation.target.same_surface(requested):
            self._state = SessionState.DEGRADED
            self._state_reason = "target_changed"
            self._last_observation = None
            raise LifecycleError(
                "target_changed",
                "The app/window/tab changed while observing; explicit reselection is required.",
                owner=self.owner,
                details={"requested_target": requested.as_dict(), "observed_target": observation.target.as_dict()},
            )
        try:
            self.budget.charge_capture(observation.capture_bytes)
            if observation.screenshot is not None:
                self.budget.charge_screenshot()
        except BudgetExceeded:
            self._state = SessionState.DEGRADED
            self._state_reason = "capture_budget_exhausted"
            raise
        # A frame-generation change is accepted as a fresh observation, but it
        # cannot authorize an action bound to the old frame.  The old action
        # will fail the exact target/digest check below.
        with self._operation_condition:
            token.throw_if_cancelled()
            if self._settled or self._session_token.cancelled:
                raise LifecycleError("owner_settled", "Observation owner is no longer active.")
            self._selected_target = observation.target
            self._last_observation = observation
            self._history[observation.observation_id] = observation
            self._history.move_to_end(observation.observation_id)
            while len(self._history) > MAX_HISTORY:
                self._history.popitem(last=False)
            self._state = SessionState.ACTIVE
            self._state_reason = "observed"
        return observation

    def observe(
        self,
        owner: Any,
        target: Optional[Any] = None,
        *,
        cancellation: Optional[CancellationToken] = None,
        timeout: Optional[float] = None,
    ) -> TargetObservation:
        selected = self._check_owner(owner)
        requested = self._selected_target if target is None else TargetIdentity.from_value(target)
        token = self._operation_token(cancellation, timeout)
        self._enter_operation(token)
        try:
            return self._observe_internal(requested, token=token, timeout=timeout)
        except _BoundedCallInterrupted as error:
            self._state = SessionState.PAUSED
            self._state_reason = error.reason
            raise LifecycleError(error.reason, "Observation was cancelled before it completed.", owner=selected) from error
        finally:
            self._leave_operation()

    def _policy_decision(
        self,
        action: ActionSpec,
        observation: TargetObservation,
        *,
        token: CancellationToken,
        timeout: Optional[float],
    ) -> PolicyDecision:
        function = getattr(self.policy, "decide", self.policy)
        if not callable(function):
            return PolicyDecision.deny("Host policy is unavailable.")
        raw = bounded_call(
            lambda: _call_with_contract(
                function,
                (self.owner, action, observation),
                {
                    "owner": self.owner,
                    "action": action,
                    "observation": observation,
                    "cancellation": token,
                    "timeout": self.budget.budget.max_action_seconds if timeout is None else timeout,
                },
            ),
            token=token,
            timeout=self.budget.budget.max_action_seconds if timeout is None else timeout,
            operation="policy decision",
        )
        return PolicyDecision.from_value(raw)

    def _approval_decision(
        self,
        action: ActionSpec,
        decision: PolicyDecision,
        *,
        token: CancellationToken,
        timeout: Optional[float],
    ) -> bool:
        if self.approval is None:
            return False
        function = getattr(self.approval, "approve", self.approval)
        if not callable(function):
            return False
        raw = bounded_call(
            lambda: _call_with_contract(
                function,
                (action,),
                {
                    "owner": self.owner,
                    "action": action,
                    "decision": decision,
                    "prompt": decision.prompt,
                    "detail": decision.detail,
                    "cancellation": token,
                    "timeout": self.budget.budget.max_action_seconds if timeout is None else timeout,
                },
            ),
            token=token,
            timeout=self.budget.budget.max_action_seconds if timeout is None else timeout,
            operation="approval",
        )
        return raw is True

    def _receipt(
        self,
        group: ActionGroup,
        action: ActionSpec,
        index: int,
        *,
        outcome: str,
        effect: str,
        acknowledged: bool,
        observation_id: Optional[str],
        detail: str = "",
        result: Optional[Mapping[str, Any]] = None,
        policy: Optional[PolicyDecision] = None,
        target: Optional[TargetIdentity] = None,
    ) -> ActionReceipt:
        return ActionReceipt(
            owner=self.owner,
            lifecycle_epoch=self.lifecycle_epoch,
            model_epoch=self.model_epoch,
            group_id=group.group_id,
            action_id=action.action_id,
            index=index,
            target=target or action.target,
            observation_id=observation_id,
            outcome=outcome,
            effect=effect,
            acknowledged=acknowledged,
            detail=bounded_text(detail, 2048),
            result=result,
            policy=policy,
            recorded_at=self.clock(),
        )

    def _verification(
        self,
        *,
        token: CancellationToken,
        timeout: Optional[float],
    ) -> Tuple[Optional[TargetObservation], Optional[VerificationResult], Optional[str]]:
        try:
            final = self._observe_internal(self._selected_target, token=token, timeout=timeout)
        except _BoundedCallInterrupted as error:
            return None, None, error.reason
        except BudgetExceeded as error:
            return None, None, error.code
        except LifecycleError as error:
            return None, None, error.code
        verification_function = getattr(self.adapter, "verify", None)
        if not callable(verification_function):
            return final, VerificationResult.from_observation(final), None
        try:
            raw = self._call_adapter(
                ("verify",),
                (self.owner, final.target),
                token=token,
                timeout=timeout,
            )
            if isinstance(raw, VerificationResult):
                if raw.owner != self.owner or raw.target != final.target:
                    raise LifecycleError("owner_mismatch", "Verification crossed an owner or target fence.", owner=self.owner)
                return final, raw, None
            if isinstance(raw, Mapping):
                supplied_owner = raw.get("owner")
                if supplied_owner is not None and OwnerIdentity.from_value(supplied_owner) != self.owner:
                    raise LifecycleError("owner_mismatch", "Verification crossed the owner fence.", owner=self.owner)
                supplied_target = raw.get("target")
                if supplied_target is not None and TargetIdentity.from_value(supplied_target) != final.target:
                    raise LifecycleError("target_changed", "Verification crossed the target fence.", owner=self.owner)
                screenshot = _screenshot_from_value(raw.get("screenshot", raw.get("screenshot_ref")), self.owner)
                structured = raw.get("structured", raw.get("result"))
                if structured is not None and not isinstance(structured, Mapping):
                    structured = {"value": bounded_text(structured, MAX_OUTPUT_CHARS)}
                return (
                    final,
                    VerificationResult(
                        owner=self.owner,
                        target=final.target,
                        observation_id=bounded_text(raw.get("observation_id", final.observation_id), MAX_OBSERVATION_ID_CHARS),
                        digest=raw.get("digest", final.digest),
                        trusted=bool(raw.get("trusted", True)),
                        screenshot=screenshot or final.screenshot,
                        structured=dict(_bounded_value(structured)) if isinstance(structured, Mapping) else final.structured,
                        detail=bounded_text(raw.get("detail", ""), 2048),
                    ),
                    None,
                )
            raise LifecycleError("invalid_verification", "Adapter verification was not a bounded result.", owner=self.owner)
        except _BoundedCallInterrupted as error:
            return final, None, error.reason
        except LifecycleError as error:
            return final, None, error.code
        except BaseException:
            return final, None, "verification_failed"

    def execute_group(
        self,
        owner: Any,
        actions: Union[ActionGroup, Sequence[Union[ActionSpec, Mapping[str, Any]]]],
        *,
        group_id: Optional[str] = None,
        cancellation: Optional[CancellationToken] = None,
        timeout: Optional[float] = None,
    ) -> ActionGroupResult:
        selected = self._check_owner(owner)
        group = actions if isinstance(actions, ActionGroup) else ActionGroup(tuple(actions), group_id=group_id or "")
        if group_id is not None and isinstance(actions, ActionGroup) and group.group_id != group_id:
            raise LifecycleError("invalid_action_group", "group_id does not match the action group.", owner=selected)
        if self._state in {SessionState.DEGRADED, SessionState.STOPPING}:
            raise LifecycleError(
                "lifecycle_not_ready",
                "The lifecycle requires host recovery and a fresh observation before acting.",
                owner=selected,
                details={"state": self._state.value, "reason": self._state_reason},
            )
        token = self._operation_token(cancellation, timeout)
        try:
            self._enter_operation(token)
        except LifecycleError:
            raise
        lease: Optional[InputLease] = None
        receipts: List[ActionReceipt] = []
        committed: List[str] = []
        unknown: List[Mapping[str, Any]] = []
        status = "succeeded"
        cleanup: Optional[Mapping[str, Any]] = None
        try:
            try:
                lease = self.input_ownership.acquire(self.owner, token=token)
            except InputBusy as error:
                status = ActionOutcome.INPUT_BUSY.value
                return ActionGroupResult(
                    owner=self.owner,
                    lifecycle_epoch=self.lifecycle_epoch,
                    model_epoch=self.model_epoch,
                    group_id=group.group_id,
                    status=status,
                    receipts=tuple(),
                    committed_prefix=tuple(),
                    unknown_effects=tuple(),
                    state=self._state.value,
                )
            for index, action in enumerate(group.actions):
                try:
                    token.throw_if_cancelled()
                    self.budget.charge_action()
                except BudgetExceeded as error:
                    status = ActionOutcome.BUDGET_EXHAUSTED.value
                    self._state = SessionState.DEGRADED
                    self._state_reason = error.code
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=None,
                            detail=error.message,
                        )
                    )
                    break
                except LifecycleError as error:
                    status = ActionOutcome.CANCELLED.value
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=None,
                            detail=error.code,
                        )
                    )
                    break

                try:
                    # This is a new selected-target capture for every action;
                    # the model's old observation only binds what it saw.
                    fresh = self._observe_internal(action.target, token=token, timeout=timeout)
                except _BoundedCallInterrupted as error:
                    status = ActionOutcome.CANCELLED.value
                    self._state = SessionState.PAUSED
                    self._state_reason = error.reason
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=None,
                            detail=error.reason,
                        )
                    )
                    break
                except LifecycleError as error:
                    status = ActionOutcome.TARGET_CHANGED.value if error.code == "target_changed" else ActionOutcome.STALE_OBSERVATION.value
                    self._state = SessionState.PAUSED if error.code != "target_changed" else SessionState.DEGRADED
                    self._state_reason = error.code
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=None,
                            detail=error.code,
                        )
                    )
                    break

                prior = self._history.get(action.observation_id)
                stale_reason: Optional[str] = None
                if prior is None:
                    stale_reason = "observation_not_retained"
                elif prior.owner != self.owner or prior.target != action.target:
                    stale_reason = "observation_target_mismatch"
                elif action.expected_digest is not None and prior.digest != action.expected_digest:
                    stale_reason = "observation_digest_mismatch"
                elif fresh.target != action.target:
                    stale_reason = "frame_generation_changed"
                elif prior.digest != fresh.digest:
                    stale_reason = "target_digest_changed"
                if stale_reason is not None:
                    status = ActionOutcome.STALE_OBSERVATION.value
                    self._state = SessionState.PAUSED
                    self._state_reason = "reobserve_and_reselect"
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=fresh.observation_id,
                            detail=stale_reason,
                            target=fresh.target,
                        )
                    )
                    break

                try:
                    decision = self._policy_decision(action, fresh, token=token, timeout=timeout)
                except _BoundedCallInterrupted as error:
                    status = ActionOutcome.CANCELLED.value
                    self._state = SessionState.PAUSED
                    self._state_reason = error.reason
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=fresh.observation_id,
                            detail=error.reason,
                            target=fresh.target,
                        )
                    )
                    break
                except LifecycleError as error:
                    status = ActionOutcome.DENIED.value
                    self._state = SessionState.PAUSED
                    self._state_reason = error.code
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=fresh.observation_id,
                            detail=error.code,
                            target=fresh.target,
                        )
                    )
                    break

                if decision.decision == "deny":
                    status = ActionOutcome.DENIED.value
                    self._state = SessionState.PAUSED
                    self._state_reason = "policy_denied"
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.NONE.value,
                            acknowledged=False,
                            observation_id=fresh.observation_id,
                            detail=decision.reason,
                            policy=decision,
                            target=fresh.target,
                        )
                    )
                    break
                if decision.decision == "approval":
                    self._state = SessionState.AWAITING_APPROVAL
                    self._state_reason = "approval_required"
                    try:
                        approved = self._approval_decision(action, decision, token=token, timeout=timeout)
                    except _BoundedCallInterrupted as error:
                        status = ActionOutcome.CANCELLED.value
                        self._state = SessionState.PAUSED
                        self._state_reason = error.reason
                        receipts.append(
                            self._receipt(
                                group,
                                action,
                                index,
                                outcome=status,
                                effect=EffectState.NONE.value,
                                acknowledged=False,
                                observation_id=fresh.observation_id,
                                detail=error.reason,
                                policy=decision,
                                target=fresh.target,
                            )
                        )
                        break
                    if not approved:
                        status = ActionOutcome.APPROVAL_REQUIRED.value
                        self._state = SessionState.AWAITING_APPROVAL
                        self._state_reason = "approval_required"
                        receipts.append(
                            self._receipt(
                                group,
                                action,
                                index,
                                outcome=status,
                                effect=EffectState.NONE.value,
                                acknowledged=False,
                                observation_id=fresh.observation_id,
                                detail="Host approval was not received; the group stopped here.",
                                policy=decision,
                                target=fresh.target,
                            )
                        )
                        break
                    self._state = SessionState.ACTIVE
                    self._state_reason = "approved"

                try:
                    raw_execution = self._call_adapter(
                        ("perform_action", "act", "execute_action"),
                        (self.owner, fresh.target, action),
                        token=token,
                        timeout=timeout,
                    )
                    execution = ActionExecution.from_value(raw_execution)
                    self.budget.charge_output(execution.output_chars)
                    self.budget.charge_capture(execution.capture_bytes)
                except _BoundedCallInterrupted as error:
                    status = ActionOutcome.UNKNOWN_EFFECT.value
                    self._state = SessionState.DEGRADED
                    self._state_reason = error.reason
                    receipt = self._receipt(
                        group,
                        action,
                        index,
                        outcome=status,
                        effect=EffectState.UNKNOWN.value,
                        acknowledged=False,
                        observation_id=fresh.observation_id,
                        detail="Action acknowledgement was lost; no replay is permitted.",
                        policy=decision,
                        target=fresh.target,
                    )
                    receipts.append(receipt)
                    unknown.append({"action_id": action.action_id, "reason": error.reason, "receipt": receipt.as_dict()})
                    break
                except AdapterActionError as error:
                    if error.may_have_effect:
                        status = ActionOutcome.UNKNOWN_EFFECT.value
                        self._state = SessionState.DEGRADED
                        self._state_reason = error.code
                        effect = EffectState.UNKNOWN.value
                        unknown_item: Dict[str, Any] = {"action_id": action.action_id, "reason": error.code}
                        unknown.append(unknown_item)
                    else:
                        status = ActionOutcome.FAILED.value
                        self._state = SessionState.PAUSED
                        self._state_reason = error.code
                        effect = EffectState.NONE.value
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=effect,
                            acknowledged=False,
                            observation_id=fresh.observation_id,
                            detail=error.message,
                            policy=decision,
                            target=fresh.target,
                        )
                    )
                    break
                except BudgetExceeded as error:
                    status = ActionOutcome.BUDGET_EXHAUSTED.value
                    self._state = SessionState.DEGRADED
                    self._state_reason = error.code
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=status,
                            effect=EffectState.UNKNOWN.value,
                            acknowledged=False,
                            observation_id=fresh.observation_id,
                            detail=error.message,
                            policy=decision,
                            target=fresh.target,
                        )
                    )
                    unknown.append({"action_id": action.action_id, "reason": error.code})
                    break
                except LifecycleError as error:
                    status = ActionOutcome.UNKNOWN_EFFECT.value
                    self._state = SessionState.DEGRADED
                    self._state_reason = error.code
                    receipt = self._receipt(
                        group,
                        action,
                        index,
                        outcome=status,
                        effect=EffectState.UNKNOWN.value,
                        acknowledged=False,
                        observation_id=fresh.observation_id,
                        detail="The adapter failed after action dispatch; no replay is permitted.",
                        policy=decision,
                        target=fresh.target,
                    )
                    receipts.append(receipt)
                    unknown.append({"action_id": action.action_id, "reason": error.code, "receipt": receipt.as_dict()})
                    break
                except BaseException as error:
                    status = ActionOutcome.UNKNOWN_EFFECT.value
                    self._state = SessionState.DEGRADED
                    self._state_reason = "adapter_failure"
                    receipt = self._receipt(
                        group,
                        action,
                        index,
                        outcome=status,
                        effect=EffectState.UNKNOWN.value,
                        acknowledged=False,
                        observation_id=fresh.observation_id,
                        detail="The adapter failed after action dispatch; no replay is permitted.",
                        policy=decision,
                        target=fresh.target,
                    )
                    receipts.append(receipt)
                    unknown.append({"action_id": action.action_id, "reason": "adapter_failure", "receipt": receipt.as_dict()})
                    break

                if execution.outcome == "committed":
                    receipts.append(
                        self._receipt(
                            group,
                            action,
                            index,
                            outcome=ActionOutcome.COMMITTED.value,
                            effect=EffectState.COMMITTED.value,
                            acknowledged=True,
                            observation_id=fresh.observation_id,
                            detail=execution.detail,
                            result=execution.result,
                            policy=decision,
                            target=fresh.target,
                        )
                    )
                    committed.append(action.action_id)
                    continue
                if execution.outcome == "unknown_effect":
                    status = ActionOutcome.UNKNOWN_EFFECT.value
                    self._state = SessionState.DEGRADED
                    self._state_reason = "unknown_effect"
                    receipt = self._receipt(
                        group,
                        action,
                        index,
                        outcome=status,
                        effect=EffectState.UNKNOWN.value,
                        acknowledged=execution.acknowledged,
                        observation_id=fresh.observation_id,
                        detail=execution.detail or "Action effect was not acknowledged; no replay is permitted.",
                        result=execution.result,
                        policy=decision,
                        target=fresh.target,
                    )
                    receipts.append(receipt)
                    unknown.append({"action_id": action.action_id, "reason": "unacknowledged_effect", "receipt": receipt.as_dict()})
                    break
                status = ActionOutcome.FAILED.value if execution.outcome == "failed" else ActionOutcome.CANCELLED.value
                self._state = SessionState.PAUSED
                self._state_reason = execution.outcome
                receipts.append(
                    self._receipt(
                        group,
                        action,
                        index,
                        outcome=status,
                        effect=EffectState.NONE.value,
                        acknowledged=execution.acknowledged,
                        observation_id=fresh.observation_id,
                        detail=execution.detail,
                        result=execution.result,
                        policy=decision,
                        target=fresh.target,
                    )
                )
                break

            if committed or unknown:
                if not token.cancelled:
                    final_observation, verification, verification_error = self._verification(
                        token=token,
                        timeout=timeout,
                    )
                    if verification_error is not None and status == "succeeded":
                        status = "verification_unavailable"
                        self._state = SessionState.DEGRADED
                        self._state_reason = verification_error
                else:
                    final_observation, verification = None, None
            else:
                final_observation, verification = None, None
            if status == "succeeded" and len(committed) == len(group.actions):
                self._state = SessionState.ACTIVE
                self._state_reason = "verified" if verification is not None else "acted"
            elif status == ActionOutcome.CANCELLED.value and self._state != SessionState.DEGRADED:
                self._state = SessionState.PAUSED
            return ActionGroupResult(
                owner=self.owner,
                lifecycle_epoch=self.lifecycle_epoch,
                model_epoch=self.model_epoch,
                group_id=group.group_id,
                status=status,
                receipts=tuple(receipts),
                committed_prefix=tuple(committed),
                unknown_effects=tuple(unknown),
                final_observation=final_observation,
                verification=verification,
                cleanup=cleanup,
                state=self._state.value,
            )
        finally:
            if lease is not None:
                if not self.input_ownership.release(lease):
                    self._cleanup_degraded = True
                    self._state = SessionState.DEGRADED
                    self._state_reason = "input_release_failed"
            self._leave_operation()

    # Convenient names used by transport adapters.
    act = execute_group
    execute = execute_group

    def _release_adapter_input(self, token: CancellationToken) -> bool:
        function = None
        for name in ("release_input", "release", "release_keys_and_buttons"):
            candidate = getattr(self.adapter, name, None)
            if callable(candidate):
                function = candidate
                break
        if function is None:
            # An adapter with no held-input implementation is not proof that
            # input was released; host qualification must provide this seam.
            return False
        try:
            released = bounded_call(
                lambda: _call_with_contract(
                    function,
                    (self.owner,),
                    {"owner": self.owner, "cancellation": token, "timeout": DEFAULT_STOP_SECONDS},
                ),
                token=token,
                timeout=DEFAULT_STOP_SECONDS,
                operation="input release",
            )
            return released is True
        except BaseException:
            return False

    def stop(self, owner: Any, *, reason: str = "host_stop") -> Dict[str, Any]:
        selected = OwnerIdentity.from_value(owner)
        if selected != self.owner:
            raise LifecycleError("owner_mismatch", "Stop owner does not match the lifecycle owner.", owner=selected)
        if self._settled:
            return self.status()
        self._state = SessionState.STOPPING
        self._state_reason = bounded_text(reason, 256) or "host_stop"
        self._session_token.cancel(self._state_reason)
        cleanup_token = CancellationToken(deadline=self.clock() + DEFAULT_STOP_SECONDS, clock=self.clock)
        abort_function = None
        for name in ("abort", "cancel", "interrupt", "abort_all"):
            candidate = getattr(self.adapter, name, None)
            if callable(candidate):
                abort_function = lambda candidate=candidate: _call_with_contract(
                    candidate,
                    (self.owner,),
                    {"owner": self.owner, "reason": self._state_reason, "cancellation": cleanup_token, "timeout": DEFAULT_STOP_SECONDS},
                )
                break
        if abort_function is not None:
            try:
                bounded_call(
                    abort_function,
                    token=cleanup_token,
                    timeout=DEFAULT_STOP_SECONDS,
                    operation="adapter cancellation",
                )
            except BaseException:
                self._cleanup_degraded = True
        if not self._release_adapter_input(cleanup_token):
            self._cleanup_degraded = True
        self.input_ownership.release_owner(self.owner)
        with self._operation_condition:
            self._last_observation = None
            self._history.clear()
        self._safe_fallback_required = True
        if self._cleanup_degraded:
            self._state = SessionState.DEGRADED
            self._state_reason = "cleanup_failed"
        else:
            self._state = SessionState.PAUSED
            self._state_reason = "fresh_observation_required_after_stop"
        for callback in tuple(self._cleanup_callbacks):
            _best_effort_abort(lambda callback=callback: callback(self.owner, self._state_reason))
        return self.status()

    def settle(self, owner: Any, *, reason: str) -> Dict[str, Any]:
        selected = OwnerIdentity.from_value(owner)
        if selected != self.owner:
            raise LifecycleError("owner_mismatch", "Settlement owner does not match the lifecycle owner.", owner=selected)
        if self._settled:
            return self.status()
        self.stop(selected, reason=reason)
        self._settled = True
        self._settlement_reason = bounded_text(reason, 256)
        self._state = SessionState.DEGRADED if self._cleanup_degraded else SessionState.PAUSED
        self._state_reason = self._settlement_reason
        return self.status()

    def acknowledge_safe_fallback(self, owner: Any) -> Dict[str, Any]:
        selected = self._check_owner(owner)
        self._safe_fallback_required = False
        if self._state == SessionState.DEGRADED and self._state_reason == "cleanup_failed":
            raise LifecycleError("cleanup_failed", "Safe fallback cannot clear a failed input cleanup.", owner=selected)
        self._state = SessionState.PAUSED
        self._state_reason = "observation_required_after_takeover"
        return self.status()

    def status(self) -> Dict[str, Any]:
        lease = self.input_ownership.current
        result: Dict[str, Any] = {
            "owner": self.owner.as_dict(),
            "lifecycle_epoch": self.lifecycle_epoch,
            "model_epoch": self.model_epoch,
            "state": self._state.value,
            "reason": self._state_reason,
            "settled": self._settled,
            "settlement_reason": self._settlement_reason,
            "selected_target": self._selected_target.as_dict(),
            "input_owner": lease.owner.as_dict() if lease is not None else None,
            "safe_fallback_required": self._safe_fallback_required,
            "budget": self.budget.as_dict(),
        }
        if self._last_observation is not None:
            result["last_observation"] = self._last_observation.as_dict()
        return result


class _CombinedCancellationToken(CancellationToken):
    """A small two-parent token used for host and session cancellation."""

    def __init__(self, first: CancellationToken, second: CancellationToken, *, clock: Callable[[], float]) -> None:
        super().__init__(parent=first, clock=clock)
        self.second = second

    @staticmethod
    def _value(token: Any, name: str, default: Any = None) -> Any:
        value = getattr(token, name, default)
        return value() if callable(value) else value

    @property
    def cancelled(self) -> bool:  # type: ignore[override]
        second_cancelled = self._value(self.second, "cancelled", None)
        if second_cancelled is None:
            second_cancelled = self._value(self.second, "is_cancelled", False)
        return super().cancelled or bool(second_cancelled)

    @property
    def reason(self) -> Optional[str]:  # type: ignore[override]
        reason = self._value(self.second, "reason", None)
        return super().reason or (str(reason) if reason else None) or "cancelled"

    def remaining(self) -> Optional[float]:  # type: ignore[override]
        values: List[float] = []
        first = super().remaining()
        if first is not None:
            values.append(first)
        remaining = getattr(self.second, "remaining", None)
        second = remaining() if callable(remaining) else None
        if second is not None:
            try:
                second_value = float(second)
            except (TypeError, ValueError):
                second_value = 0.0
            if math.isfinite(second_value):
                values.append(max(0.0, second_value))
        return min(values) if values else None


@dataclass(frozen=True)
class TakeoverResult:
    owner: OwnerIdentity
    supported: bool
    detected: bool
    safe_fallback_required: bool
    state: str
    detail: str

    def as_dict(self) -> Dict[str, Any]:
        return {
            "owner": self.owner.as_dict(),
            "supported": self.supported,
            "detected": self.detected,
            "safe_fallback_required": self.safe_fallback_required,
            "state": self.state,
            "detail": bounded_text(self.detail, 2048),
        }


# ---------------------------------------------------------------------------
# Manager / host settlement boundary


class LifecycleManager:
    """Own all persistent sessions and host-only lifecycle transitions."""

    def __init__(
        self,
        adapter_factory: Any,
        *,
        policy: Optional[Any] = None,
        approval: Optional[Any] = None,
        budget: Optional[LifecycleBudget] = None,
        clock: Callable[[], float] = time.monotonic,
    ) -> None:
        if adapter_factory is None:
            raise ValueError("an explicit adapter factory is required")
        self.adapter_factory = adapter_factory
        self.policy = policy
        self.approval = approval
        self.budget = budget or LifecycleBudget()
        self.clock = clock
        self.input_ownership = InputOwnership()
        self._sessions: Dict[Tuple[str, str, int], LifecycleSession] = {}
        self._latest_generation: Dict[Tuple[str, str], int] = {}
        self._epochs: Dict[Tuple[str, str, int], int] = {}
        self._lock = threading.RLock()

    def _adapter(self, owner: OwnerIdentity) -> Any:
        factory = self.adapter_factory
        if callable(factory) and not any(callable(getattr(factory, name, None)) for name in ("observe", "capture", "perform_action", "act")):
            return factory(owner)
        return factory

    def open_session(self, owner: Any, target: Any) -> LifecycleSession:
        selected = OwnerIdentity.from_value(owner)
        selected_target = TargetIdentity.from_value(target)
        identity_key = (selected.session_id, selected.extension_instance_id)
        with self._lock:
            latest = self._latest_generation.get(identity_key)
            if latest is not None and selected.process_generation < latest:
                raise LifecycleError("stale_owner", "An older extension-generation owner cannot open a session.", owner=selected)
            if latest is not None and selected.process_generation > latest:
                for old_owner_key, old_session in tuple(self._sessions.items()):
                    if old_owner_key[:2] == identity_key and old_owner_key[2] < selected.process_generation:
                        try:
                            old_session.settle(old_session.owner, reason="extension_reload")
                        except BaseException:
                            pass
            self._latest_generation[identity_key] = max(selected.process_generation, latest or selected.process_generation)
            key = selected.key
            existing = self._sessions.get(key)
            if existing is not None and not existing.settled:
                if existing.selected_target != selected_target:
                    existing.select_target(selected, selected_target)
                return existing
            epoch = self._epochs.get(key, 0) + 1
            self._epochs[key] = epoch
            session = LifecycleSession(
                selected,
                selected_target,
                self._adapter(selected),
                policy=self.policy,
                approval=self.approval,
                budget=self.budget,
                input_ownership=self.input_ownership,
                clock=self.clock,
                lifecycle_epoch=epoch,
            )
            self._sessions[key] = session
            return session

    def session(self, owner: Any) -> LifecycleSession:
        selected = OwnerIdentity.from_value(owner)
        identity_key = (selected.session_id, selected.extension_instance_id)
        with self._lock:
            latest = self._latest_generation.get(identity_key)
            if latest is not None and selected.process_generation < latest:
                raise LifecycleError("stale_owner", "The owner belongs to an older extension generation.", owner=selected)
            session = self._sessions.get(selected.key)
            if session is None:
                raise LifecycleError("session_unavailable", "No persistent lifecycle session exists for this owner.", owner=selected)
            return session

    def observe(self, owner: Any, target: Optional[Any] = None, **kwargs: Any) -> TargetObservation:
        return self.session(owner).observe(owner, target, **kwargs)

    def select_target(self, owner: Any, target: Any) -> Dict[str, Any]:
        return self.session(owner).select_target(owner, target)

    def execute_group(self, owner: Any, actions: Any, **kwargs: Any) -> ActionGroupResult:
        return self.session(owner).execute_group(owner, actions, **kwargs)

    act = execute_group
    execute = execute_group

    def stop(self, owner: Any, *, reason: str = "host_stop") -> Dict[str, Any]:
        return self.session(owner).stop(owner, reason=reason)

    def host_takeover(self, owner: Any, *, safe_fallback: bool = False) -> TakeoverResult:
        """Cancel model work and ask the adapter about physical takeover.

        This method is intentionally named ``host_takeover`` and is not part
        of the typed tool catalog.  A frontend calls it only through a
        host-authorized control event.
        """

        session = self.session(owner)
        session.stop(owner, reason="host_takeover")
        detected: Optional[bool] = None
        function = getattr(session.adapter, "detect_takeover", None)
        if callable(function):
            token = CancellationToken(deadline=self.clock() + DEFAULT_STOP_SECONDS, clock=self.clock)
            try:
                raw = bounded_call(
                    lambda: _call_with_contract(
                        function,
                        (session.owner,),
                        {"owner": session.owner, "cancellation": token, "timeout": DEFAULT_STOP_SECONDS},
                    ),
                    token=token,
                    timeout=DEFAULT_STOP_SECONDS,
                    operation="takeover detection",
                )
                detected = bool(raw)
            except BaseException:
                detected = None
        supported = detected is not None
        if not supported:
            session._safe_fallback_required = True
            session._state = SessionState.PAUSED
            session._state_reason = "takeover_detection_unsupported"
            return TakeoverResult(
                owner=session.owner,
                supported=False,
                detected=False,
                safe_fallback_required=not safe_fallback,
                state=session.state.value,
                detail="Physical takeover detection is unavailable; an explicit safe fallback is required.",
            )
        if not safe_fallback:
            session._safe_fallback_required = True
            session._state = SessionState.PAUSED
            session._state_reason = "safe_fallback_required"
        else:
            session.acknowledge_safe_fallback(owner)
        return TakeoverResult(
            owner=session.owner,
            supported=True,
            detected=bool(detected),
            safe_fallback_required=session._safe_fallback_required,
            state=session.state.value,
            detail="Physical takeover was detected." if detected else "No physical takeover was detected.",
        )

    takeover = host_takeover

    def acknowledge_safe_fallback(self, owner: Any) -> Dict[str, Any]:
        return self.session(owner).acknowledge_safe_fallback(owner)

    def settle(self, owner: Any, *, reason: str) -> Dict[str, Any]:
        session = self.session(owner)
        return session.settle(owner, reason=reason)

    def owner_settled(self, owner: Any, *, reason: str = "owner_settled") -> Dict[str, Any]:
        return self.settle(owner, reason=reason)

    def model_changed(self, owner: Any) -> Dict[str, Any]:
        return self.settle(owner, reason="model_change")

    def extension_reloaded(self, owner: Any) -> Dict[str, Any]:
        return self.settle(owner, reason="extension_reload")

    def frontend_disconnected(self, owner: Any) -> Dict[str, Any]:
        return self.settle(owner, reason="frontend_disconnect")

    def helper_lost(self, owner: Any) -> Dict[str, Any]:
        session = self.session(owner)
        marker = getattr(session.adapter, "helper_lost", None)
        if callable(marker):
            try:
                marker(session.owner)
            except BaseException:
                pass
        return session.settle(owner, reason="helper_loss")

    def timeout(self, owner: Any) -> Dict[str, Any]:
        return self.settle(owner, reason="timeout")

    def statuses(self) -> List[Dict[str, Any]]:
        with self._lock:
            return [session.status() for session in self._sessions.values()]


# Names suitable for host integration and tests.
PersistentLifecycle = LifecycleManager
ComputerUseLifecycle = LifecycleManager
RuntimeLifecycle = LifecycleManager


__all__ = [
    "ActionExecution",
    "ActionGroup",
    "ActionGroupResult",
    "ActionOutcome",
    "ActionReceipt",
    "ActionSpec",
    "AdapterActionError",
    "Budget",
    "BudgetExceeded",
    "BudgetLedger",
    "CancellationToken",
    "ComputerUseAdapter",
    "ComputerUseLifecycle",
    "DenyByDefaultPolicy",
    "EffectState",
    "InputBusy",
    "InputLease",
    "InputOwnership",
    "LifecycleBudget",
    "LifecycleError",
    "LifecycleManager",
    "LifecycleSession",
    "LifecycleState",
    "Observation",
    "OwnerFence",
    "OwnerIdentity",
    "PersistentLifecycle",
    "PolicyDecision",
    "ResourceOwner",
    "RuntimeLifecycle",
    "ScreenshotReference",
    "SessionState",
    "TakeoverResult",
    "TargetBinding",
    "TargetIdentity",
    "TargetObservation",
    "TYPED_ACTIONS",
    "VerificationResult",
    "bounded_call",
    "bounded_text",
]
