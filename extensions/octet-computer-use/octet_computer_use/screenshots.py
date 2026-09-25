"""Bounded, owner-scoped automation screenshot references.

This module deliberately keeps screenshot bytes out of extension state.  A
backend supplies an already captured image as bytes, the bytes are admitted
under local limits, and :class:`HostArtifactTransport` hands them to the host
``publish_artifact`` API.  The host is the only durable media authority.  The
store below retains only immutable metadata and opaque artifact references.

The public projection is intentionally API-0.2-shaped: it contains an
explicit text part and image parts carrying ``artifact_id`` values.  It never
returns a data URL or a base64 image body.  Callers must use
:meth:`Projection.to_request` (or ``to_tool_result``), which refuses a request
that was not admitted by the request-byte and retry budgets.
"""

from __future__ import annotations

import hashlib
import json
import math
import os
import re
import stat
import struct
import threading
import time
import uuid
import zlib
from collections import OrderedDict
from dataclasses import dataclass, field, replace
from pathlib import Path
from typing import Any, Callable, Collection, Iterable, Mapping, Optional, Protocol, Sequence, Tuple, Union


SCREENSHOT_SCHEMA = "octet.computer-use.screenshot.v1"
PROJECTION_SCHEMA = "octet.computer-use.screenshot-projection.v1"
DEFAULT_INLINE_ARTIFACT_BYTES = 256 * 1024
DEFAULT_MAX_FRAME_ENCODED_BYTES = 5 * 1024 * 1024
DEFAULT_MAX_FRAME_PIXELS = 16 * 1024 * 1024
DEFAULT_MAX_FRAME_DECODED_BYTES = 64 * 1024 * 1024
DEFAULT_MAX_FRAMES = 64
DEFAULT_MAX_RETAINED_BYTES = 64 * 1024 * 1024
DEFAULT_MAX_RETENTION_SECONDS = 15 * 60.0
DEFAULT_MAX_SESSION_LIFETIME_SECONDS = 60 * 60.0
DEFAULT_MAX_SELECTED_IMAGES = 4
DEFAULT_MAX_REQUEST_ENCODED_BYTES = 8 * 1024 * 1024
DEFAULT_MAX_CUMULATIVE_REQUEST_ENCODED_BYTES = 16 * 1024 * 1024
DEFAULT_MAX_ATTEMPTS = 2
DEFAULT_MAX_HISTORY_ENTRIES = 128
DEFAULT_MAX_ID_BYTES = 256
DEFAULT_MAX_TEXT_BYTES = 4096
DEFAULT_PROTOCOL_OVERHEAD_BYTES = 256
MAX_REASON_BYTES = 128
MAX_IMAGE_DIMENSION = 1 << 24

PNG_SIGNATURE = b"\x89PNG\r\n\x1a\n"
JPEG_SIGNATURE = b"\xff\xd8\xff"
GIF_SIGNATURES = (b"GIF87a", b"GIF89a")
WEBP_SIGNATURE = b"WEBP"
_SUPPORTED_MIME_TYPES = frozenset({"image/png", "image/jpeg", "image/gif", "image/webp"})
_SAFE_CODE = re.compile(r"^[a-z][a-z0-9_.-]{0,127}$")
_FRAME_ID = re.compile(r"^frame_[a-f0-9]{64}$")
_ARTIFACT_ID = re.compile(r"^[A-Za-z0-9_.:-]{1,256}$")
_SHA256 = re.compile(r"^[0-9a-f]{64}$")
_TARGET_TYPE = re.compile(r"^[a-z][a-z0-9_.-]{0,63}$")


class ScreenshotError(RuntimeError):
    """A bounded failure safe to expose as an automation-tool error."""

    def __init__(self, code: str, message: str) -> None:
        if not isinstance(code, str) or _SAFE_CODE.fullmatch(code) is None:
            code = "screenshot_error"
        super().__init__(message)
        self.code = code
        self.message = message[:2048]


class ScreenshotBudgetError(ScreenshotError):
    """A request or retention budget prevented admission."""


class ArtifactTransportError(ScreenshotError):
    """The negotiated host durable-media transport was unavailable."""


class FreshObservationRequired(ScreenshotError):
    """An old observation cannot be used as an action attachment."""


class ArtifactPublisher(Protocol):
    """The narrow host interface consumed by :class:`HostArtifactTransport`.

    This is intentionally the existing extension API rather than a second
    screenshot store.  The host derives owner and process-generation context
    from the active parent request; an extension must not send those values
    as artifact-publication authority.
    """

    negotiated_features: Collection[str]

    def publish_artifact(self, **arguments: Any) -> str:
        ...


class DurableMediaTransport(Protocol):
    """Already-adapted durable media transport used by ``ScreenshotStore``."""

    def publish(
        self,
        *,
        mime_type: str,
        data: bytes,
        size: int,
        sha256: str,
        parent_request_id: Any = None,
    ) -> str:
        ...

    def cleanup(self) -> None:
        ...



def _bounded_string(value: Any, label: str, *, max_bytes: int, allow_empty: bool = False) -> str:
    if not isinstance(value, str) or (not allow_empty and not value):
        raise ScreenshotError("invalid_" + label, "The screenshot " + label + " is invalid.")
    try:
        encoded = value.encode("utf-8")
    except UnicodeEncodeError as error:
        raise ScreenshotError("invalid_" + label, "The screenshot " + label + " is invalid.") from error
    if len(encoded) > max_bytes or any(ord(character) < 0x20 or ord(character) == 0x7F for character in value):
        raise ScreenshotError("invalid_" + label, "The screenshot " + label + " is invalid.")
    return value



def _safe_reason(value: Any, default: str) -> str:
    if not isinstance(value, str):
        return default
    value = value.strip().lower()
    if len(value.encode("utf-8")) > MAX_REASON_BYTES or _SAFE_CODE.fullmatch(value) is None:
        return default
    return value



def _checked_generation(value: Any, label: str = "generation") -> int:
    if not isinstance(value, int) or isinstance(value, bool) or value < 1 or value > (2**64 - 1):
        raise ScreenshotError("invalid_" + label, "The screenshot generation is invalid.")
    return value



def _checked_timestamp(value: Any, label: str = "capture_time") -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise ScreenshotError("invalid_" + label, "The screenshot capture time is invalid.")
    result = float(value)
    if not math.isfinite(result) or result < 0:
        raise ScreenshotError("invalid_" + label, "The screenshot capture time is invalid.")
    return result


@dataclass(frozen=True)
class ResourceOwner:
    """The host-derived owner fence carried by every screenshot reference."""

    session_id: str
    extension_instance_id: str
    process_generation: int

    def __post_init__(self) -> None:
        _bounded_string(self.session_id, "session_id", max_bytes=256)
        _bounded_string(self.extension_instance_id, "extension_instance_id", max_bytes=256)
        _checked_generation(self.process_generation, "process_generation")

    @classmethod
    def from_context(cls, context: Mapping[str, Any]) -> "ResourceOwner":
        value = context.get("resource_owner") if isinstance(context, Mapping) else None
        if not isinstance(value, Mapping):
            raise ScreenshotError(
                "owner_unavailable",
                "Automation screenshots require an active host-derived resource owner.",
            )
        try:
            return cls(
                session_id=value.get("session_id"),
                extension_instance_id=value.get("extension_instance_id"),
                process_generation=value.get("process_generation"),
            )
        except ScreenshotError as error:
            raise ScreenshotError(
                "owner_unavailable",
                "Automation screenshots require a valid host-derived resource owner.",
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


# The Browse extension uses this spelling; retaining the alias makes the
# computer-use contract easy to pass between the two extension adapters.
Owner = ResourceOwner


@dataclass(frozen=True)
class TargetIdentity:
    """An opaque selected window/tab identity, never a page-derived label."""

    target_type: str
    target_id: str
    window_id: Optional[str] = None

    def __post_init__(self) -> None:
        if not isinstance(self.target_type, str) or _TARGET_TYPE.fullmatch(self.target_type) is None:
            raise ScreenshotError("invalid_target", "The screenshot target identity is invalid.")
        _bounded_string(self.target_id, "target_id", max_bytes=256)
        if "/" in self.target_id or "\\" in self.target_id:
            raise ScreenshotError("invalid_target", "The screenshot target identity is invalid.")
        if self.window_id is not None:
            _bounded_string(self.window_id, "window_id", max_bytes=256)
            if "/" in self.window_id or "\\" in self.window_id:
                raise ScreenshotError("invalid_target", "The screenshot target identity is invalid.")

    @classmethod
    def from_value(cls, value: Any) -> "TargetIdentity":
        if isinstance(value, cls):
            return value
        if not isinstance(value, Mapping):
            raise ScreenshotError("invalid_target", "The screenshot target identity is invalid.")
        target_type = value.get("target_type", value.get("kind"))
        target_id = value.get("target_id", value.get("id"))
        return cls(target_type=target_type, target_id=target_id, window_id=value.get("window_id"))

    @property
    def key(self) -> Tuple[str, str, Optional[str]]:
        return (self.target_type, self.target_id, self.window_id)

    def as_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "target_type": self.target_type,
            "target_id": self.target_id,
        }
        if self.window_id is not None:
            result["window_id"] = self.window_id
        return result


Target = TargetIdentity


@dataclass(frozen=True)
class ScreenshotBinding:
    """The complete owner/target/authorization fence for one frame."""

    owner: ResourceOwner
    target: TargetIdentity
    authorization_generation: int

    def __post_init__(self) -> None:
        if not isinstance(self.owner, ResourceOwner):
            raise ScreenshotError("invalid_owner", "The screenshot owner is invalid.")
        if not isinstance(self.target, TargetIdentity):
            raise ScreenshotError("invalid_target", "The screenshot target identity is invalid.")
        _checked_generation(self.authorization_generation, "authorization_generation")

    @property
    def key(self) -> Tuple[Any, ...]:
        return (self.owner.key, self.target.key, self.authorization_generation)

    def as_dict(self) -> dict[str, Any]:
        return {
            "owner": self.owner.as_dict(),
            "target": self.target.as_dict(),
            "authorization_generation": self.authorization_generation,
        }


CaptureContext = ScreenshotBinding


