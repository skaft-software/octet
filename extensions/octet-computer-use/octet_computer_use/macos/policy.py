"""Policy checks for the native macOS boundary."""

from __future__ import annotations

import math
from dataclasses import dataclass
from typing import Any, FrozenSet, Optional

from .model import (
    MAX_DEPTH,
    MAX_NODES,
    MAX_SCREENSHOT_BYTES,
    MAX_TEXT_BYTES,
    MacOSBackendError,
    TargetIdentity,
)


SENSITIVE_TERMS = (
    "password",
    "passcode",
    "one-time code",
    "one time code",
    "security code",
    "verification code",
    "authentication code",
    "auth code",
    "username",
    "user name",
    "login",
    "credential",
    "credit card",
    "card number",
    "cvv",
    "cvc",
    "keychain",
    "private key",
    "secret",
    "recovery code",
)

MAX_ALLOWED_BUNDLE_IDS = 128

_ALLOWED_KEYS = frozenset(
    {
        "Return",
        "Enter",
        "Escape",
        "Tab",
        "Space",
        "Backspace",
        "Delete",
        "Left",
        "Right",
        "Up",
        "Down",
        "Home",
        "End",
        "PageUp",
        "PageDown",
    }
)


@dataclass(frozen=True)
class MacOSPolicy:
    """Explicit opt-in and bounded limits for one backend instance.

    ``allowed_bundle_ids=None`` means that the caller must still provide an
    exact target identity, but does not impose a static application allowlist.
    Supplying a collection makes that allowlist exact and case-sensitive.
    """

    opt_in: bool = False
    allow_input: bool = False
    allowed_bundle_ids: Optional[FrozenSet[str]] = None
    require_confirmation: bool = True
    require_foreground: bool = True
    max_nodes: int = MAX_NODES
    max_depth: int = MAX_DEPTH
    max_text_bytes: int = MAX_TEXT_BYTES
    max_screenshot_bytes: int = MAX_SCREENSHOT_BYTES
    max_scroll_delta: int = 2_000
    max_drag_seconds: float = 5.0

    def __post_init__(self) -> None:
        for name in ("opt_in", "allow_input", "require_confirmation", "require_foreground"):
            if type(getattr(self, name)) is not bool:
                raise ValueError(f"{name} must be a boolean")
        if self.allowed_bundle_ids is not None:
            if isinstance(self.allowed_bundle_ids, (str, bytes, bytearray)):
                raise ValueError("allowed_bundle_ids must be a collection of bundle identifiers")
            try:
                values = []
                for index, item in enumerate(self.allowed_bundle_ids):
                    if index >= MAX_ALLOWED_BUNDLE_IDS:
                        raise ValueError("allowed_bundle_ids exceeds its bounded item limit")
                    values.append(item)
                normalized = frozenset(values)
            except (TypeError, ValueError) as error:
                raise ValueError("allowed_bundle_ids must be a collection of bundle identifiers") from error
            try:
                for item in normalized:
                    TargetIdentity(bundle_id=item, pid=1, window_id=1)
            except (TypeError, ValueError) as error:
                raise ValueError("allowed_bundle_ids must contain valid bundle identifiers") from error
            object.__setattr__(self, "allowed_bundle_ids", normalized)
        for name, maximum in (
            ("max_nodes", MAX_NODES),
            ("max_depth", MAX_DEPTH),
            ("max_text_bytes", MAX_TEXT_BYTES),
            ("max_screenshot_bytes", MAX_SCREENSHOT_BYTES),
            ("max_scroll_delta", 2_000),
        ):
            value = getattr(self, name)
            if type(value) is not int or not 1 <= value <= maximum:
                raise ValueError(f"{name} is outside its bounded range")
        if type(self.max_drag_seconds) is not int and type(self.max_drag_seconds) is not float:
            raise ValueError("max_drag_seconds is outside its bounded range")
        max_drag_seconds = float(self.max_drag_seconds)
        if not math.isfinite(max_drag_seconds) or not 0.05 <= max_drag_seconds <= 5.0:
            raise ValueError("max_drag_seconds is outside its bounded range")
        object.__setattr__(self, "max_drag_seconds", max_drag_seconds)

    def allows_target(self, target: TargetIdentity) -> bool:
        return self.allowed_bundle_ids is None or target.bundle_id in self.allowed_bundle_ids

    def as_dict(self) -> dict[str, Any]:
        return {
            "opt_in": bool(self.opt_in),
            "allow_input": bool(self.allow_input),
            "allowed_bundle_ids": None
            if self.allowed_bundle_ids is None
            else sorted(self.allowed_bundle_ids),
            "require_confirmation": bool(self.require_confirmation),
            "require_foreground": bool(self.require_foreground),
            "limits": {
                "max_nodes": self.max_nodes,
                "max_depth": self.max_depth,
                "max_text_bytes": self.max_text_bytes,
                "max_screenshot_bytes": self.max_screenshot_bytes,
                "max_scroll_delta": self.max_scroll_delta,
                "max_drag_seconds": self.max_drag_seconds,
            },
        }


