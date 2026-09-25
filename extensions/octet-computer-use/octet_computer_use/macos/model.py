"""Bounded data types shared by the dependency-free macOS backend.

The types in this module deliberately contain identity and geometry rather than
opaque window handles. Handles are reacquired by the native adapter for every
operation so a stale observation cannot silently act on a replaced window.
"""

from __future__ import annotations

import math
import re
from dataclasses import dataclass, field
from typing import Any, Mapping, Optional, Tuple


MAX_BUNDLE_ID_BYTES = 256
MAX_PROCESS_TOKEN_BYTES = 128
MAX_NODE_REF_BYTES = 96
MAX_ROLE_BYTES = 96
MAX_LABEL_BYTES = 512
MAX_DESCRIPTION_BYTES = 1_024
MAX_NODES = 512
MAX_DEPTH = 24
MAX_ACTIONS = 32
MAX_CHILDREN = 512
MAX_COORDINATE = 65_536.0
MAX_DIMENSION = 65_536.0
MAX_TEXT_BYTES = 4_096
MAX_SCREENSHOT_BYTES = 5 * 1024 * 1024
MAX_OWNER_BYTES = 256
MAX_GENERATION = 2**63 - 1

_BUNDLE_ID_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9._-]{0,255}$")
_NODE_REF_RE = re.compile(r"^ax:(?:root|[0-9]+(?:\.[0-9]+)*)$")
_CONTROL_RE = re.compile(r"[\x00-\x08\x0b\x0c\x0e-\x1f\x7f]")


class MacOSBackendError(RuntimeError):
    """A bounded, model-safe native backend failure."""

    def __init__(self, code: str, message: str) -> None:
        self.code = _bounded_string(code, "error code", 96)
        self.message = _bounded_string(message, "error message", 2_048)
        super().__init__(self.message)

    def as_dict(self) -> dict[str, str]:
        return {"code": self.code, "message": self.message}


def _bounded_string(value: Any, label: str, maximum: int) -> str:
    if not isinstance(value, str) or not value:
        raise ValueError(f"{label} must be a non-empty string")
    try:
        encoded_length = len(value.encode("utf-8"))
    except UnicodeError as error:
        raise ValueError(f"{label} must be valid UTF-8 text") from error
    if encoded_length > maximum:
        raise ValueError(f"{label} exceeds its bounded size")
    if _CONTROL_RE.search(value):
        raise ValueError(f"{label} contains control characters")
    return value


def _safe_text(value: Any, label: str, maximum: int) -> str:
    if value is None:
        return ""
    if not isinstance(value, str):
        raise ValueError(f"{label} must be text")
    value = value.replace("\x00", "").replace("\r", " ").replace("\n", " ")
    value = "".join(character if ord(character) >= 0x20 else " " for character in value)
    try:
        encoded = value.encode("utf-8")
    except UnicodeError as error:
        raise ValueError(f"{label} must be valid UTF-8 text") from error
    if len(encoded) > maximum:
        value = encoded[:maximum].decode("utf-8", "ignore")
    return value


def _validate_node_ref(value: Any, label: str) -> str:
    if not isinstance(value, str):
        raise ValueError(f"{label} is malformed")
    try:
        encoded_length = len(value.encode("utf-8"))
    except UnicodeError as error:
        raise ValueError(f"{label} is malformed") from error
    if encoded_length > MAX_NODE_REF_BYTES or not _NODE_REF_RE.fullmatch(value):
        raise ValueError(f"{label} is malformed")
    if value == "ax:root":
        return value
    try:
        parts = value[3:].split(".")
        if len(parts) > MAX_DEPTH or any(int(part) > 65_535 for part in parts):
            raise ValueError(f"{label} is outside the bounded range")
    except (TypeError, ValueError, OverflowError) as error:
        raise ValueError(f"{label} is malformed") from error
    return value


def _finite_float(value: Any, label: str) -> float:
    if type(value) is not int and type(value) is not float:
        raise ValueError(f"{label} must be numeric")
    try:
        result = float(value)
    except (TypeError, ValueError, OverflowError) as error:
        raise ValueError(f"{label} must be finite") from error
    if not math.isfinite(result):
        raise ValueError(f"{label} must be finite")
    return result


