"""Private, per-operation standard elicitation and bounded MCP 2026 MRTR.

Integration (only AFTER exact host action approval):
  handler = make_interaction_handler(extension, owner=owner.as_dict(),
      parent_request_id=captured_extension_request_id, cancellation=token,
      deadline=absolute_monotonic_deadline, is_active=host_owner_parent_check,
      server_label=config.label, private_ui=True)
  run_operation(send, method, params, handler=handler, deadline=deadline,
                cancellation=token)
``send`` accepts the existing request keywords timeout_ms, _deadline and
cancellation, allocates a fresh RPC ID, and returns one RAW result (including
input_required). It must preserve the clientCapabilities added here. Legacy
HTTP instead accepts interaction_handler=handler on request/call_tool and must
explicitly enable_elicitation at construction. Legacy stdio has no exact reverse
origin correlation and deliberately does not advertise or route elicitation.

UI callbacks are ONLY Extension.request_input(secret=True) and confirm with an
EXPLICIT captured parent_request_id, including on callback threads. No ambient
current-request lookup, model prompts, notifications, URL opening/fetching, or
policy approvals exist here. private_ui is a host-composition assertion, never
server/model metadata. A handler cannot be reused for another operation.

Wire evidence: MCP aa8ce049f089f92618340190d4ece141f663310d,
2026-07-28 client/elicitation, basic/patterns/mrtr and schema.ts. This is a bounded
flat primitive/single-enum subset, not full JSON Schema or Codex extensions.
"""
from __future__ import annotations

from contextlib import contextmanager, nullcontext
from dataclasses import dataclass, field
import datetime
import json
import math
import re
import threading
import time
from typing import Any, Callable, Mapping, Optional
from urllib.parse import urlsplit

from .protocol import McpCancelled, McpError, McpProtocolError, McpTimeout

MAX_MRTR_ROUNDS = 4  # continuations, in addition to the original request
MAX_OPERATION_BYTES = 32 * 1024 * 1024
MAX_STATE_BYTES = 64 * 1024
MAX_INPUT_REQUESTS = 4
MAX_INTERACTIONS = 8
MAX_INTERACTION_BYTES = 128 * 1024
MAX_SCHEMA_BYTES = 8 * 1024
MAX_ANSWER_BYTES = 4 * 1024
MAX_FIELDS = 16
MAX_ENUM_VALUES = 64
MAX_REVIEW_ROUNDS = 3
MAX_PRIVATE_CALLBACKS = 8
_PRIVATE_CALLBACK_SLOTS = threading.BoundedSemaphore(MAX_PRIVATE_CALLBACKS)
ELIGIBLE_METHODS = frozenset({"tools/call", "resources/read"})
META_CLIENT_CAPABILITIES = "io.modelcontextprotocol/clientCapabilities"
_OWNER_KEYS = ("session_id", "extension_instance_id", "process_generation")
_SENSITIVE = re.compile(
    r"password|passphrase|passwd|secret|credential|api.?key|access.?token|refresh.?token|"
    r"bearer|private.?key|seed.?phrase|mnemonic|credit.?card|debit.?card|card.?number|"
    r"payment|\bcvv|\bcvc|\bpin\b|pin.?code|one.?time.?code|verification.?code|security.?code|"
    r"\btoken\b|\botp\b|(?:api|auth|authentication|authorization).?(?:key|token|code)", re.I,
)


def _error(code: str = "unsupported_elicitation") -> McpError:
    return McpError(code, "MCP private interaction is unsupported, unavailable, or exceeds its bound")


def _json(value: Any, maximum: int) -> str:
    try:
        text = json.dumps(value, ensure_ascii=False, separators=(",", ":"), allow_nan=False)
        if len(text.encode("utf-8")) > maximum:
            raise ValueError
        return text
    except (ValueError, TypeError, UnicodeError, RecursionError):
        raise _error("interaction_bound") from None


