"""Typed, fail-closed policy boundaries for computer use.

This module deliberately does not perform desktop or browser operations. It
models the value objects an owner/runtime adapter presents to host policy and
the exact binding checked again before dispatch. Model-produced text and action
arguments are data; neither is an approval or a source of authority.
"""

from __future__ import annotations

from dataclasses import dataclass
from enum import Enum
import hashlib
import json
import re
import time
from typing import Any, Callable, FrozenSet, Mapping, Optional, Protocol, Sequence, Tuple, Union
from urllib.parse import urlsplit

MAX_IDENTIFIER_BYTES = 256
MAX_ORIGIN_BYTES = 2048
MAX_DATA_CLASSES = 32
MAX_ACTION_ARGUMENT_BYTES = 64 * 1024
MAX_POLICY_REASON_BYTES = 4096
MAX_ACTIONS_PER_GROUP = 8
MAX_TOTAL_ACTIONS = 256
MAX_AUTHORIZATION_TTL_MS = 5 * 60 * 1000
MAX_LOCAL_AUTHORIZATION_TTL_MS = 5 * 1000
MAX_POLICY_JSON_DEPTH = 32
MAX_PORTABLE_JSON_INTEGER = 9_007_199_254_740_991

_IDENTIFIER_RE = re.compile(r"^[A-Za-z0-9][A-Za-z0-9_.:@-]{0,255}$")
_DIGEST_RE = re.compile(r"^[0-9a-f]{64}$")
_APPROVAL_RE = re.compile(r"^[0-9a-f]{64}$")


class Capability(str, Enum):
    """Narrow computer-use capabilities understood by the policy adapter."""

    OBSERVE = "computer.observe"
    START = "computer.start"
    CLICK = "computer.click"
    DOUBLE_CLICK = "computer.double_click"
    DRAG = "computer.drag"
    MOVE = "computer.move"
    SCROLL = "computer.scroll"
    KEYPRESS = "computer.keypress"
    TYPE = "computer.type"
    WAIT = "computer.wait"
    SCREENSHOT = "computer.screenshot"


class Effect(str, Enum):
    """Host policy effect classes."""

    OBSERVATION = "observation"
    INPUT = "input"
    EXTERNAL_SIDE_EFFECT = "external_side_effect"
    DESTRUCTIVE = "destructive"
    AUTHENTICATION = "authentication"


class Decision(str, Enum):
    ALLOW = "allow"
    ASK = "ask"
    DENY = "deny"


class PolicyError(ValueError):
    """A malformed or unsafe policy value."""


class PolicyDenied(PermissionError):
    """A policy decision did not authorize dispatch."""


def _bounded_text(value: Any, label: str, maximum: int = MAX_IDENTIFIER_BYTES) -> str:
    if not isinstance(value, str) or not value:
        raise PolicyError(f"{label} must be a non-empty string")
    try:
        size = len(value.encode("utf-8"))
    except UnicodeEncodeError as exc:
        raise PolicyError(f"{label} must be valid UTF-8") from exc
    if size > maximum:
        raise PolicyError(f"{label} exceeds its byte bound")
    return value


def _opaque_identifier(value: Any, label: str) -> str:
    value = _bounded_text(value, label)
    if not _IDENTIFIER_RE.fullmatch(value):
        raise PolicyError(f"{label} must be a bounded opaque identifier")
    return value


def _positive_integer(value: Any, label: str, maximum: int = MAX_PORTABLE_JSON_INTEGER) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value <= 0 or value > maximum:
        raise PolicyError(f"{label} must be a positive portable integer")
    return value


def _nonnegative_integer(value: Any, label: str, maximum: int = MAX_PORTABLE_JSON_INTEGER) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or value < 0 or value > maximum:
        raise PolicyError(f"{label} must be a non-negative portable integer")
    return value


