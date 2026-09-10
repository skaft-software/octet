from __future__ import annotations

import copy
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

from octet_mcp.catalog import (
    CatalogError,
    classify_approval,
    MAX_SCHEMA_BYTES,
    MAX_TEXT_RESULT_BYTES,
    ToolInputError,
    ToolResultError,
    lower_tool_result,
    normalize_catalog_tool,
    normalize_schema,
    validate_arguments,
)

from .helpers import FakeExtension


# Pydantic-style input that previously became an unconstrained {} when $ref
# and $defs were dropped. Both input and output use the same bounded resolver.
REFERENCED_SCHEMA = {
    "type": "object",
    "properties": {"record": {"$ref": "#/$defs/Record"}},
    "required": ["record"],
    "additionalProperties": False,
    "$defs": {
        "Record": {
            "type": "object",
            "properties": {"count": {"type": "integer", "minimum": 1}},
            "required": ["count"],
            "additionalProperties": False,
        }
    },
}


class CatalogSchemaTests(unittest.TestCase):
    def schema(self, value):
        return normalize_schema(value, require_object=True)

    def test_local_refs_are_inlined_and_enforced_without_mutating_source(self):
        original = copy.deepcopy(REFERENCED_SCHEMA)
        normalized = self.schema(original)
        self.assertEqual(original, REFERENCED_SCHEMA)
        self.assertNotIn("$defs", normalized)
        self.assertEqual(normalized["properties"]["record"], REFERENCED_SCHEMA["$defs"]["Record"])
        self.assertEqual(validate_arguments({"record": {"count": 1}}, normalized), {"record": {"count": 1}})
        for value in (None, "anything", {}, {"count": 0}, {"count": True}, {"count": 1, "extra": 2}):
            with self.subTest(value=value), self.assertRaises(ToolInputError):
                validate_arguments({"record": value}, normalized)

    def test_reference_siblings_remain_conjunctive(self):
        schema = self.schema({
            "properties": {"n": {"$ref": "#/$defs/a~1b~0c", "maximum": 3}},
            "$defs": {"a/b~c": {"type": "integer", "minimum": 2}},
        })
        validate_arguments({"n": 2}, schema)
        for value in (1, 4, "2"):
            with self.assertRaises(ToolInputError):
                validate_arguments({"n": value}, schema)

    def test_unsupported_references_dialects_and_keywords_are_rejected(self):
        for value in (
            {"$ref": "https://example.invalid/schema"},
            {"$ref": "file:///secret"}, {"$ref": "#"},
            {"$ref": "#/$defs/missing"},
            {"$defs": {"x": {"$ref": "#/$defs/x"}}, "$ref": "#/$defs/x"},
            {"$defs": {"x": {"$ref": "#/$defs/y"}, "y": {"$ref": "#/$defs/x"}}},
            {"$defs": {"unused": {"pattern": "ignored before"}}},
            {"$ref": "#/$defs/x~2", "$defs": {"x~2": {}}},
            {"$schema": "http://json-schema.org/draft-07/schema#"},
            {"$id": "https://example.invalid/new-base"},
            {"properties": {"value": {"pattern": "^valid$"}}},
            {"properties": {"value": {"format": "email"}}},
            {"not": {}}, {"unevaluatedProperties": False},
            {"items": [{"type": "string"}]}, {"properties": {"x": False}},
            {"properties": {"x": {"$dynamicRef": "#anchor"}}},
        ):
            with self.subTest(value=value), self.assertRaises(CatalogError):
                self.schema(value)

    def test_malformed_supported_keywords_fail_at_catalog_admission(self):
        for value in (
            {"type": []}, {"type": ["string", "string"]}, {"type": {}},
            {"required": [False]}, {"required": ["x", "x"]},
            {"additionalProperties": "false"}, {"uniqueItems": 1},
            {"minimum": True}, {"minimum": float("nan")},
            {"maxLength": -1}, {"maxItems": 2.0},
            {"allOf": []}, {"oneOf": {}}, {"enum": []}, {"enum": [1, 1.0]},
        ):
            with self.subTest(value=value), self.assertRaises(CatalogError):
                self.schema(value)

    def test_const_enum_and_property_names_are_not_sanitized_or_truncated(self):
        exact = "\x00\n" + "x" * 5000
        schema = self.schema({"properties": {"x\n": {"const": exact, "enum": [exact]}}})
        self.assertEqual(schema["properties"]["x\n"]["const"], exact)
        validate_arguments({"x\n": exact}, schema)
        with self.assertRaises(ToolInputError):
            validate_arguments({"x\n": exact[:4096]}, schema)
        with self.assertRaises(CatalogError):
            self.schema({"const": "x" * (MAX_SCHEMA_BYTES + 1)})

    def test_all_supported_assertions_have_matching_validation(self):
        cases = [
            ({"type": "integer", "minimum": 2, "maximum": 3}, 2.0, 1),
            ({"type": "number", "exclusiveMinimum": 2, "exclusiveMaximum": 3}, 2.5, 3),
            ({"type": "string", "minLength": 2, "maxLength": 3}, "世界", "a"),
            ({"type": "array", "minItems": 1, "maxItems": 2, "items": {"type": "integer"}}, [1], ["1"]),
            ({"type": "object", "minProperties": 1, "maxProperties": 2}, {"a": 1}, {}),
            ({"additionalProperties": {"type": "integer", "minimum": 1}}, {"n": 1}, {"n": 0}),
            ({"allOf": [{"type": "integer"}, {"minimum": 2}]}, 2, 1),
            ({"anyOf": [{"const": "yes"}, {"type": "integer"}]}, "yes", "no"),
            ({"oneOf": [{"type": "number"}, {"type": "integer"}]}, 1.5, 1),
            ({"type": ["integer", "null"]}, None, True),
        ]
        for child, valid, invalid in cases:
            with self.subTest(schema=child):
                schema = self.schema({"properties": {"value": child}})
                validate_arguments({"value": valid}, schema)
                with self.assertRaises(ToolInputError):
                    validate_arguments({"value": invalid}, schema)

    def test_json_equality_does_not_confuse_booleans_and_numbers(self):
        schema = self.schema({"properties": {"value": {"enum": [True]}}})
        with self.assertRaises(ToolInputError):
            validate_arguments({"value": 1}, schema)
        schema = self.schema({"properties": {"value": {"const": {"n": True}}}})
        with self.assertRaises(ToolInputError):
            validate_arguments({"value": {"n": 1}}, schema)
        schema = self.schema({"properties": {"value": {"uniqueItems": True}}})
        validate_arguments({"value": [True, 1, {"a": 1}]}, schema)
        with self.assertRaises(ToolInputError):
            validate_arguments({"value": [1, 1.0]}, schema)
        with self.assertRaises(ToolInputError):
            validate_arguments({"value": [{"a": 1}, {"a": 1.0}]}, schema)

    def test_expansion_and_validation_share_non_resetting_budgets(self):
        definitions = {"leaf": {"type": "integer"}}
        last = "leaf"
        for index in range(13):
            current = f"n{index}"
            definitions[current] = {"allOf": [{"$ref": f"#/$defs/{last}"}] * 2}
            last = current
        with self.assertRaises(CatalogError):
            self.schema({"$defs": definitions, "properties": {"x": {"$ref": f"#/$defs/{last}"}}})
        schema = self.schema({"properties": {"x": {"anyOf": [{"type": "string"}, {}]}}})
        with patch("octet_mcp.catalog.MAX_VALIDATION_STEPS", 6):
            with self.assertRaisesRegex(ToolInputError, "work bounds"):
                validate_arguments({"x": [1, 2, 3]}, schema)
        deep = None
        for _ in range(40):
            deep = [deep]
        with self.assertRaises(ToolInputError):
            validate_arguments({"unconstrained": deep}, self.schema({}))

    def test_output_references_and_non_complete_results_fail_explicitly(self):
        binding = normalize_catalog_tool("fixture", "Fixture", {
            "name": "t", "inputSchema": {"type": "object"}, "outputSchema": REFERENCED_SCHEMA,
        }, server_catalog_revision=1)
        with tempfile.TemporaryDirectory() as directory:
            extension = FakeExtension(Path(directory))
            for result in (
                {"structuredContent": {"record": {"count": 0}}, "content": []},
                {"resultType": "input_required", "requestState": "never-replay"},
                {"resultType": "extension-result"},
            ):
                with self.subTest(result=result), self.assertRaises(ToolResultError):
                    lower_tool_result(extension, binding, result, scratch_directory=Path(directory))

    def test_oversized_text_is_never_silently_truncated(self):
        binding = normalize_catalog_tool("fixture", "Fixture", {"name": "t"}, server_catalog_revision=1)
        with tempfile.TemporaryDirectory() as directory:
            extension = FakeExtension(Path(directory))
            for text in ("x" * (MAX_TEXT_RESULT_BYTES + 1), "\x00" * (MAX_TEXT_RESULT_BYTES // 2)):
                with self.assertRaises(ToolResultError):
                    lower_tool_result(extension, binding, {"content": [{"type": "text", "text": text}]}, scratch_directory=Path(directory))

    def test_expanded_schema_must_fit_the_published_json_tree_bounds(self):
        definitions = {"leaf": {"type": "object"}}
        last = "leaf"
        for index in range(20):
            name = f"n{index}"
            definitions[name] = {"$ref": f"#/$defs/{last}", "minProperties": 0}
            last = name
        with self.assertRaises(CatalogError):
            self.schema({"$defs": definitions, "$ref": f"#/$defs/{last}"})

    def test_output_header_annotations_are_rejected_not_published_to_api02(self):
        with self.assertRaises(CatalogError):
            normalize_schema({"properties": {"x": {"type": "string", "x-mcp-header": "X"}}}, require_object=False)
        # Annotation-looking literal data does not become schema vocabulary.
        normalize_schema({"const": {"x-mcp-header": "data"}}, require_object=False)

    def test_malformed_hints_cannot_gain_read_only_authority(self):
        for key in ("readOnlyHint", "destructiveHint", "openWorldHint", "idempotentHint"):
            for value in (1, 0, "false", None, {}, []):
                with self.subTest(key=key, value=value):
                    self.assertEqual(classify_approval({"readOnlyHint": True, key: value}), "unknown")
        self.assertEqual(classify_approval({"readOnlyHint": True, "destructiveHint": False}), "readOnly")
        self.assertEqual(classify_approval({"readOnlyHint": "true", "destructiveHint": True}), "destructive")
        self.assertEqual(classify_approval({"readOnlyHint": True, "openWorldHint": True}), "destructive")

    def test_non_json_arguments_fail_as_tool_input_errors(self):
        cyclic = {}
        cyclic["self"] = cyclic
        for value in (cyclic, {"text": "\ud800"}, {"number": float("inf")}):
            with self.assertRaises(ToolInputError):
                validate_arguments(value, self.schema({}))


class StructuredOnlyResultTests(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.scratch = Path(self.directory.name)
        self.extension = FakeExtension(self.scratch)

    def tearDown(self):
        self.directory.cleanup()

    def lower(self, result, schema=None):
        tool = {"name": "structured-only", "inputSchema": {"type": "object"}}
        if schema is not None:
            tool["outputSchema"] = schema
        binding = normalize_catalog_tool("fixture", "Fixture", tool, server_catalog_revision=1)
        return lower_tool_result(self.extension, binding, result, scratch_directory=self.scratch)

    def test_structured_only_is_explicit_untrusted_json_text_with_and_without_schema(self):
        import json

        structured = {"answer": "Hello, 世界\u0085\x00"}
        for schema in (None, {"type": "object", "properties": {"answer": {"type": "string"}}, "required": ["answer"]}):
            with self.subTest(schema=schema):
                result = self.lower({"structuredContent": structured}, schema)
                self.assertFalse(result["is_error"])
                self.assertEqual(len(result["content"]), 1)
                text = result["content"][0]["text"]
                self.assertTrue(text.startswith("Untrusted MCP structured content (JSON):\n"))
                self.assertEqual(json.loads(text.split("\n", 1)[1]), structured)
                self.assertNotIn("\u0085", text)
                self.assertNotIn("\x00", text)
                if schema is None:
                    self.assertNotIn("structured_content", result)
                    self.assertEqual(result["metadata"]["mcp"]["structuredContent"], structured)
                else:
                    self.assertEqual(result["structured_content"], structured)
                    self.assertNotIn("structuredContent", result["metadata"]["mcp"])

    def test_empty_and_whitespace_text_get_fallback_but_nonempty_text_does_not(self):
        for text in ("", " \n\t"):
            result = self.lower({"content": [{"type": "text", "text": text}], "structuredContent": {"n": 1}})
            self.assertEqual(result["content"][0]["text"], 'Untrusted MCP structured content (JSON):\n{"n":1}')
            self.assertEqual(result["content"][1]["text"], text)
        result = self.lower({"content": [{"type": "text", "text": "upstream explanation"}], "structuredContent": {"n": 1}})
        self.assertEqual(result["content"], [{"type": "text", "text": "upstream explanation"}])

    def test_structured_error_keeps_error_disposition_and_validates_declared_output(self):
        for schema in (None, {"type": "object", "required": ["error"]}):
            result = self.lower({"isError": True, "structuredContent": {"error": "failed"}}, schema)
            self.assertTrue(result["is_error"])
            self.assertEqual(result["content"][0]["text"], 'MCP tool reported an error. Untrusted MCP structured content (JSON):\n{"error":"failed"}')
        with self.assertRaises(ToolResultError):
            self.lower({"isError": True, "structuredContent": {}}, {"required": ["error"]})

    def test_explicit_null_and_array_structured_values_are_rendered_without_inventing_data(self):
        for value, schema, expected in ((None, None, "null"), ([1, 2], {"type": "array", "items": {"type": "integer"}}, "[1,2]")):
            result = self.lower({"structuredContent": value}, schema)
            self.assertEqual(result["content"][0]["text"], "Untrusted MCP structured content (JSON):\n" + expected)

    def test_all_schema_content_and_rendering_budgets_are_checked_before_artifacts(self):
        from octet_mcp.catalog import MAX_METADATA_STRUCTURED_BYTES, MAX_STRUCTURED_BYTES

        image = {"type": "image", "mimeType": "image/png", "data": "AA=="}
        cases = (
            ({"structuredContent": "x" * MAX_METADATA_STRUCTURED_BYTES, "content": [image]}, None),
            ({"structuredContent": "x" * MAX_STRUCTURED_BYTES, "content": [image]}, {"type": "string"}),
            # Valid UTF-8 byte size, but exact ASCII JSON rendering exceeds the
            # text budget. Reject; never slice the JSON or publish partial media.
            ({"structuredContent": "\u0085" * 90000, "content": [image]}, {"type": "string"}),
            ({"structuredContent": {}, "content": [image, {"type": "text", "text": " " * MAX_TEXT_RESULT_BYTES}]}, None),
            ({"structuredContent": {}, "content": [image]}, {"required": ["missing"]}),
            ({"structuredContent": {}, "content": [image, {"type": "resource", "resource": {}}]}, None),
        )
        for result, schema in cases:
            with self.subTest(schema=schema), patch("octet_mcp.catalog._publish_media") as publish:
                with self.assertRaises(ToolResultError):
                    self.lower(result, schema)
                publish.assert_not_called()
        self.assertEqual(self.extension.artifacts, {})

    def test_empty_success_is_rejected_not_replaced_by_fabricated_content(self):
        for result in ({}, {"content": []}, {"content": [{"type": "text", "text": ""}]}, {"content": [{"type": "text", "text": "  "}]}, {"content": [{"type": "image", "mimeType": "image/png", "data": ""}]}):
            with self.subTest(result=result), self.assertRaises(ToolResultError):
                self.lower(result)
        result = self.lower({"isError": True})
        self.assertTrue(result["is_error"])
        self.assertEqual(result["content"], [{"type": "text", "text": "MCP tool reported an error."}])

    def test_retained_metadata_obeys_host_keys_and_wrapper_depth(self):
        for key in ("", "x" * 257, "line\nkey", "control\u0085"):
            with self.subTest(key=key), self.assertRaises(ToolResultError):
                self.lower({"structuredContent": {key: 1}})
            # These are legal JSON keys in declared structured content, which
            # has a different contract than non-model-visible metadata.
            self.assertEqual(self.lower({"structuredContent": {key: 1}}, {})["structured_content"], {key: 1})
        nested = None
        for _ in range(30):
            nested = [nested]
        self.lower({"structuredContent": nested}, {})
        with self.assertRaisesRegex(ToolResultError, "host depth"):
            self.lower({"structuredContent": nested})

    def test_retained_node_bounds_fail_before_publishing_media(self):
        for schema in (None, {}):
            with patch("octet_mcp.catalog._publish_media") as publish:
                with self.assertRaisesRegex(ToolResultError, "host depth or node"):
                    self.lower({"structuredContent": [0] * (16 * 1024), "content": [
                        {"type": "image", "mimeType": "image/png", "data": "AA=="},
                    ]}, schema)
                publish.assert_not_called()

    def test_resource_links_and_embedded_resources_keep_order_and_exact_data(self):
        parts = [
            {"type": "resource_link", "uri": "https://127.0.0.1/private", "name": "private link",
             "title": "Untrusted", "size": 3, "annotations": {"audience": ["assistant"]}},
            {"type": "resource", "resource": {"uri": "file:///private/secret", "text": "世界\n\x00"}},
            {"type": "resource", "resource": {"uri": "opaque:blob", "blob": "AAEC", "mimeType": "application/octet-stream"},
             "_meta": {"custom": "kept"}},
        ]
        with patch("urllib.request.urlopen") as fetch, patch("octet_mcp.catalog._publish_media") as publish:
            result = self.lower({"content": [{"type": "text", "text": "start"}, *parts, {"type": "text", "text": "end"}]})
            self.assertEqual(result["content"][0]["text"], "start")
            self.assertEqual(result["content"][-1]["text"], "end")
            for lowered, original in zip(result["content"][1:-1], parts):
                self.assertTrue(lowered["text"].startswith("Untrusted MCP"))
                self.assertEqual(json.loads(lowered["text"].split("\n", 1)[1]), original)
            fetch.assert_not_called()
            publish.assert_not_called()

    def test_resource_text_does_not_hide_structured_only_data(self):
        resource = {"type": "resource_link", "uri": "opaque:ref", "name": "reference"}
        result = self.lower({"content": [resource], "structuredContent": {"answer": 42}})
        self.assertEqual(json.loads(result["content"][0]["text"].split("\n", 1)[1]), {"answer": 42})
        self.assertEqual(json.loads(result["content"][1]["text"].split("\n", 1)[1]), resource)

    def test_malformed_or_oversized_resources_fail_without_partial_artifacts(self):
        link = {"type": "resource_link", "uri": "opaque:ref", "name": "reference"}
        cases = [
            {**link, "uri": "https://host/with space"}, {**link, "uri": "relative"},
            {**link, "name": ""}, {**link, "size": 1.5}, {**link, "size": True},
            {**link, "size": 2**53}, {**link, "title": "\ud800"},
            {"type": "resource", "resource": {"uri": "opaque:x"}},
            {"type": "resource", "resource": {"uri": "opaque:x", "text": "x", "blob": "AA=="}},
            {"type": "resource", "resource": {"uri": "opaque:x", "blob": "!not-base64"}},
            {"type": "resource", "resource": {"uri": "opaque:x", "text": "x" * MAX_TEXT_RESULT_BYTES}},
        ]
        image = {"type": "image", "mimeType": "image/png", "data": "AA=="}
        for part in cases:
            with self.subTest(part_type=part["type"]), patch("octet_mcp.catalog._publish_media") as publish:
                with self.assertRaises(ToolResultError):
                    self.lower({"content": [image, part]})
                publish.assert_not_called()
        resource = {"type": "resource", "resource": {"uri": "opaque:x", "text": "x" * (MAX_TEXT_RESULT_BYTES // 2)}}
        with self.assertRaises(ToolResultError):
            self.lower({"content": [resource, resource]})


if __name__ == "__main__":
    unittest.main()