@dataclass(frozen=True)
class ScreenshotLimits:
    """Independent bounds for capture, retention, projection, and retries."""

    max_frame_pixels: int = DEFAULT_MAX_FRAME_PIXELS
    max_frame_encoded_bytes: int = DEFAULT_MAX_FRAME_ENCODED_BYTES
    max_frame_decoded_bytes: int = DEFAULT_MAX_FRAME_DECODED_BYTES
    max_frames: int = DEFAULT_MAX_FRAMES
    max_retained_bytes: int = DEFAULT_MAX_RETAINED_BYTES
    max_retention_seconds: float = DEFAULT_MAX_RETENTION_SECONDS
    max_session_lifetime_seconds: float = DEFAULT_MAX_SESSION_LIFETIME_SECONDS
    max_selected_images: int = DEFAULT_MAX_SELECTED_IMAGES
    max_request_encoded_bytes: int = DEFAULT_MAX_REQUEST_ENCODED_BYTES
    max_cumulative_request_encoded_bytes: int = DEFAULT_MAX_CUMULATIVE_REQUEST_ENCODED_BYTES
    max_attempts: int = DEFAULT_MAX_ATTEMPTS
    max_history_entries: int = DEFAULT_MAX_HISTORY_ENTRIES
    inline_artifact_bytes: int = DEFAULT_INLINE_ARTIFACT_BYTES

    def __post_init__(self) -> None:
        integer_fields = (
            "max_frame_pixels",
            "max_frame_encoded_bytes",
            "max_frame_decoded_bytes",
            "max_frames",
            "max_retained_bytes",
            "max_selected_images",
            "max_request_encoded_bytes",
            "max_cumulative_request_encoded_bytes",
            "max_attempts",
            "max_history_entries",
            "inline_artifact_bytes",
        )
        for name in integer_fields:
            value = getattr(self, name)
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise ScreenshotError("invalid_limits", "Screenshot limits must be positive integers.")
        for name in ("max_retention_seconds", "max_session_lifetime_seconds"):
            value = getattr(self, name)
            if isinstance(value, bool) or not isinstance(value, (int, float)) or not math.isfinite(float(value)) or value <= 0:
                raise ScreenshotError("invalid_limits", "Screenshot lifetimes must be positive and finite.")
        if self.inline_artifact_bytes > self.max_frame_encoded_bytes:
            raise ScreenshotError("invalid_limits", "The inline artifact bound exceeds the frame bound.")
        if self.max_frame_encoded_bytes > self.max_retained_bytes:
            raise ScreenshotError("invalid_limits", "The frame bound exceeds the retained-byte bound.")
        if self.max_selected_images > self.max_frames:
            raise ScreenshotError("invalid_limits", "The selected-image bound exceeds the frame bound.")
        if self.max_request_encoded_bytes > self.max_cumulative_request_encoded_bytes:
            raise ScreenshotError("invalid_limits", "The request bound exceeds the cumulative request bound.")


@dataclass(frozen=True)
class ScreenshotOmission:
    """A payload-free capture or projection omission reason."""

    reason: str
    owner: ResourceOwner
    target: TargetIdentity
    authorization_generation: int
    occurred_at: float
    phase: str = "capture"
    frame_id: Optional[str] = None
    redaction_state: str = "none"
    retention_state: str = "not_retained"
    details: Tuple[Tuple[str, Union[int, str, bool]], ...] = field(default_factory=tuple)

    def __post_init__(self) -> None:
        if _SAFE_CODE.fullmatch(self.reason) is None:
            raise ScreenshotError("invalid_omission", "The screenshot omission reason is invalid.")
        if _SAFE_CODE.fullmatch(self.phase) is None:
            raise ScreenshotError("invalid_omission", "The screenshot omission phase is invalid.")
        _checked_timestamp(self.occurred_at, "omission_time")
        _checked_generation(self.authorization_generation, "authorization_generation")
        if self.frame_id is not None and _FRAME_ID.fullmatch(self.frame_id) is None:
            raise ScreenshotError("invalid_omission", "The screenshot frame reference is invalid.")
        if self.redaction_state not in {"none", "withheld", "redacted", "manual"}:
            raise ScreenshotError("invalid_omission", "The screenshot redaction state is invalid.")
        if self.retention_state not in {"not_retained", "expired", "evicted", "missing", "corrupt", "settled"}:
            raise ScreenshotError("invalid_omission", "The screenshot retention state is invalid.")
        for key, value in self.details:
            _bounded_string(key, "metadata_key", max_bytes=64)
            if not isinstance(value, (bool, int, str)):
                raise ScreenshotError("invalid_omission", "Screenshot omission metadata is invalid.")
            if isinstance(value, str):
                _bounded_string(value, "metadata_value", max_bytes=256, allow_empty=True)

    @classmethod
    def make(
        cls,
        *,
        reason: str,
        binding: ScreenshotBinding,
        occurred_at: float,
        phase: str,
        frame_id: Optional[str] = None,
        redaction_state: str = "none",
        retention_state: str = "not_retained",
        details: Optional[Mapping[str, Union[int, str, bool]]] = None,
    ) -> "ScreenshotOmission":
        pairs: list[Tuple[str, Union[int, str, bool]]] = []
        if details:
            for key in sorted(details):
                if len(pairs) >= 8:
                    break
                pairs.append((str(key), details[key]))
        return cls(
            reason=_safe_reason(reason, "screenshot_omitted"),
            owner=binding.owner,
            target=binding.target,
            authorization_generation=binding.authorization_generation,
            occurred_at=occurred_at,
            phase=_safe_reason(phase, "capture"),
            frame_id=frame_id,
            redaction_state=redaction_state,
            retention_state=retention_state,
            details=tuple(pairs),
        )

    @property
    def details_dict(self) -> dict[str, Union[int, str, bool]]:
        return dict(self.details)

    def as_dict(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "schema": SCREENSHOT_SCHEMA,
            "kind": "omission",
            "phase": self.phase,
            "reason": self.reason,
            "owner": self.owner.as_dict(),
            "target": self.target.as_dict(),
            "authorization_generation": self.authorization_generation,
            "occurred_at": self.occurred_at,
            "redaction_state": self.redaction_state,
            "retention_state": self.retention_state,
        }
        if self.frame_id is not None:
            result["frame_id"] = self.frame_id
        if self.details:
            result["details"] = self.details_dict
        return result


# A shorter name is useful to callers handling capture receipts.
CaptureOmission = ScreenshotOmission


@dataclass(frozen=True)
class ScreenshotReference:
    """Immutable metadata for one owner-bound screenshot observation."""

    frame_id: str
    content_id: str
    artifact_id: str
    sha256: str
    mime_type: str
    width: int
    height: int
    pixel_count: int
    decoded_bytes: int
    encoded_bytes: int
    captured_at: float
    expires_at: float
    owner: ResourceOwner
    target: TargetIdentity
    authorization_generation: int
    retention_state: str = "retained"
    retention_reason: Optional[str] = None
    redaction_state: str = "none"
    action_eligible: bool = False

    def __post_init__(self) -> None:
        if _FRAME_ID.fullmatch(self.frame_id) is None:
            raise ScreenshotError("invalid_reference", "The screenshot frame reference is invalid.")
        if not isinstance(self.content_id, str) or self.content_id != "sha256:" + self.sha256:
            raise ScreenshotError("invalid_reference", "The screenshot content reference is invalid.")
        if _SHA256.fullmatch(self.sha256) is None:
            raise ScreenshotError("invalid_reference", "The screenshot digest is invalid.")
        if _ARTIFACT_ID.fullmatch(self.artifact_id) is None:
            raise ScreenshotError("invalid_reference", "The host artifact reference is invalid.")
        if self.mime_type not in _SUPPORTED_MIME_TYPES:
            raise ScreenshotError("invalid_reference", "The screenshot media type is invalid.")
        for name in ("width", "height", "pixel_count", "decoded_bytes", "encoded_bytes"):
            value = getattr(self, name)
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise ScreenshotError("invalid_reference", "The screenshot dimensions or byte sizes are invalid.")
        if self.pixel_count != self.width * self.height:
            raise ScreenshotError("invalid_reference", "The screenshot pixel count is invalid.")
        _checked_timestamp(self.captured_at, "capture_time")
        _checked_timestamp(self.expires_at, "expiry_time")
        if self.expires_at < self.captured_at:
            raise ScreenshotError("invalid_reference", "The screenshot expiry is invalid.")
        _checked_generation(self.authorization_generation, "authorization_generation")
        if self.retention_state not in {"retained", "expired", "evicted", "missing", "corrupt", "settled"}:
            raise ScreenshotError("invalid_reference", "The screenshot retention state is invalid.")
        if self.retention_reason is not None:
            _bounded_string(self.retention_reason, "retention_reason", max_bytes=MAX_REASON_BYTES)
        if self.redaction_state not in {"none", "withheld", "redacted", "manual"}:
            raise ScreenshotError("invalid_reference", "The screenshot redaction state is invalid.")
        if self.action_eligible:
            raise ScreenshotError("invalid_reference", "Stored screenshot references cannot authorize actions.")

    @property
    def dimensions(self) -> Tuple[int, int]:
        return (self.width, self.height)

    @property
    def digest(self) -> str:
        return self.sha256

    @property
    def is_retained(self) -> bool:
        return self.retention_state == "retained"

    def as_dict(self) -> dict[str, Any]:
        return {
            "schema": SCREENSHOT_SCHEMA,
            "kind": "reference",
            "frame_id": self.frame_id,
            "content_id": self.content_id,
            "artifact_id": self.artifact_id,
            "sha256": self.sha256,
            "mime_type": self.mime_type,
            "width": self.width,
            "height": self.height,
            "pixel_count": self.pixel_count,
            "decoded_bytes": self.decoded_bytes,
            "encoded_bytes": self.encoded_bytes,
            "captured_at": self.captured_at,
            "expires_at": self.expires_at,
            "owner": self.owner.as_dict(),
            "target": self.target.as_dict(),
            "authorization_generation": self.authorization_generation,
            "retention_state": self.retention_state,
            "retention_reason": self.retention_reason,
            "redaction_state": self.redaction_state,
            # A screenshot observation is never an action authorization.
            "action_eligible": False,
        }


@dataclass(frozen=True)
class HistoryEntry:
    """One bounded canonical-history item; never contains image bytes."""

    reference: Optional[ScreenshotReference] = None
    omission: Optional[ScreenshotOmission] = None

    def __post_init__(self) -> None:
        if (self.reference is None) == (self.omission is None):
            raise ScreenshotError("invalid_history", "A screenshot history item must have one kind.")

    def as_dict(self) -> dict[str, Any]:
        if self.reference is not None:
            return {"kind": "reference", "reference": self.reference.as_dict()}
        assert self.omission is not None
        return self.omission.as_dict()


@dataclass(frozen=True)
class CaptureResult:
    """A captured reference or an honest, payload-free omission."""

    reference: Optional[ScreenshotReference] = None
    omission: Optional[ScreenshotOmission] = None
    published_new_asset: bool = False
    deduplicated_asset: bool = False

    def __post_init__(self) -> None:
        if (self.reference is None) == (self.omission is None):
            raise ScreenshotError("invalid_capture_result", "A capture result must have one outcome.")
        if self.omission is not None and (self.published_new_asset or self.deduplicated_asset):
            raise ScreenshotError("invalid_capture_result", "An omitted capture cannot publish an asset.")

    @property
    def captured(self) -> bool:
        return self.reference is not None

    @property
    def artifact_id(self) -> Optional[str]:
        return self.reference.artifact_id if self.reference else None

    @property
    def frame_id(self) -> Optional[str]:
        return self.reference.frame_id if self.reference else self.omission.frame_id if self.omission else None

    def as_dict(self) -> dict[str, Any]:
        if self.reference is not None:
            return {
                "schema": SCREENSHOT_SCHEMA,
                "status": "captured",
                "reference": self.reference.as_dict(),
                "published_new_asset": self.published_new_asset,
                "deduplicated_asset": self.deduplicated_asset,
            }
        assert self.omission is not None
        return {
            "schema": SCREENSHOT_SCHEMA,
            "status": "omitted",
            "omission": self.omission.as_dict(),
        }