def _validate_json(value: Any, depth: int = 0) -> None:
    if depth > MAX_POLICY_JSON_DEPTH:
        raise PolicyError("policy value exceeds the maximum JSON depth")
    if value is None or isinstance(value, bool):
        return
    if isinstance(value, int):
        if abs(value) > MAX_PORTABLE_JSON_INTEGER:
            raise PolicyError("policy integer exceeds the portable range")
        return
    if isinstance(value, float):
        raise PolicyError("policy JSON does not permit floating-point values")
    if isinstance(value, str):
        _bounded_text(value, "policy string", MAX_ACTION_ARGUMENT_BYTES)
        return
    if isinstance(value, list):
        for item in value:
            _validate_json(item, depth + 1)
        return
    if isinstance(value, dict):
        for key, item in value.items():
            _bounded_text(key, "policy object key", MAX_IDENTIFIER_BYTES)
            _validate_json(item, depth + 1)
        return
    raise PolicyError("policy value is not canonical JSON")


def _canonical(value: Any) -> bytes:
    _validate_json(value)
    try:
        return json.dumps(
            value,
            ensure_ascii=False,
            sort_keys=True,
            separators=(",", ":"),
            allow_nan=False,
        ).encode("utf-8")
    except (TypeError, UnicodeEncodeError, ValueError) as exc:
        raise PolicyError("policy value is not canonical JSON") from exc


def _digest(value: Any) -> str:
    return hashlib.sha256(_canonical(value)).hexdigest()


def _normalise_origin(value: Any) -> str:
    origin = _bounded_text(value, "origin", MAX_ORIGIN_BYTES)
    if any(character.isspace() or ord(character) < 0x20 for character in origin):
        raise PolicyError("origin must not contain whitespace or control characters")
    try:
        parsed = urlsplit(origin)
        hostname = parsed.hostname
        port = parsed.port
    except ValueError as exc:
        raise PolicyError("origin has invalid URL authority") from exc
    if parsed.scheme not in {"http", "https"} or not hostname:
        raise PolicyError("origin must be an http or https origin")
    if parsed.username is not None or parsed.password is not None:
        raise PolicyError("origin must not contain credentials")
    if parsed.path not in {"", "/"} or parsed.query or parsed.fragment:
        raise PolicyError("origin must not contain a path, query, or fragment")
    try:
        host = hostname.encode("idna").decode("ascii").lower()
    except UnicodeError as exc:
        raise PolicyError("origin hostname is invalid") from exc
    if not host or any(character in host for character in "/\\%"):
        raise PolicyError("origin hostname is invalid")
    if ":" in host and not host.startswith("["):
        host = f"[{host}]"
    default_port = (parsed.scheme == "http" and port == 80) or (
        parsed.scheme == "https" and port == 443
    )
    suffix = "" if port is None or default_port else f":{port}"
    return f"{parsed.scheme}://{host}{suffix}"


def _known_text(value: Any, label: str, known: Sequence[str]) -> str:
    value = _bounded_text(value, label)
    if value not in known:
        raise PolicyError(f"{label} is not supported")
    return value


def _bounded_data_classes(value: Any, label: str) -> FrozenSet[str]:
    if isinstance(value, (str, bytes)):
        raise PolicyError(f"{label} must be a set-like collection")
    try:
        values = frozenset(value)
    except (TypeError, ValueError) as exc:
        raise PolicyError(f"{label} must be a set-like collection") from exc
    if len(values) > MAX_DATA_CLASSES:
        raise PolicyError(f"too many {label}s")
    for item in values:
        _bounded_text(item, label)
    return values


