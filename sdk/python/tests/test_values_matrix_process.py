"""Light SDK subprocess checks, supplementary to production-host matrix tests."""
import json
from pathlib import Path
import unittest

from test_typed_process import ProcessHarness, TOOLS, VALID


class ValuesMatrixProcessTests(ProcessHarness, unittest.TestCase):
    fixture_name = "values_matrix_fixture.py"
    tools = TOOLS + ["defaults", "hostile"]

    def test_shared_generated_schema(self):
        catalog = self.initialize()
        tool = next(t for t in catalog["tools"] if t["name"] == "typed_roundtrip")
        shared = json.loads((Path(__file__).resolve().parents[2] / "conformance" / "typed-values-v1.json").read_text())
        expected = shared["cases"][0]["schema"]
        expected["required"].sort()  # Contract permits ordering only, not weakened constraints.
        self.assertEqual(tool["parameters"], expected)
        self.assertEqual(tool["output_schema"], tool["parameters"])
        self.shutdown()

    def test_required_nullable_nonnull_defaults_and_fresh_containers(self):
        self.initialize()
        for args in ({}, {"required_nullable": None, "count": None}):
            before = self.events()
            self.assertIn("error", self.call("defaults", args))
            self.assertEqual(self.events(), before)
        for args in ({"required_nullable": None}, {"required_nullable": None},
                     {"required_nullable": "present", "default_nullable": None},
                     {"required_nullable": "present", "default_nullable": "override", "count": 9, "values": [1]}):
            expected = {"default_nullable": "fallback", "count": 7, "values": [], **args}
            expected["values"] = expected["values"] + [expected["count"]]
            self.assertEqual(self.call("defaults", args)["result"]["structured_content"], expected)
        self.shutdown()

    def test_duplicate_sdk_terminal_fault_is_exact(self):
        self.initialize()
        first = self.call("hostile", {"mode": "duplicate"})
        self.assertEqual(first, self.receive())
        self.assertEqual(first["result"]["structured_content"], 42)
        self.assertEqual(self.call(arguments=VALID)["result"]["content"][0]["text"], "Echoed typed record.")
        self.shutdown()


if __name__ == "__main__":
    unittest.main()