@dataclass(frozen=True)
class ResourceOwner:
    """Host-derived ownership fence used by the runtime integration."""

    session_id: str
    extension_instance_id: str
    process_generation: int

    def __post_init__(self) -> None:
        try:
            _bounded_string(self.session_id, "session_id", MAX_OWNER_BYTES)
            _bounded_string(self.extension_instance_id, "extension_instance_id", MAX_OWNER_BYTES)
        except (TypeError, ValueError) as error:
            raise ValueError("resource owner identifiers are invalid") from error
        if (
            type(self.process_generation) is not int
            or not 1 <= self.process_generation <= 2**64 - 1
        ):
            raise ValueError("process_generation is outside the bounded range")

    @classmethod
    def from_value(cls, value: Any) -> "ResourceOwner":
        if isinstance(value, Mapping) and "resource_owner" in value:
            value = value.get("resource_owner")
        if not isinstance(value, Mapping):
            raise MacOSBackendError(
                "owner_unavailable",
                "Native macOS operations require a host-derived resource owner.",
            )
        try:
            return cls(
                session_id=value.get("session_id"),
                extension_instance_id=value.get("extension_instance_id"),
                process_generation=value.get("process_generation"),
            )
        except (TypeError, ValueError) as error:
            raise MacOSBackendError(
                "owner_unavailable",
                "Native macOS operations require a valid host-derived resource owner.",
            ) from error

    @property
    def key(self) -> Tuple[str, str, int]:
        return (self.session_id, self.extension_instance_id, self.process_generation)

    def as_dict(self) -> dict[str, Any]:
        return {
            "session_id": self.session_id,
            "extension_instance_id": self.extension_instance_id,
            "process_generation": self.process_generation,
        }


@dataclass(frozen=True)
class TargetIdentity:
    """The exact application process and window an operation may address."""

    bundle_id: str
    pid: int
    window_id: int
    process_start_token: Optional[str] = None

    def __post_init__(self) -> None:
        if (
            not isinstance(self.bundle_id, str)
            or not _BUNDLE_ID_RE.fullmatch(self.bundle_id)
            or len(self.bundle_id.encode("utf-8")) > MAX_BUNDLE_ID_BYTES
        ):
            raise ValueError("bundle_id must be a bounded macOS bundle identifier")
        if (
            not isinstance(self.pid, int)
            or isinstance(self.pid, bool)
            or self.pid < 1
            or self.pid > 2**31 - 1
        ):
            raise ValueError("pid must be a positive process identifier")
        if (
            not isinstance(self.window_id, int)
            or isinstance(self.window_id, bool)
            or self.window_id < 1
            or self.window_id > 2**32 - 1
        ):
            raise ValueError("window_id must be a positive CGWindowID")
        if self.process_start_token is not None:
            _bounded_string(self.process_start_token, "process_start_token", MAX_PROCESS_TOKEN_BYTES)

    @classmethod
    def from_value(cls, value: Any) -> "TargetIdentity":
        if isinstance(value, cls):
            return value
        if not isinstance(value, Mapping):
            raise MacOSBackendError(
                "invalid_target",
                "An explicit bundle_id, pid, and window_id are required.",
            )
        token = value.get("process_start_token")
        if token is None and value.get("process_start_time") is not None:
            token = value.get("process_start_time")
        if token is not None and not isinstance(token, str):
            if isinstance(token, (int, float)) and not isinstance(token, bool):
                try:
                    numeric_token = float(token)
                except (TypeError, ValueError, OverflowError) as error:
                    raise MacOSBackendError("invalid_target", "The process identity token is invalid.") from error
                if not math.isfinite(numeric_token):
                    raise MacOSBackendError("invalid_target", "The process identity token is invalid.")
                token = format(token, ".17g")
            else:
                raise MacOSBackendError("invalid_target", "The process identity token is invalid.")
        try:
            return cls(
                bundle_id=value.get("bundle_id"),
                pid=value.get("pid"),
                window_id=value.get("window_id"),
                process_start_token=token,
            )
        except (TypeError, ValueError) as error:
            raise MacOSBackendError(
                "invalid_target",
                "An explicit bundle_id, pid, and window_id are required and must be bounded.",
            ) from error

    @property
    def key(self) -> Tuple[str, int, int, Optional[str]]:
        return (self.bundle_id, self.pid, self.window_id, self.process_start_token)

    def same_process_window(self, other: "TargetIdentity") -> bool:
        return (
            self.bundle_id == other.bundle_id
            and self.pid == other.pid
            and self.window_id == other.window_id
            and (
                self.process_start_token is None
                or other.process_start_token == self.process_start_token
            )
        )

    def as_dict(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "bundle_id": self.bundle_id,
            "pid": self.pid,
            "window_id": self.window_id,
        }
        if self.process_start_token is not None:
            value["process_start_token"] = self.process_start_token
        return value