@dataclass(frozen=True)
class TargetIdentity:
    """Exact app/window/origin identity used by one authorization.

    ``app_id`` is a stable application identity, never a filesystem path.
    ``window_id`` is an opaque backend handle and is revalidated immediately
    before dispatch.
    """

    kind: str
    app_id: str
    window_id: str
    origin: Optional[str] = None
    display_id: Optional[str] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "kind", _known_text(self.kind, "target kind", ("browser", "desktop")))
        object.__setattr__(self, "app_id", _opaque_identifier(self.app_id, "app_id"))
        object.__setattr__(self, "window_id", _opaque_identifier(self.window_id, "window_id"))
        if self.origin is not None:
            object.__setattr__(self, "origin", _normalise_origin(self.origin))
        if self.kind == "browser" and self.origin is None:
            raise PolicyError("browser targets require an origin")
        if self.display_id is not None:
            object.__setattr__(self, "display_id", _opaque_identifier(self.display_id, "display_id"))

    @classmethod
    def from_wire(cls, value: Any) -> "TargetIdentity":
        if not isinstance(value, Mapping):
            raise PolicyError("target must be an object")
        allowed = {"kind", "app_id", "window_id", "origin", "display_id"}
        if set(value) - allowed:
            raise PolicyError("target contains unknown fields")
        required = {"kind", "app_id", "window_id"}
        if required - set(value):
            raise PolicyError("target is missing required fields")
        origin = value.get("origin")
        display_id = value.get("display_id")
        if origin is not None and not isinstance(origin, str):
            raise PolicyError("target.origin must be a string or absent")
        if display_id is not None and not isinstance(display_id, str):
            raise PolicyError("target.display_id must be a string or absent")
        return cls(
            kind=value["kind"],
            app_id=value["app_id"],
            window_id=value["window_id"],
            origin=origin,
            display_id=display_id,
        )

    def to_wire(self) -> dict[str, str]:
        result: dict[str, str] = {
            "kind": self.kind,
            "app_id": self.app_id,
            "window_id": self.window_id,
        }
        if self.origin is not None:
            result["origin"] = self.origin
        if self.display_id is not None:
            result["display_id"] = self.display_id
        return result

    @property
    def fingerprint(self) -> str:
        return _digest(self.to_wire())


@dataclass(frozen=True)
class Permission:
    """One typed permission requested by a computer action."""

    capability: str
    effect: str
    scope: str = "session"

    def __post_init__(self) -> None:
        object.__setattr__(
            self,
            "capability",
            _known_text(self.capability, "permission capability", tuple(item.value for item in Capability)),
        )
        object.__setattr__(
            self,
            "effect",
            _known_text(self.effect, "permission effect", tuple(item.value for item in Effect)),
        )
        object.__setattr__(self, "scope", _opaque_identifier(self.scope, "permission scope"))

    def to_wire(self) -> dict[str, str]:
        return {
            "capability": self.capability,
            "effect": self.effect,
            "scope": self.scope,
        }


