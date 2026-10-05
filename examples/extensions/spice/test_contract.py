"""Static SDK registration checks only: no RPC, host, references granted or solver."""
import json
import unittest

from extension import WaveformRef, ext
from octet_extension import BlobDigest, BlobRef


class GeneratedContractTests(unittest.TestCase):
    def test_exact_catalog_has_no_pinned_release_tool(self):
        self.assertEqual(set(ext._tools),
                         {"spice_open", "spice_instantiate", "spice_transient", "spice_measure"})

    def test_native_schema_and_descriptor_derive_from_same_declarations(self):
        opened = ext._tools["spice_open"]
        instantiate = ext._tools["spice_instantiate"]
        transient = ext._tools["spice_transient"]
        self.assertEqual(opened.output_schema, instantiate.parameters)
        self.assertEqual(instantiate.output_schema, transient.parameters)
        for tool, field, nominal in ((instantiate, "circuit", "spice.Circuit.v1"),
                                     (transient, "session", "spice.SimulationSession.v1")):
            schema = tool.parameters["properties"][field]
            self.assertEqual(schema["properties"]["type"]["enum"], [nominal])
            self.assertFalse(schema["additionalProperties"])
            self.assertEqual(tool.operation["receiver"], "/" + field)
            self.assertEqual(tool.operation["resource_inputs"],
                             [{"path": "/" + field, "type": nominal, "access": "exclusive"}])
        self.assertEqual(opened.operation["resource_outputs"],
                         [{"path": "/circuit", "type": "spice.Circuit.v1"}])
        self.assertEqual(instantiate.operation["resource_outputs"],
                         [{"path": "/session", "type": "spice.SimulationSession.v1"}])
        self.assertEqual(transient.operation["resource_outputs"], [])

    def test_waveform_schema_is_descriptor_only_and_measurement_reuses_it(self):
        schema = ext._tools["spice_transient"].output_schema
        self.assertEqual(schema, ext._tools["spice_measure"].parameters)
        self.assertEqual(set(schema["properties"]),
                         {"blob", "samples", "encoding", "signal", "time_unit", "value_unit"})
        blob = schema["properties"]["blob"]
        self.assertEqual(set(blob["properties"]), {"$blob", "bytes", "digest", "media_type"})
        self.assertFalse(blob["additionalProperties"])
        self.assertEqual(blob["properties"]["digest"]["properties"]["algorithm"]["enum"], ["sha256"])
        digest = blob["properties"]["digest"]["properties"]["value"]
        self.assertEqual(digest, {"type": "string", "minLength": 64, "maxLength": 64})
        self.assertEqual(blob["properties"]["$blob"]["maxLength"], 128)
        self.assertIsNone(ext._tools["spice_measure"].operation)
        self.assertIsNotNone(ext.bulk)  # Typed BlobRef opted in; no transfer performed.

    def test_explicit_model_summary_contains_descriptor_not_numerical_payload(self):
        # Metadata-only unit value, not a host grant or a simulated waveform.
        blob = BlobRef("test-descriptor-only", 48, BlobDigest("sha256", "0" * 64), "application/octet-stream")
        summary = WaveformRef(blob, 3).summary()
        self.assertLess(len(summary.encode()), 512)
        self.assertEqual(json.loads(summary.partition(": ")[2]), blob.to_wire())
        self.assertNotIn("locator", summary)
        self.assertNotIn("payload", summary)

    def test_measurement_schema_contains_only_scalars(self):
        schema = ext._tools["spice_measure"].output_schema
        self.assertEqual(set(schema["properties"]),
                         {"samples", "final_time_s", "final_voltage_v", "expected_voltage_v", "absolute_error_v"})
        self.assertEqual({value["type"] for value in schema["properties"].values()}, {"integer", "number"})


if __name__ == "__main__":
    unittest.main()
