"""Bounded, extension-scoped event bus for octet executable extensions.

The bus is **host-mediated by design**: extensions never share a process, a
queue, or a socket directly, and no extension can address another extension's
state. An extension only declares its own typed topics, publishes to them, and
subscribes to topics declared by others; the host owns the registry, the
queues, and the identities.

This module is the normative enforcement **kernel** plus the extension-side
participant:

* :class:`EventBusKernel` is a deterministic, dependency-free reference for the
  semantics the host must enforce (declaration ownership, bounded queues,
  cursor-stable ordering, fail-closed validation).
* :class:`HostEventBus` is the extension-side client that validates outbound and
  inbound envelopes with the same kernel rules and carries them over the SDK
  host-request API.

Fail-closed rules, all of them bounded by :class:`BusLimits`:

* unknown, malformed, or foreign-owned topics are rejected;
* payloads must match the declared topic spec exactly (no unknown fields, no
  wrong types, no oversized strings or nesting);
* credential-, capability-, trust-, handle-, and path-shaped fields are refused,
  and PII/secret-shaped string values are refused before they can reach a peer;
* queues are bounded by message count and byte budget, and a full queue raises
  instead of silently dropping a message;
* envelopes are inert, frozen data: no callables, no handles, no capability
  grants, and no field can widen project trust.

Host status: API 0.3 hosts may offer ``event_bus`` only for a session-isolated
service. Select the methods used, including ``bus/event`` for subscriptions.
Legacy runtimes do not implement this wire. An absent service fails closed.
"""

from __future__ import annotations

import json
import re
import unicodedata
from collections import deque
from dataclasses import dataclass, field
from types import MappingProxyType
from typing import Any, Callable, Deque, List, Mapping, Optional, Sequence, Tuple


# JSON-RPC error codes from the extension API 0.3 error table.
INVALID_PARAMS = -32602
UNKNOWN_METHOD = -32601
CAPABILITY_MISMATCH = -32011
RESOURCE_EXHAUSTED = -32012

# Bounded defaults. Every one of these can only be lowered, never bypassed.
DEFAULT_MAX_MESSAGE_BYTES = 8192
DEFAULT_MAX_PAYLOAD_FIELDS = 24
DEFAULT_MAX_PAYLOAD_DEPTH = 4
DEFAULT_MAX_STRING_BYTES = 1024
DEFAULT_MAX_QUEUE_MESSAGES = 64
DEFAULT_MAX_QUEUE_BYTES = 256 * 1024
DEFAULT_MAX_SUBSCRIPTIONS = 16
DEFAULT_MAX_TOPIC_BYTES = 96
DEFAULT_MAX_TOPIC_SEGMENTS = 6
DEFAULT_MAX_IDENTIFIER_BYTES = 64
DEFAULT_MAX_MESSAGE_AGE_MS = 30_000
DEFAULT_MAX_DRAIN_MESSAGES = 64

TOPIC_PREFIX = "bus"
_IDENTIFIER = re.compile(r"^[a-z0-9][a-z0-9_-]*$")

# Field names that could smuggle authority, credentials, private state, or a
# capability grant by shape. Matching is a substring test on the normalized
# (lowercased, alphanumeric-only) field name, so `api_key`, `apiKey`, and
# `x-api-key` are all refused. This is deliberately conservative and fails
# closed: an extension that wants a similar-looking field must rename it.
FORBIDDEN_FIELD_PARTS = (
    "apikey",
    "authorization",
    "capability",
    "credential",
    "cookie",
    "handle",
    "keychain",
    "oauth",
    "passwd",
    "password",
    "path",
    "pem",
    "private",
    "secret",
    "session",
    "token",
    "trust",
)


def forbidden_field(name: str) -> bool:
    """Return whether a payload field name is authority/credential/private-shaped."""

    normalized = _normalized_field(name)
    return any(part in normalized for part in FORBIDDEN_FIELD_PARTS)

# Deny-list shape checks for string values. This is a bounded guard rail, not a
# classifier: the host implementation must apply at least these rules.
_SECRET_PREFIX = re.compile(
    r"^(sk|pk|rk|ghp|gho|ghu|ghs|github_pat|xox[abprs])[-_][A-Za-z0-9_-]{8,}$"
)
_BEARER = re.compile(r"^bearer\s+\S+$", re.IGNORECASE)
_EMAIL = re.compile(r"^[^@\s]+@[^@\s]+\.[A-Za-z]{2,}$")
_PHONE = re.compile(r"^\+?[0-9][0-9 ()\-.]{7,}$")
_ABSOLUTE_PATH = re.compile(r"^(/|[A-Za-z]:\\|~[/\\])")
_PEM = "-----begin"


class BusError(Exception):
    """A fail-closed bus violation carrying the JSON-RPC error code to report."""

    def __init__(self, code: int, reason: str, detail: str = "") -> None:
        self.code = code
        self.reason = reason
        self.detail = detail
        message = reason if not detail else "{0}: {1}".format(reason, detail)
        super().__init__(message)

    def error_object(self) -> dict:
        """Return the bounded error payload a host or peer should receive."""

        return {"code": self.code, "reason": self.reason}