@dataclass(frozen=True)
class Scope:
    """Host-bound scope for an authorization decision."""

    scope_id: str
    session_id: str
    owner_id: str
    extension_generation: int
    target: TargetIdentity
    capabilities: FrozenSet[str]
    effects: FrozenSet[str]
    expires_at: int
    max_actions: int = 1

    def __post_init__(self) -> None:
        object.__setattr__(self, "scope_id", _opaque_identifier(self.scope_id, "scope_id"))
        object.__setattr__(self, "session_id", _opaque_identifier(self.session_id, "session_id"))
        object.__setattr__(self, "owner_id", _opaque_identifier(self.owner_id, "owner_id"))
        object.__setattr__(
            self,
            "extension_generation",
            _positive_integer(self.extension_generation, "extension_generation"),
        )
        if not isinstance(self.target, TargetIdentity):
            raise PolicyError("scope target must be a TargetIdentity")
        if isinstance(self.capabilities, (str, bytes)) or isinstance(self.effects, (str, bytes)):
            raise PolicyError("scope capability/effect sets are malformed")
        try:
            capabilities = frozenset(self.capabilities)
            effects = frozenset(self.effects)
        except (TypeError, ValueError) as exc:
            raise PolicyError("scope capability/effect sets are malformed") from exc
        if not capabilities or len(capabilities) > len(Capability):
            raise PolicyError("scope must contain a bounded capability set")
        if not effects or len(effects) > len(Effect):
            raise PolicyError("scope must contain a bounded effect set")
        for capability in capabilities:
            _known_text(capability, "scope capability", tuple(item.value for item in Capability))
        for effect in effects:
            _known_text(effect, "scope effect", tuple(item.value for item in Effect))
        object.__setattr__(self, "capabilities", capabilities)
        object.__setattr__(self, "effects", effects)
        object.__setattr__(self, "expires_at", _positive_integer(self.expires_at, "scope expiry"))
        if isinstance(self.max_actions, bool) or not isinstance(self.max_actions, int):
            raise PolicyError("scope max_actions must be an integer")
        if self.max_actions < 1 or self.max_actions > MAX_TOTAL_ACTIONS:
            raise PolicyError("scope max_actions is outside its bound")

    @property
    def expires_at_ms(self) -> int:
        return self.expires_at

    def allows(
        self,
        *,
        session_id: str,
        owner_id: str,
        extension_generation: int,
        target: TargetIdentity,
        capability: str,
        effect: str,
        scope: str,
        now: int,
    ) -> bool:
        return (
            now < self.expires_at
            and session_id == self.session_id
            and owner_id == self.owner_id
            and extension_generation == self.extension_generation
            and target == self.target
            and capability in self.capabilities
            and effect in self.effects
            and scope == self.scope_id
        )

    def to_wire(self) -> dict[str, Any]:
        return {
            "scope_id": self.scope_id,
            "session_id": self.session_id,
            "owner_id": self.owner_id,
            "extension_generation": self.extension_generation,
            "target": self.target.to_wire(),
            "capabilities": sorted(self.capabilities),
            "effects": sorted(self.effects),
            "expires_at": self.expires_at,
            "max_actions": self.max_actions,
        }


@dataclass(frozen=True)
class ActionRequest:
    """A model-requested operation after host owner context is attached."""

    operation: str
    target: TargetIdentity
    capability: str
    effect: str
    session_id: str
    owner_id: str
    extension_generation: int
    scope: str
    arguments_digest: str
    frame_generation: int
    data_classes: FrozenSet[str] = frozenset()
    destination: Optional[str] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "operation", _opaque_identifier(self.operation, "operation"))
        if not isinstance(self.target, TargetIdentity):
            raise PolicyError("action target must be a TargetIdentity")
        object.__setattr__(
            self,
            "capability",
            _known_text(self.capability, "action capability", tuple(item.value for item in Capability)),
        )
        object.__setattr__(
            self,
            "effect",
            _known_text(self.effect, "action effect", tuple(item.value for item in Effect)),
        )
        object.__setattr__(self, "session_id", _opaque_identifier(self.session_id, "session_id"))
        object.__setattr__(self, "owner_id", _opaque_identifier(self.owner_id, "owner_id"))
        object.__setattr__(
            self,
            "extension_generation",
            _positive_integer(self.extension_generation, "extension_generation"),
        )
        object.__setattr__(self, "scope", _opaque_identifier(self.scope, "scope"))
        if not isinstance(self.arguments_digest, str) or not _DIGEST_RE.fullmatch(self.arguments_digest):
            raise PolicyError("arguments_digest must be a lowercase SHA-256 digest")
        object.__setattr__(self, "frame_generation", _positive_integer(self.frame_generation, "frame_generation"))
        object.__setattr__(self, "data_classes", _bounded_data_classes(self.data_classes, "data class"))
        if self.destination is not None:
            object.__setattr__(self, "destination", _normalise_origin(self.destination))

    @property
    def fingerprint(self) -> str:
        return _digest(self.to_binding_wire())

    def to_binding_wire(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "operation": self.operation,
            "target": self.target.to_wire(),
            "capability": self.capability,
            "effect": self.effect,
            "session_id": self.session_id,
            "owner_id": self.owner_id,
            "extension_generation": self.extension_generation,
            "scope": self.scope,
            "arguments_digest": self.arguments_digest,
            "frame_generation": self.frame_generation,
            "data_classes": sorted(self.data_classes),
        }
        if self.destination is not None:
            result["destination"] = self.destination
        return result

    def to_intent(self) -> "PolicyIntent":
        return PolicyIntent(
            kind="observation" if self.effect == Effect.OBSERVATION.value else Effect.EXTERNAL_SIDE_EFFECT.value,
            operation=self.operation,
            target=self.target,
            data_classes=self.data_classes,
            read_only=self.effect == Effect.OBSERVATION.value,
            destructive=self.effect == Effect.DESTRUCTIVE.value,
        )