def is_sensitive_node(node: Any) -> bool:
    """Classify by role and visible labels without reading an AX value."""

    sensitive = getattr(node, "sensitive", False)
    if type(sensitive) is not bool:
        return True
    if sensitive:
        return True
    labels = [
        value.lower()
        for value in (
            getattr(node, "role", ""),
            getattr(node, "title", ""),
            getattr(node, "description", ""),
        )
        if isinstance(value, str)
    ]
    haystack = " ".join(labels)
    return any(term in haystack for term in SENSITIVE_TERMS)


def validate_key(key: str) -> str:
    if not isinstance(key, str) or key not in _ALLOWED_KEYS:
        raise MacOSBackendError(
            "key_not_allowed",
            "Only bounded, non-text keyboard keys are supported by the native backend.",
        )
    return key


def validate_text(text: str, maximum_bytes: int = MAX_TEXT_BYTES) -> str:
    if not isinstance(text, str) or not text:
        raise MacOSBackendError("invalid_text", "Typed text must be non-empty text.")
    try:
        encoded_length = len(text.encode("utf-8"))
    except UnicodeError as error:
        raise MacOSBackendError("invalid_text", "Typed text is not valid UTF-8 text.") from error
    if encoded_length > maximum_bytes:
        raise MacOSBackendError("invalid_text", "Typed text exceeds the bounded input limit.")
    if "\x00" in text or "\x1b" in text:
        raise MacOSBackendError(
            "invalid_text",
            "NUL and escape characters are not permitted in native text input.",
        )
    if any(ord(character) < 0x20 and character not in "\n\t" for character in text):
        raise MacOSBackendError(
            "invalid_text",
            "Control characters are not permitted in native text input.",
        )
    return text


def validate_scroll(delta_x: int, delta_y: int, maximum: int) -> tuple[int, int]:
    if (
        not isinstance(delta_x, int)
        or isinstance(delta_x, bool)
        or not isinstance(delta_y, int)
        or isinstance(delta_y, bool)
        or abs(delta_x) > maximum
        or abs(delta_y) > maximum
        or (delta_x == 0 and delta_y == 0)
    ):
        raise MacOSBackendError(
            "invalid_scroll",
            "Scroll deltas must be bounded non-zero integers.",
        )
    return delta_x, delta_y


def validate_drag_duration(duration: float, maximum: float) -> float:
    if type(duration) is not int and type(duration) is not float:
        raise MacOSBackendError("invalid_drag", "Drag duration is outside the bounded range.")
    if type(maximum) is not int and type(maximum) is not float:
        raise MacOSBackendError("invalid_drag", "Drag duration is outside the bounded range.")
    duration = float(duration)
    maximum = float(maximum)
    if not math.isfinite(duration) or not math.isfinite(maximum) or duration < 0.0 or duration > maximum:
        raise MacOSBackendError("invalid_drag", "Drag duration is outside the bounded range.")
    return duration


__all__ = [
    "MacOSPolicy",
    "SENSITIVE_TERMS",
    "is_sensitive_node",
    "validate_drag_duration",
    "validate_key",
    "validate_scroll",
    "validate_text",
]