@dataclass(frozen=True)
class BusLimits:
    """Bounded bus limits. Construction validates every field."""

    max_message_bytes: int = DEFAULT_MAX_MESSAGE_BYTES
    max_payload_fields: int = DEFAULT_MAX_PAYLOAD_FIELDS
    max_payload_depth: int = DEFAULT_MAX_PAYLOAD_DEPTH
    max_string_bytes: int = DEFAULT_MAX_STRING_BYTES
    max_queue_messages: int = DEFAULT_MAX_QUEUE_MESSAGES
    max_queue_bytes: int = DEFAULT_MAX_QUEUE_BYTES
    max_subscriptions: int = DEFAULT_MAX_SUBSCRIPTIONS
    max_topic_bytes: int = DEFAULT_MAX_TOPIC_BYTES
    max_topic_segments: int = DEFAULT_MAX_TOPIC_SEGMENTS
    max_identifier_bytes: int = DEFAULT_MAX_IDENTIFIER_BYTES
    max_message_age_ms: int = DEFAULT_MAX_MESSAGE_AGE_MS
    max_drain_messages: int = DEFAULT_MAX_DRAIN_MESSAGES

    def __post_init__(self) -> None:
        for name, value in self.__dict__.items():
            if not isinstance(value, int) or isinstance(value, bool) or value <= 0:
                raise BusError(
                    INVALID_PARAMS,
                    "invalid_limit",
                    "{0} must be a positive integer".format(name),
                )
        for name, value in self.__dict__.items():
            if value > self.__dataclass_fields__[name].default:
                raise BusError(RESOURCE_EXHAUSTED, "limit_above_ceiling")


DEFAULT_LIMITS = BusLimits()


@dataclass(frozen=True)
class FieldSpec:
    """One typed payload field of a declared topic."""

    name: str
    kind: str
    required: bool = True
    max_bytes: int = DEFAULT_MAX_STRING_BYTES
    values: Tuple[str, ...] = ()
    minimum: Optional[int] = None
    maximum: Optional[int] = None

    @classmethod
    def string(cls, name: str, *, required: bool = True, max_bytes: int = DEFAULT_MAX_STRING_BYTES) -> "FieldSpec":
        return cls(name=name, kind="string", required=required, max_bytes=max_bytes)

    @classmethod
    def integer(
        cls,
        name: str,
        *,
        required: bool = True,
        minimum: Optional[int] = None,
        maximum: Optional[int] = None,
    ) -> "FieldSpec":
        return cls(name=name, kind="integer", required=required, minimum=minimum, maximum=maximum)

    @classmethod
    def boolean(cls, name: str, *, required: bool = True) -> "FieldSpec":
        return cls(name=name, kind="boolean", required=required)

    @classmethod
    def enum(cls, name: str, values: Sequence[str], *, required: bool = True) -> "FieldSpec":
        return cls(name=name, kind="enum", required=required, values=tuple(values))


@dataclass(frozen=True)
class TopicSpec:
    """A declared topic owned by exactly one extension."""

    owner: str
    name: str
    fields: Tuple[FieldSpec, ...]
    description: str = ""

    @property
    def topic(self) -> str:
        return "{0}.{1}.{2}".format(TOPIC_PREFIX, self.owner, self.name)


@dataclass(frozen=True)
class BusEnvelope:
    """One inert, ordered bus message.

    ``payload`` is an immutable mapping view of the validated fields: a peer or
    subscriber cannot mutate queued data after publication.
    """

    topic: str
    publisher: str
    sequence: int
    published_at_ms: int
    payload: Mapping[str, Any]
    byte_len: int
    publisher_instance_id: str = ""
    process_generation: int = 0
    binding_id: str = ""

    def public(self) -> dict:
        """Return the JSON-safe projection that may cross a process boundary."""

        return {
            "binding_id": self.binding_id,
            "topic": self.topic,
            "publisher": self.publisher,
            "sequence": self.sequence,
            "published_at_ms": self.published_at_ms,
            "publisher_instance_id": self.publisher_instance_id,
            "process_generation": self.process_generation,
            "payload": dict(self.payload),
        }


def _identifier(value: Any, limits: BusLimits, label: str) -> str:
    if not isinstance(value, str) or not value:
        raise BusError(INVALID_PARAMS, "invalid_{0}".format(label))
    encoded = value.encode("utf-8")
    if len(encoded) > limits.max_identifier_bytes:
        raise BusError(INVALID_PARAMS, "{0}_too_long".format(label))
    if not _IDENTIFIER.match(value):
        raise BusError(INVALID_PARAMS, "invalid_{0}".format(label))
    return value


def validate_topic(topic: Any, limits: BusLimits = DEFAULT_LIMITS) -> Tuple[str, str]:
    """Validate ``bus.<owner>.<name>`` and return ``(owner, name)``.

    Unknown or malformed topics fail closed instead of being treated as a
    wildcard or falling back to a default topic.
    """

    if not isinstance(topic, str) or not topic:
        raise BusError(INVALID_PARAMS, "invalid_topic")
    if len(topic.encode("utf-8")) > limits.max_topic_bytes:
        raise BusError(INVALID_PARAMS, "topic_too_long")
    segments = topic.split(".")
    if len(segments) != 3 or len(segments) > limits.max_topic_segments:
        raise BusError(INVALID_PARAMS, "invalid_topic")
    prefix, owner, name = segments
    if prefix != TOPIC_PREFIX:
        raise BusError(INVALID_PARAMS, "unknown_topic_namespace")
    return _identifier(owner, limits, "topic_owner"), _identifier(name, limits, "topic_name")


