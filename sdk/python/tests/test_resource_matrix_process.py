"""Light raw-peer tests verify SDK fixture behavior, not host admission."""
import json
import unittest

import test_resource_process


class ResourceMatrixProcessTests(test_resource_process.ResourceProcessTests):
    fixture_name = "resource_matrix_fixture.py"
    tools = test_resource_process.ResourceProcessTests.tools + [
        "create_held", "invalid_pair", "quota", "invalid_captured", "failed_captured",
    ]

    def grant(self, parent, index=0):
        request = self.receive()
        self.assertEqual(request["method"], "resource/register")
        self.assertEqual(request["params"]["parent_request_id"], parent)
        reference = {"$resource": f"host-{parent}-{index}", "type": "example.Counter.v1"}
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": reference}) + "\n")
        self.process.stdin.flush()
        return reference

    def test_two_invalid_outputs_capture_two_distinct_refs(self):
        self.initialize()
        parent = self.send("tool/call", {"name": "invalid_pair", "arguments": {}})
        refs = [self.grant(parent, i) for i in range(2)]
        result = self.receive()
        self.assertEqual(result["error"]["code"], -32603)
        self.assertEqual([r["reference"] for r in self.events() if r["event"] == "registered"], refs)
        for reference in refs:
            self.assertEqual(self.dispose(reference), "completed")
        self.shutdown()

    def test_cancel_held_creator_observes_cancellation_before_terminal(self):
        self.initialize()
        parent = self.send("tool/call", {"name": "create_held", "arguments": {}})
        reference = self.grant(parent)
        self.barrier("output_ready")
        self.send("$/cancelRequest", {"id": parent}, notification=True)
        self.barrier("cancel_observed")
        self.assertNotIn("disposed", [r["event"] for r in self.events()])
        (self.root / "allow_terminal").touch()
        result = self.receive()
        self.assertEqual(result["error"]["code"], -32800)
        self.assertEqual(self.dispose(reference), "completed")
        self.shutdown()


if __name__ == "__main__":
    unittest.main()
