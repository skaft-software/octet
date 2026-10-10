"""Raw process checks supplement, but do not replace, production host admission."""
import json
import unittest

from octet_extension.resources import RESOURCE_LIMITS
from test_typed_process import ProcessHarness


class ResourceProcessTests(ProcessHarness, unittest.TestCase):
    fixture_name = "resource_fixture.py"
    tools = ["create", "increment", "invalid_output", "failed_parent", "hold"]

    def initialize(self, *, resources=True):
        self.send("initialize", {"api_version": "0.4", "contributes": {"tools": self.tools}, "protocol": {
            "version": "0.4", "required_features": ["request_cancellation", "content_parts"],
            "optional_features": ["resource_refs_v1", "operation_descriptors_v1"] if resources else [],
            "limits": {"max_concurrent_requests": 1, "resource_refs_v1": RESOURCE_LIMITS}}})
        return self.receive()

    def export_call(self, name="create", arguments=None):
        parent = self.send("tool/call", {"name": name, "arguments": arguments or {}})
        register = self.receive()
        self.assertEqual(register["method"], "resource/register")
        self.assertEqual(register["params"], {"type": "example.Counter.v1", "parent_request_id": parent})
        reference = {"$resource": f"host-token-{parent}", "type": "example.Counter.v1"}
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": register["id"], "result": reference}) + "\n")
        self.process.stdin.flush()
        return reference, self.receive()

    def dispose(self, reference):
        self.send("resource/dispose", {"resources": [reference], "reason": "retired"})
        return self.receive()["result"]["results"][0]["status"]

    def test_resource_lifetime_generated_descriptor_and_rejected_reuse(self):
        catalog = self.initialize()["result"]
        self.assertEqual(catalog["protocol"]["limits"]["resource_refs_v1"], RESOURCE_LIMITS)
        increment = next(t for t in catalog["tools"] if t["name"] == "increment")
        self.assertEqual(increment["operation"]["receiver"], "/counter")
        self.assertEqual(increment["operation"]["resource_inputs"], [{"path": "/counter", "type": "example.Counter.v1", "access": "exclusive"}])
        reference, response = self.export_call(arguments={"n": 40})
        self.assertEqual(response["result"]["structured_content"], {"counter": reference})
        for expected in (41, 42):
            self.assertEqual(self.call("increment", {"counter": reference})["result"]["structured_content"], expected)
        self.assertEqual(self.dispose(reference), "completed")
        before = len(self.events())
        self.assertIn("error", self.call("increment", {"counter": reference}))
        self.assertEqual(len(self.events()), before)
        self.assertEqual([r for r in self.events() if r["event"] == "disposed"][0]["invalidated"], True)
        self.shutdown()

    def test_disposer_failure_keeps_reference_invalid(self):
        self.initialize()
        reference, _ = self.export_call(arguments={"fail_cleanup": True})
        self.assertEqual(self.dispose(reference), "failed")
        before = len(self.events())
        self.assertIn("error", self.call("increment", {"counter": reference}))
        self.assertEqual(len(self.events()), before)
        self.shutdown()

    def test_cancel_is_not_disposal_and_same_lane_waits_for_execution(self):
        self.initialize()
        reference, _ = self.export_call(arguments={"n": 3})
        parent = self.send("tool/call", {"name": "hold", "arguments": {"counter": reference}})
        self.barrier("entered")
        self.send("$/cancelRequest", {"id": parent}, notification=True)
        disposal = self.send("resource/dispose", {"resources": [reference], "reason": "retired"})
        self.assertNotIn("disposed", [r["event"] for r in self.events()])
        (self.root / "allow_terminal").touch()
        terminal = self.receive()
        self.assertEqual(terminal["id"], parent)
        self.assertEqual(terminal["error"]["code"], -32800)
        result = self.receive()
        self.assertEqual(result["id"], disposal)
        self.assertEqual(result["result"]["results"][0]["status"], "completed")
        events = [r["event"] for r in self.events()]
        self.assertLess(events.index("settled"), events.index("disposed"))
        self.shutdown()

    def test_failed_and_invalid_parent_outputs_wait_for_host_disposal(self):
        self.initialize()
        for name in ("failed_parent", "invalid_output"):
            reference, result = self.export_call(name)
            if name == "failed_parent":
                self.assertTrue(result["result"]["is_error"])
                self.assertNotIn("structured_content", result["result"])
            else:
                self.assertEqual(result["error"]["code"], -32603)
            self.assertEqual(self.dispose(reference), "completed")
        self.shutdown()

    def test_old_host_cannot_implicitly_upgrade_resource_tools(self):
        self.assertIn("error", self.initialize(resources=False))
        self.assertEqual([r["event"] for r in self.events()], ["started"])
        self.process.stdin.close()
        self.assertEqual(self.process.wait(timeout=3), 0)


if __name__ == "__main__":
    unittest.main()
