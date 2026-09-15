"""Fail-closed native Windows computer-use backend.

This module deliberately keeps the Windows implementation behind a small
``Win32System`` adapter.  The adapter uses only Win32 APIs and the Python
standard library; tests can provide a fully deterministic adapter without
making the production backend trust test data.  A caller must identify one
specific application before any input is sent.  HWND, process creation time,
application identity, desktop, integrity, and foreground state are checked
again immediately before input.

The module is importable on every platform, but constructing the default
adapter on a non-Windows platform is rejected.  There is no desktop-wide
fallback and no mock is used by the production adapter.
"""

from __future__ import annotations

import ctypes
import hashlib
import ntpath
import os
import re
import stat
import struct
import sys
import threading
import time
from ctypes import wintypes
from dataclasses import dataclass, field
from typing import Any, Callable, Dict, Iterable, List, Mapping, Optional, Protocol, Sequence, Set, Tuple, Union


_IS_WINDOWS = sys.platform == "win32"

# All limits are intentionally conservative.  They apply before data is sent
# to a Win32 API and also apply to values returned by an adapter.
MAX_TITLE_CHARS = 512
MAX_CLASS_CHARS = 256
MAX_AUMID_CHARS = 512
MAX_IDENTITY_FIELD_CHARS = 1024
MAX_ENUMERATED_WINDOWS = 4096
MAX_TOKEN_INFO_BYTES = 64 * 1024
MAX_VERSION_INFO_BYTES = 4 * 1024 * 1024
MAX_TEXT_CHARS = 4096
MAX_TEXT_BYTES = 16 * 1024
MAX_ACTIONS = 64
MAX_FRAME_WIDTH = 4096
MAX_FRAME_HEIGHT = 4096
MAX_FRAME_PIXELS = 16 * 1024 * 1024
MAX_FRAME_BYTES = 64 * 1024 * 1024
MAX_BINARY_HASH_BYTES = 512 * 1024 * 1024
MAX_OPERATION_TIMEOUT_MS = 30_000
MAX_DRAG_DURATION_MS = 2_000
MAX_DRAG_DISTANCE = 16_384
MAX_SCROLL_DELTA = 2_000
MAX_COORDINATE_ARGUMENT = 1_000_000
MAX_DRAG_STEPS = 128

INTEGRITY_UNTRUSTED = 0x0000
INTEGRITY_LOW = 0x1000
INTEGRITY_MEDIUM = 0x2000
INTEGRITY_HIGH = 0x3000
INTEGRITY_SYSTEM = 0x4000

SECURE_DESKTOP_NAMES = frozenset(
    {
        "winlogon",
        "screensaver",
        "screen-saver",
        "disconnect",
        "secure",
        "credential",
    }
)
SECURITY_UI_EXECUTABLES = frozenset(
    {
        "consent.exe",
        "credentialui.dll",
        "credwiz.exe",
        "logonui.exe",
        "winlogon.exe",
        "runas.exe",
    }
)
SENSITIVE_FIELD_ROLES = frozenset(
    {
        "password",
        "credential",
        "username",
        "user-name",
        "pin",
        "otp",
        "one-time-password",
        "verification-code",
        "security-code",
        "secret",
        "token",
        "payment",
        "card-number",
        "credit-card",
        "debit-card",
        "cvv",
        "cvc",
        "ssn",
        "authentication",
        "auth",
    }
)
SAFE_TEXT_FIELD_ROLES = frozenset(
    {
        "text",
        "search",
        "name",
        "address",
        "non-sensitive",
        "non_sensitive",
    }
)

# This is deliberately navigation-only.  Modifier chords, shell keys, and
# arbitrary virtual keys are not exposed by the retained backend.
KEY_VIRTUAL_CODES: Dict[str, int] = {
    "BACKSPACE": 0x08,
    "TAB": 0x09,
    "ENTER": 0x0D,
    "ESCAPE": 0x1B,
    "SPACE": 0x20,
    "PAGEUP": 0x21,
    "PAGEDOWN": 0x22,
    "END": 0x23,
    "HOME": 0x24,
    "LEFT": 0x25,
    "UP": 0x26,
    "RIGHT": 0x27,
    "DOWN": 0x28,
    "INSERT": 0x2D,
    "DELETE": 0x2E,
    "F1": 0x70,
    "F2": 0x71,
    "F3": 0x72,
    "F4": 0x73,
    "F5": 0x74,
    "F6": 0x75,
    "F7": 0x76,
    "F8": 0x77,
    "F9": 0x78,
    "F10": 0x79,
    "F11": 0x7A,
    "F12": 0x7B,
}
# These keys can submit, delete, or otherwise commit an operation.  The host
# must provide an explicit approval bit or callback for them.
CONSEQUENTIAL_KEYS = frozenset({"ENTER", "SPACE", "DELETE", "INSERT"})


class BackendError(RuntimeError):
    """A stable, non-secret error returned by the backend."""

    def __init__(self, code: str, message: str):
        self.code = str(code)
        self.message = str(message)[:512]
        super().__init__(self.message)


class OperationCancelled(BackendError):
    def __init__(self, message: str = "operation cancelled"):
        super().__init__("cancelled", message)


class OperationTimedOut(BackendError):
    def __init__(self, message: str = "operation timed out"):
        super().__init__("timeout", message)


ComputerUseError = BackendError
WindowsBackendError = BackendError


def _check_string(value: Any, name: str, limit: int, *, allow_empty: bool = False) -> str:
    if not isinstance(value, str):
        raise BackendError("invalid_input", f"{name} must be a string")
    if not allow_empty and not value:
        raise BackendError("invalid_input", f"{name} must not be empty")
    if len(value) > limit:
        raise BackendError("input_too_large", f"{name} exceeds its size limit")
    if any(ord(char) < 0x20 or 0xD800 <= ord(char) <= 0xDFFF for char in value):
        raise BackendError("invalid_input", f"{name} contains a control or surrogate character")
    return value


def _optional_string(value: Any, name: str, limit: int) -> Optional[str]:
    if value is None:
        return None
    return _check_string(value, name, limit)


def _bounded_int(value: Any, name: str, lower: int, upper: int) -> int:
    if isinstance(value, bool) or not isinstance(value, int):
        raise BackendError("invalid_input", f"{name} must be an integer")
    if value < lower or value > upper:
        raise BackendError("input_out_of_bounds", f"{name} is outside its permitted range")
    return value


def _strict_bool(value: Any, name: str) -> bool:
    if not isinstance(value, bool):
        raise BackendError("invalid_input", f"{name} must be boolean")
    return value


def _native_bool(value: Any, name: str) -> bool:
    """Accept only the two values permitted by a Win32 BOOL result."""
    if isinstance(value, bool):
        return value
    if isinstance(value, int) and value in (0, 1):
        return bool(value)
    raw = getattr(value, "value", None)
    if isinstance(raw, bool):
        return raw
    if isinstance(raw, int) and not isinstance(raw, bool) and raw in (0, 1):
        return bool(raw)
    raise BackendError("backend_unavailable", f"{name} returned a non-boolean result")


def _canonical_windows_path(value: str) -> str:
    """Canonicalize only the comparison form of a Windows path.

    The process path is obtained from QueryFullProcessImageNameW.  This
    function does not resolve a new path from user input, which avoids turning
    target selection into an uncontrolled filesystem lookup.
    """

    value = value.replace("/", "\\")
    if value.startswith("\\\\?\\UNC\\"):
        value = "\\\\" + value[8:]
    elif value.startswith("\\\\?\\"):
        value = value[4:]
    return ntpath.normcase(ntpath.normpath(value)).casefold()


def _binary_name(path: Optional[str]) -> str:
    if not path:
        return ""
    return path.replace("/", "\\").rsplit("\\", 1)[-1].casefold()


def _coerce_rect(value: Any, name: str = "rect") -> "Rect":
    if isinstance(value, Rect):
        return value
    if isinstance(value, Mapping):
        try:
            coordinates = (
                _bounded_int(value["left"], f"{name}.left", -(1 << 63), (1 << 63) - 1),
                _bounded_int(value["top"], f"{name}.top", -(1 << 63), (1 << 63) - 1),
                _bounded_int(value["right"], f"{name}.right", -(1 << 63), (1 << 63) - 1),
                _bounded_int(value["bottom"], f"{name}.bottom", -(1 << 63), (1 << 63) - 1),
            )
        except (KeyError, TypeError):
            raise BackendError("invalid_input", f"{name} is malformed")
        return Rect(*coordinates)
    if isinstance(value, (tuple, list)) and len(value) == 4:
        try:
            coordinates = tuple(
                _bounded_int(item, f"{name} coordinate", -(1 << 63), (1 << 63) - 1)
                for item in value
            )
        except (TypeError, ValueError):
            pass
        else:
            return Rect(*coordinates)
    raise BackendError("invalid_input", f"{name} is malformed")


@dataclass(frozen=True)
class Rect:
    left: int
    top: int
    right: int
    bottom: int

    def __post_init__(self) -> None:
        for value, name in (
            (self.left, "rect.left"),
            (self.top, "rect.top"),
            (self.right, "rect.right"),
            (self.bottom, "rect.bottom"),
        ):
            _bounded_int(value, name, -(1 << 63), (1 << 63) - 1)

    @property
    def width(self) -> int:
        return self.right - self.left

    @property
    def height(self) -> int:
        return self.bottom - self.top

    @property
    def is_valid(self) -> bool:
        return self.width > 0 and self.height > 0

    def contains(self, x: int, y: int) -> bool:
        return self.left <= x < self.right and self.top <= y < self.bottom


@dataclass(frozen=True)
class DesktopIdentity:
    name: str
    station: str = "WinSta0"
    session_id: int = 0
    secure: bool = False

    def __post_init__(self) -> None:
        _check_string(self.name, "desktop name", MAX_CLASS_CHARS)
        _check_string(self.station, "window station", MAX_CLASS_CHARS)
        _bounded_int(self.session_id, "session id", 0, 0xFFFFFFFF)
        _strict_bool(self.secure, "desktop security state")

    @property
    def is_secure(self) -> bool:
        return self.secure or self.name.casefold() in SECURE_DESKTOP_NAMES

    def same_as(self, other: "DesktopIdentity") -> bool:
        return (
            isinstance(other, DesktopIdentity)
            and self.session_id == other.session_id
            and self.name.casefold() == other.name.casefold()
            and self.station.casefold() == other.station.casefold()
        )


@dataclass(frozen=True)
class ApplicationIdentity:
    """Identity observed from the target process.

    ``signature_valid`` is set only after WinVerifyTrust succeeds in the
    native adapter.  AUMID is read from the process token, not from a window
    title or caller-provided label.
    """

    aumid: Optional[str] = None
    publisher: Optional[str] = None
    product: Optional[str] = None
    binary_path: Optional[str] = None
    binary_sha256: Optional[str] = None
    signature_valid: bool = False

    def __post_init__(self) -> None:
        for value, name, limit in (
            (self.aumid, "AUMID", MAX_AUMID_CHARS),
            (self.publisher, "publisher", MAX_IDENTITY_FIELD_CHARS),
            (self.product, "product", MAX_IDENTITY_FIELD_CHARS),
            (self.binary_path, "binary path", MAX_IDENTITY_FIELD_CHARS),
        ):
            if value is not None:
                _check_string(value, name, limit)
        if self.binary_sha256 is not None:
            digest = _check_string(self.binary_sha256, "binary SHA-256", 64)
            if re.fullmatch(r"[0-9a-fA-F]{64}", digest) is None:
                raise BackendError("invalid_input", "binary SHA-256 is malformed")
        if not isinstance(self.signature_valid, bool):
            raise BackendError("invalid_input", "signature validity must be boolean")

    @property
    def binary(self) -> Optional[str]:
        return self.binary_path

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "ApplicationIdentity":
        return cls(
            aumid=value.get("aumid"),
            publisher=value.get("publisher"),
            product=value.get("product"),
            binary_path=value.get("binary_path", value.get("binary")),
            binary_sha256=value.get("binary_sha256", value.get("sha256")),
            signature_valid=_strict_bool(value.get("signature_valid", False), "signature validity"),
        )