@dataclass(frozen=True)
class CleanupReport:
    expired: int = 0
    skipped_pinned: int = 0
    staged_removed: int = 0
    staged_errors: int = 0

    def as_dict(self) -> dict[str, int]:
        return {
            "expired": self.expired,
            "skipped_pinned": self.skipped_pinned,
            "staged_removed": self.staged_removed,
            "staged_errors": self.staged_errors,
        }


@dataclass(frozen=True)
class RecoveryReport:
    checked: int = 0
    missing: int = 0
    corrupt: int = 0
    staged_removed: int = 0
    staged_errors: int = 0

    def as_dict(self) -> dict[str, int]:
        return {
            "checked": self.checked,
            "missing": self.missing,
            "corrupt": self.corrupt,
            "staged_removed": self.staged_removed,
            "staged_errors": self.staged_errors,
        }


@dataclass(frozen=True)
class AttemptAdmission:
    admitted: bool
    reason: Optional[str]
    attempt: int
    encoded_bytes: int
    cumulative_encoded_bytes: int

    def as_dict(self) -> dict[str, Any]:
        return {
            "admitted": self.admitted,
            "reason": self.reason,
            "attempt": self.attempt,
            "encoded_bytes": self.encoded_bytes,
            "cumulative_encoded_bytes": self.cumulative_encoded_bytes,
        }


@dataclass
class RequestBudget:
    """Admission ledger for one provider request and its bounded retries."""

    max_encoded_bytes: int
    max_cumulative_encoded_bytes: int
    max_attempts: int = DEFAULT_MAX_ATTEMPTS
    allow_retries: bool = False
    attempts: int = 0
    cumulative_encoded_bytes: int = 0

    def __post_init__(self) -> None:
        for value in (self.max_encoded_bytes, self.max_cumulative_encoded_bytes, self.max_attempts):
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise ScreenshotError("invalid_request_budget", "Request budgets must be positive integers.")
        if self.max_encoded_bytes > self.max_cumulative_encoded_bytes:
            raise ScreenshotError("invalid_request_budget", "The request budget exceeds its cumulative bound.")
        if not isinstance(self.attempts, int) or self.attempts < 0 or not isinstance(self.cumulative_encoded_bytes, int) or self.cumulative_encoded_bytes < 0:
            raise ScreenshotError("invalid_request_budget", "Request budget accounting is invalid.")

    def admit(
        self,
        projection: "Projection",
        *,
        retry: bool = False,
        host_recovery: bool = False,
    ) -> AttemptAdmission:
        size = projection.request_encoded_bytes
        next_attempt = self.attempts + 1
        reason: Optional[str] = None
        if retry and (not self.allow_retries or not host_recovery):
            reason = "retry_not_authorized"
        elif not projection.request_admitted:
            reason = projection.request_admission_reason or "request_not_admitted"
        elif self.attempts >= self.max_attempts:
            reason = "attempt_count_budget"
        elif size > self.max_encoded_bytes:
            reason = "request_encoded_byte_budget"
        elif self.cumulative_encoded_bytes + size > self.max_cumulative_encoded_bytes:
            reason = "cumulative_request_encoded_byte_budget"
        if reason is not None:
            return AttemptAdmission(
                admitted=False,
                reason=reason,
                attempt=next_attempt,
                encoded_bytes=size,
                cumulative_encoded_bytes=self.cumulative_encoded_bytes,
            )
        self.attempts = next_attempt
        self.cumulative_encoded_bytes += size
        return AttemptAdmission(
            admitted=True,
            reason=None,
            attempt=next_attempt,
            encoded_bytes=size,
            cumulative_encoded_bytes=self.cumulative_encoded_bytes,
        )

    def as_dict(self) -> dict[str, Any]:
        return {
            "max_encoded_bytes": self.max_encoded_bytes,
            "max_cumulative_encoded_bytes": self.max_cumulative_encoded_bytes,
            "max_attempts": self.max_attempts,
            "allow_retries": self.allow_retries,
            "attempts": self.attempts,
            "cumulative_encoded_bytes": self.cumulative_encoded_bytes,
        }


@dataclass(frozen=True)
class Projection:
    """Selected API-0.2 content parts and bounded payload-free accounting."""

    binding: ScreenshotBinding
    content_parts: Tuple[Mapping[str, Any], ...]
    fresh_frame_id: Optional[str]
    selected_frame_ids: Tuple[str, ...]
    requested_frame_ids: Tuple[str, ...]
    omissions: Tuple[ScreenshotOmission, ...]
    frame_metadata: Tuple[Mapping[str, Any], ...]
    selected_image_count: int
    base64_bytes: int
    json_bytes: int
    request_encoded_bytes: int
    omitted_historical_count: int
    request_admitted: bool = True
    request_admission_reason: Optional[str] = None
    attempt: Optional[AttemptAdmission] = None

    @property
    def parts(self) -> Tuple[Mapping[str, Any], ...]:
        return self.content_parts

    @property
    def image_parts(self) -> Tuple[Mapping[str, Any], ...]:
        return tuple(part for part in self.content_parts if part.get("type") == "image")

    @property
    def encoded_bytes(self) -> int:
        return self.request_encoded_bytes

    @property
    def admitted(self) -> bool:
        return self.request_admitted

    def metadata(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "schema": PROJECTION_SCHEMA,
            "owner": self.binding.owner.as_dict(),
            "target": self.binding.target.as_dict(),
            "authorization_generation": self.binding.authorization_generation,
            "fresh_frame_id": self.fresh_frame_id,
            "selected_frame_ids": list(self.selected_frame_ids),
            "requested_frame_ids": list(self.requested_frame_ids),
            "selected_image_count": self.selected_image_count,
            "omitted_historical_count": self.omitted_historical_count,
            "base64_bytes": self.base64_bytes,
            "json_bytes": self.json_bytes,
            "request_encoded_bytes": self.request_encoded_bytes,
            "omissions": [omission.as_dict() for omission in self.omissions],
            "frames": list(self.frame_metadata),
            "request_admitted": self.request_admitted,
            "request_admission_reason": self.request_admission_reason,
        }
        if self.attempt is not None:
            result["attempt"] = self.attempt.as_dict()
        return result

    def to_request(self) -> dict[str, Any]:
        """Return provider-neutral API-0.2 content after explicit admission."""

        if not self.request_admitted:
            raise ScreenshotBudgetError(
                self.request_admission_reason or "request_not_admitted",
                "The screenshot request was not admitted by its bounded transport budget.",
            )
        return {"content": [dict(part) for part in self.content_parts]}

    def to_tool_result(self) -> dict[str, Any]:
        """Return a payload-free API-0.2 tool-result-shaped object."""

        result = self.to_request()
        result["metadata"] = self.metadata()
        return result

    def with_attempt(self, admission: AttemptAdmission) -> "Projection":
        if admission.admitted:
            return replace(self, attempt=admission)
        omissions = list(self.omissions)
        for frame_id in self.selected_frame_ids:
            omissions.append(
                ScreenshotOmission.make(
                    reason=admission.reason or "request_not_admitted",
                    binding=self.binding,
                    occurred_at=time.time(),
                    phase="projection",
                    frame_id=frame_id,
                )
            )
        text_parts = tuple(part for part in self.content_parts if part.get("type") == "text")
        return replace(
            self,
            content_parts=text_parts,
            selected_frame_ids=tuple(),
            frame_metadata=tuple(),
            selected_image_count=0,
            base64_bytes=0,
            json_bytes=0,
            request_encoded_bytes=0,
            omissions=tuple(omissions),
            request_admitted=False,
            request_admission_reason=admission.reason or "request_not_admitted",
            attempt=admission,
        )


@dataclass(frozen=True)
class _ImageInfo:
    mime_type: str
    width: int
    height: int
    decoded_bytes: int


@dataclass(frozen=True)
class _Asset:
    key: Tuple[Any, ...]
    artifact_id: str
    sha256: str
    mime_type: str
    encoded_bytes: int
    created_at: float


@dataclass
class _TransportStageStats:
    removed: int = 0
    errors: int = 0


