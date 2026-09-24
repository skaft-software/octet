from __future__ import annotations

import unittest

from octet_mcp.catalog import normalize_catalog_tool, normalize_schema


class CatalogSchemaTests(unittest.TestCase):
    def test_recursive_catch_all_schema_does_not_publish_refs(self):
        # A computer-use catalog can describe arbitrary JSON values through a
        # recursive definition. Octet does not support $defs/$ref on its tool bus.
        catch_all = {"$ref": "#/$defs/JsonValue"}
        raw = {
            "name": "list_apps",
            "annotations": {"readOnlyHint": True},
            "inputSchema": {
                "type": "object",
                "$defs": {"JsonValue": {"type": ["object", "array", "string"]}},
                "additionalProperties": catch_all,
                "properties": {
                    "filter": {
                        "type": "object",
                        "additionalProperties": {
                            "type": "array",
                            "items": catch_all,
                        },
                    },
                },
            },
            "outputSchema": {"type": "object", "additionalProperties": catch_all},
        }

        tool = normalize_catalog_tool(
            "computer-use", "Computer use", raw, server_catalog_revision=1
        )
        self.assertEqual(tool.approval, "readOnly")
        self.assertEqual(tool.input_schema["additionalProperties"], {})
        self.assertEqual(
            tool.input_schema["properties"]["filter"]["additionalProperties"],
            {"type": "array", "items": {}},
        )
        self.assertEqual(tool.output_schema, {"type": "object", "additionalProperties": {}})
        self.assertEqual(raw["inputSchema"]["additionalProperties"], catch_all)
        closed = normalize_schema(
            {"type": "object", "additionalProperties": False}, require_object=True
        )
        self.assertIs(closed["additionalProperties"], False)


if __name__ == "__main__":
    unittest.main()