def _canonical_bytes(payload: Mapping[str, Any]) -> bytes:
    try:
        return json.dumps(payload, sort_keys=True, separators=(",", ":"), ensure_ascii=False).encode("utf-8")
    except (TypeError, ValueError) as error:
        raise BusError(INVALID_PARAMS, "payload_not_json", str(error)) from error


def _normalized_field(name: str) -> str:
    return "".join(character for character in name.lower() if character.isalnum())


def screen_string(value: str, limits: BusLimits, label: str, max_bytes: Optional[int] = None) -> str:
    """Refuse control characters and PII/secret-shaped text before it is queued.

    The effective budget is the tighter of the global string budget and the
    declared field budget: a field can narrow the bound, never widen it.
    """

    budget = limits.max_string_bytes if max_bytes is None else min(limits.max_string_bytes, max_bytes)
    if len(value.encode("utf-8")) > budget:
        raise BusError(RESOURCE_EXHAUSTED, "string_too_large", label)
    for character in value:
        if unicodedata.category(character) == "Cc":
            raise BusError(INVALID_PARAMS, "control_character", label)
    lowered = value.strip().lower()
    if _BEARER.match(value.strip()) or lowered.startswith(_PEM):
        raise BusError(INVALID_PARAMS, "pii_detected", label)
    if _SECRET_PREFIX.match(value.strip()) or _EMAIL.match(value.strip()):
        raise BusError(INVALID_PARAMS, "pii_detected", label)
    if _ABSOLUTE_PATH.match(value.strip()):
        raise BusError(INVALID_PARAMS, "private_path", label)
    if _PHONE.match(value.strip()) and any(separator in value for separator in " ()-."):
        raise BusError(INVALID_PARAMS, "pii_detected", label)
    return value


def _validate_value(field_spec: FieldSpec, value: Any, limits: BusLimits) -> Any:
    label = field_spec.name
    if field_spec.kind == "string":
        if not isinstance(value, str):
            raise BusError(INVALID_PARAMS, "invalid_field_type", label)
        return screen_string(value, limits, label, field_spec.max_bytes)
    if field_spec.kind == "enum":
        if not isinstance(value, str) or value not in field_spec.values:
            raise BusError(INVALID_PARAMS, "invalid_field_value", label)
        return screen_string(value, limits, label, field_spec.max_bytes)
    if field_spec.kind == "integer":
        if not isinstance(value, int) or isinstance(value, bool):
            raise BusError(INVALID_PARAMS, "invalid_field_type", label)
        if abs(value) > 9_007_199_254_740_991:
            raise BusError(INVALID_PARAMS, "nonportable_integer", label)
        if field_spec.minimum is not None and value < field_spec.minimum:
            raise BusError(INVALID_PARAMS, "field_below_minimum", label)
        if field_spec.maximum is not None and value > field_spec.maximum:
            raise BusError(INVALID_PARAMS, "field_above_maximum", label)
        return value
    if field_spec.kind == "boolean":
        if not isinstance(value, bool):
            raise BusError(INVALID_PARAMS, "invalid_field_type", label)
        return value
    raise BusError(INVALID_PARAMS, "unknown_field_kind", label)


def validate_payload(spec: TopicSpec, payload: Any, limits: BusLimits = DEFAULT_LIMITS) -> dict:
    """Validate one payload against a declared topic spec, fail-closed."""

    if not isinstance(payload, Mapping):
        raise BusError(INVALID_PARAMS, "payload_not_an_object")
    if len(payload) > limits.max_payload_fields:
        raise BusError(RESOURCE_EXHAUSTED, "too_many_fields")
    known = {field_spec.name: field_spec for field_spec in spec.fields}
    validated: dict = {}
    for key, value in payload.items():
        if not isinstance(key, str) or not key:
            raise BusError(INVALID_PARAMS, "invalid_field_name")
        if forbidden_field(key):
            raise BusError(INVALID_PARAMS, "forbidden_field", key)
        field_spec = known.get(key)
        if field_spec is None:
            raise BusError(INVALID_PARAMS, "unknown_field", key)
        validated[key] = _validate_value(field_spec, value, limits)
    for field_spec in spec.fields:
        if field_spec.required and field_spec.name not in validated:
            raise BusError(INVALID_PARAMS, "missing_field", field_spec.name)
    if _payload_depth(validated, limits) > limits.max_payload_depth:
        raise BusError(RESOURCE_EXHAUSTED, "payload_too_deep")
    return validated


def _payload_depth(payload: Mapping[str, Any], limits: BusLimits) -> int:
    depth = 1
    stack = [(payload, 1)]
    while stack:
        value, current = stack.pop()
        depth = max(depth, current)
        if current > limits.max_payload_depth:
            return depth
        if isinstance(value, Mapping):
            for nested in value.values():
                stack.append((nested, current + 1))
        elif isinstance(value, (list, tuple)):
            for nested in value:
                stack.append((nested, current + 1))
    return depth