class HostArtifactTransport:
    """Adapter over the host's existing ``Extension.publish_artifact`` API.

    Small images use the API's inline ``data`` source.  Larger admitted images
    are written only to a generated, private file beneath the host-provided
    ``OCTET_EXTENSION_SCRATCH`` directory and submitted with a relative
    ``path``.  The file is removed immediately after the host snapshots it;
    no screenshot bytes are retained by this adapter.
    """

    _STAGE_DIRECTORY = "octet-computer-use-screenshots"
    _STAGE_PREFIX = "screen-stage-"

    def __init__(
        self,
        extension: ArtifactPublisher,
        *,
        scratch_directory: Optional[Path] = None,
        inline_limit: int = DEFAULT_INLINE_ARTIFACT_BYTES,
    ) -> None:
        self.extension = extension
        self.scratch_directory = Path(scratch_directory) if scratch_directory is not None else None
        if not isinstance(inline_limit, int) or isinstance(inline_limit, bool) or inline_limit <= 0:
            raise ScreenshotError("invalid_transport", "The artifact inline bound is invalid.")
        self.inline_limit = inline_limit
        # Staging is shared by transports in one host scratch root. An instance
        # owns only its unpredictable prefix, never every file in that root.
        self._owned_prefix = self._STAGE_PREFIX + uuid.uuid4().hex + "-"
        self._stage_lock = threading.Lock()
        self._active_stages: set[str] = set()

    @property
    def negotiated_features(self) -> Collection[str]:
        value = getattr(self.extension, "negotiated_features", ())
        return value if isinstance(value, Collection) else ()

    def _require_artifacts(self) -> None:
        if "artifacts" not in self.negotiated_features:
            raise ArtifactTransportError(
                "artifacts_unavailable",
                "The host did not negotiate durable screenshot artifacts.",
            )

    def _scratch_root(self) -> Path:
        raw = self.scratch_directory
        if raw is None:
            value = os.environ.get("OCTET_EXTENSION_SCRATCH")
            if not value:
                raise ArtifactTransportError(
                    "artifacts_unavailable",
                    "The host artifact scratch directory is unavailable.",
                )
            raw = Path(value)
        if not raw.is_absolute():
            raise ArtifactTransportError("unsafe_artifact_path", "The host artifact scratch directory is unsafe.")
        try:
            metadata = raw.lstat()
        except OSError as error:
            raise ArtifactTransportError(
                "artifacts_unavailable",
                "The host artifact scratch directory is unavailable.",
            ) from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ArtifactTransportError("unsafe_artifact_path", "The host artifact scratch directory is unsafe.")
        return raw

    @classmethod
    def _stage_directory(cls, root: Path) -> Path:
        directory = root / cls._STAGE_DIRECTORY
        try:
            metadata = directory.lstat()
        except FileNotFoundError:
            try:
                directory.mkdir(mode=0o700)
            except FileExistsError:
                # Another transport may have created the shared directory.
                pass
            except OSError as error:
                raise ArtifactTransportError("artifact_stage_failed", "The screenshot staging directory is unavailable.") from error
            metadata = directory.lstat()
        except OSError as error:
            raise ArtifactTransportError("unsafe_artifact_path", "The screenshot staging directory is unsafe.") from error
        if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
            raise ArtifactTransportError("unsafe_artifact_path", "The screenshot staging directory is unsafe.")
        try:
            directory.chmod(0o700)
        except OSError:
            pass
        return directory

    @staticmethod
    def _write_private(path: Path, data: bytes) -> None:
        nofollow = getattr(os, "O_NOFOLLOW", 0)
        if not nofollow:
            raise ArtifactTransportError("unsafe_artifact_path", "The screenshot staging path cannot be safely opened.")
        flags = os.O_WRONLY | os.O_CREAT | os.O_EXCL | nofollow | getattr(os, "O_CLOEXEC", 0)
        try:
            fd = os.open(str(path), flags, 0o600)
        except OSError as error:
            raise ArtifactTransportError("artifact_stage_failed", "The screenshot could not be staged safely.") from error
        failed = False
        try:
            metadata = os.fstat(fd)
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                raise ArtifactTransportError("unsafe_artifact_path", "The screenshot staging file is unsafe.")
            view = memoryview(data)
            while view:
                written = os.write(fd, view)
                if written <= 0:
                    raise ArtifactTransportError("artifact_stage_failed", "The screenshot could not be staged safely.")
                view = view[written:]
            os.fsync(fd)
        except ArtifactTransportError:
            failed = True
            raise
        except OSError as error:
            failed = True
            raise ArtifactTransportError("artifact_stage_failed", "The screenshot could not be staged safely.") from error
        finally:
            try:
                os.close(fd)
            except OSError:
                pass
            if failed:
                HostArtifactTransport._unlink(path)

    @staticmethod
    def _unlink(path: Path) -> None:
        try:
            path.unlink()
        except FileNotFoundError:
            pass
        except OSError:
            pass

    def publish(
        self,
        *,
        mime_type: str,
        data: bytes,
        size: int,
        sha256: str,
        parent_request_id: Any = None,
    ) -> str:
        self._require_artifacts()
        if len(data) != size or _SHA256.fullmatch(sha256) is None or hashlib.sha256(data).hexdigest() != sha256:
            raise ArtifactTransportError("artifact_integrity_failed", "The screenshot integrity claim is invalid.")
        arguments: dict[str, Any] = {"mime_type": mime_type, "size": size, "sha256": sha256}
        if parent_request_id is not None:
            arguments["parent_request_id"] = parent_request_id
        if size <= self.inline_limit:
            arguments["data"] = data
            try:
                artifact_id = self.extension.publish_artifact(**arguments)
            except Exception as error:
                raise ArtifactTransportError("artifact_publish_failed", "The host did not publish the screenshot.") from error
            return _validate_artifact_id(artifact_id)

        root = self._scratch_root()
        directory = self._stage_directory(root)
        filename = self._owned_prefix + uuid.uuid4().hex + ".bin"
        destination = directory / filename
        with self._stage_lock:
            self._active_stages.add(filename)
        staged = False
        try:
            self._write_private(destination, data)
            staged = True
            relative = (Path(self._STAGE_DIRECTORY) / filename).as_posix()
            arguments["path"] = relative
            try:
                artifact_id = self.extension.publish_artifact(**arguments)
            except Exception as error:
                raise ArtifactTransportError("artifact_publish_failed", "The host did not publish the screenshot.") from error
            return _validate_artifact_id(artifact_id)
        finally:
            if staged:
                self._unlink(destination)
            with self._stage_lock:
                self._active_stages.discard(filename)

    def cleanup(self) -> None:
        """Remove only this adapter's bounded, generated staging files."""

        with self._stage_lock:
            try:
                root = self._scratch_root()
                directory = root / self._STAGE_DIRECTORY
                metadata = directory.lstat()
                if stat.S_ISLNK(metadata.st_mode) or not stat.S_ISDIR(metadata.st_mode):
                    return
                children = list(directory.iterdir())
            except (ArtifactTransportError, OSError):
                return
            for child in children:
                try:
                    child_metadata = child.lstat()
                except OSError:
                    continue
                if (
                    child.name.startswith(self._owned_prefix)
                    and child.name not in self._active_stages
                    and child.name.endswith(".bin")
                    and stat.S_ISREG(child_metadata.st_mode)
                    and not stat.S_ISLNK(child_metadata.st_mode)
                    and child_metadata.st_nlink == 1
                ):
                    self._unlink(child)



def _validate_artifact_id(value: Any) -> str:
    if not isinstance(value, str) or _ARTIFACT_ID.fullmatch(value) is None:
        raise ArtifactTransportError("artifact_publish_failed", "The host returned an invalid screenshot reference.")
    return value