@dataclass(frozen=True)
class PolicyIntent:
    """The API 0.2 policy/evaluate intent shape used by the host adapter."""

    kind: str
    operation: str
    target: TargetIdentity
    data_classes: FrozenSet[str] = frozenset()
    read_only: bool = False
    destructive: bool = False

    def __post_init__(self) -> None:
        object.__setattr__(self, "kind", _opaque_identifier(self.kind, "intent kind"))
        object.__setattr__(self, "operation", _opaque_identifier(self.operation, "intent operation"))
        if not isinstance(self.target, TargetIdentity):
            raise PolicyError("intent target must be a TargetIdentity")
        object.__setattr__(self, "data_classes", _bounded_data_classes(self.data_classes, "intent data class"))
        if not isinstance(self.read_only, bool) or not isinstance(self.destructive, bool):
            raise PolicyError("intent adapter hints must be boolean")

    def to_wire(self) -> dict[str, Any]:
        return {
            "kind": self.kind,
            "operation": self.operation,
            "target": self.target.to_wire(),
            "data_classes": sorted(self.data_classes),
            "adapter_hints": {
                "read_only": self.read_only,
                "destructive": self.destructive,
            },
        }

    @property
    def fingerprint(self) -> str:
        return _digest(self.to_wire())


@dataclass(frozen=True)
class AuthorizationBinding:
    """All facts that an allow decision is bound to."""

    target: TargetIdentity
    capability: str
    session_id: str
    owner_id: str
    extension_generation: int
    scope: str
    effect: str
    arguments_digest: str
    frame_generation: int
    expires_at: int
    data_classes: FrozenSet[str] = frozenset()
    destination: Optional[str] = None

    @classmethod
    def for_action(cls, action: ActionRequest, now: int) -> "AuthorizationBinding":
        return cls(
            target=action.target,
            capability=action.capability,
            session_id=action.session_id,
            owner_id=action.owner_id,
            extension_generation=action.extension_generation,
            scope=action.scope,
            effect=action.effect,
            arguments_digest=action.arguments_digest,
            frame_generation=action.frame_generation,
            expires_at=now + MAX_LOCAL_AUTHORIZATION_TTL_MS,
            data_classes=action.data_classes,
            destination=action.destination,
        )

    def __post_init__(self) -> None:
        if not isinstance(self.target, TargetIdentity):
            raise PolicyError("authorization target must be a TargetIdentity")
        object.__setattr__(
            self,
            "capability",
            _known_text(self.capability, "authorization capability", tuple(item.value for item in Capability)),
        )
        object.__setattr__(self, "session_id", _opaque_identifier(self.session_id, "authorization session"))
        object.__setattr__(self, "owner_id", _opaque_identifier(self.owner_id, "authorization owner"))
        object.__setattr__(
            self,
            "extension_generation",
            _positive_integer(self.extension_generation, "authorization generation"),
        )
        object.__setattr__(self, "scope", _opaque_identifier(self.scope, "authorization scope"))
        object.__setattr__(
            self,
            "effect",
            _known_text(self.effect, "authorization effect", tuple(item.value for item in Effect)),
        )
        if not isinstance(self.arguments_digest, str) or not _DIGEST_RE.fullmatch(self.arguments_digest):
            raise PolicyError("authorization arguments_digest is invalid")
        object.__setattr__(self, "frame_generation", _positive_integer(self.frame_generation, "authorization frame"))
        object.__setattr__(self, "expires_at", _positive_integer(self.expires_at, "authorization expiry"))
        object.__setattr__(self, "data_classes", _bounded_data_classes(self.data_classes, "authorization data class"))
        if self.destination is not None:
            object.__setattr__(self, "destination", _normalise_origin(self.destination))

    def matches(self, action: ActionRequest, now: int) -> bool:
        return (
            now < self.expires_at
            and self.target == action.target
            and self.capability == action.capability
            and self.session_id == action.session_id
            and self.owner_id == action.owner_id
            and self.extension_generation == action.extension_generation
            and self.scope == action.scope
            and self.effect == action.effect
            and self.arguments_digest == action.arguments_digest
            and self.frame_generation == action.frame_generation
            and self.data_classes == action.data_classes
            and self.destination == action.destination
        )

    def to_wire(self) -> dict[str, Any]:
        result: dict[str, Any] = {
            "target": self.target.to_wire(),
            "capability": self.capability,
            "session_id": self.session_id,
            "owner_id": self.owner_id,
            "extension_generation": self.extension_generation,
            "scope": self.scope,
            "effect": self.effect,
            "arguments_digest": self.arguments_digest,
            "frame_generation": self.frame_generation,
            "expires_at": self.expires_at,
            "data_classes": sorted(self.data_classes),
        }
        if self.destination is not None:
            result["destination"] = self.destination
        return result