@dataclass
class _Queue:
    envelopes: Deque[BusEnvelope] = field(default_factory=deque)
    byte_len: int = 0


class BoundedQueue:
    """One bounded, ordered delivery queue.

    A full queue raises :class:`BusError` (``resource_exhausted``); it never
    silently drops, reorders, or overwrites a queued message.
    """

    def __init__(self, limits: BusLimits = DEFAULT_LIMITS) -> None:
        self._limits = limits
        self._inner = _Queue()

    def __len__(self) -> int:
        return len(self._inner.envelopes)

    @property
    def byte_len(self) -> int:
        return self._inner.byte_len

    def check_capacity(self, envelope: BusEnvelope) -> None:
        if not isinstance(envelope, BusEnvelope):
            raise BusError(INVALID_PARAMS, "invalid_envelope")
        if len(self._inner.envelopes) + 1 > self._limits.max_queue_messages:
            raise BusError(RESOURCE_EXHAUSTED, "queue_full")
        if self._inner.byte_len + envelope.byte_len > self._limits.max_queue_bytes:
            raise BusError(RESOURCE_EXHAUSTED, "queue_bytes_exceeded")
    def push(self, envelope: BusEnvelope) -> None:
        self.check_capacity(envelope)
        self._inner.envelopes.append(envelope)
        self._inner.byte_len += envelope.byte_len

    def drain(self, max_messages: Optional[int] = None) -> List[BusEnvelope]:
        limit = self._limits.max_drain_messages if max_messages is None else max_messages
        if not isinstance(limit, int) or isinstance(limit, bool) or limit <= 0:
            raise BusError(INVALID_PARAMS, "invalid_drain_limit")
        limit = min(limit, self._limits.max_drain_messages, self._limits.max_queue_messages)
        drained: List[BusEnvelope] = []
        while self._inner.envelopes and len(drained) < limit:
            envelope = self._inner.envelopes.popleft()
            self._inner.byte_len -= envelope.byte_len
            drained.append(envelope)
        return drained


class TopicRegistry:
    """Host-owned topic registry: declarations are unique and owner-scoped."""

    def __init__(self, limits: BusLimits = DEFAULT_LIMITS) -> None:
        self._limits = limits
        self._topics: dict = {}

    def declare(self, spec: TopicSpec) -> TopicSpec:
        if not isinstance(spec, TopicSpec):
            raise BusError(INVALID_PARAMS, "invalid_topic_spec")
        owner = _identifier(spec.owner, self._limits, "topic_owner")
        name = _identifier(spec.name, self._limits, "topic_name")
        validate_topic(spec.topic, self._limits)
        if len(spec.fields) > self._limits.max_payload_fields or len(self._topics) >= 128:
            raise BusError(RESOURCE_EXHAUSTED, "declaration_limit")
        if spec.topic in self._topics:
            raise BusError(INVALID_PARAMS, "topic_already_declared", spec.topic)
        field_names = set()
        for field_spec in spec.fields:
            if not isinstance(field_spec, FieldSpec):
                raise BusError(INVALID_PARAMS, "invalid_field_spec")
            if forbidden_field(field_spec.name):
                raise BusError(INVALID_PARAMS, "forbidden_field", field_spec.name)
            _identifier(field_spec.name, self._limits, "field_name")
            if field_spec.name in field_names:
                raise BusError(INVALID_PARAMS, "duplicate_field", field_spec.name)
            field_names.add(field_spec.name)
            if field_spec.kind not in {"string", "integer", "boolean", "enum"}:
                raise BusError(INVALID_PARAMS, "unknown_field_kind")
            if type(field_spec.required) is not bool or type(field_spec.max_bytes) is not int or not 0 < field_spec.max_bytes <= self._limits.max_string_bytes:
                raise BusError(INVALID_PARAMS, "invalid_field_spec")
            for bound in (field_spec.minimum, field_spec.maximum):
                if bound is not None and (field_spec.kind != "integer" or type(bound) is not int or abs(bound) > 9_007_199_254_740_991):
                    raise BusError(INVALID_PARAMS, "invalid_field_bound")
            if field_spec.minimum is not None and field_spec.maximum is not None and field_spec.minimum > field_spec.maximum:
                raise BusError(INVALID_PARAMS, "invalid_field_bound")
            if not all(isinstance(value, str) for value in field_spec.values):
                raise BusError(INVALID_PARAMS, "invalid_enum")
            if (field_spec.kind == "enum") != bool(field_spec.values) or len(field_spec.values) > 32 or len(set(field_spec.values)) != len(field_spec.values):
                raise BusError(INVALID_PARAMS, "invalid_enum")
            for value in field_spec.values:
                if not isinstance(value, str):
                    raise BusError(INVALID_PARAMS, "invalid_enum")
                screen_string(value, self._limits, field_spec.name, field_spec.max_bytes)
        stored = TopicSpec(owner=owner, name=name, fields=tuple(spec.fields), description=spec.description)
        self._topics[stored.topic] = stored
        return stored

    def get(self, topic: Any) -> TopicSpec:
        validate_topic(topic, self._limits)
        spec = self._topics.get(topic)
        if spec is None:
            raise BusError(INVALID_PARAMS, "unknown_topic", str(topic))
        return spec

    def topics(self) -> Tuple[str, ...]:
        return tuple(sorted(self._topics))