class ScreenshotStore:
    """In-memory bounded metadata registry backed by host-owned artifacts.

    ``ScreenshotStore`` never retains the input bytes.  Its byte counters are
    intentionally admission counters: the host artifact API has no extension
    deletion capability, so cleanup cannot pretend that an evicted asset freed
    host durable bytes.  A host generation settle must call :meth:`settle`
    after invalidating its artifact generation before a new store is used.
    """

    def __init__(
        self,
        transport: Optional[Any] = None,
        *,
        limits: Optional[ScreenshotLimits] = None,
        owner: Optional[ResourceOwner] = None,
        clock: Callable[[], float] = time.time,
        scratch_directory: Optional[Path] = None,
    ) -> None:
        self.limits = limits or ScreenshotLimits()
        self.scope_owner = owner
        if owner is not None and not isinstance(owner, ResourceOwner):
            raise ScreenshotError("invalid_owner", "The screenshot store owner is invalid.")
        self._clock = clock
        started = _checked_timestamp(clock(), "store_time")
        self._started_at = started
        self._settled = False
        self._lock = threading.RLock()
        if transport is None:
            self._transport: Optional[Any] = None
        elif hasattr(transport, "publish"):
            self._transport = transport
        elif hasattr(transport, "publish_artifact"):
            self._transport = HostArtifactTransport(
                transport,
                scratch_directory=scratch_directory,
                inline_limit=self.limits.inline_artifact_bytes,
            )
        else:
            raise ScreenshotError("invalid_transport", "The screenshot durable-media transport is invalid.")
        self._references: "OrderedDict[str, ScreenshotReference]" = OrderedDict()
        self._states: dict[str, Tuple[str, Optional[str]]] = {}
        self._history: list[HistoryEntry] = []
        self._assets: dict[Tuple[Any, ...], _Asset] = {}
        self._artifact_ids: dict[str, Tuple[Any, ...]] = {}
        self._pinned: set[str] = set()
        self._fresh: dict[Tuple[Any, ...], str] = {}
        self._next_sequence = 0
        self._admitted_frames = 0
        self._admitted_bytes = 0
        self._admitted_assets = 0
        self._omitted = 0

    @property
    def started_at(self) -> float:
        return self._started_at

    @staticmethod
    def _binding(
        *,
        binding: Optional[ScreenshotBinding],
        owner: Optional[Union[ResourceOwner, Mapping[str, Any]]],
        target: Optional[Union[TargetIdentity, Mapping[str, Any]]],
        authorization_generation: Optional[int],
    ) -> ScreenshotBinding:
        if binding is not None:
            if not isinstance(binding, ScreenshotBinding):
                raise ScreenshotError("invalid_binding", "The screenshot binding is invalid.")
            if owner is not None or target is not None or authorization_generation is not None:
                raise ScreenshotError("invalid_binding", "Provide either a binding or its individual fields.")
            return binding
        if isinstance(owner, Mapping):
            owner = ResourceOwner(
                session_id=owner.get("session_id"),
                extension_instance_id=owner.get("extension_instance_id"),
                process_generation=owner.get("process_generation"),
            )
        if not isinstance(owner, ResourceOwner):
            raise ScreenshotError("invalid_owner", "The screenshot owner is required.")
        target_value = TargetIdentity.from_value(target)
        if authorization_generation is None:
            raise ScreenshotError("invalid_authorization_generation", "The screenshot authorization generation is required.")
        return ScreenshotBinding(owner, target_value, authorization_generation)

    def _check_scope(self, binding: ScreenshotBinding) -> None:
        if not isinstance(binding, ScreenshotBinding):
            raise ScreenshotError("invalid_binding", "The screenshot binding is invalid.")
        if self.scope_owner is not None and binding.owner != self.scope_owner:
            raise ScreenshotError("owner_mismatch", "The screenshot owner is not authorized for this store.")

    def _now(self) -> float:
        return _checked_timestamp(self._clock(), "store_time")

    def _lifetime_ok(self, now: float) -> bool:
        return now <= self._started_at + self.limits.max_session_lifetime_seconds

    def _append_history(self, entry: HistoryEntry) -> None:
        self._history.append(entry)
        while len(self._history) > self.limits.max_history_entries:
            self._history.pop(0)

    def _omit(
        self,
        *,
        binding: ScreenshotBinding,
        now: float,
        reason: str,
        phase: str = "capture",
        frame_id: Optional[str] = None,
        redaction_state: str = "none",
        retention_state: str = "not_retained",
        details: Optional[Mapping[str, Union[int, str, bool]]] = None,
    ) -> CaptureResult:
        omission = ScreenshotOmission.make(
            reason=reason,
            binding=binding,
            occurred_at=now,
            phase=phase,
            frame_id=frame_id,
            redaction_state=redaction_state,
            retention_state=retention_state,
            details=details,
        )
        self._append_history(HistoryEntry(omission=omission))
        self._omitted += 1
        return CaptureResult(omission=omission)

    def _check_data(self, data: Any) -> bytes:
        if isinstance(data, bytes):
            raw = data
        elif isinstance(data, bytearray):
            raw = bytes(data)
        elif isinstance(data, memoryview):
            if data.nbytes > self.limits.max_frame_encoded_bytes:
                raise ScreenshotError("frame_encoded_byte_budget", "The screenshot encoded-byte limit was exceeded.")
            raw = data.tobytes()
        else:
            raise ScreenshotError("invalid_screenshot", "The screenshot backend must provide image bytes.")
        if not raw:
            raise ScreenshotError("invalid_screenshot", "The screenshot backend returned an empty image.")
        if len(raw) > self.limits.max_frame_encoded_bytes:
            raise ScreenshotError("frame_encoded_byte_budget", "The screenshot encoded-byte limit was exceeded.")
        return raw

    def _make_frame_id(self, binding: ScreenshotBinding, digest: str) -> str:
        self._next_sequence += 1
        seed = json.dumps(
            [binding.owner.key, binding.target.key, binding.authorization_generation, digest, self._next_sequence],
            separators=(",", ":"),
            ensure_ascii=True,
        ).encode("utf-8")
        return "frame_" + hashlib.sha256(seed).hexdigest()

    def _owner_bytes(self, owner: ResourceOwner) -> int:
        return sum(asset.encoded_bytes for asset in self._assets.values() if asset.key[0] == owner.key)

    def capture(
        self,
        data: Any,
        *,
        binding: Optional[ScreenshotBinding] = None,
        owner: Optional[Union[ResourceOwner, Mapping[str, Any]]] = None,
        target: Optional[Union[TargetIdentity, Mapping[str, Any]]] = None,
        authorization_generation: Optional[int] = None,
        mime_type: str = "image/png",
        captured_at: Optional[float] = None,
        redaction_state: str = "none",
        redaction_reason: Optional[str] = None,
        width: Optional[int] = None,
        height: Optional[int] = None,
        decoded_bytes: Optional[int] = None,
        parent_request_id: Any = None,
        sensitive: bool = False,
    ) -> CaptureResult:
        """Admit and publish one backend-provided screenshot.

        ``data`` is the only accepted source.  There is deliberately no path,
        URL, or data-URL input.  A non-``none`` redaction state is withheld
        before the bytes are parsed or sent to the host.
        """

        effective_binding = self._binding(
            binding=binding,
            owner=owner,
            target=target,
            authorization_generation=authorization_generation,
        )
        self._check_scope(effective_binding)
        now = self._now()
        with self._lock:
            if self._settled:
                return self._omit(binding=effective_binding, now=now, reason="generation_settled")
            if not self._lifetime_ok(now):
                return self._omit(binding=effective_binding, now=now, reason="session_lifetime_budget")
            if sensitive:
                redaction_state = "withheld"
            if redaction_state not in {"none", "withheld", "redacted", "manual"}:
                raise ScreenshotError("invalid_redaction_state", "The screenshot redaction state is invalid.")
            if redaction_state != "none":
                return self._omit(
                    binding=effective_binding,
                    now=now,
                    reason=_safe_reason(redaction_reason, "sensitive_surface_withheld"),
                    redaction_state=redaction_state,
                    details={"capture": "withheld"},
                )
            if self._admitted_frames >= self.limits.max_frames:
                return self._omit(
                    binding=effective_binding,
                    now=now,
                    reason="frame_count_budget",
                    details={"limit": self.limits.max_frames},
                )
            if self._transport is None:
                return self._omit(binding=effective_binding, now=now, reason="artifacts_unavailable")

            raw = self._check_data(data)
            canonical_mime = _canonical_mime(mime_type)
            info = _inspect_image(raw, canonical_mime)
            if info.width * info.height > self.limits.max_frame_pixels:
                return self._omit(
                    binding=effective_binding,
                    now=now,
                    reason="frame_pixel_budget",
                    details={"actual": info.width * info.height, "limit": self.limits.max_frame_pixels},
                )
            if info.decoded_bytes > self.limits.max_frame_decoded_bytes:
                return self._omit(
                    binding=effective_binding,
                    now=now,
                    reason="frame_decoded_byte_budget",
                    details={"actual": info.decoded_bytes, "limit": self.limits.max_frame_decoded_bytes},
                )
            if width is not None or height is not None:
                if not isinstance(width, int) or isinstance(width, bool) or not isinstance(height, int) or isinstance(height, bool) or width != info.width or height != info.height:
                    raise ScreenshotError("screenshot_metadata_mismatch", "The screenshot dimensions do not match its bytes.")
            if decoded_bytes is not None:
                if not isinstance(decoded_bytes, int) or isinstance(decoded_bytes, bool) or decoded_bytes != info.decoded_bytes:
                    raise ScreenshotError("screenshot_metadata_mismatch", "The screenshot decoded size does not match its bytes.")
            capture_time = now if captured_at is None else _checked_timestamp(captured_at)
            if capture_time + self.limits.max_retention_seconds <= now:
                return self._omit(binding=effective_binding, now=now, reason="capture_expired", retention_state="expired")
            digest = hashlib.sha256(raw).hexdigest()
            asset_key = (effective_binding.owner.key, effective_binding.target.key, effective_binding.authorization_generation, digest)
            asset = self._assets.get(asset_key)
            published_new = False
            if asset is None:
                owner_bytes = self._owner_bytes(effective_binding.owner)
                if self._admitted_bytes + len(raw) > self.limits.max_retained_bytes or owner_bytes + len(raw) > self.limits.max_retained_bytes:
                    return self._omit(
                        binding=effective_binding,
                        now=now,
                        reason="retained_byte_budget",
                        details={"actual": self._admitted_bytes + len(raw), "limit": self.limits.max_retained_bytes},
                    )
                try:
                    artifact_id = self._transport.publish(
                        mime_type=canonical_mime,
                        data=raw,
                        size=len(raw),
                        sha256=digest,
                        parent_request_id=parent_request_id,
                    )
                except ScreenshotError:
                    raise
                except Exception as error:
                    raise ArtifactTransportError(
                        "artifact_publish_failed",
                        "The host did not publish the screenshot.",
                    ) from error
                artifact_id = _validate_artifact_id(artifact_id)
                previous_key = self._artifact_ids.get(artifact_id)
                if previous_key is not None and previous_key != asset_key:
                    raise ArtifactTransportError("artifact_id_collision", "The host returned a reused screenshot reference.")
                asset = _Asset(asset_key, artifact_id, digest, canonical_mime, len(raw), now)
                self._assets[asset_key] = asset
                self._artifact_ids[artifact_id] = asset_key
                self._admitted_assets += 1
                self._admitted_bytes += len(raw)
                published_new = True
            else:
                if asset.sha256 != digest or asset.encoded_bytes != len(raw) or asset.mime_type != canonical_mime:
                    raise ArtifactTransportError("artifact_integrity_failed", "The retained screenshot metadata is inconsistent.")

            frame_id = self._make_frame_id(effective_binding, digest)
            reference = ScreenshotReference(
                frame_id=frame_id,
                content_id="sha256:" + digest,
                artifact_id=asset.artifact_id,
                sha256=digest,
                mime_type=canonical_mime,
                width=info.width,
                height=info.height,
                pixel_count=info.width * info.height,
                decoded_bytes=info.decoded_bytes,
                encoded_bytes=len(raw),
                captured_at=capture_time,
                expires_at=capture_time + self.limits.max_retention_seconds,
                owner=effective_binding.owner,
                target=effective_binding.target,
                authorization_generation=effective_binding.authorization_generation,
            )
            self._references[frame_id] = reference
            self._states[frame_id] = ("retained", None)
            self._append_history(HistoryEntry(reference=reference))
            self._fresh[effective_binding.key] = frame_id
            self._admitted_frames += 1
            return CaptureResult(
                reference=reference,
                published_new_asset=published_new,
                deduplicated_asset=not published_new,
            )

    # Explicit aliases keep the boundary readable at backend call sites.
    capture_screenshot = capture
    record = capture

    def capture_from_context(
        self,
        data: Any,
        context: Mapping[str, Any],
        *,
        target: Union[TargetIdentity, Mapping[str, Any]],
        authorization_generation: int,
        **options: Any,
    ) -> CaptureResult:
        return self.capture(
            data,
            owner=ResourceOwner.from_context(context),
            target=target,
            authorization_generation=authorization_generation,
            **options,
        )

    def capture_from_path(self, *_: Any, **__: Any) -> CaptureResult:
        raise ScreenshotError(
            "path_input_forbidden",
            "Automation screenshots accept backend bytes only; arbitrary path reads are unavailable.",
        )

    capture_path = capture_from_path

    def capture_or_raise(self, data: Any, **options: Any) -> ScreenshotReference:
        result = self.capture(data, **options)
        if result.reference is None:
            assert result.omission is not None
            raise ScreenshotBudgetError(result.omission.reason, "The screenshot was not admitted.")
        return result.reference

    def _view(self, reference: ScreenshotReference) -> ScreenshotReference:
        state, reason = self._states.get(reference.frame_id, ("settled", "generation_settled"))
        return replace(reference, retention_state=state, retention_reason=reason)

    def _candidate_id(self, value: Any) -> Optional[str]:
        if isinstance(value, ScreenshotReference):
            return value.frame_id
        if isinstance(value, CaptureResult):
            return value.frame_id
        if isinstance(value, str) and _FRAME_ID.fullmatch(value):
            return value
        return None

    def _candidate_status(
        self,
        value: Any,
        binding: ScreenshotBinding,
        now: float,
    ) -> Tuple[Optional[ScreenshotReference], Optional[str]]:
        frame_id = self._candidate_id(value)
        if frame_id is None:
            return None, "invalid_frame_reference"
        reference = self._references.get(frame_id)
        if reference is None:
            return None, "frame_unavailable"
        if isinstance(value, ScreenshotReference):
            if value != reference:
                return None, "reference_integrity_mismatch"
        if reference.owner != binding.owner:
            return None, "owner_mismatch"
        if reference.target != binding.target:
            return None, "target_mismatch"
        if reference.authorization_generation != binding.authorization_generation:
            return None, "authorization_generation_mismatch"
        state, _ = self._states.get(frame_id, ("settled", "generation_settled"))
        if state != "retained":
            return None, "artifact_" + state
        if now >= reference.expires_at and frame_id not in self._pinned:
            self._states[frame_id] = ("expired", "retention_expired")
            return None, "retention_expired"
        return self._view(reference), None

    def _projection_text(self, text: Optional[str]) -> str:
        if text is None:
            return "Current automation screenshot observation."
        if not isinstance(text, str):
            raise ScreenshotError("invalid_projection_text", "The screenshot projection text is invalid.")
        cleaned = " ".join(text.replace("\x00", "").split())
        if not cleaned:
            cleaned = "Current automation screenshot observation."
        return cleaned[:DEFAULT_MAX_TEXT_BYTES]

    def _projection_metadata(
        self,
        binding: ScreenshotBinding,
        text: str,
        fresh_frame_id: Optional[str],
        requested_ids: Sequence[str],
        selected: Sequence[ScreenshotReference],
        omissions: Sequence[ScreenshotOmission],
        omitted_historical_count: int,
    ) -> dict[str, Any]:
        return {
            "schema": PROJECTION_SCHEMA,
            "owner": binding.owner.as_dict(),
            "target": binding.target.as_dict(),
            "authorization_generation": binding.authorization_generation,
            "fresh_frame_id": fresh_frame_id,
            "selected_frame_ids": [ref.frame_id for ref in selected],
            "requested_frame_ids": list(requested_ids),
            "selected_image_count": len(selected),
            "omitted_historical_count": omitted_historical_count,
            "text_bytes": len(text.encode("utf-8")),
            "omissions": [omission.as_dict() for omission in omissions],
            "frames": [ref.as_dict() for ref in selected],
        }

    def _estimate_projection(
        self,
        text: str,
        selected: Sequence[ScreenshotReference],
        metadata: Mapping[str, Any],
    ) -> Tuple[int, int, int, Tuple[Mapping[str, Any], ...]]:
        text_part: Mapping[str, Any] = {"type": "text", "text": text}
        image_parts: Tuple[Mapping[str, Any], ...] = tuple(
            {
                "type": "image",
                "artifact_id": ref.artifact_id,
                "mime_type": ref.mime_type,
                "alt": "Automation screenshot observation",
            }
            for ref in selected
        )
        parts: Tuple[Mapping[str, Any], ...] = (text_part,) + image_parts
        base64_bytes = sum((ref.encoded_bytes + 2) // 3 * 4 for ref in selected)
        reference_payload = json.dumps(
            {"content": list(parts), "metadata": metadata},
            separators=(",", ":"),
            ensure_ascii=True,
        ).encode("utf-8")
        inline_images = []
        for ref in selected:
            inline_images.append(
                {
                    "type": "image",
                    "mime_type": ref.mime_type,
                    "data": {
                        "encoding": "base64",
                        "data": "A" * ((ref.encoded_bytes + 2) // 3 * 4),
                    },
                }
            )
        inline_payload = json.dumps(
            {
                "content": [{"type": "text", "text": text}] + inline_images,
                "metadata": metadata,
            },
            separators=(",", ":"),
            ensure_ascii=True,
        ).encode("utf-8")
        json_bytes = max(len(reference_payload), len(inline_payload) - base64_bytes)
        request_bytes = json_bytes + base64_bytes + DEFAULT_PROTOCOL_OVERHEAD_BYTES
        return request_bytes, base64_bytes, json_bytes, parts

    def _history_omitted_count(self, binding: ScreenshotBinding, selected_ids: Collection[str]) -> int:
        count = 0
        for frame_id, reference in self._references.items():
            if reference.owner == binding.owner and reference.target == binding.target and reference.authorization_generation == binding.authorization_generation and frame_id not in selected_ids:
                state, _ = self._states.get(frame_id, ("settled", "generation_settled"))
                if state == "retained":
                    count += 1
        return count

    def project(
        self,
        *,
        binding: Optional[ScreenshotBinding] = None,
        owner: Optional[Union[ResourceOwner, Mapping[str, Any]]] = None,
        target: Optional[Union[TargetIdentity, Mapping[str, Any]]] = None,
        authorization_generation: Optional[int] = None,
        fresh: Optional[Union[str, ScreenshotReference, CaptureResult]] = None,
        fresh_frame: Optional[Union[str, ScreenshotReference, CaptureResult]] = None,
        selected_frame_ids: Optional[Iterable[Union[str, ScreenshotReference, CaptureResult]]] = None,
        selected: Optional[Iterable[Union[str, ScreenshotReference, CaptureResult]]] = None,
        text: Optional[str] = None,
        request_budget: Optional[RequestBudget] = None,
        retry: bool = False,
        host_recovery: bool = False,
    ) -> Projection:
        """Build a fresh-plus-explicit-comparisons projection.

        No historical image is included implicitly.  A caller may use either
        ``fresh``/``selected_frame_ids`` or the descriptive aliases
        ``fresh_frame``/``selected``; supplying both forms is rejected.
        """

        if fresh is not None and fresh_frame is not None:
            raise ScreenshotError("invalid_projection", "Provide only one fresh frame argument.")
        if selected_frame_ids is not None and selected is not None:
            raise ScreenshotError("invalid_projection", "Provide only one selected-frame argument.")
        effective_binding = self._binding(
            binding=binding,
            owner=owner,
            target=target,
            authorization_generation=authorization_generation,
        )
        self._check_scope(effective_binding)
        fresh_value = fresh if fresh is not None else fresh_frame
        selection_value = selected_frame_ids if selected_frame_ids is not None else selected
        now = self._now()
        with self._lock:
            if self._settled:
                fresh_ref, fresh_reason = None, "generation_settled"
            elif fresh_value is None:
                fresh_ref, fresh_reason = None, "fresh_frame_required"
            else:
                fresh_ref, fresh_reason = self._candidate_status(fresh_value, effective_binding, now)
                if fresh_ref is not None and self._fresh.get(effective_binding.key) != fresh_ref.frame_id:
                    fresh_ref, fresh_reason = None, "fresh_observation_required"

            requested: list[Any] = []
            if selection_value is not None:
                if isinstance(selection_value, (str, ScreenshotReference, CaptureResult)):
                    requested.append(selection_value)
                else:
                    try:
                        iterator = iter(selection_value)
                    except TypeError as error:
                        raise ScreenshotError("invalid_projection", "The selected-frame list is invalid.") from error
                    for _ in range(self.limits.max_selected_images + 16):
                        try:
                            requested.append(next(iterator))
                        except StopIteration:
                            break
                    else:
                        requested.append("__selection_overflow__")
            requested_ids: list[str] = []
            for item in requested:
                candidate_id = self._candidate_id(item)
                if candidate_id is not None:
                    requested_ids.append(candidate_id)
            omissions: list[ScreenshotOmission] = []
            if fresh_reason is not None:
                omissions.append(
                    ScreenshotOmission.make(
                        reason=fresh_reason,
                        binding=effective_binding,
                        occurred_at=now,
                        phase="projection",
                        frame_id=self._candidate_id(fresh_value),
                    )
                )
            text_value = self._projection_text(text)
            selected_refs: list[ScreenshotReference] = []
            selected_ids: set[str] = set()
            if fresh_ref is not None:
                selected_refs.append(fresh_ref)
                selected_ids.add(fresh_ref.frame_id)
            if fresh_ref is None and requested:
                for item in requested:
                    candidate_id = self._candidate_id(item)
                    if candidate_id is not None:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="fresh_frame_required",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate_id,
                            )
                        )
            else:
                seen_requested: set[str] = set()
                for item in requested:
                    if item == "__selection_overflow__":
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="selection_argument_budget",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                            )
                        )
                        continue
                    candidate_id = self._candidate_id(item)
                    if candidate_id is None:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="invalid_frame_reference",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                            )
                        )
                        continue
                    if candidate_id in seen_requested:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="duplicate_frame_reference",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate_id,
                            )
                        )
                        continue
                    seen_requested.add(candidate_id)
                    if candidate_id in selected_ids:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="fresh_frame_already_selected",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate_id,
                            )
                        )
                        continue
                    if len(selected_refs) >= self.limits.max_selected_images:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="selected_image_count_budget",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate_id,
                                details={"limit": self.limits.max_selected_images},
                            )
                        )
                        continue
                    candidate, candidate_reason = self._candidate_status(item, effective_binding, now)
                    if candidate is None:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason=candidate_reason or "frame_unavailable",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate_id,
                            )
                        )
                        continue
                    if candidate.artifact_id in {ref.artifact_id for ref in selected_refs}:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="duplicate_artifact_reference",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate.frame_id,
                            )
                        )
                        continue
                    tentative = selected_refs + [candidate]
                    metadata = self._projection_metadata(
                        effective_binding,
                        text_value,
                        fresh_ref.frame_id if fresh_ref else None,
                        requested_ids,
                        tentative,
                        omissions,
                        self._history_omitted_count(effective_binding, {ref.frame_id for ref in tentative}),
                    )
                    estimate, _, _, _ = self._estimate_projection(text_value, tentative, metadata)
                    if estimate > self.limits.max_request_encoded_bytes:
                        omissions.append(
                            ScreenshotOmission.make(
                                reason="request_encoded_byte_budget",
                                binding=effective_binding,
                                occurred_at=now,
                                phase="projection",
                                frame_id=candidate.frame_id,
                                details={"actual": estimate, "limit": self.limits.max_request_encoded_bytes},
                            )
                        )
                        continue
                    selected_refs.append(candidate)
                    selected_ids.add(candidate.frame_id)

            historical_count = self._history_omitted_count(effective_binding, selected_ids)
            metadata = self._projection_metadata(
                effective_binding,
                text_value,
                fresh_ref.frame_id if fresh_ref else None,
                requested_ids,
                selected_refs,
                omissions,
                historical_count,
            )
            request_bytes, base64_bytes, json_bytes, parts = self._estimate_projection(
                text_value,
                selected_refs,
                metadata,
            )
            frame_metadata = tuple(ref.as_dict() for ref in selected_refs)
            request_admitted = request_bytes <= self.limits.max_request_encoded_bytes
            admission_reason = None if request_admitted else "request_encoded_byte_budget"
            if not request_admitted:
                omissions.append(
                    ScreenshotOmission.make(
                        reason="request_encoded_byte_budget",
                        binding=effective_binding,
                        occurred_at=now,
                        phase="projection",
                        details={"actual": request_bytes, "limit": self.limits.max_request_encoded_bytes},
                    )
                )
                # No request with image parts may escape if final metadata made
                # the otherwise-selected body too large.
                parts = tuple(part for part in parts if part.get("type") == "text")
                selected_refs = []
                frame_metadata = tuple()
                historical_count = self._history_omitted_count(effective_binding, set())
                base64_bytes = 0
                json_bytes = 0
                request_bytes = 0
            projection = Projection(
                binding=effective_binding,
                content_parts=tuple(parts),
                fresh_frame_id=fresh_ref.frame_id if fresh_ref else None,
                selected_frame_ids=tuple(ref.frame_id for ref in selected_refs),
                requested_frame_ids=tuple(requested_ids),
                omissions=tuple(omissions),
                frame_metadata=frame_metadata,
                selected_image_count=len(selected_refs),
                base64_bytes=base64_bytes,
                json_bytes=json_bytes,
                request_encoded_bytes=request_bytes,
                omitted_historical_count=historical_count,
                request_admitted=request_admitted,
                request_admission_reason=admission_reason,
            )
            if request_budget is not None:
                admission = request_budget.admit(
                    projection,
                    retry=retry,
                    host_recovery=host_recovery,
                )
                projection = projection.with_attempt(admission)
            return projection

    project_selected = project
    project_frames = project

    def require_fresh_observation(
        self,
        value: Union[str, ScreenshotReference, CaptureResult],
        *,
        binding: ScreenshotBinding,
    ) -> ScreenshotReference:
        """Return metadata only for the latest exact observation.

        The returned object remains ``action_eligible=False``.  The host or
        backend must attach it to its own action authorization; old history
        references cannot be used as an action token.
        """

        effective_binding = self._binding(binding=binding, owner=None, target=None, authorization_generation=None)
        self._check_scope(effective_binding)
        now = self._now()
        with self._lock:
            reference, reason = self._candidate_status(value, effective_binding, now)
            if reference is None or self._fresh.get(effective_binding.key) != reference.frame_id:
                raise FreshObservationRequired(
                    reason or "fresh_observation_required",
                    "A fresh owner- and target-bound screenshot observation is required before an action.",
                )
            return reference

    def latest(self, binding: ScreenshotBinding) -> Optional[ScreenshotReference]:
        """Return the newest retained view for this exact binding.

        The newest observation may have expired while an older observation is
        pinned by a consumer.  In that case expose the pinned retained view
        rather than returning an expired observation; this is still metadata
        only and cannot authorize an action.  ``require_fresh_observation``
        continues to enforce the exact current observation for actions.
        """

        self._check_scope(binding)
        now = self._now()
        with self._lock:
            for frame_id, reference in reversed(tuple(self._references.items())):
                if (
                    reference.owner != binding.owner
                    or reference.target != binding.target
                    or reference.authorization_generation != binding.authorization_generation
                ):
                    continue
                view = self._view(reference)
                if view.retention_state != "retained":
                    continue
                if now >= view.expires_at and frame_id not in self._pinned:
                    self._states[frame_id] = ("expired", "retention_expired")
                    continue
                return view
            return None

    def get(
        self,
        value: Union[str, ScreenshotReference],
        *,
        binding: ScreenshotBinding,
        include_unavailable: bool = True,
    ) -> Optional[ScreenshotReference]:
        self._check_scope(binding)
        with self._lock:
            reference, reason = self._candidate_status(value, binding, self._now())
            if reference is not None:
                return reference
            if not include_unavailable:
                return None
            frame_id = self._candidate_id(value)
            stored = self._references.get(frame_id) if frame_id else None
            if stored is None or stored.owner != binding.owner or stored.target != binding.target or stored.authorization_generation != binding.authorization_generation:
                return None
            if isinstance(value, ScreenshotReference) and (
                value.frame_id != stored.frame_id
                or value.owner != stored.owner
                or value.target != stored.target
                or value.authorization_generation != stored.authorization_generation
                or value.artifact_id != stored.artifact_id
                or value.sha256 != stored.sha256
                or value.mime_type != stored.mime_type
                or value.encoded_bytes != stored.encoded_bytes
                or value.width != stored.width
                or value.height != stored.height
                or value.captured_at != stored.captured_at
                or value.expires_at != stored.expires_at
            ):
                return None
            state, state_reason = self._states.get(stored.frame_id, ("settled", reason or "unavailable"))
            return replace(stored, retention_state=state, retention_reason=state_reason)

    def references(self, binding: ScreenshotBinding) -> Tuple[ScreenshotReference, ...]:
        self._check_scope(binding)
        with self._lock:
            now = self._now()
            result = []
            for reference in self._references.values():
                if reference.owner == binding.owner and reference.target == binding.target and reference.authorization_generation == binding.authorization_generation:
                    view = self._view(reference)
                    if view.retention_state == "retained" and now >= view.expires_at and reference.frame_id not in self._pinned:
                        self._states[reference.frame_id] = ("expired", "retention_expired")
                        view = replace(view, retention_state="expired", retention_reason="retention_expired")
                    result.append(view)
            return tuple(result)

    def history(
        self,
        *,
        binding: Optional[ScreenshotBinding] = None,
        owner: Optional[ResourceOwner] = None,
        target: Optional[TargetIdentity] = None,
    ) -> Tuple[HistoryEntry, ...]:
        """Return bounded metadata history; no image bytes are present."""

        if binding is not None and (owner is not None or target is not None):
            raise ScreenshotError("invalid_history_filter", "Use either a binding or owner/target history filters.")
        if target is not None and not isinstance(target, TargetIdentity):
            raise ScreenshotError("invalid_target", "The screenshot target is invalid.")
        if binding is not None:
            self._check_scope(binding)
        elif owner is not None:
            if not isinstance(owner, ResourceOwner):
                raise ScreenshotError("invalid_owner", "The screenshot owner is invalid.")
            self._check_scope(ScreenshotBinding(owner, target or TargetIdentity("session", "history"), 1)) if self.scope_owner else None
        with self._lock:
            result: list[HistoryEntry] = []
            for entry in self._history:
                reference = entry.reference
                omission = entry.omission
                if binding is not None:
                    if reference is not None and (reference.owner != binding.owner or reference.target != binding.target or reference.authorization_generation != binding.authorization_generation):
                        continue
                    if omission is not None and (omission.owner != binding.owner or omission.target != binding.target or omission.authorization_generation != binding.authorization_generation):
                        continue
                elif owner is not None:
                    if reference is not None and reference.owner != owner:
                        continue
                    if omission is not None and omission.owner != owner:
                        continue
                if target is not None:
                    if reference is not None and reference.target != target:
                        continue
                    if omission is not None and omission.target != target:
                        continue
                if reference is not None:
                    result.append(HistoryEntry(reference=self._view(reference)))
                else:
                    result.append(entry)
            return tuple(result)

    def canonical_history(self, **filters: Any) -> Tuple[dict[str, Any], ...]:
        return tuple(entry.as_dict() for entry in self.history(**filters))

    receipts = canonical_history

    def pin(self, value: Union[str, ScreenshotReference], *, binding: ScreenshotBinding) -> bool:
        reference = self.get(value, binding=binding, include_unavailable=False)
        if reference is None or reference.retention_state != "retained":
            return False
        with self._lock:
            self._pinned.add(reference.frame_id)
        return True

    def unpin(self, value: Union[str, ScreenshotReference], *, binding: ScreenshotBinding) -> None:
        reference = self.get(value, binding=binding, include_unavailable=True)
        if reference is None:
            return
        with self._lock:
            self._pinned.discard(reference.frame_id)

    def mark_missing(self, value: Union[str, ScreenshotReference], *, binding: ScreenshotBinding) -> bool:
        reference = self.get(value, binding=binding, include_unavailable=True)
        if reference is None:
            return False
        with self._lock:
            self._states[reference.frame_id] = ("missing", "artifact_missing")
        return True

    def mark_corrupt(self, value: Union[str, ScreenshotReference], *, binding: ScreenshotBinding) -> bool:
        reference = self.get(value, binding=binding, include_unavailable=True)
        if reference is None:
            return False
        with self._lock:
            self._states[reference.frame_id] = ("corrupt", "artifact_corrupt")
        return True

    def cleanup(self, *, now: Optional[float] = None, binding: Optional[ScreenshotBinding] = None) -> CleanupReport:
        """Expire unpinned metadata and remove only generated staging files.

        This cannot delete a host artifact because the extension API has no
        deletion authority.  Counters therefore remain committed until host
        generation settlement, which prevents cleanup/retry from exceeding
        the durable-media bound.
        """

        current = self._now() if now is None else _checked_timestamp(now, "cleanup_time")
        if binding is not None:
            self._check_scope(binding)
        expired = 0
        skipped = 0
        with self._lock:
            for frame_id, reference in self._references.items():
                if binding is not None and (
                    reference.owner != binding.owner
                    or reference.target != binding.target
                    or reference.authorization_generation != binding.authorization_generation
                ):
                    continue
                state, _ = self._states.get(frame_id, ("settled", "generation_settled"))
                if state != "retained" or current < reference.expires_at:
                    continue
                if frame_id in self._pinned:
                    skipped += 1
                    continue
                self._states[frame_id] = ("expired", "retention_expired")
                expired += 1
            removed, errors = self._cleanup_transport()
        return CleanupReport(expired=expired, skipped_pinned=skipped, staged_removed=removed, staged_errors=errors)

    def _cleanup_transport(self) -> Tuple[int, int]:
        transport = self._transport
        if transport is None or not hasattr(transport, "cleanup"):
            return (0, 0)
        try:
            result = transport.cleanup()
            if isinstance(result, _TransportStageStats):
                return (result.removed, result.errors)
        except Exception:
            return (0, 1)
        return (0, 0)

    def recover(
        self,
        *,
        available_artifact_ids: Optional[Collection[str]] = None,
        verifier: Optional[Callable[[ScreenshotReference], Union[bool, str]]] = None,
        now: Optional[float] = None,
    ) -> RecoveryReport:
        """Recover metadata without reading arbitrary paths or image bytes.

        A host integration may provide a verifier backed by its owner-scoped
        artifact resolver.  Without one, existing opaque IDs are left alone;
        callers can still explicitly mark a missing/corrupt artifact.
        """

        current = self._now() if now is None else _checked_timestamp(now, "recovery_time")
        checked = missing = corrupt = 0
        with self._lock:
            for reference in tuple(self._references.values()):
                state, _ = self._states.get(reference.frame_id, ("settled", "generation_settled"))
                if state not in {"retained", "expired"}:
                    continue
                checked += 1
                if current >= reference.expires_at and reference.frame_id not in self._pinned:
                    self._states[reference.frame_id] = ("expired", "retention_expired")
                    continue
                if available_artifact_ids is not None and reference.artifact_id not in available_artifact_ids:
                    self._states[reference.frame_id] = ("missing", "artifact_missing")
                    missing += 1
                    continue
                if verifier is not None:
                    try:
                        result = verifier(reference)
                    except Exception:
                        result = False
                    if result is False or result == "missing":
                        self._states[reference.frame_id] = ("missing", "artifact_missing")
                        missing += 1
                    elif result == "corrupt":
                        self._states[reference.frame_id] = ("corrupt", "artifact_corrupt")
                        corrupt += 1
            removed, errors = self._cleanup_transport()
        return RecoveryReport(
            checked=checked,
            missing=missing,
            corrupt=corrupt,
            staged_removed=removed,
            staged_errors=errors,
        )

    def export_metadata(self, *, owner: Optional[ResourceOwner] = None) -> dict[str, Any]:
        """Export references/omissions only; suitable for canonical history."""

        entries = self.history(owner=owner) if owner is not None else self.history()
        return {
            "schema": SCREENSHOT_SCHEMA,
            "kind": "metadata_export",
            "entries": [entry.as_dict() for entry in entries],
            "metrics": self.metrics(owner=owner),
        }

    export_state = export_metadata

    def metrics(self, *, owner: Optional[ResourceOwner] = None) -> dict[str, Any]:
        with self._lock:
            if owner is None:
                references = tuple(self._references.values())
                assets = tuple(self._assets.values())
            else:
                references = tuple(ref for ref in self._references.values() if ref.owner == owner)
                assets = tuple(asset for asset in self._assets.values() if asset.key[0] == owner.key)
            active = sum(
                1
                for ref in references
                if self._states.get(ref.frame_id, ("settled", None))[0] == "retained"
            )
            return {
                "schema": SCREENSHOT_SCHEMA,
                "admitted_frame_count": self._admitted_frames,
                "unique_asset_count": self._admitted_assets,
                "admitted_encoded_bytes": self._admitted_bytes,
                "active_reference_count": active,
                "metadata_reference_count": len(references),
                "omitted_count": self._omitted,
                "owner_unique_asset_count": len(assets),
                "owner_encoded_bytes": sum(asset.encoded_bytes for asset in assets),
                "settled": self._settled,
            }

    def settle(self) -> dict[str, int]:
        """Drop local metadata after the host has settled its artifact generation."""

        with self._lock:
            if self._settled:
                return {"frames": 0, "assets": 0, "encoded_bytes": 0}
            self._cleanup_transport()
            frames = len(self._references)
            assets = len(self._assets)
            encoded_bytes = self._admitted_bytes
            self._references.clear()
            self._states.clear()
            self._history.clear()
            self._assets.clear()
            self._artifact_ids.clear()
            self._pinned.clear()
            self._fresh.clear()
            self._settled = True
            return {"frames": frames, "assets": assets, "encoded_bytes": encoded_bytes}

    close = settle


