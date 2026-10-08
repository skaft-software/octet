from dataclasses import dataclass, field, make_dataclass
from enum import Enum
import io
import json
from pathlib import Path
from typing import Any, Literal, Optional, Union
import unittest

from octet_extension import Extension, RpcError, TypedResult
from octet_extension.typed import _root_codec


class Mode(Enum):
    FAST = "fast"
    EXACT = "exact"


@dataclass
class Nested:
    count: int
    mode: Mode


@dataclass
class Record:
    name: str
    enabled: bool
    samples: list[float]
    note: Optional[str] = None


@dataclass
class Composite:
    nested: Nested
    required_nullable: Optional[int]
    values: list[bool] = field(default_factory=list)
    tag: Literal["record"] = "record"
    count: int = 7


@dataclass
class Recursive:
    next: Optional["Recursive"] = None


class TypedTests(unittest.TestCase):
    def extension(self):
        return Extension(api_version="0.4", stderr=io.StringIO())

    def test_A01_typed_roundtrip(self):
        ext = self.extension()

        @ext.typed_tool(name="echo", description="Typed echo", summary=lambda value: "Echoed record.")
        def echo(value: Record) -> Record:
            self.assertIsInstance(value, Record)
            return value

        self.assertIsInstance(echo(Record("direct", True, [])), Record)
        result = ext._call_tool({"name": "echo", "arguments": {"name": "λ😀", "enabled": True, "samples": [1]}})
        self.assertEqual(result["content"], [{"type": "text", "text": "Echoed record."}])
        self.assertEqual(result["structured_content"], {"name": "λ😀", "enabled": True, "samples": [1.0], "note": None})
        self.assertEqual(ext._tools["echo"].parameters, ext._tools["echo"].output_schema)
        self.assertFalse(ext._tools["echo"].parameters["additionalProperties"])

    def test_A02_invalid_input_no_handler_entry(self):
        ext, calls = self.extension(), []

        @ext.typed_tool(name="echo", description="Typed echo", summary=lambda value: "Echoed.")
        def echo(value: Record) -> Record:
            calls.append(value)
            return value

        fixture = json.loads((Path(__file__).parents[2] / "conformance/typed-values-v1.json").read_text())
        case = fixture["cases"][0]
        expected_schema = case["schema"]
        expected_schema["required"].sort()
        self.assertEqual(ext._tools["echo"].parameters, expected_schema)
        for value in case["invalid"] + [None, [], {"name": "x", "enabled": True, "samples": [float("inf")]},
                                                 {"name": "\ud800", "enabled": True, "samples": []}]:
            with self.subTest(value=value), self.assertRaises(RpcError) as raised:
                ext._call_tool({"name": "echo", "arguments": value})
            self.assertEqual(raised.exception.code, -32602)
        self.assertEqual(calls, [])
        for value in case["valid"]:
            self.assertFalse(ext._call_tool({"name": "echo", "arguments": value})["is_error"])
        self.assertEqual(len(calls), len(case["valid"]))

    def test_A03_invalid_output(self):
        ext = self.extension()

        @ext.typed_tool(name="broken", description="Broken output", summary=lambda value: "Done.")
        def broken(value: Record) -> Record:
            return {"name": "unchecked dictionary"}

        with self.assertRaises(RpcError) as raised:
            ext._call_tool({"name": "broken", "arguments": {"name": "x", "enabled": True, "samples": []}})
        self.assertEqual(raised.exception.code, -32603)

    def test_A04_optional_default_and_enum_codecs(self):
        codec = _root_codec(Composite)
        wire = {"nested": {"count": 2, "mode": "fast"}, "required_nullable": None}
        value = codec.decode(wire)
        self.assertEqual(value, Composite(Nested(2, Mode.FAST), None))
        self.assertEqual(codec.schema["properties"]["count"]["default"], 7)
        self.assertEqual(codec.schema["required"], ["nested", "required_nullable"])
        codec.decode(wire).values.append(True)
        self.assertEqual(codec.decode(wire).values, [])
        for invalid in [{"nested": wire["nested"]}, dict(wire, count=None), dict(wire, tag="unknown"),
                        dict(wire, nested={"count": 2, "mode": "unknown"})]:
            with self.subTest(invalid=invalid), self.assertRaises(ValueError):
                codec.decode(invalid)
        present = dict(wire, required_nullable=9, values=[True, False])
        self.assertEqual(codec.encode(codec.decode(present)), dict(present, count=7, tag="record"))

    def test_portable_scalars_and_value_bounds(self):
        integer, number, text = (_root_codec(t) for t in (int, float, str))
        for value in [True, 1.0, 2**53, -(2**53)]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                integer.decode(value)
        self.assertEqual(integer.decode(2**53 - 1), 2**53 - 1)
        for value in [float("nan"), float("inf"), float("-inf"), True, 2**53]:
            with self.subTest(value=value), self.assertRaises(ValueError):
                number.encode(value)
        for value in ["\ud800", "x" * (128 * 1024 + 1)]:
            with self.assertRaises(ValueError):
                text.encode(value)
        with self.assertRaises(ValueError):
            _root_codec(list[int]).encode([1] * 16385)
        with self.assertRaises(ValueError):
            _root_codec(list[list[int]]).encode([[1] * 1024] * 17)
        with self.assertRaises(ValueError):
            _root_codec(list[str]).encode(["x" * 128000] * 3)
        nested = str
        for _ in range(34):
            nested = list[nested]
        with self.assertRaises(TypeError):
            _root_codec(nested)

    def test_unsupported_types_fail_registration_atomically(self):
        ext = self.extension()
        for annotation in [Any, dict[str, int], tuple[int, str], set[int], Union[str, int], Recursive,
                           make_dataclass("BadDefault", [("n", int, "not an integer")])]:
            def handler(value: Record):
                return value
            handler.__annotations__["return"] = annotation
            with self.subTest(annotation=annotation), self.assertRaises((TypeError, ValueError)):
                ext.typed_tool(name="bad", description="Unsupported", summary=lambda value: "No.")(handler)
            self.assertEqual(ext._tools, {})

    def test_explicit_projection_and_signature_required(self):
        ext = self.extension()
        def missing(value: Record) -> Record:
            return value
        with self.assertRaises(TypeError):
            ext.typed_tool(name="missing", description="Missing summary")(missing)
        async def asynchronous(value: Record) -> Record:
            return value
        with self.assertRaises(TypeError):
            ext.typed_tool(name="async", description="No runtime", summary=str)(asynchronous)
        with self.assertRaises(ValueError):
            Extension(api_version="0.1").typed_tool(name="old", description="Old wire")
        self.assertEqual(ext._tools, {})

    def test_projection_bound_and_typed_envelope(self):
        ext = self.extension()

        @ext.typed_tool(name="envelope", description="Explicit envelope")
        def envelope(value: Record) -> TypedResult[Record]:
            return TypedResult(value, value.name)

        for summary in ["", "x" * 4097, "bad\x1b[31m"]:
            with self.assertRaises(RpcError) as raised:
                ext._call_tool({"name": "envelope", "arguments": {"name": summary, "enabled": True, "samples": []}})
            self.assertEqual(raised.exception.code, -32603)
        self.assertFalse(ext._call_tool({"name": "envelope", "arguments": {"name": "Done.", "enabled": True, "samples": []}})["is_error"])

    def test_existing_explicit_dict_api_is_unchanged(self):
        ext = self.extension()
        @ext.tool(name="legacy", description="Existing explicit dictionary handler")
        def legacy(args):
            return str(args["unchecked"])
        self.assertEqual(ext._call_tool({"name": "legacy", "arguments": {"unchecked": "yes"}})["content"],
                         [{"type": "text", "text": "yes"}])


if __name__ == "__main__":
    unittest.main()