@dataclass(frozen=True)
class Authorization:
    """A host decision associated with one exact action binding."""

    binding: AuthorizationBinding
    decision: Decision
    issued_at: int
    approval_token: Optional[str] = None

    def __post_init__(self) -> None:
        if self.decision != Decision.ALLOW:
            raise PolicyError("an Authorization can only represent allow")
        object.__setattr__(self, "issued_at", _nonnegative_integer(self.issued_at, "authorization issued_at"))
        if self.approval_token is not None:
            raise PolicyError("approval tokens are not retained on allow grants")

    def permits(self, action: ActionRequest, now: int) -> bool:
        return self.binding.matches(action, now)


@dataclass(frozen=True)
class PolicyDecision:
    """Bounded host policy response."""

    decision: Decision
    reason: str
    approval_token: Optional[str] = None
    authorization: Optional[Authorization] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "reason", _bounded_text(self.reason, "policy reason", MAX_POLICY_REASON_BYTES))
        if self.decision == Decision.ASK:
            if self.approval_token is not None and not _APPROVAL_RE.fullmatch(self.approval_token):
                raise PolicyError("approval_token must be 64 lowercase hexadecimal characters")
            if self.authorization is not None:
                raise PolicyError("ask cannot carry an authorization")
        elif self.approval_token is not None:
            raise PolicyError("only ask may carry an approval token")
        if self.decision == Decision.ALLOW and self.authorization is None:
            raise PolicyError("allow must carry an exact authorization binding")
        if self.decision == Decision.DENY and self.authorization is not None:
            raise PolicyError("deny cannot carry an authorization")

    @classmethod
    def deny(cls, reason: str) -> "PolicyDecision":
        return cls(Decision.DENY, reason)

    @classmethod
    def ask(cls, reason: str, approval_token: Optional[str] = None) -> "PolicyDecision":
        return cls(Decision.ASK, reason, approval_token=approval_token)


class PolicyEvaluator(Protocol):
    """Host adapter boundary; implementations own the JSON-RPC transport."""

    def evaluate(
        self,
        intent: Mapping[str, Any],
        *,
        parent_request_id: Union[int, str],
        approval_token: Optional[str] = None,
    ) -> Mapping[str, Any]:
        ...


