"""Bounded dataclass codecs for the additive typed-tool authoring API."""
from __future__ import annotations

import copy
import inspect
import json
import math
import types
from dataclasses import MISSING, dataclass, fields, is_dataclass
from enum import Enum
from typing import Any, Callable, Generic, Literal, Optional, TypeVar, Union, get_args, get_origin, get_type_hints

from .protocol import RpcError
from .bulk import BlobRef, _blob_codec
from .resources import Resource, nominal

MAX_TYPED_BYTES = 256 * 1024
MAX_TYPED_TEXT_BYTES = 128 * 1024
MAX_TYPED_SUMMARY_BYTES = 4096
PORTABLE_INTEGER = 2**53 - 1
T = TypeVar("T")


@dataclass
class TypedResult(Generic[T]):
    """Explicit text projection, typed value and optional domain diagnostics.

    An error may omit its value; diagnostics are suggestions, never authority.
    """

    value: Optional[T]
    summary: str
    diagnostics: tuple = ()
    is_error: bool = False


def _bounded_json(value: Any, *, max_bytes: int = MAX_TYPED_BYTES, native: bool = False) -> None:
    budget, text_bytes = 16384, 0

    def visit(item: Any, depth: int) -> None:
        nonlocal budget, text_bytes
        budget -= 1
        if depth > 32 or budget < 0:
            raise ValueError("typed JSON exceeds depth/node bounds")
        if native and isinstance(item, (Resource, BlobRef)):
            item = item.to_wire()
        if native and isinstance(item, Enum):
            item = item.value
        if type(item) is str:
            if len(item) > MAX_TYPED_TEXT_BYTES:
                raise ValueError("typed string exceeds byte bound")
            length = len(item.encode("utf-8"))
            text_bytes += length
            if length > MAX_TYPED_TEXT_BYTES or text_bytes > max_bytes:
                raise ValueError("typed string exceeds byte bound")
        elif type(item) is int:
            if abs(item) > PORTABLE_INTEGER:
                raise ValueError("typed integer is not portable")
        elif type(item) is float:
            if not math.isfinite(item):
                raise ValueError("typed float must be finite")
        elif type(item) is list:
            for child in item:
                visit(child, depth + 1)
        elif type(item) is dict or native and is_dataclass(item) and not isinstance(item, type):
            members = item.items() if type(item) is dict else ((f.name, getattr(item, f.name)) for f in fields(item))
            for key, child in members:
                if type(key) is not str or len(key.encode("utf-8")) > 256:
                    raise ValueError("invalid typed JSON key")
                visit(child, depth + 1)
        elif item is not None and type(item) is not bool:
            raise ValueError("unsupported typed JSON value")

    visit(value, 0)
    if not native and len(json.dumps(value, ensure_ascii=False, allow_nan=False, separators=(",", ":")).encode("utf-8")) > max_bytes:
        raise ValueError("typed JSON exceeds byte bound")


def _summary(text: str) -> str:
    if type(text) is not str or not text.strip() or len(text.encode("utf-8")) > MAX_TYPED_SUMMARY_BYTES:
        raise ValueError("typed summary must be 1..4096 UTF-8 bytes")
    if any(ord(c) < 32 and c not in "\n\t" or 127 <= ord(c) <= 159 for c in text):
        raise ValueError("typed summary must be plain text")
    return text


@dataclass
class _Codec:
    schema: dict
    convert: Callable[[Any, bool], Any]
    resource_slots: tuple = ()
    features: frozenset = frozenset()

    def decode(self, value: Any) -> Any:
        _bounded_json(value)
        return self.convert(value, False)

    def encode(self, value: Any) -> Any:
        _bounded_json(value, native=True)
        result = self.convert(value, True)
        _bounded_json(result)
        return result


