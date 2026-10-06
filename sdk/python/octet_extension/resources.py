"""Extension-local native resources; the host owns identity, pins and lifetime."""
from __future__ import annotations

from dataclasses import dataclass, field
import inspect
import re
import threading
from typing import Any, Generic, TypeVar

from .protocol import RpcError

T = TypeVar("T")
RESOURCE_LIMITS = {"max_records": 256, "max_registrations_per_parent": 32}


def nominal(value: str) -> str:
    if type(value) is not str or not re.fullmatch(r"[A-Za-z][A-Za-z0-9_.-]{0,127}", value):
        raise ValueError("nominal identifier must be 1..128 ASCII letters/digits/_.- starting with a letter")
    return value


def _reference(value: Any) -> dict:
    if type(value) is not dict or set(value) != {"$resource", "type"}:
        raise ValueError("expected a closed ResourceRef")
    token = value["$resource"]
    if type(token) is not str or not 1 <= len(token) <= 128 or not token.isascii():
        raise ValueError("invalid resource token")
    nominal(value["type"])
    return dict(value)


@dataclass(frozen=True)
class Resource(Generic[T]):
    """A host-issued reference. Copies alias one native object, not its lifetime.

    ``value`` resolves extension-local state only while this reference remains live.
    It does not make a remote method call or acquire host execution authority.
    """

    token: str
    type: str
    _registry: Any = field(repr=False, compare=False)

    @property
    def value(self) -> T:
        return self._registry.resolve(self)

    def to_wire(self) -> dict:
        return {"$resource": self.token, "type": self.type}


class _Resources:
    def __init__(self, extension):
        self.extension = extension
        self.types = {}
        self.records = {}
        self.identities = set()
        self.lock = threading.RLock()

    def declare(self, native_type, name, dispose):
        nominal(name)
        if (not isinstance(native_type, type) or dispose is not None and
                (not callable(dispose) or inspect.iscoroutinefunction(dispose) or
                 inspect.iscoroutinefunction(getattr(dispose, "__call__", None)))):
            raise TypeError("resource_type requires a class and optional synchronous disposer")
        if native_type in self.types or any(item[0] == name for item in self.types.values()):
            raise ValueError("duplicate nominal resource type")
        self.types[native_type] = (name, dispose)
        return native_type

    def export(self, value):
        """Adopt native state only after host registration succeeds."""
        self.extension._require_feature("resource_refs_v1")
        native_type = type(value)
        if native_type not in self.types:
            raise TypeError("declare the native class with @ext.resource_type first")
        name, dispose = self.types[native_type]
        identity = id(value)
        with self.lock:
            if identity in self.identities:
                raise ValueError("native object already exported; reuse its Resource instead")
            if len(self.identities) >= RESOURCE_LIMITS["max_records"]:
                raise RpcError(-32000, "quota_exceeded")
            self.identities.add(identity)
        try:
            wire = _reference(self.extension.request("resource/register", {"type": name}, operation_scoped=True))
            if wire["type"] != name:
                raise ValueError("host returned a different nominal resource type")
            reference = Resource(wire["$resource"], name, self)
            with self.lock:
                if reference.token in self.records:
                    raise ValueError("host reused a resource token")
                self.records[reference.token] = (reference, value, dispose)
            return reference
        except BaseException:
            with self.lock:
                self.identities.discard(identity)
            # Registration failure never transfers ownership. The author still
            # owns value and its cleanup; cancellation is not an execution fence.
            raise

    def resolve(self, reference):
        with self.lock:
            record = self.records.get(reference.token)
            if reference._registry is not self or record is None or record[0].type != reference.type:
                raise RpcError(-32000, "resource_unavailable")
            return record[1]

    def codec(self, native_type):
        from .typed import _Codec

        if native_type not in self.types:
            raise TypeError("Resource[T] requires a class declared by @ext.resource_type")
        name = self.types[native_type][0]
        schema = {"type": "object", "additionalProperties": False,
                  "properties": {"$resource": {"type": "string", "minLength": 1, "maxLength": 128},
                                 "type": {"type": "string", "enum": [name]}},
                  "required": ["$resource", "type"]}

        def convert(value, encode):
            if encode:
                if type(value) is not Resource or value.type != name:
                    raise ValueError("typed resource has the wrong nominal type")
                self.resolve(value)
                return value.to_wire()
            wire = _reference(value)
            if wire["type"] != name:
                raise ValueError("typed resource has the wrong nominal type")
            reference = Resource(wire["$resource"], name, self)
            self.resolve(reference)
            return reference

        return _Codec(schema, convert, (("", name),))

    def dispose(self, params):
        self.extension._require_feature("resource_refs_v1")
        if type(params) is not dict or set(params) != {"resources", "reason"} or params["reason"] != "retired":
            raise RpcError(-32602, "invalid resource/dispose request")
        values = params["resources"]
        if type(values) is not list or not 1 <= len(values) <= RESOURCE_LIMITS["max_records"]:
            raise RpcError(-32602, "invalid resource/dispose batch")
        try:
            references = [_reference(value) for value in values]
        except (TypeError, ValueError) as error:
            raise RpcError(-32602, "invalid resource/dispose reference") from error
        if len({r["$resource"] for r in references}) != len(references):
            raise RpcError(-32602, "duplicate resource/dispose reference")
        retired = []
        with self.lock:
            for wire in references:
                record = self.records.get(wire["$resource"])
                if record is not None and record[0].type != wire["type"]:
                    raise RpcError(-32602, "resource/dispose type mismatch")
            for wire in references:
                record = self.records.pop(wire["$resource"], None)
                if record is not None:
                    self.identities.remove(id(record[1]))
                retired.append((wire, record))
        # Remove the whole retired batch before invoking any fallible author code.
        # This runs on the existing request executor, never the reader thread.
        results = []
        for wire, record in retired:
            status = "failed" if record is None else "completed"
            if record is not None and record[2] is not None:
                try:
                    record[2](record[1])
                except Exception:
                    status = "failed"
                    self.extension.logger.warning("native resource disposer failed")
            results.append({"resource": wire, "status": status})
        return {"results": results}