def _result_mapping(result: Any) -> Mapping[str, Any]:
    if not isinstance(result, Mapping):
        raise PolicyError("host policy response must be an object")
    if set(result) - {"decision", "approval_token"}:
        raise PolicyError("host policy response has unknown fields")
    return result


def parse_host_decision(result: Any) -> Tuple[Decision, Optional[str]]:
    """Validate the exact API 0.2 policy/evaluate response shape."""
    result = _result_mapping(result)
    try:
        decision = Decision(result.get("decision"))
    except ValueError as exc:
        raise PolicyError("host policy decision must be allow, ask, or deny") from exc
    token = result.get("approval_token")
    if token is not None and (not isinstance(token, str) or not _APPROVAL_RE.fullmatch(token)):
        raise PolicyError("host policy approval token is malformed")
    if decision == Decision.ASK:
        return decision, token
    if token is not None:
        raise PolicyError("allow and deny responses must omit approval_token")
    return decision, None


def arguments_digest(arguments: Any) -> str:
    """Hash action arguments; never return the arguments in a receipt."""
    encoded = _canonical(arguments)
    if len(encoded) > MAX_ACTION_ARGUMENT_BYTES:
        raise PolicyError("action arguments exceed their byte bound")
    return hashlib.sha256(encoded).hexdigest()


def permission_for_operation(operation: str) -> Permission:
    """Return the static capability/effect classification for an operation."""
    operation = _bounded_text(operation, "operation")
    name = operation.rsplit(".", 1)[-1]
    mapping = {
        "observe": (Capability.OBSERVE.value, Effect.OBSERVATION.value),
        "start": (Capability.START.value, Effect.INPUT.value),
        "click": (Capability.CLICK.value, Effect.EXTERNAL_SIDE_EFFECT.value),
        "double_click": (Capability.DOUBLE_CLICK.value, Effect.EXTERNAL_SIDE_EFFECT.value),
        "drag": (Capability.DRAG.value, Effect.EXTERNAL_SIDE_EFFECT.value),
        "move": (Capability.MOVE.value, Effect.INPUT.value),
        "scroll": (Capability.SCROLL.value, Effect.INPUT.value),
        "keypress": (Capability.KEYPRESS.value, Effect.EXTERNAL_SIDE_EFFECT.value),
        "type": (Capability.TYPE.value, Effect.EXTERNAL_SIDE_EFFECT.value),
        "wait": (Capability.WAIT.value, Effect.OBSERVATION.value),
        "screenshot": (Capability.SCREENSHOT.value, Effect.OBSERVATION.value),
    }
    try:
        capability, effect = mapping[name]
    except KeyError as exc:
        raise PolicyError("unsupported computer operation") from exc
    return Permission(capability=capability, effect=effect)


def make_action_request(
    *,
    operation: str,
    target: TargetIdentity,
    session_id: str,
    owner_id: str,
    extension_generation: int,
    frame_generation: int,
    arguments: Any,
    scope: str = "session",
    trusted_data_classes: Sequence[str] = (),
    destination: Optional[str] = None,
) -> ActionRequest:
    """Construct an action with host owner context and a private arg digest."""
    permission = permission_for_operation(operation)
    return ActionRequest(
        operation=operation,
        target=target,
        capability=permission.capability,
        effect=permission.effect,
        session_id=session_id,
        owner_id=owner_id,
        extension_generation=extension_generation,
        scope=scope,
        arguments_digest=arguments_digest(arguments),
        frame_generation=frame_generation,
        data_classes=frozenset(trusted_data_classes),
        destination=destination,
    )