def _text(value: Any, maximum: int, *, empty: bool = False) -> str:
    if (not isinstance(value, str) or (not empty and not value.strip())
            or any(ord(c) < 32 and c not in "\n\t" for c in value)):
        raise _error()
    try:
        if len(value.encode("utf-8")) > maximum:
            raise _error("interaction_bound")
    except UnicodeError:
        raise _error() from None
    return value


def _finite_number(value: Any) -> bool:
    try:
        return type(value) in (int, float) and math.isfinite(value)
    except OverflowError:
        return False


def _check_boundary(deadline: float, cancellation: Any) -> None:
    if cancellation is not None and bool(getattr(cancellation, "cancelled", False)):
        raise McpCancelled("request_cancelled", "MCP private interaction was cancelled; no continuation was sent")
    if time.monotonic() >= deadline:
        raise McpTimeout("request_timeout", "MCP private interaction timed out; no continuation was sent")


def validate_form_schema(value: Any) -> dict[str, Any]:
    """Reject unknown vocabulary, nested schemas and credential collection."""
    schema = json.loads(_json(value, MAX_SCHEMA_BYTES))
    if (not isinstance(schema, dict) or schema.get("type") != "object"
            or set(schema) - {"$schema", "type", "properties", "required", "additionalProperties"}
            or schema.get("additionalProperties", False) is not False):
        raise _error()
    if "$schema" in schema and (not isinstance(schema["$schema"], str) or schema["$schema"] not in {
        "https://json-schema.org/draft/2020-12/schema", "http://json-schema.org/draft-07/schema#",
    }):
        raise _error()
    props = schema.get("properties")
    required = schema.get("required", [])
    if (not isinstance(props, dict) or not 1 <= len(props) <= MAX_FIELDS
            or not isinstance(required, list) or any(not isinstance(k, str) for k in required)
            or len(set(required)) != len(required) or set(required) - set(props)):
        raise _error()
    for name, spec in props.items():
        _text(name, 128)
        if not isinstance(spec, dict):
            raise _error()
        kind = spec.get("type")
        common = {"type", "title", "description", "default"}
        vocabulary = {
            "string": {"minLength", "maxLength", "format", "enum", "enumNames", "oneOf"},
            "integer": {"minimum", "maximum"}, "number": {"minimum", "maximum"}, "boolean": set(),
        }
        if not isinstance(kind, str) or kind not in vocabulary or set(spec) - common - vocabulary[kind]:
            raise _error()
        labels = [name]
        for key in ("title", "description"):
            if key in spec:
                labels.append(_text(spec[key], 1024))
        if _SENSITIVE.search(" ".join(labels)):
            raise _error("sensitive_elicitation")
        for key in ("minLength", "maxLength"):
            if key in spec and (type(spec[key]) is not int or not 0 <= spec[key] <= MAX_ANSWER_BYTES):
                raise _error()
        for key in ("minimum", "maximum"):
            if key in spec and (not _finite_number(spec[key])):
                raise _error()
        for lo, hi in (("minLength", "maxLength"), ("minimum", "maximum")):
            if lo in spec and hi in spec and spec[lo] > spec[hi]:
                raise _error()
        if "format" in spec and (not isinstance(spec["format"], str) or spec["format"] not in {"email", "uri", "date", "date-time"}):
            raise _error()
        if "oneOf" in spec:
            options = spec["oneOf"]
            if ("enum" in spec or not isinstance(options, list) or not 1 <= len(options) <= MAX_ENUM_VALUES
                    or any(not isinstance(o, dict) or set(o) != {"const", "title"} for o in options)):
                raise _error()
            for option in options:
                _text(option["title"], 256)
            values = [o["const"] for o in options]
        else:
            values = spec.get("enum")
        if "enum" in spec or "oneOf" in spec:
            if not isinstance(values, list) or not 1 <= len(values) <= MAX_ENUM_VALUES:
                raise _error()
            for item in values:
                _text(item, 256, empty=True)
            if len(set(values)) != len(values):
                raise _error()
        if "enumNames" in spec:
            names = spec["enumNames"]
            if "enum" not in spec or not isinstance(names, list) or len(names) != len(spec["enum"]):
                raise _error()
            for name in names:
                _text(name, 256)
        if "default" in spec:
            _validate_primitive(spec["default"], spec)
    return schema