# A descriptive alias used by integrations that prefer “registry” wording.
ScreenshotRegistry = ScreenshotStore



def _canonical_mime(value: Any) -> str:
    if not isinstance(value, str) or value != value.lower() or value not in _SUPPORTED_MIME_TYPES:
        raise ScreenshotError("unsupported_mime", "Only canonical bounded screenshot image types are supported.")
    return value



def _check_dimension(width: int, height: int) -> Tuple[int, int]:
    if width <= 0 or height <= 0 or width > MAX_IMAGE_DIMENSION or height > MAX_IMAGE_DIMENSION:
        raise ScreenshotError("invalid_screenshot", "The screenshot dimensions are invalid.")
    return width, height



def _inspect_image(data: bytes, mime_type: str) -> _ImageInfo:
    if mime_type == "image/png":
        return _inspect_png(data)
    if mime_type == "image/jpeg":
        return _inspect_jpeg(data)
    if mime_type == "image/gif":
        return _inspect_gif(data)
    if mime_type == "image/webp":
        return _inspect_webp(data)
    raise ScreenshotError("unsupported_mime", "Only canonical bounded screenshot image types are supported.")



def _inspect_png(data: bytes) -> _ImageInfo:
    if not data.startswith(PNG_SIGNATURE) or len(data) < 33:
        raise ScreenshotError("invalid_screenshot", "The screenshot is not a valid PNG image.")
    position = len(PNG_SIGNATURE)
    ihdr: Optional[bytes] = None
    saw_idat = False
    saw_iend = False
    while position + 12 <= len(data):
        length = struct.unpack_from(">I", data, position)[0]
        chunk_start = position + 8
        chunk_end = chunk_start + length
        end = chunk_end + 4
        if end > len(data):
            raise ScreenshotError("invalid_screenshot", "The screenshot PNG is truncated.")
        chunk_type = data[position + 4 : position + 8]
        chunk_data = data[chunk_start:chunk_end]
        expected_crc = struct.unpack_from(">I", data, chunk_end)[0]
        if zlib.crc32(chunk_type + chunk_data) & 0xFFFFFFFF != expected_crc:
            raise ScreenshotError("invalid_screenshot", "The screenshot PNG integrity is invalid.")
        if not all(0x20 <= byte <= 0x7E for byte in chunk_type):
            raise ScreenshotError("invalid_screenshot", "The screenshot PNG chunk type is invalid.")
        if position == len(PNG_SIGNATURE) and chunk_type != b"IHDR":
            raise ScreenshotError("invalid_screenshot", "The screenshot PNG header is invalid.")
        if chunk_type == b"IHDR":
            if ihdr is not None or len(chunk_data) != 13:
                raise ScreenshotError("invalid_screenshot", "The screenshot PNG header is invalid.")
            ihdr = chunk_data
        elif chunk_type == b"IDAT":
            saw_idat = True
        elif chunk_type == b"IEND":
            if chunk_data:
                raise ScreenshotError("invalid_screenshot", "The screenshot PNG end marker is invalid.")
            saw_iend = True
            position = end
            break
        position = end
    if ihdr is None or not saw_idat or not saw_iend or position != len(data):
        raise ScreenshotError("invalid_screenshot", "The screenshot PNG is incomplete.")
    width, height, bit_depth, color_type, compression, filter_method, interlace = struct.unpack(
        ">IIBBBBB", ihdr
    )
    _check_dimension(width, height)
    channels_by_type = {0: 1, 2: 3, 3: 4, 4: 2, 6: 4}
    if color_type not in channels_by_type or compression != 0 or filter_method != 0 or interlace not in {0, 1}:
        raise ScreenshotError("invalid_screenshot", "The screenshot PNG encoding is unsupported.")
    valid_depths = {
        0: {1, 2, 4, 8, 16},
        2: {8, 16},
        3: {1, 2, 4, 8},
        4: {8, 16},
        6: {8, 16},
    }
    if bit_depth not in valid_depths[color_type]:
        raise ScreenshotError("invalid_screenshot", "The screenshot PNG bit depth is unsupported.")
    bytes_per_sample = 2 if bit_depth == 16 else 1
    decoded = width * height * channels_by_type[color_type] * bytes_per_sample
    return _ImageInfo("image/png", width, height, decoded)