@dataclass(frozen=True)
class Point:
    x: float
    y: float

    def __post_init__(self) -> None:
        x = _finite_float(self.x, "x")
        y = _finite_float(self.y, "y")
        if not -MAX_COORDINATE <= x <= MAX_COORDINATE:
            raise ValueError("x is outside the bounded screen coordinate range")
        if not -MAX_COORDINATE <= y <= MAX_COORDINATE:
            raise ValueError("y is outside the bounded screen coordinate range")

    def as_dict(self) -> dict[str, float]:
        return {"x": float(self.x), "y": float(self.y)}

    @classmethod
    def from_value(cls, value: Any, *, label: str = "point") -> "Point":
        if isinstance(value, cls):
            return value
        if not isinstance(value, Mapping):
            raise MacOSBackendError("invalid_geometry", f"{label} must contain x and y coordinates.")
        try:
            return cls(value.get("x"), value.get("y"))
        except (TypeError, ValueError, OverflowError) as error:
            raise MacOSBackendError("invalid_geometry", f"{label} is outside the bounded coordinate range.") from error


@dataclass(frozen=True)
class WindowGeometry:
    x: float
    y: float
    width: float
    height: float

    def __post_init__(self) -> None:
        x = _finite_float(self.x, "x")
        y = _finite_float(self.y, "y")
        width = _finite_float(self.width, "width")
        height = _finite_float(self.height, "height")
        if any(abs(value) > MAX_COORDINATE for value in (x, y, width, height)):
            raise ValueError("window geometry is outside the bounded coordinate range")
        if width <= 0 or height <= 0:
            raise ValueError("window geometry must have positive dimensions")
        if width > MAX_DIMENSION or height > MAX_DIMENSION:
            raise ValueError("window geometry dimensions are too large")
        if x + width > MAX_COORDINATE or y + height > MAX_COORDINATE:
            raise ValueError("window geometry exceeds the bounded screen coordinate range")

    @property
    def right(self) -> float:
        return float(self.x) + float(self.width)

    @property
    def bottom(self) -> float:
        return float(self.y) + float(self.height)

    def contains(self, point: Point, *, margin: float = 0.0) -> bool:
        margin = max(0.0, float(margin))
        return (
            float(self.x) - margin <= point.x <= self.right + margin
            and float(self.y) - margin <= point.y <= self.bottom + margin
        )

    def center(self) -> Point:
        return Point(float(self.x) + float(self.width) / 2.0, float(self.y) + float(self.height) / 2.0)

    def approximately_equals(self, other: "WindowGeometry", tolerance: float = 0.5) -> bool:
        return all(
            abs(float(left) - float(right)) <= tolerance
            for left, right in zip(
                (self.x, self.y, self.width, self.height),
                (other.x, other.y, other.width, other.height),
            )
        )

    def as_dict(self) -> dict[str, float]:
        return {
            "x": float(self.x),
            "y": float(self.y),
            "width": float(self.width),
            "height": float(self.height),
        }

    @classmethod
    def from_value(cls, value: Any, *, label: str = "geometry") -> "WindowGeometry":
        if isinstance(value, cls):
            return value
        if not isinstance(value, Mapping):
            raise MacOSBackendError("invalid_geometry", f"{label} is unavailable or malformed.")
        try:
            return cls(
                value.get("x"),
                value.get("y"),
                value.get("width"),
                value.get("height"),
            )
        except (TypeError, ValueError, OverflowError) as error:
            raise MacOSBackendError("invalid_geometry", f"{label} is unavailable or malformed.") from error


@dataclass(frozen=True)
class WindowSnapshot:
    identity: TargetIdentity
    title: str
    geometry: WindowGeometry
    owner_name: str = ""
    frontmost: bool = False

    def __post_init__(self) -> None:
        if not isinstance(self.identity, TargetIdentity):
            raise ValueError("window identity is invalid")
        if not isinstance(self.geometry, WindowGeometry):
            raise ValueError("window geometry is invalid")
        if not isinstance(self.title, str) or not isinstance(self.owner_name, str):
            raise ValueError("window text fields must be text")
        if type(self.frontmost) is not bool:
            raise ValueError("frontmost must be a boolean")
        _safe_text(self.title, "window title", MAX_LABEL_BYTES)
        _safe_text(self.owner_name, "owner name", MAX_LABEL_BYTES)

    def as_dict(self) -> dict[str, Any]:
        return {
            "identity": self.identity.as_dict(),
            "title": _safe_text(self.title, "window title", MAX_LABEL_BYTES),
            "geometry": self.geometry.as_dict(),
            "owner_name": _safe_text(self.owner_name, "owner name", MAX_LABEL_BYTES),
            "frontmost": bool(self.frontmost),
        }