@dataclass(frozen=True)
class TargetSpec:
    """Exact application identity and optional window filters.

    A target is either selected by AUMID, or by a signed publisher/product and
    a binary path or digest.  Window titles and classes are filters only; they
    are never treated as application identity.
    """

    aumid: Optional[str] = None
    publisher: Optional[str] = None
    product: Optional[str] = None
    binary_path: Optional[str] = None
    binary_sha256: Optional[str] = None
    window_title: Optional[str] = None
    window_class: Optional[str] = None
    max_integrity_level: Optional[int] = None

    @classmethod
    def from_mapping(cls, value: Union["TargetSpec", Mapping[str, Any]]) -> "TargetSpec":
        if isinstance(value, TargetSpec):
            value.validate()
            return value
        if not isinstance(value, Mapping):
            raise BackendError("invalid_target", "target must be a TargetSpec or mapping")
        allowed = {
            "aumid",
            "publisher",
            "product",
            "binary",
            "binary_path",
            "binary_sha256",
            "sha256",
            "window_title",
            "window_class",
            "max_integrity_level",
            "integrity_level",
        }
        unknown = set(value) - allowed
        if unknown:
            raise BackendError("invalid_target", "target contains an unsupported field")
        binary_path = value.get("binary_path", value.get("binary"))
        digest = value.get("binary_sha256", value.get("sha256"))
        max_integrity = value.get("max_integrity_level", value.get("integrity_level"))
        result = cls(
            aumid=value.get("aumid"),
            publisher=value.get("publisher"),
            product=value.get("product"),
            binary_path=binary_path,
            binary_sha256=digest,
            window_title=value.get("window_title"),
            window_class=value.get("window_class"),
            max_integrity_level=max_integrity,
        )
        result.validate()
        return result

    def validate(self) -> None:
        if self.aumid is not None:
            _check_string(self.aumid, "AUMID", MAX_AUMID_CHARS)
        if self.publisher is not None:
            _check_string(self.publisher, "publisher", MAX_IDENTITY_FIELD_CHARS)
        if self.product is not None:
            _check_string(self.product, "product", MAX_IDENTITY_FIELD_CHARS)
        if self.binary_path is not None:
            _check_string(self.binary_path, "binary path", MAX_IDENTITY_FIELD_CHARS)
        if self.binary_sha256 is not None:
            digest = _check_string(self.binary_sha256, "binary SHA-256", 64)
            if re.fullmatch(r"[0-9a-fA-F]{64}", digest) is None:
                raise BackendError("invalid_target", "binary SHA-256 is malformed")
        if self.window_title is not None:
            _check_string(self.window_title, "window title", MAX_TITLE_CHARS, allow_empty=True)
        if self.window_class is not None:
            _check_string(self.window_class, "window class", MAX_CLASS_CHARS)
        if self.max_integrity_level is not None:
            _bounded_int(self.max_integrity_level, "maximum integrity level", 0, INTEGRITY_SYSTEM)

        if self.aumid is None:
            if not self.publisher or not self.product or not (self.binary_path or self.binary_sha256):
                raise BackendError(
                    "invalid_target",
                    "target requires an AUMID or signed publisher/product/binary identity",
                )
        elif (self.publisher is None) != (self.product is None):
            raise BackendError("invalid_target", "publisher and product must be supplied together")

    def match_reason(self, window: "WindowIdentity") -> Optional[str]:
        app = window.app
        if not app.binary_path:
            return "binary_path_unavailable"
        if self.aumid is not None:
            if app.aumid != self.aumid:
                return "aumid_mismatch"
        else:
            if not app.signature_valid:
                return "binary_signature_unverified"
        if self.publisher is not None:
            if not app.signature_valid:
                return "binary_signature_unverified"
            if app.publisher != self.publisher:
                return "publisher_mismatch"
        if self.product is not None and app.product != self.product:
            return "product_mismatch"
        if self.binary_path is not None:
            if _canonical_windows_path(app.binary_path) != _canonical_windows_path(self.binary_path):
                return "binary_path_mismatch"
            if not app.signature_valid and self.aumid is None:
                return "binary_signature_unverified"
        if self.binary_sha256 is not None:
            if app.binary_sha256 is None:
                return "binary_hash_unavailable"
            if app.binary_sha256.casefold() != self.binary_sha256.casefold():
                return "binary_hash_mismatch"
        if self.window_class is not None and window.class_name != self.window_class:
            return "window_class_mismatch"
        if self.window_title is not None and window.title != self.window_title:
            return "window_title_mismatch"
        return None

    def matches(self, window: "WindowIdentity") -> bool:
        return self.match_reason(window) is None


@dataclass(frozen=True)
class WindowIdentity:
    hwnd: int
    process_id: int
    process_start_time: int
    app: ApplicationIdentity
    desktop: DesktopIdentity
    class_name: str = ""
    title: str = ""
    thread_id: int = 0
    parent_hwnd: int = 0
    owner_hwnd: int = 0
    integrity_level: int = INTEGRITY_MEDIUM
    rect: Rect = field(default_factory=lambda: Rect(0, 0, 0, 0))
    client_rect: Rect = field(default_factory=lambda: Rect(0, 0, 0, 0))
    visible: bool = True
    enabled: bool = True
    minimized: bool = False
    security_ui: bool = False

    def __post_init__(self) -> None:
        _bounded_int(self.hwnd, "HWND", 1, (1 << 63) - 1)
        _bounded_int(self.process_id, "process id", 1, 0xFFFFFFFF)
        _bounded_int(self.process_start_time, "process creation time", 1, (1 << 63) - 1)
        _check_string(self.class_name, "window class", MAX_CLASS_CHARS, allow_empty=True)
        _check_string(self.title, "window title", MAX_TITLE_CHARS, allow_empty=True)
        _bounded_int(self.thread_id, "thread id", 0, 0xFFFFFFFF)
        _bounded_int(self.parent_hwnd, "parent HWND", 0, (1 << 63) - 1)
        _bounded_int(self.owner_hwnd, "owner HWND", 0, (1 << 63) - 1)
        _bounded_int(self.integrity_level, "integrity level", 0, INTEGRITY_SYSTEM)
        if not isinstance(self.app, ApplicationIdentity) or not isinstance(self.desktop, DesktopIdentity):
            raise BackendError("invalid_input", "window identity is malformed")
        if not isinstance(self.visible, bool) or not isinstance(self.enabled, bool):
            raise BackendError("invalid_input", "window visibility is malformed")
        if not isinstance(self.minimized, bool) or not isinstance(self.security_ui, bool):
            raise BackendError("invalid_input", "window state is malformed")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "WindowIdentity":
        app_value = value.get("app", value.get("application"))
        desktop_value = value.get("desktop")
        if isinstance(app_value, Mapping):
            app = ApplicationIdentity.from_mapping(app_value)
        elif isinstance(app_value, ApplicationIdentity):
            app = app_value
        else:
            raise BackendError("invalid_input", "window application identity is missing")
        if isinstance(desktop_value, Mapping):
            try:
                session_id = desktop_value.get("session_id", 0)
            except AttributeError:
                raise BackendError("invalid_input", "window desktop identity is malformed")
            desktop = DesktopIdentity(
                name=desktop_value.get("name", ""),
                station=desktop_value.get("station", "WinSta0"),
                session_id=_bounded_int(session_id, "session id", 0, 0xFFFFFFFF),
                secure=_strict_bool(desktop_value.get("secure", False), "desktop security state"),
            )
        elif isinstance(desktop_value, DesktopIdentity):
            desktop = desktop_value
        else:
            raise BackendError("invalid_input", "window desktop identity is missing")
        try:
            hwnd = value["hwnd"]
            process_id = value["process_id"]
            process_start_time = value["process_start_time"]
        except (KeyError, TypeError):
            raise BackendError("invalid_input", "window identity is missing required fields")
        return cls(
            hwnd=_bounded_int(hwnd, "HWND", 1, (1 << 63) - 1),
            process_id=_bounded_int(process_id, "process id", 1, 0xFFFFFFFF),
            process_start_time=_bounded_int(process_start_time, "process creation time", 1, (1 << 63) - 1),
            app=app,
            desktop=desktop,
            class_name=value.get("class_name", ""),
            title=value.get("title", ""),
            thread_id=_bounded_int(value.get("thread_id", 0), "thread id", 0, 0xFFFFFFFF),
            parent_hwnd=_bounded_int(value.get("parent_hwnd", 0), "parent HWND", 0, (1 << 63) - 1),
            owner_hwnd=_bounded_int(value.get("owner_hwnd", 0), "owner HWND", 0, (1 << 63) - 1),
            integrity_level=_bounded_int(value.get("integrity_level", INTEGRITY_MEDIUM), "integrity level", 0, INTEGRITY_SYSTEM),
            rect=_coerce_rect(value.get("rect", (0, 0, 0, 0))),
            client_rect=_coerce_rect(value.get("client_rect", (0, 0, 0, 0))),
            visible=_strict_bool(value.get("visible", True), "window visibility"),
            enabled=_strict_bool(value.get("enabled", True), "window enabled state"),
            minimized=_strict_bool(value.get("minimized", False), "window minimized state"),
            security_ui=_strict_bool(value.get("security_ui", False), "window security state"),
        )

    def same_instance(self, other: "WindowIdentity") -> bool:
        """Return whether two observations are the same HWND/process instance."""

        return (
            isinstance(other, WindowIdentity)
            and self.hwnd == other.hwnd
            and self.process_id == other.process_id
            and self.process_start_time == other.process_start_time
            and self.thread_id == other.thread_id
            and self.class_name == other.class_name
            and self.parent_hwnd == other.parent_hwnd
            and self.owner_hwnd == other.owner_hwnd
            and self.integrity_level == other.integrity_level
            and self.desktop.same_as(other.desktop)
            and self.app == other.app
        )


@dataclass(frozen=True)
class Screenshot:
    data: bytes
    width: int
    height: int
    stride: int
    format: str = "bmp"

    def __post_init__(self) -> None:
        if not isinstance(self.data, (bytes, bytearray, memoryview)):
            raise BackendError("observation_invalid", "screenshot data is not bytes")
        data = bytes(self.data)
        object.__setattr__(self, "data", data)
        _bounded_int(self.width, "screenshot width", 1, MAX_FRAME_WIDTH)
        _bounded_int(self.height, "screenshot height", 1, MAX_FRAME_HEIGHT)
        _bounded_int(self.stride, "screenshot stride", 1, MAX_FRAME_BYTES)
        _check_string(self.format, "screenshot format", 32)
        if self.width * self.height > MAX_FRAME_PIXELS:
            raise BackendError("observation_too_large", "screenshot pixel count exceeds its limit")
        if self.stride * self.height > MAX_FRAME_BYTES:
            raise BackendError("observation_too_large", "screenshot stride exceeds its limit")
        if len(data) == 0 or len(data) > MAX_FRAME_BYTES:
            raise BackendError("observation_too_large", "screenshot byte count exceeds its limit")

    @classmethod
    def from_mapping(cls, value: Mapping[str, Any]) -> "Screenshot":
        return cls(
            data=value.get("data", b""),
            width=value.get("width", 0),
            height=value.get("height", 0),
            stride=value.get("stride", 0),
            format=value.get("format", "bmp"),
        )


@dataclass(frozen=True)
class Observation:
    identity: WindowIdentity
    screenshot: Screenshot
    desktop: DesktopIdentity
    captured_at: float

    @property
    def data(self) -> bytes:
        return self.screenshot.data

    @property
    def width(self) -> int:
        return self.screenshot.width

    @property
    def height(self) -> int:
        return self.screenshot.height

    def as_dict(self) -> Dict[str, Any]:
        return {
            "hwnd": self.identity.hwnd,
            "process_id": self.identity.process_id,
            "process_start_time": self.identity.process_start_time,
            "desktop": self.desktop.name,
            "width": self.width,
            "height": self.height,
            "stride": self.screenshot.stride,
            "format": self.screenshot.format,
            "data": self.data,
        }


@dataclass(frozen=True)
class InputFieldInfo:
    """Trusted accessibility classification for a text input location."""

    window_hwnd: int
    role: str
    sensitive: bool = False
    enabled: bool = True
    rect: Optional[Rect] = None

    def __post_init__(self) -> None:
        _bounded_int(self.window_hwnd, "input field HWND", 1, (1 << 63) - 1)
        _check_string(self.role, "input field role", 64)
        if not isinstance(self.sensitive, bool) or not isinstance(self.enabled, bool):
            raise BackendError("invalid_input", "input field state is malformed")

    @classmethod
    def from_value(cls, value: Union["InputFieldInfo", Mapping[str, Any]]) -> "InputFieldInfo":
        if isinstance(value, InputFieldInfo):
            return value
        if not isinstance(value, Mapping):
            raise BackendError("input_rejected", "input field classification is malformed")
        rect = value.get("rect")
        return cls(
            window_hwnd=_bounded_int(value.get("window_hwnd", value.get("hwnd", 0)), "input field HWND", 1, (1 << 63) - 1),
            role=value.get("role", ""),
            sensitive=_strict_bool(value.get("sensitive", False), "input field sensitivity"),
            enabled=_strict_bool(value.get("enabled", True), "input field enabled state"),
            rect=None if rect is None else _coerce_rect(rect, "input field rect"),
        )