class EventBusKernel:
    """Deterministic reference semantics for the host-mediated bus.

    Nothing here performs I/O, touches the filesystem, or grants authority: a
    publish is data-only, a subscribe only adds the caller to a bounded queue,
    and every queue is keyed by ``(extension, topic)`` so no extension can read
    another extension's stream.
    """

    def __init__(self, limits: BusLimits = DEFAULT_LIMITS) -> None:
        self._limits = limits
        self._registry = TopicRegistry(limits)
        self._subscriptions: dict = {}
        self._queues: dict = {}
        self._sequences: dict = {}
        self._pending: List[BusEnvelope] = []

    @property
    def registry(self) -> TopicRegistry:
        return self._registry

    def declare(self, spec: TopicSpec) -> TopicSpec:
        return self._registry.declare(spec)

    def subscribe(self, extension_id: str, topic: Any) -> TopicSpec:
        subscriber = _identifier(extension_id, self._limits, "extension_id")
        spec = self._registry.get(topic)
        if subscriber not in self._subscriptions and len(self._subscriptions) >= 64:
            raise BusError(RESOURCE_EXHAUSTED, "peer_limit")
        topics = self._subscriptions.setdefault(subscriber, set())
        if spec.topic not in topics:
            if len(topics) + 1 > self._limits.max_subscriptions:
                raise BusError(RESOURCE_EXHAUSTED, "subscription_limit")
            topics.add(spec.topic)
        self._queues.setdefault((subscriber, spec.topic), BoundedQueue(self._limits))
        return spec

    def unsubscribe(self, extension_id: str, topic: Any) -> None:
        subscriber = _identifier(extension_id, self._limits, "extension_id")
        spec = self._registry.get(topic)
        self._subscriptions.get(subscriber, set()).discard(spec.topic)
        self._queues.pop((subscriber, spec.topic), None)

    def subscribers(self, topic: Any) -> Tuple[str, ...]:
        spec = self._registry.get(topic)
        return tuple(
            sorted(
                extension
                for extension, topics in self._subscriptions.items()
                if spec.topic in topics
            )
        )

    def publish(
        self,
        publisher: str,
        topic: Any,
        payload: Any,
        *,
        published_at_ms: int,
    ) -> BusEnvelope:
        owner = _identifier(publisher, self._limits, "extension_id")
        spec = self._registry.get(topic)
        if spec.owner != owner:
            raise BusError(CAPABILITY_MISMATCH, "foreign_topic", spec.topic)
        validated = validate_payload(spec, payload, self._limits)
        encoded = _canonical_bytes(validated)
        if len(encoded) > self._limits.max_message_bytes:
            raise BusError(RESOURCE_EXHAUSTED, "message_too_large")
        if not isinstance(published_at_ms, int) or isinstance(published_at_ms, bool) or published_at_ms < 0:
            raise BusError(INVALID_PARAMS, "invalid_timestamp")
        # One counter belongs to the publisher, across every declared topic.
        sequence = self._sequences.get(owner, 0) + 1
        envelope = BusEnvelope(
            topic=spec.topic,
            publisher=owner,
            sequence=sequence,
            published_at_ms=published_at_ms,
            payload=MappingProxyType(dict(validated)),
            byte_len=len(encoded),
        )
        for subscriber in self.subscribers(spec.topic):
            # The host budget is shared by all topics of a subscriber, not a
            # fresh allowance for each subscription. Validate every recipient
            # before mutating any queue or consuming a sequence.
            queues = [self._queues[(subscriber, topic)] for topic in self._subscriptions[subscriber]]
            if sum(len(queue) for queue in queues) + 1 > self._limits.max_queue_messages:
                raise BusError(RESOURCE_EXHAUSTED, "queue_full")
            if sum(queue.byte_len for queue in queues) + envelope.byte_len > self._limits.max_queue_bytes:
                raise BusError(RESOURCE_EXHAUSTED, "queue_bytes_exceeded")
            self._queues[(subscriber, spec.topic)].check_capacity(envelope)
        for subscriber in self.subscribers(spec.topic):
            self._queues[(subscriber, spec.topic)].push(envelope)
        self._sequences[owner] = sequence
        return envelope

    def deliver(
        self,
        extension_id: str,
        topic: Any,
        *,
        max_messages: Optional[int] = None,
        now_ms: Optional[int] = None,
    ) -> List[BusEnvelope]:
        subscriber = _identifier(extension_id, self._limits, "extension_id")
        spec = self._registry.get(topic)
        if spec.topic not in self._subscriptions.get(subscriber, set()):
            raise BusError(CAPABILITY_MISMATCH, "not_subscribed", spec.topic)
        queue = self._queues[(subscriber, spec.topic)]
        drained = queue.drain(max_messages)
        if now_ms is None:
            return drained
        return [
            envelope
            for envelope in drained
            if now_ms - envelope.published_at_ms <= self._limits.max_message_age_ms
        ]