@dataclass(frozen=True)
class AccessibilityNode:
    """A value-free, bounded accessibility node.

    Editable values are intentionally not represented. ``path`` is resolved
    again by the native adapter and is only usable with its matching
    observation generation.
    """

    path: Tuple[int, ...]
    role: str
    title: str = ""
    description: str = ""
    bounds: Optional[WindowGeometry] = None
    enabled: bool = True
    focused: bool = False
    selected: bool = False
    editable: bool = False
    sensitive: bool = False
    actions: Tuple[str, ...] = field(default_factory=tuple)
    children: Tuple[str, ...] = field(default_factory=tuple)

    def __post_init__(self) -> None:
        if not isinstance(self.path, tuple):
            raise ValueError("accessibility path must be a tuple")
        if len(self.path) > MAX_DEPTH or any(
            not isinstance(index, int) or isinstance(index, bool) or index < 0 or index > 65_535
            for index in self.path
        ):
            raise ValueError("accessibility path is outside the bounded range")
        _bounded_string(self.role, "accessibility role", MAX_ROLE_BYTES)
        if not isinstance(self.title, str) or not isinstance(self.description, str):
            raise ValueError("accessibility text fields must be text")
        _safe_text(self.title, "accessibility title", MAX_LABEL_BYTES)
        _safe_text(self.description, "accessibility description", MAX_DESCRIPTION_BYTES)
        for name, value in (
            ("enabled", self.enabled),
            ("focused", self.focused),
            ("selected", self.selected),
            ("editable", self.editable),
            ("sensitive", self.sensitive),
        ):
            if type(value) is not bool:
                raise ValueError(f"{name} must be a boolean")
        if not isinstance(self.actions, tuple) or len(self.actions) > MAX_ACTIONS:
            raise ValueError("accessibility actions are outside the bounded range")
        for action in self.actions:
            _bounded_string(action, "accessibility action", MAX_ROLE_BYTES)
        if not isinstance(self.children, tuple) or len(self.children) > MAX_CHILDREN:
            raise ValueError("accessibility children are outside the bounded range")
        for child in self.children:
            try:
                _validate_node_ref(child, "accessibility child reference")
            except (TypeError, ValueError) as error:
                raise ValueError("accessibility child reference is malformed") from error
        if self.bounds is not None and not isinstance(self.bounds, WindowGeometry):
            raise ValueError("accessibility bounds are invalid")
        try:
            _bounded_string(self.ref, "accessibility node reference", MAX_NODE_REF_BYTES)
        except (TypeError, ValueError) as error:
            raise ValueError("accessibility path is outside the bounded reference range") from error

    @property
    def ref(self) -> str:
        if not self.path:
            return "ax:root"
        return "ax:" + ".".join(str(index) for index in self.path)

    def as_dict(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "ref": self.ref,
            "role": self.role,
            "title": _safe_text(self.title, "accessibility title", MAX_LABEL_BYTES),
            "description": _safe_text(self.description, "accessibility description", MAX_DESCRIPTION_BYTES),
            "enabled": bool(self.enabled),
            "focused": bool(self.focused),
            "selected": bool(self.selected),
            "editable": bool(self.editable),
            "sensitive": bool(self.sensitive),
            "actions": list(self.actions),
            "children": list(self.children),
        }
        if self.bounds is not None:
            value["bounds"] = self.bounds.as_dict()
        return value