def _inspect_jpeg(data: bytes) -> _ImageInfo:
    if not data.startswith(JPEG_SIGNATURE) or len(data) < 4:
        raise ScreenshotError("invalid_screenshot", "The screenshot is not a valid JPEG image.")
    position = 2
    sof: Optional[Tuple[int, int, int]] = None
    while position < len(data):
        if data[position] != 0xFF:
            raise ScreenshotError("invalid_screenshot", "The screenshot JPEG markers are invalid.")
        while position < len(data) and data[position] == 0xFF:
            position += 1
        if position >= len(data):
            break
        marker = data[position]
        position += 1
        if marker == 0xD9:
            break
        if marker == 0xDA:
            if position + 2 > len(data):
                break
            segment_length = struct.unpack_from(">H", data, position)[0]
            if segment_length < 2 or position + segment_length > len(data):
                raise ScreenshotError("invalid_screenshot", "The screenshot JPEG is truncated.")
            # The dimensions must occur before the entropy-coded scan.
            break
        if marker in {0x01} or 0xD0 <= marker <= 0xD7:
            continue
        if position + 2 > len(data):
            raise ScreenshotError("invalid_screenshot", "The screenshot JPEG is truncated.")
        segment_length = struct.unpack_from(">H", data, position)[0]
        if segment_length < 2 or position + segment_length > len(data):
            raise ScreenshotError("invalid_screenshot", "The screenshot JPEG is truncated.")
        if marker in {0xC0, 0xC1, 0xC2, 0xC3, 0xC5, 0xC6, 0xC7, 0xC9, 0xCA, 0xCB, 0xCD, 0xCE, 0xCF}:
            if segment_length < 8:
                raise ScreenshotError("invalid_screenshot", "The screenshot JPEG frame is invalid.")
            precision = data[position + 2]
            height = struct.unpack_from(">H", data, position + 3)[0]
            width = struct.unpack_from(">H", data, position + 5)[0]
            components = data[position + 7]
            if precision not in {8, 12, 16} or components <= 0:
                raise ScreenshotError("invalid_screenshot", "The screenshot JPEG frame is invalid.")
            sof = (width, height, components)
            break
        position += segment_length
    if sof is None:
        raise ScreenshotError("invalid_screenshot", "The screenshot JPEG dimensions are unavailable.")
    width, height, components = sof
    _check_dimension(width, height)
    channels = 4 if components >= 4 else 3 if components >= 3 else 1
    return _ImageInfo("image/jpeg", width, height, width * height * channels)