def _validate_primitive(value: Any, spec: Mapping[str, Any]) -> None:
    kind = spec["type"]
    if kind == "string":
        _text(value, MAX_ANSWER_BYTES, empty=True)
        if not spec.get("minLength", 0) <= len(value) <= spec.get("maxLength", MAX_ANSWER_BYTES):
            raise _error("invalid_elicitation_answer")
        choices = spec.get("enum", [x["const"] for x in spec.get("oneOf", [])])
        if choices and value not in choices:
            raise _error("invalid_elicitation_answer")
        fmt = spec.get("format")
        try:
            if fmt == "email" and not re.fullmatch(r"[^\s@]+@[^\s@]+\.[^\s@]+", value):
                raise ValueError
            if fmt == "uri" and (not urlsplit(value).scheme or any(c.isspace() for c in value)):
                raise ValueError
            if fmt == "date":
                if not re.fullmatch(r"\d{4}-\d{2}-\d{2}", value):
                    raise ValueError
                datetime.date.fromisoformat(value)
            if fmt == "date-time":
                if not re.fullmatch(r"\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}(?:\.\d+)?(?:Z|[+-]\d{2}:\d{2})", value):
                    raise ValueError
                datetime.datetime.fromisoformat(value.replace("Z", "+00:00"))
        except ValueError:
            raise _error("invalid_elicitation_answer") from None
    elif kind == "boolean":
        if type(value) is not bool:
            raise _error("invalid_elicitation_answer")
    else:
        if (not _finite_number(value)
                or (kind == "integer" and int(value) != value)
                or value < spec.get("minimum", -math.inf) or value > spec.get("maximum", math.inf)):
            raise _error("invalid_elicitation_answer")


def _answer(text: str, schema: Mapping[str, Any]) -> dict[str, Any]:
    def unique(pairs: Any) -> dict[str, Any]:
        result = {}
        for key, value in pairs:
            if key in result:
                raise ValueError
            result[key] = value
        return result
    try:
        value = json.loads(text, object_pairs_hook=unique)
    except (ValueError, RecursionError):
        raise _error("invalid_elicitation_answer") from None
    props = schema["properties"]
    if (not isinstance(value, dict) or set(value) - set(props)
            or set(schema.get("required", [])) - set(value)):
        raise _error("invalid_elicitation_answer")
    for key, item in value.items():
        _validate_primitive(item, props[key])
    return value


@dataclass(repr=False)
class _Usage:
    lock: Any = field(default_factory=threading.Lock)
    busy: Any = field(default_factory=threading.Lock)
    claimed: bool = False
    active: bool = False
    deadline: Optional[float] = None
    count: int = 0
    bytes: int = 0
    private: list[Any] = field(default_factory=list)