class HostEventBus:
    """Binding-scoped API 0.3 participant with one bounded rebind worker.

    ``request(method, params, cancelled)`` must be thread-safe, bounded in time,
    and stop waiting when the supplied threading.Event is set. Its response is
    delivered by the ordinary serial protocol reader. That reader calls
    :meth:`accept_lifecycle` (never an RPC), and :meth:`accept_event`.
    Explicit operations may block and must run outside the reader callback.
    Call :meth:`close` at process shutdown. No publication is queued or replayed.
    """

    def __init__(self, request: Callable, registry: TopicRegistry, *, extension_id: str,
                 limits: BusLimits = DEFAULT_LIMITS, now_ms: Optional[Callable[[], int]] = None) -> None:
        import threading
        if not callable(request):
            raise BusError(INVALID_PARAMS, "invalid_host_request")
        self._request, self._registry, self._limits = request, registry, limits
        self._extension_id = _identifier(extension_id, limits, "extension_id")
        self._condition = threading.Condition(threading.RLock())
        self._operation = threading.Lock()
        self._cancelled = threading.Event()
        self._closed = False
        self._binding_id = ""
        self._binding_revision = 0
        self._desired_declarations: dict = {}
        self._desired_interests: set = set()
        self._declared: set = set()
        self._interests: set = set()
        self._subscribing: set = set()
        self._subscribed: dict = {}
        self._observed: dict = {}
        self._sequences: dict = {}
        self._dirty = False
        self._working = False
        self._rebind_error: Optional[str] = None
        self._worker = threading.Thread(target=self._rebind, name="octet-bus-rebind", daemon=True)
        self._worker.start()

    @property
    def extension_id(self) -> str:
        return self._extension_id

    def snapshot(self) -> dict:
        """Content-free current ACK ledger; pending is explicitly not active."""
        with self._condition:
            return {"binding_id": self._binding_id, "binding_revision": self._binding_revision,
                    "declared": sorted(self._declared), "subscribed": sorted(self._subscribed),
                    "pending": sorted(self._interests - self._subscribed.keys()),
                    "rebind_error": self._rebind_error}

    def close(self, *, wait: bool = True) -> None:
        with self._condition:
            self._closed = True
            self._cancelled.set()
            self._declared.clear()
            self._subscribed.clear()
            self._interests.clear()
            self._subscribing.clear()
            self._sequences.clear()
            self._condition.notify_all()
        if wait:
            self._worker.join(timeout=2)

    def wait_rebound(self, timeout: float = 5) -> bool:
        """Wait outside the reader for this wake-up's bounded reconciliation."""
        with self._condition:
            return self._condition.wait_for(
                lambda: self._closed or (bool(self._binding_id) and not self._dirty and not self._working), timeout
            ) and not self._closed and self._rebind_error is None

    def _wake(self) -> None:
        self._dirty = True
        self._condition.notify_all()

    def accept_lifecycle(self, params: Any) -> bool:
        """Serial-reader callback: validate/update ledgers and wake one worker.

        Old/repeated notices are inert. Invalid control fails the participant
        closed; it cannot continue with an apparently active stale ledger.
        """
        from .api_v03 import ContractError, parse_bus_lifecycle_params
        try:
            # The generated tagged union has no single `from_wire`; the parser
            # selects the variant by `kind` and validates the exact shape.
            notice = parse_bus_lifecycle_params(params).to_wire()
            with self._condition:
                if self._closed:
                    return False
                if not notice["binding_id"]:
                    raise BusError(INVALID_PARAMS, "invalid_binding")
                if notice["kind"] == "binding":
                    revision = notice["binding_revision"]
                    if revision <= 0:
                        raise BusError(INVALID_PARAMS, "invalid_binding_revision")
                    if revision < self._binding_revision:
                        return False
                    if revision == self._binding_revision:
                        if notice["binding_id"] != self._binding_id:
                            raise BusError(INVALID_PARAMS, "conflicting_binding")
                        return False
                    if notice["binding_id"] == self._binding_id:
                        raise BusError(INVALID_PARAMS, "reused_binding")
                    import threading
                    self._cancelled.set()
                    self._cancelled = threading.Event()
                    self._binding_id, self._binding_revision = notice["binding_id"], revision
                    self._declared.clear()
                    self._subscribed.clear()
                    self._interests.clear()
                    self._subscribing.clear()
                    self._observed.clear()
                    self._sequences.clear()
                    self._rebind_error = None
                    self._wake()
                    return True
                if notice["binding_id"] != self._binding_id:
                    return False
                topic = notice["topic"]
                validate_topic(topic, self._limits)
                if notice["topic_revision"] <= 0 or notice["process_generation"] <= 0 or not notice["publisher_instance_id"]:
                    raise BusError(INVALID_PARAMS, "invalid_topic_provenance")
                # Only schemas explicitly installed by this extension can be
                # observed. This map cannot exceed the bounded local registry.
                if topic not in self._registry.topics():
                    return False
                previous = self._observed.get(topic)
                current = (notice["topic_revision"], notice["kind"] == "topic_available",
                           notice["publisher_instance_id"], notice["process_generation"])
                if previous is not None and current[0] <= previous[0]:
                    if current[0] == previous[0] and current != previous:
                        raise BusError(INVALID_PARAMS, "conflicting_topic_revision")
                    return False
                self._observed[topic] = current
                self._subscribed.pop(topic, None)
                self._sequences.pop(topic, None)
                # Unavailable retains an ACKed pending interest; availability
                # requires a fresh active subscription ACK, never a guessed gen.
                if current[1]:
                    self._interests.discard(topic)
                if topic in self._desired_interests:
                    self._wake()
                return True
        except (ContractError, BusError) as error:
            self.close(wait=False)
            if isinstance(error, BusError):
                raise
            raise BusError(INVALID_PARAMS, "invalid_lifecycle") from error

    def _rpc(self, method: str, params: dict, parse: Any) -> dict:
        """Send one binding-scoped request and validate its typed result.

        `parse` is the generated parser for the reply model. Tagged unions have
        no single `from_wire`, so the generated `parse_*` selector is the only
        correct entry point for those shapes.
        """
        from .api_v03 import ContractError
        with self._condition:
            if self._closed or not self._binding_id:
                raise BusError(CAPABILITY_MISMATCH, "bus_unbound")
            # Capture here, never replace a queued operation's binding later.
            binding, cancelled = self._binding_id, self._cancelled
            params = {**params, "binding_id": binding}
        response = self._request(method, params, cancelled)
        try:
            result = parse(response).to_wire()
        except ContractError as error:
            raise BusError(INVALID_PARAMS, "invalid_ack") from error
        if result["binding_id"] != binding:
            raise BusError(CAPABILITY_MISMATCH, "stale_ack")
        return result

    def _ack_current(self, result: dict) -> None:
        if self._closed or result["binding_id"] != self._binding_id:
            raise BusError(CAPABILITY_MISMATCH, "stale_ack")

    @staticmethod
    def _fields(spec: TopicSpec) -> list:
        values = []
        for field_spec in spec.fields:
            value = {"name": field_spec.name, "kind": field_spec.kind, "required": field_spec.required,
                     "max_bytes": field_spec.max_bytes, "values": list(field_spec.values)}
            for key in ("minimum", "maximum"):
                bound = getattr(field_spec, key)
                if bound is not None:
                    value[key] = bound
            values.append(value)
        return values

    def _declare(self, spec: TopicSpec) -> None:
        from .api_v03 import parse_bus_ack
        with self._condition:
            if spec.topic in self._declared:
                return
        reply = self._rpc("bus/declare", {"topic": spec.topic, "fields": self._fields(spec)}, parse_bus_ack)
        with self._condition:
            self._ack_current(reply)
            self._declared.add(spec.topic)

    def declare(self, *, name: str, fields: Sequence[FieldSpec], description: str = "") -> TopicSpec:
        spec = TopicRegistry(self._limits).declare(TopicSpec(self._extension_id, name, tuple(fields), description))
        with self._operation:
            with self._condition:
                existing = self._desired_declarations.get(spec.topic)
                if existing is not None and existing != spec:
                    raise BusError(INVALID_PARAMS, "topic_already_declared")
                if existing is None and spec.topic in self._registry.topics():
                    raise BusError(INVALID_PARAMS, "topic_already_declared")
                if existing is None and len(self._registry.topics()) >= 128:
                    raise BusError(RESOURCE_EXHAUSTED, "declaration_limit")
            self._declare(spec)
            with self._condition:
                if existing is None:
                    self._registry.declare(spec)
                self._desired_declarations[spec.topic] = spec
            return spec

    def _subscribe(self, topic: str) -> None:
        from .api_v03 import parse_bus_subscribe_result
        with self._condition:
            if topic in self._interests:
                return
            self._subscribing.add(topic)
        try:
            reply = self._rpc("bus/subscribe", {"topic": topic}, parse_bus_subscribe_result)
            with self._condition:
                self._ack_current(reply)
                previous = self._observed.get(topic)
                active = reply["state"] == "active"
                if active and (not reply["publisher_instance_id"] or reply["process_generation"] <= 0 or reply["topic_revision"] <= 0):
                    raise BusError(INVALID_PARAMS, "invalid_topic_provenance")
                principal = (reply["publisher_instance_id"], reply["process_generation"]) if active else None
                if previous is not None and reply["topic_revision"] < previous[0]:
                    # The host admitted this interest before an availability
                    # transition, but its pending/active ACK is now obsolete.
                    # Do not activate it or discard the desired subscription:
                    # the worker must get a fresh host-provenance ACK.
                    self._wake()
                    return
                if previous is not None and reply["topic_revision"] == previous[0] and (
                    active != previous[1] or (active and principal != previous[2:])
                ):
                    raise BusError(CAPABILITY_MISMATCH, "conflicting_topic_revision")
                self._interests.add(topic)
                if active:
                    self._subscribed[topic] = principal
                    self._observed[topic] = (reply["topic_revision"], True, *principal)
                else:
                    self._subscribed.pop(topic, None)
        finally:
            with self._condition:
                self._subscribing.discard(topic)
                self._condition.notify_all()

    def subscribe(self, topic: Any) -> TopicSpec:
        with self._operation:
            with self._condition:
                spec = self._registry.get(topic)
                if topic not in self._desired_interests and len(self._desired_interests) >= self._limits.max_subscriptions:
                    raise BusError(RESOURCE_EXHAUSTED, "subscription_limit")
                # Register intent before the RPC: a topic_available notice may
                # arrive before the pending ACK returns on the worker thread.
                new_interest = topic not in self._desired_interests
                binding = self._binding_id
                self._desired_interests.add(topic)
            try:
                self._subscribe(topic)
            except Exception:
                with self._condition:
                    if new_interest and binding == self._binding_id:
                        self._desired_interests.discard(topic)
                raise
            return spec

    def unsubscribe(self, topic: Any) -> None:
        from .api_v03 import parse_bus_ack
        with self._operation:
            with self._condition:
                self._registry.get(topic)
                if topic not in self._desired_interests:
                    return
            reply = self._rpc("bus/unsubscribe", {"topic": topic}, parse_bus_ack)
            with self._condition:
                self._ack_current(reply)
                self._desired_interests.discard(topic)
                self._interests.discard(topic)
                self._subscribed.pop(topic, None)
                self._sequences.pop(topic, None)

    def _rebind(self) -> None:
        while True:
            with self._condition:
                self._condition.wait_for(lambda: self._closed or self._dirty)
                if self._closed:
                    return
                self._dirty = False
                self._working = True
                self._rebind_error = None
                binding = self._binding_id
            # One serialized worker, coalesced wakeup bit, bounded desired maps.
            # Never hold the reader's condition while waiting for an RPC reply.
            with self._operation:
                with self._condition:
                    declarations = list(self._desired_declarations.values())
                    interests = sorted(self._desired_interests)
                for item in [*(('declare', spec) for spec in declarations), *(('subscribe', topic) for topic in interests)]:
                    with self._condition:
                        if self._closed or binding != self._binding_id:
                            break
                    try:
                        if item[0] == 'declare':
                            self._declare(item[1])
                        else:
                            self._subscribe(item[1])
                    except Exception:
                        # No retry loop and no publication replay. Keep inactive;
                        # a new lifecycle transition or explicit operation may try.
                        with self._condition:
                            if binding == self._binding_id:
                                self._rebind_error = "rebind_failed"
            with self._condition:
                self._working = False
                self._condition.notify_all()

    def publish(self, topic: Any, payload: Any) -> BusEnvelope:
        from .api_v03 import parse_bus_publish_result
        with self._operation:
            spec = self._registry.get(topic)
            if spec.owner != self._extension_id:
                raise BusError(CAPABILITY_MISMATCH, "foreign_topic", spec.topic)
            validated = validate_payload(spec, payload, self._limits)
            encoded = _canonical_bytes(validated)
            if len(encoded) > self._limits.max_message_bytes:
                raise BusError(RESOURCE_EXHAUSTED, "message_too_large")
            reply = self._rpc("bus/publish", {"topic": topic, "payload": validated}, parse_bus_publish_result)
            with self._condition:
                self._ack_current(reply)
                if reply["sequence"] <= 0:
                    raise BusError(INVALID_PARAMS, "invalid_sequence")
                return BusEnvelope(topic=topic, publisher=self._extension_id, sequence=reply["sequence"],
                    published_at_ms=reply["published_at_ms"], payload=MappingProxyType(dict(validated)),
                    byte_len=len(encoded), binding_id=reply["binding_id"])

    def accept_event(self, params: Any) -> Optional[BusEnvelope]:
        from .api_v03 import ContractError, parse_bus_event_params
        try:
            event = parse_bus_event_params(params)
        except ContractError as error:
            raise BusError(INVALID_PARAMS, "invalid_envelope") from error
        with self._condition:
            if self._closed or not self._binding_id or event.binding_id != self._binding_id:
                raise BusError(CAPABILITY_MISMATCH, "stale_binding")
            spec = self._registry.get(event.topic)
            # The host queues the active ACK before any delivery, but the
            # serial reader can consume the next frame before the request
            # thread commits that ACK. Wait only for an in-flight subscribe,
            # never for a request response still unread on this reader.
            if spec.topic in self._subscribing and spec.topic not in self._subscribed:
                self._condition.wait_for(
                    lambda: self._closed or spec.topic not in self._subscribing or spec.topic in self._subscribed,
                    timeout=5,
                )
            principal = self._subscribed.get(spec.topic)
            if principal is None:
                raise BusError(CAPABILITY_MISMATCH, "not_subscribed", spec.topic)
            if event.publisher != spec.owner:
                raise BusError(INVALID_PARAMS, "foreign_publisher")
            if principal != (event.publisher_instance_id, event.process_generation):
                raise BusError(CAPABILITY_MISMATCH, "stale_publisher")
            if event.sequence <= self._sequences.get(spec.topic, 0):
                raise BusError(INVALID_PARAMS, "stale_sequence", spec.topic)
            validated = validate_payload(spec, event.payload, self._limits)
            encoded = _canonical_bytes(validated)
            if len(encoded) > self._limits.max_message_bytes:
                raise BusError(RESOURCE_EXHAUSTED, "message_too_large")
            self._sequences[spec.topic] = event.sequence
            return BusEnvelope(topic=spec.topic, publisher=event.publisher, sequence=event.sequence,
                published_at_ms=event.published_at_ms, payload=MappingProxyType(dict(validated)), byte_len=len(encoded),
                publisher_instance_id=event.publisher_instance_id, process_generation=event.process_generation,
                binding_id=event.binding_id)

def _monotonic_ms() -> int:
    import time

    return int(time.monotonic() * 1000)