def _inspect_gif(data: bytes) -> _ImageInfo:
    if not any(data.startswith(signature) for signature in GIF_SIGNATURES) or len(data) < 10:
        raise ScreenshotError("invalid_screenshot", "The screenshot is not a valid GIF image.")
    width, height = struct.unpack_from("<HH", data, 6)
    _check_dimension(width, height)
    return _ImageInfo("image/gif", width, height, width * height * 4)



def _inspect_webp(data: bytes) -> _ImageInfo:
    if len(data) < 20 or data[:4] != b"RIFF" or data[8:12] != WEBP_SIGNATURE:
        raise ScreenshotError("invalid_screenshot", "The screenshot is not a valid WebP image.")
    position = 12
    dimensions: Optional[Tuple[int, int]] = None
    while position + 8 <= len(data):
        chunk_type = data[position : position + 4]
        length = struct.unpack_from("<I", data, position + 4)[0]
        start = position + 8
        end = start + length
        if end > len(data):
            raise ScreenshotError("invalid_screenshot", "The screenshot WebP is truncated.")
        chunk = data[start:end]
        if chunk_type == b"VP8X" and len(chunk) >= 10:
            width = 1 + int.from_bytes(chunk[4:7], "little")
            height = 1 + int.from_bytes(chunk[7:10], "little")
            dimensions = (width, height)
            break
        if chunk_type == b"VP8L" and len(chunk) >= 5 and chunk[0] == 0x2F:
            bits = int.from_bytes(chunk[1:5], "little")
            dimensions = (1 + (bits & 0x3FFF), 1 + ((bits >> 14) & 0x3FFF))
            break
        if chunk_type == b"VP8 " and len(chunk) >= 13 and chunk[6:9] == b"\x9d\x01\x2a":
            width = struct.unpack_from("<H", chunk, 9)[0] & 0x3FFF
            height = struct.unpack_from("<H", chunk, 11)[0] & 0x3FFF
            dimensions = (width, height)
            break
        position = end + (length & 1)
    if dimensions is None:
        raise ScreenshotError("invalid_screenshot", "The screenshot WebP dimensions are unavailable.")
    width, height = dimensions
    _check_dimension(width, height)
    return _ImageInfo("image/webp", width, height, width * height * 4)



def validate_screenshot_reference(reference: ScreenshotReference) -> ScreenshotReference:
    """Validate an immutable metadata object at an integration boundary."""

    if not isinstance(reference, ScreenshotReference):
        raise ScreenshotError("invalid_reference", "The screenshot reference is invalid.")
    return reference


__all__ = [
    "ArtifactPublisher",
    "ArtifactTransportError",
    "AttemptAdmission",
    "CaptureContext",
    "CaptureOmission",
    "CaptureResult",
    "CleanupReport",
    "DurableMediaTransport",
    "FreshObservationRequired",
    "HistoryEntry",
    "HostArtifactTransport",
    "Owner",
    "Projection",
    "PROJECTION_SCHEMA",
    "RequestBudget",
    "ResourceOwner",
    "SCREENSHOT_SCHEMA",
    "ScreenshotBinding",
    "ScreenshotError",
    "ScreenshotLimits",
    "ScreenshotOmission",
    "ScreenshotReference",
    "ScreenshotRegistry",
    "ScreenshotStore",
    "Target",
    "TargetIdentity",
    "RecoveryReport",
    "validate_screenshot_reference",
]