@dataclass(frozen=True, repr=False)
class InteractionHandler:
    """Immutable host attribution, with bounded state local to this operation."""
    owner: tuple[str, str, int]
    parent_request_id: int
    cancellation: Any
    deadline: float
    is_active: Callable[[Mapping[str, Any], int], bool]
    request_input: Callable[..., Optional[str]]
    confirm: Callable[..., bool]
    server_label: str
    _usage: _Usage = field(default_factory=_Usage, init=False)

    @property
    def client_capabilities(self) -> dict[str, Any]:
        return {"elicitation": {"form": {}, "url": {}}}

    def check_active(self) -> None:
        _check_boundary(self._usage.deadline or self.deadline, self.cancellation)
        if self._usage.claimed and not self._usage.active:
            raise _error("stale_interaction")
        try:
            active = self.is_active(dict(zip(_OWNER_KEYS, self.owner)), self.parent_request_id) is True
        except Exception:
            active = False
        if not active:
            raise _error("stale_interaction")

    @contextmanager
    def operation(self, method: str, *, deadline: float, cancellation: Any):
        if method not in ELIGIBLE_METHODS or cancellation is not self.cancellation or deadline > self.deadline:
            raise _error("unbound_interaction")
        self.check_active()
        with self._usage.lock:
            if self._usage.claimed:
                raise _error("reused_interaction")
            self._usage.claimed = self._usage.active = True
            self._usage.deadline = deadline
        try:
            yield self
        finally:
            with self._usage.lock:
                self._usage.active = False
                self._usage.private.clear()

    def _consume(self, value: Any) -> None:
        size = len(_json(value, MAX_INTERACTION_BYTES).encode("utf-8"))
        self._usage.bytes += size
        if self._usage.bytes > MAX_INTERACTION_BYTES:
            raise _error("interaction_bound")

    def _callback(self, callback: Callable[..., Any], *args: Any, **kwargs: Any) -> Any:
        self.check_active()
        if not self._usage.active or not _PRIVATE_CALLBACK_SLOTS.acquire(blocking=False):
            raise _error("interaction_unavailable")
        done = threading.Event()
        result: list[Any] = []
        def run() -> None:
            try:
                self.check_active()
                if self._usage.active:
                    result.append(callback(*args, parent_request_id=self.parent_request_id, **kwargs))
            except Exception:
                pass  # Never retain UI exception text or answers in diagnostics.
            finally:
                _PRIVATE_CALLBACK_SLOTS.release()
                done.set()
        try:
            threading.Thread(target=run, name="mcp-private-input", daemon=True).start()
        except BaseException:
            _PRIVATE_CALLBACK_SLOTS.release()
            raise
        while not done.wait(.05):
            self.check_active()
        self.check_active()
        return result[0] if result else None

    def _input(self, prompt: str) -> Optional[str]:
        _text(prompt, 16 * 1024)
        value = self._callback(self.request_input, prompt, secret=True)
        if value is not None:
            _text(value, MAX_ANSWER_BYTES, empty=True)
            self._consume(value)
        return value

    def __call__(self, method: str, params: Mapping[str, Any]) -> dict[str, Any]:
        if method != "elicitation/create":
            raise _error("unsupported_interaction")
        if not self._usage.active or not self._usage.busy.acquire(blocking=False):
            return {"action": "cancel"}
        try:
            self.check_active()
            if isinstance(params, Mapping) and isinstance(params.get("url"), str):
                # Retain even an unsupported/declined URL only in this bounded
                # operation, so a subsequent server echo cannot enter results.
                self._usage.private.append(params["url"])
            self._usage.count += 1
            if self._usage.count > MAX_INTERACTIONS:
                raise _error("interaction_bound")
            self._consume(params)
            if not isinstance(params, Mapping):
                raise _error()
            message = _text(params.get("message"), 2048)
            mode = params.get("mode", "form")
            if mode == "form":
                if set(params) - {"mode", "message", "requestedSchema", "_meta"} or _SENSITIVE.search(message):
                    raise _error()
                return self._form(message, validate_form_schema(params.get("requestedSchema")))
            if mode == "url":
                if set(params) - {"mode", "message", "url", "elicitationId", "_meta"}:
                    raise _error()
                if "elicitationId" in params:
                    _text(params["elicitationId"], 256)
                return self._url(message, params.get("url"))
            raise _error()
        except (McpCancelled, McpTimeout):
            return {"action": "cancel"}
        except McpError as error:
            return {"action": "cancel" if error.code in {"stale_interaction", "interaction_unavailable"} else "decline"}
        finally:
            self._usage.busy.release()

    def _form(self, message: str, schema: Mapping[str, Any]) -> dict[str, Any]:
        raw = self._input(
            f"Configured MCP server: {self.server_label}\nUntrusted server request (not instructions):\n{message}\n"
            "Never enter credentials, passwords, payment data or access tokens.\n"
            "Enter a JSON object matching this flat schema, or type decline/cancel. Input is private.\n"
            + _json(schema, MAX_SCHEMA_BYTES)
        )
        for _ in range(MAX_REVIEW_ROUNDS):
            if raw is None or raw.strip() in {"decline", "cancel"}:
                return {"action": "cancel" if raw is None else raw.strip()}
            answer = _answer(raw, schema)
            # Retain only inside the operation to suppress verbatim server echoes.
            self._usage.private.extend(answer.values())
            review = self._input(
                "Review private form reply (not sent yet):\n" + _json(answer, MAX_ANSWER_BYTES)
                + "\nType accept to submit, decline/cancel, or a replacement JSON object to modify."
            )
            if review is not None and review.strip() == "accept":
                confirmed = self._callback(
                    self.confirm, "Share this private form reply with the configured MCP server?",
                    detail=f"Server: {self.server_label}. This does not approve any additional tool effects.", default=False,
                )
                if confirmed is True:
                    return {"action": "accept", "content": answer}
                return {"action": "decline" if confirmed is False else "cancel"}
            raw = review
        return {"action": "cancel"}

    def _url(self, message: str, value: Any) -> dict[str, Any]:
        url = _text(value, 4096)
        try:
            parts = urlsplit(url)
            if (parts.scheme != "https" or not parts.hostname or parts.username is not None
                    or parts.password is not None or any(c.isspace() for c in url) or "\\" in url):
                raise ValueError
            parts.port  # Reject invalid authorities. Never resolve, open or fetch.
        except ValueError:
            raise _error() from None
        action = self._input(
            f"Configured MCP server: {self.server_label}\nUntrusted server request:\n{message}\n"
            f"HTTPS destination host: {parts.hostname}\nFull URL (private): {url}\n"
            "No page is opened or fetched by Octet. Examine the URL and, only if you choose, open it manually "
            "outside the assistant. Never paste credentials here. Type accept to resume this operation, "
            "decline, or cancel. Accept is consent, not proof that the external interaction completed."
        )
        action = action.strip() if action is not None else "cancel"
        return {"action": action if action in {"accept", "decline", "cancel"} else "cancel"}

    def redact_result(self, value: Any) -> Any:
        """Suppress direct private echoes; transformed server data is still untrusted."""
        private = tuple(self._usage.private)
        def redact(item: Any) -> Any:
            if isinstance(item, str):
                for secret in private:
                    texts = (secret, json.dumps(secret, ensure_ascii=False)[1:-1], json.dumps(secret)[1:-1]) if isinstance(secret, str) else (_json(secret, MAX_ANSWER_BYTES),)
                    for text in texts:
                        if text:
                            item = item.replace(text, "[private input]")
                return item
            if isinstance(item, list):
                return [redact(x) for x in item]
            if isinstance(item, dict):
                return {redact(k): redact(v) for k, v in item.items()}
            if any(type(item) is type(secret) and item == secret for secret in private):
                return "[private input]"
            return item
        return redact(value)