def _codec(annotation: Any, stack: tuple = (), budget: Optional[list] = None, resources=None) -> _Codec:
    budget = [4096] if budget is None else budget
    budget[0] -= 1
    if budget[0] < 0:
        raise TypeError("typed schema exceeds node bound")
    if len(stack) > 32 or annotation in stack:
        raise TypeError("recursive or excessively nested typed values are unsupported")
    stack = (*stack, annotation)
    origin, args = get_origin(annotation), get_args(annotation)
    if annotation is BlobRef:
        return _blob_codec()
    if origin is Resource:
        if resources is None or len(args) != 1:
            raise TypeError("Resource[T] requires @ext.resource_type on T")
        return resources.codec(args[0])
    if annotation in (str, int, float, bool, type(None)):
        kind = {str: "string", int: "integer", float: "number", bool: "boolean", type(None): "null"}[annotation]

        def scalar(value: Any, encode: bool) -> Any:
            if annotation is float and type(value) in (int, float):
                if type(value) is int and abs(value) > PORTABLE_INTEGER:
                    raise ValueError("typed integer is not portable")
                value = float(value)
            if type(value) is not annotation:
                raise ValueError("typed scalar has the wrong type")
            _bounded_json(value)
            return value

        schema = {"type": kind}
        if annotation is int:
            schema.update(minimum=-PORTABLE_INTEGER, maximum=PORTABLE_INTEGER)
        return _Codec(schema, scalar)
    if origin in (Union, getattr(types, "UnionType", Union)):
        if len(args) != 2 or type(None) not in args:
            raise TypeError("only Optional unions are supported; use a string Enum for variants")
        child = _codec(next(a for a in args if a is not type(None)), stack, budget, resources)
        if child.resource_slots:
            raise TypeError("resource slots cannot occur in Optional/union schemas")
        schema = {"anyOf": [copy.deepcopy(child.schema), {"type": "null"}]}
        return _Codec(schema, lambda value, encode: None if value is None else child.convert(value, encode), features=child.features)
    if origin is list and len(args) == 1:
        child = _codec(args[0], stack, budget, resources)
        if child.resource_slots:
            raise TypeError("resource slots cannot occur in lists")

        def array(value: Any, encode: bool) -> list:
            if type(value) is not list or len(value) > 16384:
                raise ValueError("typed list is invalid or exceeds node bound")
            return [child.convert(item, encode) for item in value]

        return _Codec({"type": "array", "items": child.schema}, array, features=child.features)
    if origin is Literal or isinstance(annotation, type) and issubclass(annotation, Enum):
        enum_type = annotation if origin is not Literal else None
        values = [member.value for member in annotation] if enum_type else list(args)
        if not values or any(type(v) is not str for v in values) or len(set(values)) != len(values):
            raise TypeError("typed enums require distinct string values")
        _bounded_json(values, max_bytes=64 * 1024)

        def variant(value: Any, encode: bool) -> Any:
            if encode and enum_type:
                if type(value) is not enum_type:
                    raise ValueError("typed enum has the wrong type")
                return value.value
            if type(value) is not str or value not in values:
                raise ValueError("unknown typed enum variant")
            return enum_type(value) if enum_type else value

        return _Codec({"type": "string", "enum": values}, variant)
    if isinstance(annotation, type) and is_dataclass(annotation):
        hints = get_type_hints(annotation)
        members = fields(annotation)
        # InitVar, ClassVar and init=False have no portable record codec.
        if set(hints) != {f.name for f in members} or any(not f.init for f in members):
            raise TypeError("typed records require ordinary initialized dataclass fields")
        codecs, defaults, required, properties, slots = {}, {}, [], {}, []
        for member in members:
            if len(member.name.encode("utf-8")) > 256:
                raise TypeError("typed property name exceeds byte bound")
            child = _codec(hints[member.name], stack, budget, resources)
            prefix = "/" + member.name.replace("~", "~0").replace("/", "~1")
            slots.extend((prefix + path, name) for path, name in child.resource_slots)
            codecs[member.name] = child
            schema = copy.deepcopy(child.schema)
            if member.default is not MISSING or member.default_factory is not MISSING:
                if child.resource_slots:
                    raise TypeError("resource slots cannot have persistent defaults")
                default = member.default if member.default is not MISSING else member.default_factory()
                encoded = child.encode(default)
                defaults[member.name] = encoded
                schema["default"] = encoded
            else:
                required.append(member.name)
            properties[member.name] = schema
        schema = {"type": "object", "properties": properties, "additionalProperties": False,
                  "required": sorted(required)}

        def record(value: Any, encode: bool) -> Any:
            if encode:
                if type(value) is not annotation:
                    raise ValueError("typed record has the wrong type")
                return {name: child.convert(getattr(value, name), True) for name, child in codecs.items()}
            if type(value) is not dict or set(value) - set(codecs) or any(name not in value for name in required):
                raise ValueError("typed record has missing, extra or invalid fields")
            decoded = {name: child.convert(value[name] if name in value else copy.deepcopy(defaults[name]), False)
                       for name, child in codecs.items()}
            return annotation(**decoded)

        return _Codec(schema, record, tuple(slots), frozenset().union(*(child.features for child in codecs.values())))
    raise TypeError("unsupported typed annotation; use scalars, dataclasses, lists, Optional or string enums")