@dataclass(frozen=True)
class AccessibilityTree:
    nodes: Tuple[AccessibilityNode, ...]
    truncated: bool = False

    def __post_init__(self) -> None:
        if not isinstance(self.nodes, tuple) or len(self.nodes) > MAX_NODES:
            raise ValueError("accessibility tree exceeds the bounded node limit")
        if type(self.truncated) is not bool:
            raise ValueError("accessibility tree truncation flag must be a boolean")
        seen: set[str] = set()
        for node in self.nodes:
            if not isinstance(node, AccessibilityNode):
                raise ValueError("accessibility tree contains an invalid node")
            if node.ref in seen:
                raise ValueError("accessibility tree contains duplicate node references")
            seen.add(node.ref)

    def by_ref(self, ref: str) -> Optional[AccessibilityNode]:
        return next((node for node in self.nodes if node.ref == ref), None)

    def as_dict(self) -> dict[str, Any]:
        return {
            "begin_marker": "BEGIN UNTRUSTED NATIVE CONTENT",
            "nodes": [node.as_dict() for node in self.nodes],
            "truncated": bool(self.truncated),
            "end_marker": "END UNTRUSTED NATIVE CONTENT",
        }


@dataclass(frozen=True)
class Observation:
    generation: int
    target: TargetIdentity
    window: WindowSnapshot
    accessibility: AccessibilityTree

    def __post_init__(self) -> None:
        if type(self.generation) is not int or not 1 <= self.generation <= MAX_GENERATION:
            raise ValueError("observation generation is outside the bounded range")
        if not isinstance(self.target, TargetIdentity):
            raise ValueError("observation target is invalid")
        if not isinstance(self.window, WindowSnapshot):
            raise ValueError("observation window is invalid")
        if not isinstance(self.accessibility, AccessibilityTree):
            raise ValueError("observation accessibility tree is invalid")
        if self.window.identity != self.target:
            raise ValueError("observation target and window identity differ")

    def as_dict(self) -> dict[str, Any]:
        return {
            "schema": "octet.macos.observation.v1",
            "generation": self.generation,
            "target": self.target.as_dict(),
            "window": self.window.as_dict(),
            "accessibility": self.accessibility.as_dict(),
        }


@dataclass(frozen=True)
class PermissionReport:
    supported: bool
    accessibility: str
    screen_recording: str
    synthetic_input: str
    detail: str = ""

    def __post_init__(self) -> None:
        if type(self.supported) is not bool:
            raise ValueError("permission support flag must be a boolean")
        valid_states = {"granted", "denied", "unsupported", "not_opted_in", "unknown"}
        for name, value in (
            ("accessibility", self.accessibility),
            ("screen_recording", self.screen_recording),
            ("synthetic_input", self.synthetic_input),
        ):
            if not isinstance(value, str) or value not in valid_states:
                raise ValueError(f"{name} permission state is invalid")
        if not isinstance(self.detail, str):
            raise ValueError("permission detail must be text")
        _safe_text(self.detail, "permission detail", MAX_DESCRIPTION_BYTES)

    @property
    def ready(self) -> bool:
        return self.supported and all(
            state == "granted"
            for state in (self.accessibility, self.screen_recording, self.synthetic_input)
        )

    def as_dict(self) -> dict[str, Any]:
        return {
            "supported": bool(self.supported),
            "accessibility": self.accessibility,
            "screen_recording": self.screen_recording,
            "synthetic_input": self.synthetic_input,
            "ready": self.ready,
            "detail": _safe_text(self.detail, "permission detail", MAX_DESCRIPTION_BYTES),
            "requests_permissions": False,
        }


@dataclass(frozen=True)
class ConfirmationRequest:
    operation: str
    target: TargetIdentity
    ref: Optional[str] = None
    consequence: str = "The selected native target will receive an input action."

    def __post_init__(self) -> None:
        _bounded_string(self.operation, "confirmation operation", MAX_ROLE_BYTES)
        if not isinstance(self.target, TargetIdentity):
            raise ValueError("confirmation target is invalid")
        if self.ref is not None:
            try:
                _validate_node_ref(self.ref, "confirmation node reference")
            except (TypeError, ValueError) as error:
                raise ValueError("confirmation node reference is malformed") from error
        if not isinstance(self.consequence, str):
            raise ValueError("confirmation consequence must be text")
        _safe_text(self.consequence, "confirmation consequence", MAX_DESCRIPTION_BYTES)

    def as_dict(self) -> dict[str, Any]:
        value: dict[str, Any] = {
            "operation": self.operation,
            "target": self.target.as_dict(),
            "consequence": self.consequence,
        }
        if self.ref is not None:
            value["ref"] = self.ref
        return value


__all__ = [
    "AccessibilityNode",
    "AccessibilityTree",
    "ConfirmationRequest",
    "MacOSBackendError",
    "Observation",
    "PermissionReport",
    "Point",
    "ResourceOwner",
    "TargetIdentity",
    "WindowGeometry",
    "WindowSnapshot",
]