def make_interaction_handler(
    extension: Any, *, owner: Mapping[str, Any], parent_request_id: int,
    cancellation: Any, deadline: float, is_active: Callable[[Mapping[str, Any], int], bool],
    server_label: str, private_ui: bool = False,
) -> Optional[InteractionHandler]:
    """Host-only factory; unavailable/headless composition advertises nothing."""
    if private_ui is not True or getattr(extension, "api_version", None) != "0.2":
        return None
    if (not isinstance(owner, Mapping) or set(owner) != set(_OWNER_KEYS)
            or type(parent_request_id) is not int or not 0 <= parent_request_id < 2**64
            or type(owner["process_generation"]) is not int or not 1 <= owner["process_generation"] < 2**64
            or not math.isfinite(deadline) or not callable(is_active)):
        return None
    try:
        for key in _OWNER_KEYS[:2]:
            _text(owner[key], 256)
        label = _text(server_label, 256)
    except McpError:
        return None
    if not callable(getattr(extension, "request_input", None)) or not callable(getattr(extension, "confirm", None)):
        return None
    return InteractionHandler(
        tuple(owner[k] for k in _OWNER_KEYS), parent_request_id, cancellation, deadline,
        is_active, extension.request_input, extension.confirm, label,
    )


def run_operation(
    send: Callable[..., Any], method: str, params: Mapping[str, Any], *,
    handler: Optional[InteractionHandler] = None, deadline: float, cancellation: Any = None,
) -> Mapping[str, Any]:
    """Drive successful input_required continuations only; NEVER retry failures.

    Original arguments are snapshotted. inputResponses and exact opaque string
    requestState apply only to the next round, never to a parallel operation.
    State-only rounds are supported without falsely declaring client inputs.
    """
    if not math.isfinite(deadline):
        raise ValueError("MCP operation requires a finite deadline")
    original = json.loads(_json(params, MAX_OPERATION_BYTES))
    if not isinstance(original, dict) or {"requestState", "inputResponses"} & set(original):
        raise _error("unbound_interaction")
    metadata = original.get("_meta", {})
    if not isinstance(metadata, dict):
        raise _error("invalid_outbound")
    original["_meta"] = {**metadata, META_CLIENT_CAPABILITIES: handler.client_capabilities if handler else {}}
    if handler:
        deadline = min(deadline, handler.deadline)
    scope = handler.operation(method, deadline=deadline, cancellation=cancellation) if handler else nullcontext()
    with scope:
        outgoing = original
        consumed = 0
        for round_index in range(MAX_MRTR_ROUNDS + 1):
            _check_boundary(deadline, cancellation)
            if handler:
                handler.check_active()
            # Detach each wire call: a transport cannot mutate the original args.
            wire = json.loads(_json(outgoing, MAX_OPERATION_BYTES))
            consumed += len(_json(wire, MAX_OPERATION_BYTES).encode("utf-8"))
            if consumed > MAX_OPERATION_BYTES:
                raise _error("interaction_bound")
            result = send(method, wire, timeout_ms=max(1, math.ceil((deadline - time.monotonic()) * 1000)),
                          _deadline=deadline, cancellation=cancellation)
            _check_boundary(deadline, cancellation)
            if handler:
                handler.check_active()
            consumed += len(_json(result, MAX_OPERATION_BYTES).encode("utf-8"))
            if consumed > MAX_OPERATION_BYTES:
                raise _error("interaction_bound")
            if not isinstance(result, Mapping):
                raise McpProtocolError("invalid_result", "MCP operation result was malformed")
            kind = result.get("resultType", "complete")
            if kind == "complete":
                if {"requestState", "inputRequests"} & set(result):
                    raise McpProtocolError("invalid_result", "MCP complete result contained continuation data")
                field = {"tools/call": "content", "resources/read": "contents"}.get(method)
                if field is not None and not isinstance(result.get(field), list):
                    raise McpProtocolError("invalid_result", "MCP complete operation omitted its required content array")
                return handler.redact_result(result) if handler else result
            if kind != "input_required":
                raise McpProtocolError("invalid_result_type", "MCP result type was unsupported")
            if method not in ELIGIBLE_METHODS:
                raise _error("unsupported_interaction")
            if round_index == MAX_MRTR_ROUNDS:
                raise _error("interaction_round_limit")
            if (set(result) - {"resultType", "inputRequests", "requestState", "_meta"}
                    or not {"inputRequests", "requestState"} & set(result)):
                raise _error("invalid_input_required")
            outgoing = dict(original)
            if "requestState" in result:
                state = result["requestState"]
                # Only bound the string's bytes. Never parse, normalize or redact.
                if not isinstance(state, str) or len(state.encode("utf-8")) > MAX_STATE_BYTES:
                    raise _error("invalid_request_state")
                outgoing["requestState"] = state
            if "inputRequests" in result:
                requests = result["inputRequests"]
                if not isinstance(requests, dict) or len(requests) > MAX_INPUT_REQUESTS:
                    raise _error("interaction_bound")
                # Validate the entire fanout before ANY prompt; never sample/root.
                for key, request in requests.items():
                    _text(key, 256)
                    if (handler is None or not isinstance(request, dict) or set(request) != {"method", "params"}
                            or request["method"] != "elicitation/create" or not isinstance(request["params"], dict)):
                        raise _error("unsupported_interaction")
                outgoing["inputResponses"] = {
                    key: handler(request["method"], request["params"]) for key, request in requests.items()
                }