def _root_codec(annotation: Any, resources=None) -> _Codec:
    codec = _codec(annotation, resources=resources)
    _bounded_json(codec.schema, max_bytes=64 * 1024)
    return codec


def typed_handler(handler: Callable, summary: Optional[Callable], *, resources=None,
                  operation_id=None, receiver=None, name=None) -> tuple:
    """Compile both contracts before publishing a static tool registration."""
    signature = inspect.signature(handler)
    parameters = list(signature.parameters.values())
    if inspect.iscoroutinefunction(handler) or len(parameters) not in (1, 2) or any(
        p.kind not in (p.POSITIONAL_ONLY, p.POSITIONAL_OR_KEYWORD) or p.default is not p.empty
        for p in parameters
    ):
        raise TypeError("typed tools require a synchronous (input[, context]) signature")
    hints = get_type_hints(handler)
    input_type = hints.get(parameters[0].name)
    if not isinstance(input_type, type) or not is_dataclass(input_type):
        raise TypeError("typed tool input must be an annotated dataclass")
    output_type = hints.get("return")
    wrapped = get_origin(output_type) is TypedResult
    if wrapped:
        output_type = get_args(output_type)[0]
        if summary is not None:
            raise TypeError("TypedResult supplies its own explicit summary")
    elif not callable(summary):
        raise TypeError("typed tool requires an explicit summary callback or TypedResult return annotation")
    if output_type is None:
        raise TypeError("typed tool requires a return annotation")
    input_codec, output_codec = _root_codec(input_type, resources), _root_codec(output_type, resources)
    operation = None
    if operation_id is not None or input_codec.resource_slots or output_codec.resource_slots:
        if any(not path or len(path.encode("utf-8")) > 1024
               for path, _ in (*input_codec.resource_slots, *output_codec.resource_slots)):
            raise TypeError("resource slots require bounded fixed record-property paths")
        operation = {
            "id": nominal(operation_id if operation_id is not None else name),
            "resource_inputs": [{"path": path, "type": kind, "access": "exclusive"}
                                for path, kind in input_codec.resource_slots],
            "resource_outputs": [{"path": path, "type": kind} for path, kind in output_codec.resource_slots],
        }
    if receiver is not None:
        if operation is None or receiver not in {path for path, _ in input_codec.resource_slots}:
            raise TypeError("receiver must identify a generated resource input path")
        operation["receiver"] = receiver

    def invoke(arguments: Any, context: Any) -> dict:
        from .extension import text_content, tool_result

        try:
            decoded = input_codec.decode(arguments)
        except (ValueError, TypeError, OverflowError) as error:
            raise RpcError(-32602, "arguments do not match the typed input contract") from error
        value = handler(decoded, context) if len(parameters) == 2 else handler(decoded)
        try:
            if wrapped:
                if type(value) is not TypedResult or type(value.is_error) is not bool:
                    raise ValueError("expected TypedResult")
                text, diagnostics, is_error = _summary(value.summary), value.diagnostics, value.is_error
                value = value.value
            else:
                text, diagnostics, is_error = _summary(summary(value)), (), False
            result = tool_result(text_content(text), is_error=is_error,
                                 diagnostics=diagnostics if diagnostics else None)
            if value is not None or not is_error:
                result["structured_content"] = output_codec.encode(value)
            return result
        except (ValueError, TypeError, OverflowError) as error:
            raise RpcError(-32603, "result does not match the typed output contract") from error

    return input_codec.schema, output_codec.schema, invoke, operation, input_codec.features | output_codec.features