class AuthorizationLedger:
    """Local one-use dispatch ledger; it is never a token issuer."""

    def __init__(self) -> None:
        self._used: set[str] = set()

    def consume(self, authorization: Authorization, action: ActionRequest, now: int) -> None:
        if not authorization.permits(action, now):
            raise PolicyDenied("authorization is stale or does not match the exact action")
        key = authorization.binding.arguments_digest + ":" + str(authorization.binding.frame_generation)
        if key in self._used:
            raise PolicyDenied("authorization has already been consumed")
        self._used.add(key)


class PolicyGate:
    """Fail-closed policy adapter for action admission.

    This class does not dispatch actions. A runtime must call ``evaluate``
    before each action and ``authorize`` immediately before backend dispatch.
    """

    def __init__(
        self,
        evaluator: Optional[PolicyEvaluator] = None,
        *,
        clock_ms: Optional[Callable[[], int]] = None,
    ) -> None:
        self._evaluator = evaluator
        self._clock_ms = clock_ms or (lambda: int(time.time() * 1000))
        self._pending: dict[Tuple[Union[int, str], str], str] = {}
        self._ledger = AuthorizationLedger()

    def _now(self) -> int:
        return _nonnegative_integer(self._clock_ms(), "policy clock")

    def evaluate(
        self,
        action: ActionRequest,
        *,
        parent_request_id: Optional[Union[int, str]],
        approval_token: Optional[str] = None,
    ) -> PolicyDecision:
        now = self._now()
        if parent_request_id is None or isinstance(parent_request_id, bool):
            return PolicyDecision.deny("host owner request is unavailable")
        if not isinstance(parent_request_id, (int, str)):
            return PolicyDecision.deny("host owner request is invalid")
        if isinstance(parent_request_id, int) and parent_request_id < 0:
            return PolicyDecision.deny("host owner request is invalid")
        intent = action.to_intent()
        key = (parent_request_id, action.fingerprint)
        if approval_token is not None:
            if not _APPROVAL_RE.fullmatch(approval_token):
                return PolicyDecision.deny("approval token is malformed")
            if self._pending.get(key) != intent.fingerprint:
                return PolicyDecision.deny("approval is not bound to this exact action")
        if self._evaluator is None:
            return PolicyDecision.deny("host policy adapter is unavailable")
        try:
            result = self._evaluator.evaluate(
                intent.to_wire(),
                parent_request_id=parent_request_id,
                approval_token=approval_token,
            )
            decision, token = parse_host_decision(result)
        except (PolicyError, ValueError, TypeError):
            return PolicyDecision.deny("host policy response is invalid")
        if decision == Decision.DENY:
            self._pending.pop(key, None)
            return PolicyDecision.deny("host policy denied the exact action")
        if decision == Decision.ASK:
            self._pending[key] = intent.fingerprint
            return PolicyDecision.ask("host approval is required", token)
        self._pending.pop(key, None)
        binding = AuthorizationBinding.for_action(action, now)
        return PolicyDecision(
            Decision.ALLOW,
            "host policy allowed the exact action",
            authorization=Authorization(binding=binding, decision=Decision.ALLOW, issued_at=now),
        )

    def authorize(self, action: ActionRequest, decision: PolicyDecision) -> None:
        """Perform the final exact binding check immediately before dispatch."""
        if decision.decision != Decision.ALLOW or decision.authorization is None:
            raise PolicyDenied(decision.reason)
        self._ledger.consume(decision.authorization, action, self._now())


__all__ = [
    "ActionRequest",
    "Authorization",
    "AuthorizationBinding",
    "AuthorizationLedger",
    "Capability",
    "Decision",
    "Effect",
    "MAX_ACTIONS_PER_GROUP",
    "MAX_AUTHORIZATION_TTL_MS",
    "MAX_TOTAL_ACTIONS",
    "Permission",
    "PolicyDecision",
    "PolicyDenied",
    "PolicyError",
    "PolicyEvaluator",
    "PolicyGate",
    "PolicyIntent",
    "Scope",
    "TargetIdentity",
    "arguments_digest",
    "make_action_request",
    "parse_host_decision",
    "permission_for_operation",
]