@dataclass(frozen=True)
class InputAction:
    kind: str
    x: Optional[int] = None
    y: Optional[int] = None
    end_x: Optional[int] = None
    end_y: Optional[int] = None
    button: str = "left"
    key: Optional[str] = None
    text: Optional[str] = None
    delta_x: int = 0
    delta_y: int = 0
    duration_ms: int = 250
    field_role: Optional[str] = None
    confirmed: bool = False

    @classmethod
    def from_value(cls, value: Union["InputAction", Mapping[str, Any]]) -> "InputAction":
        if isinstance(value, InputAction):
            value.validate()
            return value
        if not isinstance(value, Mapping):
            raise BackendError("invalid_input", "action must be a mapping")
        if "kind" not in value:
            raise BackendError("invalid_input", "action kind is required")
        kind = value["kind"]
        if not isinstance(kind, str):
            raise BackendError("invalid_input", "action kind must be a string")
        kind = kind.casefold().replace("-", "_")
        allowed = {
            "kind",
            "x",
            "y",
            "x2",
            "y2",
            "to_x",
            "to_y",
            "button",
            "key",
            "text",
            "delta_x",
            "delta_y",
            "duration_ms",
            "field_role",
            "confirmed",
        }
        if set(value) - allowed:
            raise BackendError("invalid_input", "action contains an unsupported field")
        end_x = value.get("x2", value.get("to_x"))
        end_y = value.get("y2", value.get("to_y"))
        action = cls(
            kind=kind,
            x=value.get("x"),
            y=value.get("y"),
            end_x=end_x,
            end_y=end_y,
            button=value.get("button", "left"),
            key=value.get("key"),
            text=value.get("text"),
            delta_x=value.get("delta_x", 0),
            delta_y=value.get("delta_y", 0),
            duration_ms=value.get("duration_ms", 250),
            field_role=value.get("field_role"),
            confirmed=_strict_bool(value.get("confirmed", False), "action confirmation"),
        )
        action.validate()
        return action

    def validate(self) -> None:
        kinds = {"move", "click", "double_click", "drag", "scroll", "key", "text"}
        if self.kind not in kinds:
            raise BackendError("invalid_input", "unsupported action kind")
        if not isinstance(self.confirmed, bool):
            raise BackendError("invalid_input", "action confirmation must be boolean")
        if self.x is not None:
            _bounded_int(self.x, "x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        if self.y is not None:
            _bounded_int(self.y, "y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        if self.end_x is not None:
            _bounded_int(self.end_x, "end x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        if self.end_y is not None:
            _bounded_int(self.end_y, "end y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        if self.kind in {"move", "click", "double_click", "drag", "scroll", "text"}:
            if self.x is None or self.y is None:
                raise BackendError("invalid_input", "action coordinates are required")
        if self.kind == "drag":
            if self.end_x is None or self.end_y is None:
                raise BackendError("invalid_input", "drag end coordinates are required")
            _bounded_int(self.duration_ms, "drag duration", 0, MAX_DRAG_DURATION_MS)
            if self.button not in {"left", "right", "middle"}:
                raise BackendError("invalid_input", "unsupported mouse button")
        elif self.kind in {"click", "double_click"}:
            if self.button not in {"left", "right", "middle"}:
                raise BackendError("invalid_input", "unsupported mouse button")
            if self.kind == "double_click" and self.button != "left":
                raise BackendError("invalid_input", "double-click only supports the left button")
        if self.kind == "scroll":
            _bounded_int(self.delta_x, "horizontal scroll delta", -MAX_SCROLL_DELTA, MAX_SCROLL_DELTA)
            _bounded_int(self.delta_y, "vertical scroll delta", -MAX_SCROLL_DELTA, MAX_SCROLL_DELTA)
            if self.delta_x == 0 and self.delta_y == 0:
                raise BackendError("invalid_input", "scroll delta must not be zero")
        if self.kind == "key":
            if not isinstance(self.key, str):
                raise BackendError("invalid_input", "key is required")
            key = self.key.upper().replace(" ", "_")
            if key not in KEY_VIRTUAL_CODES:
                raise BackendError("input_rejected", "key is not in the navigation allowlist")
        if self.kind == "text":
            if not isinstance(self.text, str) or not self.text:
                raise BackendError("invalid_input", "text is required")
            if len(self.text) > MAX_TEXT_CHARS or len(self.text.encode("utf-8")) > MAX_TEXT_BYTES:
                raise BackendError("input_too_large", "text exceeds its size limit")
            if any(ord(char) < 0x20 or 0xD800 <= ord(char) <= 0xDFFF for char in self.text):
                raise BackendError("input_rejected", "text contains a control or surrogate character")
            if self.field_role is not None:
                _check_string(self.field_role, "field role", 64)


@dataclass(frozen=True)
class ActionResult:
    completed: int
    target: WindowIdentity

    def as_dict(self) -> Dict[str, Any]:
        return {"completed": self.completed, "hwnd": self.target.hwnd}


class WindowsSystem(Protocol):
    """Adapter contract used by :class:`WindowsComputerUseBackend`."""

    def input_desktop(self) -> DesktopIdentity: ...

    def controller_integrity_level(self) -> int: ...

    def enumerate_top_level_windows(self) -> Iterable[int]: ...

    def snapshot_window(self, hwnd: int, include_binary_hash: bool = False) -> WindowIdentity: ...

    def foreground_window(self) -> int: ...

    def activate_window(self, hwnd: int) -> bool: ...

    def client_to_screen(self, hwnd: int, x: int, y: int) -> Tuple[int, int]: ...

    def virtual_screen_rect(self) -> Rect: ...

    def send_mouse_move(self, x: int, y: int) -> bool: ...

    def send_mouse_button(self, button: str, down: bool) -> bool: ...

    def send_mouse_wheel(self, delta_x: int, delta_y: int) -> bool: ...

    def send_key(self, key: str, down: bool) -> bool: ...

    def send_unicode_text(self, text: str) -> bool: ...

    def capture_window(self, hwnd: int, max_width: int, max_height: int) -> Screenshot: ...

    def input_field_at(self, hwnd: int, x: int, y: int) -> Optional[InputFieldInfo]: ...


class _Operation:
    def __init__(
        self,
        cancellation: Optional[Any],
        timeout_ms: int,
    ) -> None:
        self.cancellation = cancellation
        self.deadline = time.monotonic() + timeout_ms / 1000.0

    def check(self) -> None:
        if time.monotonic() >= self.deadline:
            raise OperationTimedOut()
        if self.cancellation is None:
            return
        try:
            if callable(self.cancellation):
                value = self.cancellation()
            elif hasattr(self.cancellation, "is_cancelled"):
                value = self.cancellation.is_cancelled
                value = value() if callable(value) else value
            else:
                value = getattr(self.cancellation, "cancelled", False)
            cancelled = _strict_bool(value, "cancellation state")
        except Exception:
            # A broken cancellation boundary must not permit more input.
            raise BackendError("cancelled", "cancellation state could not be read")
        if cancelled:
            raise OperationCancelled()

    def sleep(self, milliseconds: int) -> None:
        end = min(self.deadline, time.monotonic() + milliseconds / 1000.0)
        while True:
            self.check()
            remaining = end - time.monotonic()
            if remaining <= 0:
                return
            time.sleep(min(remaining, 0.01))


class WindowsComputerUseBackend:
    """Bounded, identity-fenced computer-use operations for one HWND."""

    def __init__(
        self,
        target: Union[TargetSpec, Mapping[str, Any]],
        *,
        api: Optional[WindowsSystem] = None,
        operation_timeout_ms: int = 10_000,
    ) -> None:
        self.target_spec = TargetSpec.from_mapping(target)
        _bounded_int(operation_timeout_ms, "operation timeout", 1, MAX_OPERATION_TIMEOUT_MS)
        self.operation_timeout_ms = operation_timeout_ms
        self.api: WindowsSystem = api if api is not None else Win32System()
        self._lock = threading.RLock()
        self._target: Optional[WindowIdentity] = None
        self._desktop_fence: Optional[DesktopIdentity] = None
        self._closed = False

    @property
    def attached(self) -> bool:
        return self._target is not None and not self._closed

    @property
    def target(self) -> Optional[WindowIdentity]:
        return self._target

    def _ensure_open(self) -> None:
        if self._closed:
            raise BackendError("backend_closed", "backend is closed")

    def _method(self, name: str) -> Callable[..., Any]:
        method = getattr(self.api, name, None)
        if not callable(method):
            raise BackendError("backend_unavailable", f"Windows adapter lacks {name}")
        return method

    def _input_desktop(self) -> DesktopIdentity:
        method = getattr(self.api, "input_desktop", None)
        if method is None:
            method = getattr(self.api, "get_input_desktop", None)
        if not callable(method):
            raise BackendError("backend_unavailable", "input desktop verification is unavailable")
        try:
            desktop = method()
        except BackendError:
            raise
        except Exception:
            raise BackendError("permission_denied", "input desktop could not be verified")
        if not isinstance(desktop, DesktopIdentity) or not desktop.name:
            raise BackendError("security_ui", "input desktop identity is unavailable")
        if desktop.is_secure:
            raise BackendError("security_ui", "secure desktop is not automatable")
        return desktop

    def _controller_integrity(self) -> int:
        method = getattr(self.api, "controller_integrity_level", None)
        if method is None:
            method = getattr(self.api, "controller_integrity", None)
        if not callable(method):
            raise BackendError("backend_unavailable", "controller integrity verification is unavailable")
        try:
            value = method()
        except BackendError:
            raise
        except Exception:
            raise BackendError("permission_denied", "controller integrity could not be verified")
        return _bounded_int(value, "controller integrity", 0, INTEGRITY_SYSTEM)

    def _enumerate_windows(self) -> List[int]:
        method = getattr(self.api, "enumerate_top_level_windows", None)
        if method is None:
            method = getattr(self.api, "enumerate_windows", None)
        if not callable(method):
            raise BackendError("backend_unavailable", "top-level window enumeration is unavailable")
        try:
            values = method()
            if isinstance(values, (str, bytes)):
                raise TypeError("window enumeration is not iterable window data")
            result: List[int] = []
            for value in values:
                if len(result) >= MAX_ENUMERATED_WINDOWS:
                    raise BackendError("backend_unavailable", "top-level window enumeration exceeds its limit")
                try:
                    result.append(_bounded_int(value, "HWND", 1, (1 << 63) - 1))
                except BackendError:
                    # An adapter cannot turn malformed enumeration data into a
                    # target; ignore it rather than widening the search.
                    continue
            return result
        except BackendError:
            raise
        except Exception:
            raise BackendError("permission_denied", "top-level windows could not be enumerated")

    def _snapshot(self, hwnd: int) -> WindowIdentity:
        method = getattr(self.api, "snapshot_window", None)
        if method is None:
            method = getattr(self.api, "get_window_identity", None)
        if not callable(method):
            raise BackendError("backend_unavailable", "window identity verification is unavailable")
        include_hash = self.target_spec.binary_sha256 is not None
        try:
            try:
                value = method(hwnd, include_binary_hash=include_hash)
            except TypeError as first_error:
                # Permit small test adapters that implement the older one
                # argument shape, while retaining the same identity contract.
                try:
                    value = method(hwnd)
                except TypeError:
                    raise first_error
        except BackendError:
            raise
        except Exception:
            raise BackendError("stale_window", "window identity could not be revalidated")
        if isinstance(value, Mapping):
            value = WindowIdentity.from_mapping(value)
        if not isinstance(value, WindowIdentity):
            raise BackendError("stale_window", "window identity is malformed")
        if value.hwnd != hwnd:
            raise BackendError("stale_window", "window handle identity changed")
        return value

    def _check_desktop(self, desktop: DesktopIdentity, *, fence: Optional[DesktopIdentity]) -> None:
        if desktop.is_secure:
            raise BackendError("security_ui", "secure desktop is not automatable")
        if fence is not None and not desktop.same_as(fence):
            raise BackendError("desktop_changed", "interactive desktop changed")

    def _check_security(self, identity: WindowIdentity, desktop: DesktopIdentity) -> None:
        self._check_desktop(desktop, fence=None)
        if identity.desktop.is_secure or identity.security_ui:
            raise BackendError("security_ui", "security UI is not automatable")
        if not identity.desktop.same_as(desktop):
            raise BackendError("desktop_changed", "target is not on the input desktop")
        if _binary_name(identity.app.binary_path) in SECURITY_UI_EXECUTABLES:
            raise BackendError("security_ui", "security UI process is not automatable")
        controller_integrity = self._controller_integrity()
        if identity.integrity_level > controller_integrity:
            raise BackendError("integrity_boundary", "target integrity exceeds controller integrity")
        if (
            self.target_spec.max_integrity_level is not None
            and identity.integrity_level > self.target_spec.max_integrity_level
        ):
            raise BackendError("integrity_boundary", "target exceeds configured integrity bound")
        if not identity.visible or not identity.enabled or identity.minimized:
            raise BackendError("target_unavailable", "target window is not interactable")
        if not identity.client_rect.is_valid or not identity.rect.is_valid:
            raise BackendError("target_unavailable", "target window has no usable bounds")

    def _matches_target(self, identity: WindowIdentity) -> bool:
        return self.target_spec.matches(identity)

    def discover(self) -> Tuple[WindowIdentity, ...]:
        """Return all exact identity matches without attaching or sending input."""

        with self._lock:
            self._ensure_open()
            desktop = self._input_desktop()
            self._check_desktop(desktop, fence=None)
            matches: List[WindowIdentity] = []
            for hwnd in self._enumerate_windows():
                try:
                    identity = self._snapshot(hwnd)
                    if not self._matches_target(identity):
                        continue
                    self._check_security(identity, desktop)
                    matches.append(identity)
                except BackendError as error:
                    # Discovery is a filter operation.  A stale or unrelated
                    # window is not evidence that an unsafe target is usable.
                    if error.code in {
                        "stale_window",
                        "target_unavailable",
                        "invalid_target",
                        "identity_mismatch",
                        "security_ui",
                        "integrity_boundary",
                    }:
                        continue
                    raise
            current_desktop = self._input_desktop()
            self._check_desktop(current_desktop, fence=desktop)
            return tuple(matches)

    def attach(self, hwnd: Optional[int] = None) -> WindowIdentity:
        """Attach to exactly one identity-matching top-level window."""

        with self._lock:
            self._ensure_open()
            desktop = self._input_desktop()
            self._check_desktop(desktop, fence=None)
            if hwnd is None:
                candidates = list(self.discover())
                if not candidates:
                    raise BackendError("target_not_found", "no exact application window was found")
                if len(candidates) != 1:
                    raise BackendError("target_ambiguous", "more than one exact application window was found")
                identity = candidates[0]
                if not identity.desktop.same_as(desktop):
                    raise BackendError("desktop_changed", "interactive desktop changed during discovery")
            else:
                hwnd = _bounded_int(hwnd, "HWND", 1, (1 << 63) - 1)
                identity = self._snapshot(hwnd)
                reason = self.target_spec.match_reason(identity)
                if reason is not None:
                    raise BackendError("identity_mismatch", "requested window does not match target identity")
                self._check_security(identity, desktop)
            self._target = identity
            self._desktop_fence = desktop
            return identity

    def detach(self) -> None:
        with self._lock:
            self._target = None
            self._desktop_fence = None

    def revalidate(self) -> WindowIdentity:
        """Re-read and fence the attached HWND without performing input."""

        with self._lock:
            self._ensure_open()
            if self._target is None or self._desktop_fence is None:
                raise BackendError("target_not_attached", "no target window is attached")
            desktop = self._input_desktop()
            self._check_desktop(desktop, fence=self._desktop_fence)
            try:
                current = self._snapshot(self._target.hwnd)
            except BackendError as error:
                if error.code == "stale_window":
                    raise BackendError("stale_window", "attached window is no longer valid")
                raise
            if not self._target.same_instance(current):
                raise BackendError("stale_window", "attached HWND or process identity changed")
            if not self._matches_target(current):
                raise BackendError("identity_mismatch", "attached application identity changed")
            self._check_security(current, desktop)
            self._target = current
            return current

    def _operation(self, cancellation: Optional[Any], timeout_ms: Optional[int]) -> _Operation:
        value = self.operation_timeout_ms if timeout_ms is None else timeout_ms
        _bounded_int(value, "operation timeout", 1, MAX_OPERATION_TIMEOUT_MS)
        return _Operation(cancellation, value)

    def _foreground(self) -> int:
        method = self._method("foreground_window")
        try:
            value = method()
        except BackendError:
            raise
        except Exception:
            raise BackendError("focus_unavailable", "foreground window could not be verified")
        return _bounded_int(value, "foreground HWND", 0, (1 << 63) - 1)

    def _prepare(self, operation: _Operation, *, require_foreground: bool) -> WindowIdentity:
        operation.check()
        identity = self.revalidate()
        if not require_foreground:
            return identity
        foreground = self._foreground()
        if foreground != identity.hwnd:
            activate = getattr(self.api, "activate_window", None)
            if not callable(activate):
                raise BackendError("focus_required", "target is not foreground")
            try:
                activated = _strict_bool(activate(identity.hwnd), "window activation result")
                if not activated:
                    raise BackendError("focus_required", "target could not be foreground")
            except BackendError:
                raise
            except Exception:
                raise BackendError("focus_required", "target could not be foreground")
            operation.check()
            identity = self.revalidate()
            if self._foreground() != identity.hwnd:
                raise BackendError("focus_required", "target did not become foreground")
        return identity

    def _screen_point(self, identity: WindowIdentity, x: int, y: int) -> Tuple[int, int]:
        x = _bounded_int(x, "x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        y = _bounded_int(y, "y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        if not identity.client_rect.contains(x, y):
            raise BackendError("coordinate_out_of_bounds", "point is outside target client bounds")
        method = self._method("client_to_screen")
        try:
            value = method(identity.hwnd, x, y)
        except BackendError:
            raise
        except Exception:
            raise BackendError("coordinate_out_of_bounds", "client coordinate could not be transformed")
        if not isinstance(value, (tuple, list)) or len(value) != 2:
            raise BackendError("coordinate_out_of_bounds", "screen coordinate is malformed")
        sx = _bounded_int(value[0], "screen x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        sy = _bounded_int(value[1], "screen y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        bounds_method = getattr(self.api, "virtual_screen_rect", None)
        if not callable(bounds_method):
            raise BackendError("backend_unavailable", "virtual screen bounds are unavailable")
        try:
            bounds = bounds_method()
        except BackendError:
            raise
        except Exception:
            raise BackendError("coordinate_out_of_bounds", "virtual screen bounds could not be verified")
        if not isinstance(bounds, Rect) or not bounds.is_valid or not bounds.contains(sx, sy):
            raise BackendError("coordinate_out_of_bounds", "screen coordinate is outside virtual desktop bounds")
        return sx, sy

    def _send_result(self, method_name: str, *args: Any) -> None:
        method = self._method(method_name)
        try:
            result = method(*args)
        except BackendError:
            raise
        except Exception:
            raise BackendError("input_failed", "native input was rejected")
        if not _strict_bool(result, "native input result"):
            raise BackendError("input_failed", "native input was rejected")

    def _release_all(self, held_buttons: Set[str], held_keys: Set[str]) -> None:
        # Release in deterministic order.  A release is attempted even when
        # desktop fencing failed: leaving a button/key held is worse than the
        # unavoidable global nature of a native release event.
        for button in tuple(sorted(held_buttons)):
            try:
                self._send_result("send_mouse_button", button, False)
            except Exception:
                pass
            held_buttons.discard(button)
        for key in tuple(sorted(held_keys)):
            try:
                self._send_result("send_key", key, False)
            except Exception:
                pass
            held_keys.discard(key)

    def _post_input_check(self, operation: _Operation) -> None:
        operation.check()
        self.revalidate()
        if self._foreground() != self._target.hwnd:  # type: ignore[union-attr]
            raise BackendError("focus_lost", "target lost the foreground during input")

    def _move(self, identity: WindowIdentity, x: int, y: int, operation: _Operation) -> None:
        sx, sy = self._screen_point(identity, x, y)
        operation.check()
        self._send_result("send_mouse_move", sx, sy)

    def _field_at(self, identity: WindowIdentity, sx: int, sy: int, role: Optional[str]) -> InputFieldInfo:
        method = getattr(self.api, "input_field_at", None)
        if not callable(method):
            raise BackendError("manual_required", "text input requires accessibility classification")
        try:
            value = method(identity.hwnd, sx, sy)
        except BackendError:
            raise
        except Exception:
            raise BackendError("manual_required", "text input classification failed")
        if value is None:
            raise BackendError("manual_required", "text input requires a classified non-sensitive field")
        field_info = InputFieldInfo.from_value(value)
        normalized_role = field_info.role.casefold().replace("_", "-")
        if field_info.window_hwnd != identity.hwnd:
            raise BackendError("input_rejected", "input field is outside the attached window")
        if field_info.rect is not None and not field_info.rect.contains(sx, sy):
            raise BackendError("input_rejected", "input field classification does not cover the requested point")
        if not field_info.enabled:
            raise BackendError("input_rejected", "input field is disabled")
        if field_info.sensitive or normalized_role in SENSITIVE_FIELD_ROLES:
            raise BackendError("manual_required", "credentials and sensitive fields require manual input")
        if normalized_role not in SAFE_TEXT_FIELD_ROLES:
            raise BackendError("manual_required", "unclassified text field requires manual input")
        if role is not None and normalized_role != role.casefold().replace("_", "-"):
            raise BackendError("input_rejected", "input field role changed")
        return field_info

    def _run_action(
        self,
        action: InputAction,
        operation: _Operation,
        held_buttons: Set[str],
        held_keys: Set[str],
        approval: Optional[Callable[[InputAction], bool]],
    ) -> None:
        identity = self._prepare(operation, require_foreground=True)
        if action.kind == "move":
            self._move(identity, action.x, action.y, operation)  # type: ignore[arg-type]
            self._post_input_check(operation)
            return

        if action.kind in {"click", "double_click"}:
            self._move(identity, action.x, action.y, operation)  # type: ignore[arg-type]
            count = 2 if action.kind == "double_click" else 1
            for index in range(count):
                operation.check()
                held_buttons.add(action.button)
                self._send_result("send_mouse_button", action.button, True)
                operation.check()
                self._send_result("send_mouse_button", action.button, False)
                held_buttons.discard(action.button)
                if count == 2 and index == 0:
                    operation.sleep(50)
            self._post_input_check(operation)
            return

        if action.kind == "drag":
            assert action.end_x is not None and action.end_y is not None
            if abs(action.end_x - action.x) > MAX_DRAG_DISTANCE or abs(action.end_y - action.y) > MAX_DRAG_DISTANCE:  # type: ignore[operator]
                raise BackendError("input_out_of_bounds", "drag distance exceeds its limit")
            self._move(identity, action.x, action.y, operation)  # type: ignore[arg-type]
            held_buttons.add(action.button)
            self._send_result("send_mouse_button", action.button, True)
            try:
                distance = max(abs(action.end_x - action.x), abs(action.end_y - action.y))  # type: ignore[operator]
                steps = max(1, min(MAX_DRAG_STEPS, distance // 64 + 1))
                for step in range(1, steps + 1):
                    operation.check()
                    current = self.revalidate()
                    if self._foreground() != current.hwnd:
                        raise BackendError("focus_lost", "target lost the foreground during drag")
                    x = action.x + (action.end_x - action.x) * step // steps  # type: ignore[operator]
                    y = action.y + (action.end_y - action.y) * step // steps  # type: ignore[operator]
                    self._move(current, x, y, operation)
                    if action.duration_ms:
                        operation.sleep(max(1, action.duration_ms // steps))
            finally:
                # The outer finally is also present; this immediate release
                # bounds the time a drag button can remain held.
                try:
                    self._send_result("send_mouse_button", action.button, False)
                finally:
                    held_buttons.discard(action.button)
            self._post_input_check(operation)
            return

        if action.kind == "scroll":
            self._move(identity, action.x, action.y, operation)  # type: ignore[arg-type]
            operation.check()
            self._send_result("send_mouse_wheel", action.delta_x, action.delta_y)
            self._post_input_check(operation)
            return

        if action.kind == "key":
            assert action.key is not None
            key = action.key.upper().replace(" ", "_")
            if key in CONSEQUENTIAL_KEYS and not action.confirmed:
                approved = False
                if approval is not None:
                    try:
                        approved = _strict_bool(approval(action), "action approval")
                    except BackendError:
                        raise BackendError("confirmation_required", "action approval must be an explicit boolean")
                    except Exception:
                        raise BackendError("confirmation_required", "action approval could not be verified")
                if not approved:
                    raise BackendError("confirmation_required", "consequential key requires explicit approval")
            operation.check()
            self._prepare(operation, require_foreground=True)
            held_keys.add(key)
            self._send_result("send_key", key, True)
            try:
                operation.check()
                self._send_result("send_key", key, False)
            finally:
                held_keys.discard(key)
            self._post_input_check(operation)
            return

        if action.kind == "text":
            assert action.text is not None
            self._move(identity, action.x, action.y, operation)  # type: ignore[arg-type]
            current = self._prepare(operation, require_foreground=True)
            sx, sy = self._screen_point(current, action.x, action.y)  # type: ignore[arg-type]
            self._field_at(current, sx, sy, action.field_role)
            operation.check()
            if self._foreground() != current.hwnd:
                raise BackendError("focus_lost", "target lost the foreground during text input")
            self._send_result("send_unicode_text", action.text)
            self._post_input_check(operation)
            return

        raise BackendError("invalid_input", "unsupported action kind")

    def execute(
        self,
        actions: Sequence[Union[InputAction, Mapping[str, Any]]],
        *,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
        approval: Optional[Callable[[InputAction], bool]] = None,
    ) -> ActionResult:
        """Execute a bounded action list, releasing any held input on exit."""

        if isinstance(actions, (str, bytes)) or not isinstance(actions, Sequence):
            raise BackendError("invalid_input", "actions must be a sequence")
        if len(actions) == 0 or len(actions) > MAX_ACTIONS:
            raise BackendError("input_out_of_bounds", "action count is outside its limit")
        parsed = [InputAction.from_value(action) for action in actions]
        operation = self._operation(cancellation, timeout_ms)
        held_buttons: Set[str] = set()
        held_keys: Set[str] = set()
        completed = 0
        with self._lock:
            self._ensure_open()
            try:
                for action in parsed:
                    operation.check()
                    self._run_action(action, operation, held_buttons, held_keys, approval)
                    completed += 1
                target = self.revalidate()
                return ActionResult(completed=completed, target=target)
            finally:
                self._release_all(held_buttons, held_keys)

    def observe(
        self,
        *,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> Observation:
        """Capture only the attached client area after identity checks."""

        operation = self._operation(cancellation, timeout_ms)
        with self._lock:
            self._ensure_open()
            identity = self._prepare(operation, require_foreground=False)
            width = identity.client_rect.width
            height = identity.client_rect.height
            if width <= 0 or height <= 0 or width > MAX_FRAME_WIDTH or height > MAX_FRAME_HEIGHT:
                raise BackendError("observation_too_large", "target client area exceeds observation bounds")
            if width * height > MAX_FRAME_PIXELS:
                raise BackendError("observation_too_large", "target client area exceeds pixel bounds")
            operation.check()
            capture = self._method("capture_window")
            try:
                try:
                    value = capture(identity.hwnd, max_width=MAX_FRAME_WIDTH, max_height=MAX_FRAME_HEIGHT)
                except TypeError as first_error:
                    try:
                        value = capture(identity.hwnd)
                    except TypeError:
                        raise first_error
            except BackendError:
                raise
            except Exception:
                raise BackendError("observation_failed", "window observation failed")
            if isinstance(value, Mapping):
                value = Screenshot.from_mapping(value)
            if not isinstance(value, Screenshot):
                raise BackendError("observation_invalid", "window observation is malformed")
            if value.width > MAX_FRAME_WIDTH or value.height > MAX_FRAME_HEIGHT:
                raise BackendError("observation_too_large", "window observation exceeds its bounds")
            if value.width * value.height > MAX_FRAME_PIXELS or len(value.data) > MAX_FRAME_BYTES:
                raise BackendError("observation_too_large", "window observation exceeds its bounds")
            operation.check()
            current = self.revalidate()
            if not current.same_instance(identity):
                raise BackendError("stale_window", "window changed while being observed")
            if value.width != current.client_rect.width or value.height != current.client_rect.height:
                raise BackendError("stale_window", "window size changed while being observed")
            return Observation(
                identity=current,
                screenshot=value,
                desktop=self._desktop_fence,  # type: ignore[arg-type]
                captured_at=time.time(),
            )

    def focus(self, *, cancellation: Optional[Any] = None, timeout_ms: Optional[int] = None) -> WindowIdentity:
        """Bring the attached target forward only after checking its identity."""

        operation = self._operation(cancellation, timeout_ms)
        with self._lock:
            self._ensure_open()
            return self._prepare(operation, require_foreground=True)

    def click(
        self,
        x: int,
        y: int,
        *,
        button: str = "left",
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> ActionResult:
        return self.execute(
            [{"kind": "click", "x": x, "y": y, "button": button}],
            cancellation=cancellation,
            timeout_ms=timeout_ms,
        )

    def double_click(
        self,
        x: int,
        y: int,
        *,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> ActionResult:
        return self.execute(
            [{"kind": "double_click", "x": x, "y": y}],
            cancellation=cancellation,
            timeout_ms=timeout_ms,
        )

    def drag(
        self,
        x: int,
        y: int,
        end_x: int,
        end_y: int,
        *,
        duration_ms: int = 250,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> ActionResult:
        return self.execute(
            [
                {
                    "kind": "drag",
                    "x": x,
                    "y": y,
                    "x2": end_x,
                    "y2": end_y,
                    "duration_ms": duration_ms,
                }
            ],
            cancellation=cancellation,
            timeout_ms=timeout_ms,
        )

    def press_key(
        self,
        key: str,
        *,
        confirmed: bool = False,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
        approval: Optional[Callable[[InputAction], bool]] = None,
    ) -> ActionResult:
        return self.execute(
            [{"kind": "key", "key": key, "confirmed": confirmed}],
            cancellation=cancellation,
            timeout_ms=timeout_ms,
            approval=approval,
        )

    def type_text(
        self,
        x: int,
        y: int,
        text: str,
        *,
        field_role: Optional[str] = None,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> ActionResult:
        return self.execute(
            [{"kind": "text", "x": x, "y": y, "text": text, "field_role": field_role}],
            cancellation=cancellation,
            timeout_ms=timeout_ms,
        )

    def scroll(
        self,
        x: int,
        y: int,
        *,
        delta_y: int,
        delta_x: int = 0,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> ActionResult:
        return self.execute(
            [{"kind": "scroll", "x": x, "y": y, "delta_x": delta_x, "delta_y": delta_y}],
            cancellation=cancellation,
            timeout_ms=timeout_ms,
        )

    def navigate(
        self,
        direction: str,
        *,
        confirmed: bool = False,
        cancellation: Optional[Any] = None,
        timeout_ms: Optional[int] = None,
    ) -> ActionResult:
        mapping = {
            "next": "TAB",
            "up": "UP",
            "down": "DOWN",
            "left": "LEFT",
            "right": "RIGHT",
            "home": "HOME",
            "end": "END",
            "pageup": "PAGEUP",
            "pagedown": "PAGEDOWN",
        }
        if not isinstance(direction, str):
            raise BackendError("invalid_input", "navigation direction must be a string")
        normalized = direction.casefold().replace("-", "")
        if normalized == "previous":
            raise BackendError("input_rejected", "reverse tab navigation requires manual handling")
        if normalized not in mapping:
            raise BackendError("invalid_input", "unsupported navigation direction")
        return self.press_key(
            mapping[normalized],
            confirmed=confirmed,
            cancellation=cancellation,
            timeout_ms=timeout_ms,
        )

    def close(self) -> None:
        with self._lock:
            self.detach()
            self._closed = True

    shutdown = close


# Compatibility names for callers that use the backend/config terminology.
WindowsBackend = WindowsComputerUseBackend
WindowsBackendConfig = TargetSpec
ApplicationTarget = TargetSpec


# ctypes definitions are kept at module scope so SendInput structures have a
# stable layout on both 32-bit and 64-bit Windows.  They are harmless on other
# platforms because no WinDLL is loaded there.
class _POINT(ctypes.Structure):
    _fields_ = [("x", ctypes.c_long), ("y", ctypes.c_long)]


class _FILETIME(ctypes.Structure):
    _fields_ = [("dwLowDateTime", ctypes.c_uint32), ("dwHighDateTime", ctypes.c_uint32)]


class _SID_AND_ATTRIBUTES(ctypes.Structure):
    _fields_ = [("Sid", ctypes.c_void_p), ("Attributes", ctypes.c_uint32)]


class _TOKEN_MANDATORY_LABEL(ctypes.Structure):
    _fields_ = [("Label", _SID_AND_ATTRIBUTES)]


class _MOUSEINPUT(ctypes.Structure):
    _fields_ = [
        ("dx", ctypes.c_long),
        ("dy", ctypes.c_long),
        ("mouseData", ctypes.c_uint32),
        ("dwFlags", ctypes.c_uint32),
        ("time", ctypes.c_uint32),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class _KEYBDINPUT(ctypes.Structure):
    _fields_ = [
        ("wVk", ctypes.c_uint16),
        ("wScan", ctypes.c_uint16),
        ("dwFlags", ctypes.c_uint32),
        ("time", ctypes.c_uint32),
        ("dwExtraInfo", ctypes.c_void_p),
    ]


class _HARDWAREINPUT(ctypes.Structure):
    _fields_ = [
        ("uMsg", ctypes.c_uint32),
        ("wParamL", ctypes.c_uint16),
        ("wParamH", ctypes.c_uint16),
    ]


class _INPUTUNION(ctypes.Union):
    _fields_ = [("mi", _MOUSEINPUT), ("ki", _KEYBDINPUT), ("hi", _HARDWAREINPUT)]


class _INPUT(ctypes.Structure):
    _anonymous_ = ("union",)
    _fields_ = [("type", ctypes.c_uint32), ("union", _INPUTUNION)]


class _GUID(ctypes.Structure):
    _fields_ = [
        ("Data1", ctypes.c_uint32),
        ("Data2", ctypes.c_uint16),
        ("Data3", ctypes.c_uint16),
        ("Data4", ctypes.c_ubyte * 8),
    ]


class _WINTRUST_FILE_INFO(ctypes.Structure):
    _fields_ = [
        ("cbStruct", ctypes.c_uint32),
        ("pcwszFilePath", ctypes.c_wchar_p),
        ("hFile", ctypes.c_void_p),
        ("pgKnownSubject", ctypes.POINTER(_GUID)),
    ]


class _WINTRUST_DATA(ctypes.Structure):
    _fields_ = [
        ("cbStruct", ctypes.c_uint32),
        ("pPolicyCallbackData", ctypes.c_void_p),
        ("pSIPClientData", ctypes.c_void_p),
        ("dwUIChoice", ctypes.c_uint32),
        ("fdwRevocationChecks", ctypes.c_uint32),
        ("dwUnionChoice", ctypes.c_uint32),
        ("pFile", ctypes.c_void_p),
        ("dwStateAction", ctypes.c_uint32),
        ("hWVTStateData", ctypes.c_void_p),
        ("pwszURLReference", ctypes.c_wchar_p),
        ("dwProvFlags", ctypes.c_uint32),
        ("dwUIContext", ctypes.c_uint32),
        ("pSignatureSettings", ctypes.c_void_p),
    ]


class _BITMAPINFOHEADER(ctypes.Structure):
    _fields_ = [
        ("biSize", ctypes.c_uint32),
        ("biWidth", ctypes.c_int32),
        ("biHeight", ctypes.c_int32),
        ("biPlanes", ctypes.c_uint16),
        ("biBitCount", ctypes.c_uint16),
        ("biCompression", ctypes.c_uint32),
        ("biSizeImage", ctypes.c_uint32),
        ("biXPelsPerMeter", ctypes.c_int32),
        ("biYPelsPerMeter", ctypes.c_int32),
        ("biClrUsed", ctypes.c_uint32),
        ("biClrImportant", ctypes.c_uint32),
    ]


class _BITMAPINFO(ctypes.Structure):
    _fields_ = [("bmiHeader", _BITMAPINFOHEADER), ("bmiColors", ctypes.c_uint32 * 1)]


class Win32System:
    """Small standard-library-only Win32 adapter used in production."""

    _PROCESS_QUERY_LIMITED_INFORMATION = 0x1000
    _PROCESS_QUERY_INFORMATION = 0x0400
    _TOKEN_QUERY = 0x0008
    _THREAD_QUERY_LIMITED_INFORMATION = 0x0800
    _DESKTOP_READOBJECTS = 0x0001
    _UOI_NAME = 2
    _SW_RESTORE = 9
    _GA_ROOT = 2
    _SM_XVIRTUALSCREEN = 76
    _SM_YVIRTUALSCREEN = 77
    _SM_CXVIRTUALSCREEN = 78
    _SM_CYVIRTUALSCREEN = 79
    _SRCCOPY = 0x00CC0020
    _DIB_RGB_COLORS = 0
    _BI_RGB = 0
    _MOUSEEVENTF_MOVE = 0x0001
    _MOUSEEVENTF_LEFTDOWN = 0x0002
    _MOUSEEVENTF_LEFTUP = 0x0004
    _MOUSEEVENTF_RIGHTDOWN = 0x0008
    _MOUSEEVENTF_RIGHTUP = 0x0010
    _MOUSEEVENTF_MIDDLEDOWN = 0x0020
    _MOUSEEVENTF_MIDDLEUP = 0x0040
    _MOUSEEVENTF_WHEEL = 0x0800
    _MOUSEEVENTF_HWHEEL = 0x01000
    _MOUSEEVENTF_ABSOLUTE = 0x8000
    _MOUSEEVENTF_VIRTUALDESK = 0x4000
    _KEYEVENTF_KEYUP = 0x0002
    _KEYEVENTF_UNICODE = 0x0004
    _INPUT_MOUSE = 0
    _INPUT_KEYBOARD = 1
    _ERROR_INSUFFICIENT_BUFFER = 122
    _APPMODEL_ERROR_NO_APPLICATION = 15703
    _TOKEN_INTEGRITY_LEVEL = 25

    def __init__(self) -> None:
        if not _IS_WINDOWS:
            raise BackendError("unsupported_platform", "native Windows backend requires Windows")
        try:
            self.user32 = ctypes.WinDLL("user32", use_last_error=True)
            self.kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
            self.advapi32 = ctypes.WinDLL("advapi32", use_last_error=True)
            self.version = ctypes.WinDLL("version", use_last_error=True)
            self.wintrust = ctypes.WinDLL("wintrust", use_last_error=True)
            self.gdi32 = ctypes.WinDLL("gdi32", use_last_error=True)
        except Exception:
            raise BackendError("backend_unavailable", "required Windows APIs are unavailable")
        self._configure_functions()

    def _configure_functions(self) -> None:
        # Explicit declarations keep Win32 BOOL, pointer, handle, and length
        # values from being widened or silently truncated by ctypes.
        hwnd = wintypes.HWND
        handle = wintypes.HANDLE
        dword = wintypes.DWORD
        bool_type = wintypes.BOOL
        void_pointer = ctypes.c_void_p

        self.user32.IsWindow.argtypes = [hwnd]
        self.user32.IsWindow.restype = bool_type
        self.user32.IsWindowVisible.argtypes = [hwnd]
        self.user32.IsWindowVisible.restype = bool_type
        self.user32.IsWindowEnabled.argtypes = [hwnd]
        self.user32.IsWindowEnabled.restype = bool_type
        self.user32.IsIconic.argtypes = [hwnd]
        self.user32.IsIconic.restype = bool_type
        self.user32.GetForegroundWindow.argtypes = []
        self.user32.GetForegroundWindow.restype = hwnd
        self.user32.GetDC.argtypes = [hwnd]
        self.user32.GetDC.restype = void_pointer
        self.user32.ReleaseDC.argtypes = [hwnd, void_pointer]
        self.user32.ReleaseDC.restype = ctypes.c_int
        self.user32.GetDesktopWindow.argtypes = []
        self.user32.GetDesktopWindow.restype = hwnd
        self.user32.GetWindowTextLengthW.argtypes = [hwnd]
        self.user32.GetWindowTextLengthW.restype = ctypes.c_int
        self.user32.GetWindowTextW.argtypes = [hwnd, wintypes.LPWSTR, ctypes.c_int]
        self.user32.GetWindowTextW.restype = ctypes.c_int
        self.user32.GetClassNameW.argtypes = [hwnd, wintypes.LPWSTR, ctypes.c_int]
        self.user32.GetClassNameW.restype = ctypes.c_int
        self.user32.GetWindowRect.argtypes = [hwnd, ctypes.POINTER(wintypes.RECT)]
        self.user32.GetWindowRect.restype = bool_type
        self.user32.GetClientRect.argtypes = [hwnd, ctypes.POINTER(wintypes.RECT)]
        self.user32.GetClientRect.restype = bool_type
        self.user32.GetThreadDesktop.argtypes = [dword]
        self.user32.GetThreadDesktop.restype = void_pointer
        self.user32.OpenInputDesktop.argtypes = [dword, bool_type, dword]
        self.user32.OpenInputDesktop.restype = void_pointer
        self.user32.GetUserObjectInformationW.argtypes = [handle, dword, void_pointer, dword, ctypes.POINTER(dword)]
        self.user32.GetUserObjectInformationW.restype = bool_type
        self.user32.CloseDesktop.argtypes = [void_pointer]
        self.user32.CloseDesktop.restype = bool_type
        self.user32.GetAncestor.argtypes = [hwnd, ctypes.c_uint]
        self.user32.GetAncestor.restype = hwnd
        self.user32.GetWindow.argtypes = [hwnd, ctypes.c_uint]
        self.user32.GetWindow.restype = hwnd
        self.user32.GetParent.argtypes = [hwnd]
        self.user32.GetParent.restype = hwnd
        self.user32.GetWindowThreadProcessId.argtypes = [hwnd, ctypes.POINTER(dword)]
        self.user32.GetWindowThreadProcessId.restype = dword
        self.user32.ShowWindow.argtypes = [hwnd, ctypes.c_int]
        self.user32.ShowWindow.restype = bool_type
        self.user32.SetForegroundWindow.argtypes = [hwnd]
        self.user32.SetForegroundWindow.restype = bool_type
        self.user32.ClientToScreen.argtypes = [hwnd, ctypes.POINTER(_POINT)]
        self.user32.ClientToScreen.restype = bool_type
        self.user32.GetSystemMetrics.argtypes = [ctypes.c_int]
        self.user32.GetSystemMetrics.restype = ctypes.c_int
        self.user32.SendInput.argtypes = [wintypes.UINT, ctypes.POINTER(_INPUT), ctypes.c_int]
        self.user32.SendInput.restype = wintypes.UINT
        self.user32.GetWindowLongPtrW.argtypes = [hwnd, ctypes.c_int]
        self.user32.GetWindowLongPtrW.restype = ctypes.c_ssize_t
        self._enum_callback_type = ctypes.WINFUNCTYPE(bool_type, hwnd, wintypes.LPARAM)
        self.user32.EnumWindows.argtypes = [self._enum_callback_type, wintypes.LPARAM]
        self.user32.EnumWindows.restype = bool_type

        self.kernel32.OpenProcess.argtypes = [dword, bool_type, dword]
        self.kernel32.OpenProcess.restype = handle
        self.kernel32.CloseHandle.argtypes = [handle]
        self.kernel32.CloseHandle.restype = bool_type
        self.kernel32.GetCurrentProcess.argtypes = []
        self.kernel32.GetCurrentProcess.restype = handle
        self.kernel32.GetProcessTimes.argtypes = [
            handle,
            ctypes.POINTER(_FILETIME),
            ctypes.POINTER(_FILETIME),
            ctypes.POINTER(_FILETIME),
            ctypes.POINTER(_FILETIME),
        ]
        self.kernel32.GetProcessTimes.restype = bool_type
        self.kernel32.QueryFullProcessImageNameW.argtypes = [handle, dword, wintypes.LPWSTR, ctypes.POINTER(dword)]
        self.kernel32.QueryFullProcessImageNameW.restype = bool_type
        self.kernel32.ProcessIdToSessionId.argtypes = [dword, ctypes.POINTER(dword)]
        self.kernel32.ProcessIdToSessionId.restype = bool_type
        application_user_model_id = getattr(self.kernel32, "GetApplicationUserModelId", None)
        if application_user_model_id is not None:
            application_user_model_id.argtypes = [handle, ctypes.POINTER(dword), wintypes.LPWSTR]
            application_user_model_id.restype = dword

        self.advapi32.OpenProcessToken.argtypes = [handle, dword, ctypes.POINTER(handle)]
        self.advapi32.OpenProcessToken.restype = bool_type
        self.advapi32.GetTokenInformation.argtypes = [handle, dword, void_pointer, dword, ctypes.POINTER(dword)]
        self.advapi32.GetTokenInformation.restype = bool_type
        self.advapi32.GetSidSubAuthorityCount.argtypes = [void_pointer]
        self.advapi32.GetSidSubAuthorityCount.restype = ctypes.POINTER(ctypes.c_ubyte)
        self.advapi32.GetSidSubAuthority.argtypes = [void_pointer, dword]
        self.advapi32.GetSidSubAuthority.restype = ctypes.POINTER(dword)

        self.version.GetFileVersionInfoSizeW.argtypes = [wintypes.LPCWSTR, ctypes.POINTER(dword)]
        self.version.GetFileVersionInfoSizeW.restype = dword
        self.version.GetFileVersionInfoW.argtypes = [wintypes.LPCWSTR, dword, dword, void_pointer]
        self.version.GetFileVersionInfoW.restype = bool_type
        self.version.VerQueryValueW.argtypes = [void_pointer, wintypes.LPCWSTR, ctypes.POINTER(void_pointer), ctypes.POINTER(dword)]
        self.version.VerQueryValueW.restype = bool_type

        self.wintrust.WinVerifyTrust.argtypes = [void_pointer, ctypes.POINTER(_GUID), ctypes.POINTER(_WINTRUST_DATA)]
        self.wintrust.WinVerifyTrust.restype = ctypes.c_long

        self.gdi32.CreateCompatibleDC.argtypes = [void_pointer]
        self.gdi32.CreateCompatibleDC.restype = void_pointer
        self.gdi32.CreateCompatibleBitmap.argtypes = [void_pointer, ctypes.c_int, ctypes.c_int]
        self.gdi32.CreateCompatibleBitmap.restype = void_pointer
        self.gdi32.SelectObject.argtypes = [void_pointer, void_pointer]
        self.gdi32.SelectObject.restype = void_pointer
        self.gdi32.BitBlt.argtypes = [
            void_pointer,
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_int,
            ctypes.c_int,
            void_pointer,
            ctypes.c_int,
            ctypes.c_int,
            wintypes.DWORD,
        ]
        self.gdi32.BitBlt.restype = bool_type
        self.gdi32.GetDIBits.argtypes = [
            void_pointer,
            void_pointer,
            wintypes.UINT,
            wintypes.UINT,
            void_pointer,
            ctypes.POINTER(_BITMAPINFO),
            wintypes.UINT,
        ]
        self.gdi32.GetDIBits.restype = ctypes.c_int
        self.gdi32.DeleteObject.argtypes = [void_pointer]
        self.gdi32.DeleteObject.restype = bool_type
        self.gdi32.DeleteDC.argtypes = [void_pointer]
        self.gdi32.DeleteDC.restype = bool_type

    @staticmethod
    def _handle_value(handle: Any) -> int:
        if handle is None:
            return 0
        if isinstance(handle, int):
            value = handle
        else:
            try:
                value = handle.value
            except AttributeError:
                value = handle
        if value is None:
            return 0
        if isinstance(value, bool) or not isinstance(value, int):
            raise BackendError("backend_unavailable", "native handle has an invalid representation")
        if value < 0 or value > (1 << 63) - 1:
            raise BackendError("backend_unavailable", "native handle is outside its supported range")
        return value

    def _require_handle(self, handle: Any, code: str, message: str) -> Any:
        try:
            value = self._handle_value(handle)
        except BackendError:
            raise BackendError(code, message)
        if value <= 0 or value > (1 << 63) - 1:
            raise BackendError(code, message)
        return handle

    def _close_handle(self, handle: Any, message: str = "native handle could not be closed") -> None:
        if not _native_bool(self.kernel32.CloseHandle(handle), "CloseHandle"):
            raise BackendError("backend_unavailable", message)

    def _close_desktop(self, handle: Any) -> None:
        if not _native_bool(self.user32.CloseDesktop(handle), "CloseDesktop"):
            raise BackendError("backend_unavailable", "desktop handle could not be closed")

    def input_desktop(self) -> DesktopIdentity:
        handle = self.user32.OpenInputDesktop(0, False, self._DESKTOP_READOBJECTS)
        self._require_handle(handle, "permission_denied", "input desktop could not be opened")
        try:
            name = self._user_object_name(handle)
        finally:
            self._close_desktop(handle)
        session = self._session_id(os.getpid())
        return DesktopIdentity(name=name, station="WinSta0", session_id=session, secure=name.casefold() in SECURE_DESKTOP_NAMES)

    def _user_object_name(self, handle: Any) -> str:
        needed = ctypes.c_uint32(0)
        _native_bool(
            self.user32.GetUserObjectInformationW(handle, self._UOI_NAME, None, 0, ctypes.byref(needed)),
            "GetUserObjectInformationW size query",
        )
        character_size = ctypes.sizeof(ctypes.c_wchar)
        maximum = (MAX_CLASS_CHARS + 1) * character_size
        if needed.value == 0 or needed.value > maximum or needed.value % character_size:
            raise BackendError("permission_denied", "desktop identity exceeds its size limit")
        size = max(needed.value // character_size, 2)
        buffer = ctypes.create_unicode_buffer(size)
        if not _native_bool(
            self.user32.GetUserObjectInformationW(
                handle, self._UOI_NAME, buffer, ctypes.sizeof(buffer), ctypes.byref(needed)
            ),
            "GetUserObjectInformationW",
        ):
            raise BackendError("permission_denied", "desktop identity could not be read")
        if needed.value == 0 or needed.value > maximum or needed.value % character_size:
            raise BackendError("permission_denied", "desktop identity exceeds its size limit")
        name = buffer.value
        return _check_string(name, "desktop identity", MAX_CLASS_CHARS)

    def controller_integrity_level(self) -> int:
        token = ctypes.c_void_p()
        current = self.kernel32.GetCurrentProcess()
        if not _native_bool(
            self.advapi32.OpenProcessToken(current, self._TOKEN_QUERY, ctypes.byref(token)),
            "OpenProcessToken",
        ):
            raise BackendError("permission_denied", "controller token could not be read")
        try:
            self._require_handle(token, "permission_denied", "controller token could not be read")
            return self._token_integrity(token)
        finally:
            self._close_handle(token, "controller token could not be closed")

    def _token_integrity(self, token: Any) -> int:
        needed = ctypes.c_uint32(0)
        _native_bool(
            self.advapi32.GetTokenInformation(
                token, self._TOKEN_INTEGRITY_LEVEL, None, 0, ctypes.byref(needed)
            ),
            "GetTokenInformation size query",
        )
        minimum = ctypes.sizeof(_TOKEN_MANDATORY_LABEL)
        if needed.value < minimum or needed.value > MAX_TOKEN_INFO_BYTES:
            raise BackendError("permission_denied", "integrity level exceeds its size limit")
        buffer = ctypes.create_string_buffer(needed.value)
        if not _native_bool(
            self.advapi32.GetTokenInformation(
                token, self._TOKEN_INTEGRITY_LEVEL, buffer, needed.value, ctypes.byref(needed)
            ),
            "GetTokenInformation",
        ):
            raise BackendError("permission_denied", "integrity level could not be read")
        if needed.value < minimum or needed.value > MAX_TOKEN_INFO_BYTES:
            raise BackendError("permission_denied", "integrity level exceeds its size limit")
        base = ctypes.addressof(buffer)
        end = base + needed.value
        label = ctypes.cast(buffer, ctypes.POINTER(_TOKEN_MANDATORY_LABEL)).contents
        sid_value = int(label.Label.Sid or 0)
        if sid_value < base or sid_value >= end:
            raise BackendError("permission_denied", "integrity SID is outside its token buffer")
        sid = ctypes.c_void_p(sid_value)
        count_pointer = self.advapi32.GetSidSubAuthorityCount(sid)
        count_pointer_value = ctypes.cast(count_pointer, ctypes.c_void_p).value if count_pointer else None
        if count_pointer_value is None or count_pointer_value < base or count_pointer_value >= end:
            raise BackendError("permission_denied", "integrity SID could not be read")
        count = _bounded_int(int(count_pointer[0]), "integrity SID subauthority count", 1, 15)
        authority = self.advapi32.GetSidSubAuthority(sid, count - 1)
        authority_value = ctypes.cast(authority, ctypes.c_void_p).value if authority else None
        if authority_value is None or authority_value < base or authority_value + ctypes.sizeof(ctypes.c_uint32) > end:
            raise BackendError("permission_denied", "integrity SID could not be read")
        level = int(authority[0])
        if level < INTEGRITY_UNTRUSTED or level > INTEGRITY_SYSTEM:
            raise BackendError("permission_denied", "integrity SID is outside its supported range")
        return level

    def _open_process(self, process_id: int) -> Any:
        process_id = _bounded_int(process_id, "process id", 1, 0xFFFFFFFF)
        handle = self.kernel32.OpenProcess(self._PROCESS_QUERY_LIMITED_INFORMATION, False, process_id)
        return self._require_handle(handle, "permission_denied", "target process cannot be queried")

    def _session_id(self, process_id: int) -> int:
        process_id = _bounded_int(process_id, "process id", 1, 0xFFFFFFFF)
        session = ctypes.c_uint32(0)
        if not _native_bool(
            self.kernel32.ProcessIdToSessionId(process_id, ctypes.byref(session)),
            "ProcessIdToSessionId",
        ):
            raise BackendError("permission_denied", "process session could not be read")
        return int(session.value)

    def _process_path(self, handle: Any) -> str:
        size = ctypes.c_uint32(32768)
        buffer = ctypes.create_unicode_buffer(size.value)
        if not _native_bool(
            self.kernel32.QueryFullProcessImageNameW(handle, 0, buffer, ctypes.byref(size)),
            "QueryFullProcessImageNameW",
        ):
            raise BackendError("permission_denied", "target binary path could not be read")
        if size.value == 0 or size.value > MAX_IDENTITY_FIELD_CHARS or not buffer.value:
            raise BackendError("permission_denied", "target binary path exceeds its size limit")
        path = buffer.value
        if len(path) != size.value or len(path) > MAX_IDENTITY_FIELD_CHARS:
            raise BackendError("permission_denied", "target binary path exceeds its size limit")
        return _check_string(path, "target binary path", MAX_IDENTITY_FIELD_CHARS)

    def _process_start_time(self, handle: Any) -> int:
        creation = _FILETIME()
        exit_time = _FILETIME()
        kernel_time = _FILETIME()
        user_time = _FILETIME()
        if not _native_bool(
            self.kernel32.GetProcessTimes(
                handle,
                ctypes.byref(creation),
                ctypes.byref(exit_time),
                ctypes.byref(kernel_time),
                ctypes.byref(user_time),
            ),
            "GetProcessTimes",
        ):
            raise BackendError("permission_denied", "target process creation time could not be read")
        value = (int(creation.dwHighDateTime) << 32) | int(creation.dwLowDateTime)
        return _bounded_int(value, "process creation time", 1, (1 << 63) - 1)

    def _process_aumid(self, handle: Any) -> Optional[str]:
        function = getattr(self.kernel32, "GetApplicationUserModelId", None)
        if function is None:
            return None
        token = ctypes.c_void_p()
        if not _native_bool(
            self.advapi32.OpenProcessToken(handle, self._TOKEN_QUERY, ctypes.byref(token)),
            "OpenProcessToken",
        ):
            return None
        try:
            self._require_handle(token, "permission_denied", "target token could not be read")
            length = ctypes.c_uint32(0)
            result = function(token, ctypes.byref(length), None)
            if result == self._APPMODEL_ERROR_NO_APPLICATION:
                return None
            if result != self._ERROR_INSUFFICIENT_BUFFER or not length.value:
                return None
            if length.value > MAX_AUMID_CHARS + 1:
                return None
            buffer = ctypes.create_unicode_buffer(length.value)
            result = function(token, ctypes.byref(length), buffer)
            if result != 0:
                return None
            if length.value == 0 or length.value > MAX_AUMID_CHARS + 1:
                return None
            value = buffer.value
            if not value or len(value) >= length.value or len(value) > MAX_AUMID_CHARS:
                return None
            return _check_string(value, "application user model ID", MAX_AUMID_CHARS)
        except BackendError:
            return None
        finally:
            self._close_handle(token, "target token could not be closed")

    def _file_version_strings(self, path: str) -> Tuple[Optional[str], Optional[str]]:
        try:
            zero = ctypes.c_uint32(0)
            size = int(self.version.GetFileVersionInfoSizeW(path, ctypes.byref(zero)))
            if size <= 0 or size > MAX_VERSION_INFO_BYTES:
                return None, None
            data = ctypes.create_string_buffer(size)
            if not _native_bool(
                self.version.GetFileVersionInfoW(path, 0, size, data),
                "GetFileVersionInfoW",
            ):
                return None, None
            data_start = ctypes.addressof(data)
            data_end = data_start + size
            character_size = ctypes.sizeof(ctypes.c_wchar)
            for language in ("040904b0", "000004b0", "04090000"):
                values: Dict[str, Optional[str]] = {}
                for key in ("CompanyName", "ProductName"):
                    pointer = ctypes.c_void_p()
                    length = ctypes.c_uint32(0)
                    sub_block = f"\\StringFileInfo\\{language}\\{key}"
                    if not _native_bool(
                        self.version.VerQueryValueW(
                            data, sub_block, ctypes.byref(pointer), ctypes.byref(length)
                        ),
                        "VerQueryValueW",
                    ) or not pointer.value:
                        values[key] = None
                        continue
                    pointer_value = _bounded_int(pointer.value, "version string pointer", data_start, data_end - 1)
                    if (pointer_value - data_start) % character_size:
                        return None, None
                    if length.value == 0 or length.value > MAX_IDENTITY_FIELD_CHARS + 1:
                        return None, None
                    if length.value * character_size > data_end - pointer_value:
                        return None, None
                    raw_value = ctypes.wstring_at(pointer_value, length.value)
                    if not raw_value or raw_value[-1] != "\x00":
                        return None, None
                    value = raw_value[:-1]
                    if not value or len(value) > MAX_IDENTITY_FIELD_CHARS:
                        return None, None
                    try:
                        values[key] = _check_string(value, key, MAX_IDENTITY_FIELD_CHARS)
                    except BackendError:
                        return None, None
                if values["CompanyName"] or values["ProductName"]:
                    return values["CompanyName"], values["ProductName"]
        except Exception:
            return None, None
        return None, None

    def _verify_signature(self, path: str) -> bool:
        try:
            action = _GUID(
                0x00AAC56B,
                0xCD44,
                0x11D0,
                (ctypes.c_ubyte * 8)(0x8C, 0xC2, 0x00, 0xC0, 0x4F, 0xC2, 0x95, 0xEE),
            )
            file_info = _WINTRUST_FILE_INFO(
                ctypes.sizeof(_WINTRUST_FILE_INFO),
                path,
                None,
                None,
            )
            data = _WINTRUST_DATA(
                ctypes.sizeof(_WINTRUST_DATA),
                None,
                None,
                2,  # WTD_UI_NONE
                1,  # WTD_REVOKE_WHOLECHAIN
                1,  # WTD_CHOICE_FILE
                ctypes.cast(ctypes.pointer(file_info), ctypes.c_void_p),
                1,  # WTD_STATEACTION_VERIFY
                None,
                None,
                0,
                0,
                None,
            )
            verify_result: Optional[int] = None
            close_result: Optional[int] = None
            try:
                verify_result = int(self.wintrust.WinVerifyTrust(None, ctypes.byref(action), ctypes.byref(data)))
            finally:
                data.dwStateAction = 2  # WTD_STATEACTION_CLOSE
                close_result = int(self.wintrust.WinVerifyTrust(None, ctypes.byref(action), ctypes.byref(data)))
            return verify_result == 0 and close_result == 0
        except Exception:
            return False

    @staticmethod
    def _hash_binary(path: str) -> str:
        try:
            before = os.stat(path, follow_symlinks=False)
            if stat.S_ISLNK(before.st_mode) or not stat.S_ISREG(before.st_mode):
                raise OSError
            if before.st_size < 0 or before.st_size > MAX_BINARY_HASH_BYTES:
                raise OSError
            digest = hashlib.sha256()
            with open(path, "rb") as stream:
                remaining = before.st_size
                while remaining:
                    chunk = stream.read(min(1024 * 1024, remaining))
                    if not chunk:
                        raise OSError
                    digest.update(chunk)
                    remaining -= len(chunk)
            after = os.stat(path, follow_symlinks=False)
            if (before.st_size, before.st_mtime_ns) != (after.st_size, after.st_mtime_ns):
                raise OSError
            return digest.hexdigest()
        except Exception:
            raise BackendError("permission_denied", "target binary could not be hashed")

    def _process_identity(self, process_id: int, include_binary_hash: bool) -> Tuple[ApplicationIdentity, int]:
        process_id = _bounded_int(process_id, "process id", 1, 0xFFFFFFFF)
        include_binary_hash = _strict_bool(include_binary_hash, "include binary hash")
        handle = self._open_process(process_id)
        try:
            path = self._process_path(handle)
            start = self._process_start_time(handle)
            publisher, product = self._file_version_strings(path)
            signature_valid = self._verify_signature(path)
            digest = self._hash_binary(path) if include_binary_hash else None
            app = ApplicationIdentity(
                aumid=self._process_aumid(handle),
                publisher=publisher,
                product=product,
                binary_path=path,
                binary_sha256=digest,
                signature_valid=signature_valid,
            )
            return app, start
        finally:
            self._close_handle(handle, "target process could not be closed")

    def _window_text(self, hwnd: int) -> str:
        length = int(self.user32.GetWindowTextLengthW(hwnd))
        if length < 0 or length > MAX_TITLE_CHARS:
            raise BackendError("stale_window", "window title exceeds its size limit")
        size = max(length + 1, 1)
        buffer = ctypes.create_unicode_buffer(size)
        copied = int(self.user32.GetWindowTextW(hwnd, buffer, size))
        if copied < 0 or copied > length or len(buffer.value) != copied:
            raise BackendError("stale_window", "window title could not be read consistently")
        current_length = int(self.user32.GetWindowTextLengthW(hwnd))
        if current_length != length:
            raise BackendError("stale_window", "window title changed while being read")
        return _check_string(buffer.value, "window title", MAX_TITLE_CHARS, allow_empty=True)

    def _window_class(self, hwnd: int) -> str:
        buffer = ctypes.create_unicode_buffer(MAX_CLASS_CHARS + 2)
        result = int(self.user32.GetClassNameW(hwnd, buffer, len(buffer)))
        if result <= 0 or result > MAX_CLASS_CHARS or len(buffer.value) != result:
            raise BackendError("stale_window", "window class could not be read within its limit")
        return _check_string(buffer.value, "window class", MAX_CLASS_CHARS)

    def _window_rect(self, hwnd: int) -> Rect:
        result = wintypes.RECT()
        if not _native_bool(
            self.user32.GetWindowRect(hwnd, ctypes.byref(result)),
            "GetWindowRect",
        ):
            raise BackendError("stale_window", "window bounds could not be read")
        values = (
            _bounded_int(int(result.left), "window.left", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT),
            _bounded_int(int(result.top), "window.top", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT),
            _bounded_int(int(result.right), "window.right", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT),
            _bounded_int(int(result.bottom), "window.bottom", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT),
        )
        rect = Rect(*values)
        if not rect.is_valid or rect.width > 2 * MAX_COORDINATE_ARGUMENT or rect.height > 2 * MAX_COORDINATE_ARGUMENT:
            raise BackendError("stale_window", "window bounds are invalid")
        return rect

    def _client_rect(self, hwnd: int) -> Rect:
        result = wintypes.RECT()
        if not _native_bool(
            self.user32.GetClientRect(hwnd, ctypes.byref(result)),
            "GetClientRect",
        ):
            raise BackendError("stale_window", "client bounds could not be read")
        left = int(result.left)
        top = int(result.top)
        right = int(result.right)
        bottom = int(result.bottom)
        if left != 0 or top != 0:
            raise BackendError("stale_window", "client bounds have an unexpected origin")
        _bounded_int(right, "client.right", 1, MAX_FRAME_WIDTH)
        _bounded_int(bottom, "client.bottom", 1, MAX_FRAME_HEIGHT)
        return Rect(0, 0, right, bottom)

    def _window_desktop(self, thread_id: int, process_id: int) -> DesktopIdentity:
        thread_id = _bounded_int(thread_id, "thread id", 1, 0xFFFFFFFF)
        process_id = _bounded_int(process_id, "process id", 1, 0xFFFFFFFF)
        handle = self.user32.GetThreadDesktop(thread_id)
        self._require_handle(handle, "permission_denied", "target desktop could not be opened")
        name = self._user_object_name(handle)
        return DesktopIdentity(
            name=name,
            station="WinSta0",
            session_id=self._session_id(process_id),
            secure=name.casefold() in SECURE_DESKTOP_NAMES,
        )

    def enumerate_top_level_windows(self) -> Iterable[int]:
        result: List[int] = []
        seen = 0
        overflow = False
        invalid_handle = False
        callback_type = self._enum_callback_type

        def callback(hwnd: Any, _parameter: Any) -> bool:
            nonlocal seen, overflow, invalid_handle
            if seen >= MAX_ENUMERATED_WINDOWS:
                overflow = True
                return False
            seen += 1
            try:
                value = self._handle_value(hwnd)
            except BackendError:
                invalid_handle = True
                return False
            if value <= 0:
                invalid_handle = True
                return False
            result.append(value)
            return True

        callback_pointer = callback_type(callback)
        completed = _native_bool(self.user32.EnumWindows(callback_pointer, 0), "EnumWindows")
        if overflow:
            raise BackendError("backend_unavailable", "top-level window count exceeds its limit")
        if invalid_handle:
            raise BackendError("backend_unavailable", "top-level enumeration returned an invalid handle")
        if not completed:
            raise BackendError("permission_denied", "top-level windows could not be enumerated")
        return tuple(result)

    def snapshot_window(self, hwnd: int, include_binary_hash: bool = False) -> WindowIdentity:
        hwnd = _bounded_int(hwnd, "HWND", 1, (1 << 63) - 1)
        include_binary_hash = _strict_bool(include_binary_hash, "include binary hash")
        if not _native_bool(self.user32.IsWindow(hwnd), "IsWindow"):
            raise BackendError("stale_window", "window handle is invalid")
        root = self.user32.GetAncestor(hwnd, self._GA_ROOT)
        if self._handle_value(root) != hwnd:
            raise BackendError("invalid_target", "target must be a top-level window")
        process_id = ctypes.c_uint32(0)
        thread_id = _bounded_int(
            int(self.user32.GetWindowThreadProcessId(hwnd, ctypes.byref(process_id))),
            "thread id",
            1,
            0xFFFFFFFF,
        )
        process_value = _bounded_int(int(process_id.value), "process id", 1, 0xFFFFFFFF)
        app, start = self._process_identity(process_value, include_binary_hash)
        desktop = self._window_desktop(thread_id, process_value)
        owner = self.user32.GetWindow(hwnd, 4)  # GW_OWNER
        parent = self.user32.GetParent(hwnd)
        return WindowIdentity(
            hwnd=hwnd,
            process_id=process_value,
            process_start_time=start,
            app=app,
            desktop=desktop,
            class_name=self._window_class(hwnd),
            title=self._window_text(hwnd),
            thread_id=thread_id,
            parent_hwnd=self._handle_value(parent),
            owner_hwnd=self._handle_value(owner),
            integrity_level=self.process_integrity_level(process_value),
            rect=self._window_rect(hwnd),
            client_rect=self._client_rect(hwnd),
            visible=_native_bool(self.user32.IsWindowVisible(hwnd), "IsWindowVisible"),
            enabled=_native_bool(self.user32.IsWindowEnabled(hwnd), "IsWindowEnabled"),
            minimized=_native_bool(self.user32.IsIconic(hwnd), "IsIconic"),
            security_ui=(desktop.is_secure or _binary_name(app.binary_path) in SECURITY_UI_EXECUTABLES),
        )

    def process_integrity_level(self, process_id: int) -> int:
        handle = self._open_process(process_id)
        try:
            token = ctypes.c_void_p()
            if not _native_bool(
                self.advapi32.OpenProcessToken(handle, self._TOKEN_QUERY, ctypes.byref(token)),
                "OpenProcessToken",
            ):
                raise BackendError("permission_denied", "target token could not be read")
            try:
                self._require_handle(token, "permission_denied", "target token could not be read")
                return self._token_integrity(token)
            finally:
                self._close_handle(token, "target token could not be closed")
        finally:
            self._close_handle(handle, "target process could not be closed")

    def foreground_window(self) -> int:
        return self._handle_value(self.user32.GetForegroundWindow())

    def activate_window(self, hwnd: int) -> bool:
        hwnd = _bounded_int(hwnd, "HWND", 1, (1 << 63) - 1)
        if not _native_bool(self.user32.IsWindow(hwnd), "IsWindow"):
            return False
        if _native_bool(self.user32.IsIconic(hwnd), "IsIconic"):
            _native_bool(self.user32.ShowWindow(hwnd, self._SW_RESTORE), "ShowWindow")
        return _native_bool(self.user32.SetForegroundWindow(hwnd), "SetForegroundWindow")

    def client_to_screen(self, hwnd: int, x: int, y: int) -> Tuple[int, int]:
        hwnd = _bounded_int(hwnd, "HWND", 1, (1 << 63) - 1)
        x = _bounded_int(x, "client.x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        y = _bounded_int(y, "client.y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        point = _POINT(x, y)
        if not _native_bool(
            self.user32.ClientToScreen(hwnd, ctypes.byref(point)),
            "ClientToScreen",
        ):
            raise BackendError("coordinate_out_of_bounds", "client coordinate could not be transformed")
        return (
            _bounded_int(int(point.x), "screen.x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT),
            _bounded_int(int(point.y), "screen.y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT),
        )

    def virtual_screen_rect(self) -> Rect:
        left = _bounded_int(
            int(self.user32.GetSystemMetrics(self._SM_XVIRTUALSCREEN)),
            "virtual screen.left",
            -MAX_COORDINATE_ARGUMENT,
            MAX_COORDINATE_ARGUMENT,
        )
        top = _bounded_int(
            int(self.user32.GetSystemMetrics(self._SM_YVIRTUALSCREEN)),
            "virtual screen.top",
            -MAX_COORDINATE_ARGUMENT,
            MAX_COORDINATE_ARGUMENT,
        )
        width = _bounded_int(
            int(self.user32.GetSystemMetrics(self._SM_CXVIRTUALSCREEN)),
            "virtual screen.width",
            1,
            2 * MAX_COORDINATE_ARGUMENT,
        )
        height = _bounded_int(
            int(self.user32.GetSystemMetrics(self._SM_CYVIRTUALSCREEN)),
            "virtual screen.height",
            1,
            2 * MAX_COORDINATE_ARGUMENT,
        )
        right = _bounded_int(left + width, "virtual screen.right", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        bottom = _bounded_int(top + height, "virtual screen.bottom", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        return Rect(left, top, right, bottom)

    def _send_inputs(self, inputs: Sequence[_INPUT]) -> bool:
        if not inputs:
            return True
        _bounded_int(len(inputs), "native input count", 1, MAX_TEXT_BYTES)
        array_type = _INPUT * len(inputs)
        array = array_type(*inputs)
        sent = _bounded_int(
            int(self.user32.SendInput(len(inputs), array, ctypes.sizeof(_INPUT))),
            "SendInput result",
            0,
            len(inputs),
        )
        return sent == len(inputs)

    def _absolute_coordinates(self, x: int, y: int) -> Tuple[int, int]:
        x = _bounded_int(x, "screen.x", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        y = _bounded_int(y, "screen.y", -MAX_COORDINATE_ARGUMENT, MAX_COORDINATE_ARGUMENT)
        bounds = self.virtual_screen_rect()
        if not bounds.contains(x, y):
            raise BackendError("coordinate_out_of_bounds", "screen point is outside virtual desktop")
        nx = round((x - bounds.left) * 65535 / max(bounds.width - 1, 1))
        ny = round((y - bounds.top) * 65535 / max(bounds.height - 1, 1))
        return (
            _bounded_int(int(nx), "absolute.x", 0, 65535),
            _bounded_int(int(ny), "absolute.y", 0, 65535),
        )

    def send_mouse_move(self, x: int, y: int) -> bool:
        nx, ny = self._absolute_coordinates(x, y)
        event = _INPUT(
            type=self._INPUT_MOUSE,
            mi=_MOUSEINPUT(
                dx=nx,
                dy=ny,
                mouseData=0,
                dwFlags=self._MOUSEEVENTF_MOVE | self._MOUSEEVENTF_ABSOLUTE | self._MOUSEEVENTF_VIRTUALDESK,
                time=0,
                dwExtraInfo=None,
            ),
        )
        return self._send_inputs([event])

    def send_mouse_button(self, button: str, down: bool) -> bool:
        button = _check_string(button, "mouse button", 16)
        down = _strict_bool(down, "mouse button state")
        flags = {
            "left": self._MOUSEEVENTF_LEFTDOWN if down else self._MOUSEEVENTF_LEFTUP,
            "right": self._MOUSEEVENTF_RIGHTDOWN if down else self._MOUSEEVENTF_RIGHTUP,
            "middle": self._MOUSEEVENTF_MIDDLEDOWN if down else self._MOUSEEVENTF_MIDDLEUP,
        }.get(button)
        if flags is None:
            raise BackendError("invalid_input", "unsupported mouse button")
        event = _INPUT(
            type=self._INPUT_MOUSE,
            mi=_MOUSEINPUT(dx=0, dy=0, mouseData=0, dwFlags=flags, time=0, dwExtraInfo=None),
        )
        return self._send_inputs([event])

    def send_mouse_wheel(self, delta_x: int, delta_y: int) -> bool:
        delta_x = _bounded_int(delta_x, "horizontal scroll delta", -MAX_SCROLL_DELTA, MAX_SCROLL_DELTA)
        delta_y = _bounded_int(delta_y, "vertical scroll delta", -MAX_SCROLL_DELTA, MAX_SCROLL_DELTA)
        inputs: List[_INPUT] = []
        if delta_y:
            inputs.append(
                _INPUT(
                    type=self._INPUT_MOUSE,
                    mi=_MOUSEINPUT(
                        dx=0,
                        dy=0,
                        mouseData=ctypes.c_uint32(delta_y).value,
                        dwFlags=self._MOUSEEVENTF_WHEEL,
                        time=0,
                        dwExtraInfo=None,
                    ),
                )
            )
        if delta_x:
            inputs.append(
                _INPUT(
                    type=self._INPUT_MOUSE,
                    mi=_MOUSEINPUT(
                        dx=0,
                        dy=0,
                        mouseData=ctypes.c_uint32(delta_x).value,
                        dwFlags=self._MOUSEEVENTF_HWHEEL,
                        time=0,
                        dwExtraInfo=None,
                    ),
                )
            )
        return self._send_inputs(inputs)

    def send_key(self, key: str, down: bool) -> bool:
        key = _check_string(key, "key", 32)
        down = _strict_bool(down, "key state")
        normalized = key.upper().replace(" ", "_")
        virtual_key = KEY_VIRTUAL_CODES.get(normalized)
        if virtual_key is None:
            raise BackendError("input_rejected", "key is not in the navigation allowlist")
        event = _INPUT(
            type=self._INPUT_KEYBOARD,
            ki=_KEYBDINPUT(
                wVk=virtual_key,
                wScan=0,
                dwFlags=0 if down else self._KEYEVENTF_KEYUP,
                time=0,
                dwExtraInfo=None,
            ),
        )
        return self._send_inputs([event])

    def send_unicode_text(self, text: str) -> bool:
        text = _check_string(text, "text", MAX_TEXT_CHARS)
        units = text.encode("utf-16-le", "strict")
        if len(units) > MAX_TEXT_BYTES:
            raise BackendError("input_too_large", "text exceeds its byte limit")
        inputs: List[_INPUT] = []
        for offset in range(0, len(units), 2):
            unit = units[offset] | (units[offset + 1] << 8)
            inputs.append(
                _INPUT(
                    type=self._INPUT_KEYBOARD,
                    ki=_KEYBDINPUT(
                        wVk=0,
                        wScan=unit,
                        dwFlags=self._KEYEVENTF_UNICODE,
                        time=0,
                        dwExtraInfo=None,
                    ),
                )
            )
            inputs.append(
                _INPUT(
                    type=self._INPUT_KEYBOARD,
                    ki=_KEYBDINPUT(
                        wVk=0,
                        wScan=unit,
                        dwFlags=self._KEYEVENTF_UNICODE | self._KEYEVENTF_KEYUP,
                        time=0,
                        dwExtraInfo=None,
                    ),
                )
            )
        return self._send_inputs(inputs)

    def input_field_at(self, hwnd: int, x: int, y: int) -> Optional[InputFieldInfo]:
        # UI Automation is intentionally not inferred from window class or
        # pixels.  Until a separately reviewed UIA adapter is supplied,
        # arbitrary text entry is handed off rather than guessed safe.
        return None

    def capture_window(self, hwnd: int, max_width: int, max_height: int) -> Screenshot:
        hwnd = _bounded_int(hwnd, "HWND", 1, (1 << 63) - 1)
        max_width = _bounded_int(max_width, "maximum capture width", 1, MAX_FRAME_WIDTH)
        max_height = _bounded_int(max_height, "maximum capture height", 1, MAX_FRAME_HEIGHT)
        client = self._client_rect(hwnd)
        width, height = client.width, client.height
        if width <= 0 or height <= 0 or width > max_width or height > max_height:
            raise BackendError("observation_too_large", "window client area exceeds capture bounds")
        size = width * height * 4
        if width * height > MAX_FRAME_PIXELS or size + 54 > MAX_FRAME_BYTES:
            raise BackendError("observation_too_large", "window capture exceeds byte bounds")
        window_dc = self.user32.GetDC(hwnd)
        self._require_handle(window_dc, "observation_failed", "window DC could not be opened")
        memory_dc = None
        bitmap = None
        old_bitmap = None
        try:
            memory_dc = self.gdi32.CreateCompatibleDC(window_dc)
            self._require_handle(memory_dc, "observation_failed", "capture DC could not be created")
            bitmap = self.gdi32.CreateCompatibleBitmap(window_dc, width, height)
            self._require_handle(bitmap, "observation_failed", "capture bitmap could not be created")
            old_bitmap = self.gdi32.SelectObject(memory_dc, bitmap)
            self._require_handle(old_bitmap, "observation_failed", "capture bitmap could not be selected")
            if not _native_bool(
                self.gdi32.BitBlt(memory_dc, 0, 0, width, height, window_dc, 0, 0, self._SRCCOPY),
                "BitBlt",
            ):
                raise BackendError("observation_failed", "window pixels could not be copied")

            class _BITMAPINFOHEADER(ctypes.Structure):
                _fields_ = [
                    ("biSize", ctypes.c_uint32),
                    ("biWidth", ctypes.c_int32),
                    ("biHeight", ctypes.c_int32),
                    ("biPlanes", ctypes.c_uint16),
                    ("biBitCount", ctypes.c_uint16),
                    ("biCompression", ctypes.c_uint32),
                    ("biSizeImage", ctypes.c_uint32),
                    ("biXPelsPerMeter", ctypes.c_int32),
                    ("biYPelsPerMeter", ctypes.c_int32),
                    ("biClrUsed", ctypes.c_uint32),
                    ("biClrImportant", ctypes.c_uint32),
                ]

            class _BITMAPINFO(ctypes.Structure):
                _fields_ = [("bmiHeader", _BITMAPINFOHEADER), ("bmiColors", ctypes.c_uint32 * 1)]

            info = _BITMAPINFO()
            info.bmiHeader.biSize = ctypes.sizeof(_BITMAPINFOHEADER)
            info.bmiHeader.biWidth = width
            info.bmiHeader.biHeight = -height
            info.bmiHeader.biPlanes = 1
            info.bmiHeader.biBitCount = 32
            info.bmiHeader.biCompression = self._BI_RGB
            pixels = ctypes.create_string_buffer(size)
            copied = _bounded_int(
                int(
                    self.gdi32.GetDIBits(
                        memory_dc,
                        bitmap,
                        0,
                        height,
                        pixels,
                        ctypes.byref(info),
                        self._DIB_RGB_COLORS,
                    )
                ),
                "GetDIBits result",
                0,
                height,
            )
            if copied != height:
                raise BackendError("observation_failed", "window pixels could not be read")
            payload = pixels.raw[:size]
            header = struct.pack("<2sIHHI", b"BM", 54 + len(payload), 0, 0, 54)
            dib = struct.pack(
                "<IiiHHIIiiII",
                40,
                width,
                -height,
                1,
                32,
                0,
                len(payload),
                0,
                0,
                0,
                0,
            )
            return Screenshot(header + dib + payload, width, height, width * 4, "bmp")
        finally:
            cleanup_error: Optional[BackendError] = None

            def cleanup(call: Callable[[], None]) -> None:
                nonlocal cleanup_error
                try:
                    call()
                except BackendError as error:
                    if cleanup_error is None:
                        cleanup_error = error
                except Exception:
                    if cleanup_error is None:
                        cleanup_error = BackendError("backend_unavailable", "capture resource cleanup failed")

            if old_bitmap is not None and memory_dc is not None:
                def restore_bitmap() -> None:
                    restored = self.gdi32.SelectObject(memory_dc, old_bitmap)
                    self._require_handle(restored, "backend_unavailable", "capture bitmap could not be restored")

                cleanup(restore_bitmap)
            if bitmap is not None:
                def delete_bitmap() -> None:
                    if not _native_bool(self.gdi32.DeleteObject(bitmap), "DeleteObject"):
                        raise BackendError("backend_unavailable", "capture bitmap could not be deleted")

                cleanup(delete_bitmap)
            if memory_dc is not None:
                def delete_memory_dc() -> None:
                    if not _native_bool(self.gdi32.DeleteDC(memory_dc), "DeleteDC"):
                        raise BackendError("backend_unavailable", "capture DC could not be deleted")

                cleanup(delete_memory_dc)
            def release_window_dc() -> None:
                if not _native_bool(self.user32.ReleaseDC(hwnd, window_dc), "ReleaseDC"):
                    raise BackendError("backend_unavailable", "window DC could not be released")

            cleanup(release_window_dc)
            if cleanup_error is not None:
                raise cleanup_error


__all__ = [
    "ActionResult",
    "ApplicationIdentity",
    "ApplicationTarget",
    "BackendError",
    "ComputerUseError",
    "DesktopIdentity",
    "InputAction",
    "InputFieldInfo",
    "Observation",
    "OperationCancelled",
    "OperationTimedOut",
    "Rect",
    "Screenshot",
    "TargetSpec",
    "WindowIdentity",
    "WindowsBackend",
    "WindowsBackendConfig",
    "WindowsBackendError",
    "WindowsComputerUseBackend",
    "WindowsSystem",
    "Win32System",
    "MAX_ACTIONS",
    "MAX_FRAME_BYTES",
    "MAX_FRAME_HEIGHT",
    "MAX_FRAME_PIXELS",
    "MAX_FRAME_WIDTH",
    "MAX_TEXT_BYTES",
    "MAX_TEXT_CHARS",
]
