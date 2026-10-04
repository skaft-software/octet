"""A typed native process, not a direct handler invocation (host-check is separate)."""
import json
from pathlib import Path
import tempfile
import unittest
from test_process import Peer, ROOT, offer

FIXTURE = json.loads((ROOT / "sdk/conformance/typed-values-v1.json").read_text())
BINARY = ROOT / "sdk/rust/target/debug/examples/typed-probe"


class TypedProcess(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.workspace = Path(self.directory.name)
        self.addCleanup(self.directory.cleanup)
        self.peer = Peer([str(BINARY), str(self.workspace)])
        self.addCleanup(self.peer.close)

    def initialize(self, progress=True):
        params = offer("typed")
        params["workspace"] = str(self.workspace)
        if not progress:
            params["protocol"]["optional_features"].remove("request_progress")
        self.peer.send("initialize", params, 1)
        return self.peer.receive()["result"]

    def record(self, mode):
        return {"name": mode, "enabled": True, "samples": [0.5]}

    def test_A01_typed_roundtrip_A04_optional_values(self):
        catalog = self.initialize()["tools"][0]
        self.assertEqual(catalog["parameters"], catalog["output_schema"])
        expected_schema = json.loads(json.dumps(FIXTURE["cases"][0]["schema"]))
        expected_schema["required"].sort()
        self.assertEqual(catalog["parameters"], expected_schema)
        for value in FIXTURE["cases"][0]["valid"]:
            result = self.peer.call(value, tool="typed")["result"]
            self.assertEqual(result["structured_content"], {"note": None, **value})
            self.assertEqual(result["content"], [{"type": "text", "text": "typed record"}])
            self.assertFalse(result["is_error"])
        self.peer.shutdown()

    def test_A02_invalid_input_zero_handler_entry(self):
        self.initialize()
        invalid = FIXTURE["cases"][0]["invalid"] + [
            {"name": "large", "enabled": True, "samples": [2**53]},
            {"name": "nonfinite", "enabled": True, "samples": [float("inf")]},
        ]
        for value in invalid:
            self.peer.send("tool/call", {"name": "typed", "arguments": value, "context": {}}, 2)
            result = self.peer.receive()
            self.assertIn(result["error"]["code"], (-32602, -32700))
            self.assertFalse((self.workspace / "calls.jsonl").exists())
        self.assertIn("result", self.peer.call(self.record("healthy"), tool="typed"))
        self.assertEqual(len((self.workspace / "calls.jsonl").read_text().splitlines()), 1)
        self.peer.shutdown()

    def test_A03_invalid_output(self):
        self.initialize()
        for mode in ("invalid-output", "missing-output", "nonfinite-output", "nonportable-output"):
            self.assertEqual(self.peer.call(self.record(mode), tool="typed")["error"]["code"], -32603)
        result = self.peer.call(self.record("error"), tool="typed")["result"]
        self.assertTrue(result["is_error"])
        self.assertNotIn("structured_content", result)
        self.assertIn("result", self.peer.call(self.record("healthy"), tool="typed"))
        self.peer.shutdown()

    def test_A05_diagnostics_and_malformed_diagnostic(self):
        self.initialize()
        result = self.peer.call(self.record("diagnostic"), tool="typed")["result"]
        self.assertTrue(result["is_error"])
        self.assertNotIn("structured_content", result)
        diagnostic = result["metadata"]["octet_diagnostics_v1"][0]
        self.assertEqual(diagnostic["code"], "solver.nonconvergent")
        self.assertEqual(diagnostic["fixes"][0]["edits"][0]["replacement"], "0")
        self.assertEqual(result["content"][0]["text"], "Domain failure\nerror[solver.nonconvergent]: Operating point did not converge.")
        self.assertEqual(self.peer.call(self.record("invalid-diagnostic"), tool="typed")["error"]["code"], -32603)
        self.assertIn("result", self.peer.call(self.record("healthy"), tool="typed"))
        self.peer.shutdown()

    def progress(self):
        frame = self.peer.frames.get(timeout=3)
        value = json.loads(frame)
        self.assertEqual(value["method"], "$/progress")
        return value["params"]

    def test_A07_progress_and_A06_cancellation(self):
        self.initialize()
        self.peer.send("tool/call", {"name": "typed", "arguments": self.record("progress"), "context": {}}, 9)
        progress = [self.progress(), self.progress()]
        self.assertEqual([p["sequence"] for p in progress], [1, 2])
        self.assertEqual([p["request_id"] for p in progress], [9, 9])
        self.assertEqual(progress[1]["event"], {"type": "status", "message": "typed finished", "current": 2, "total": 2, "unit": "steps"})
        self.assertEqual(self.peer.receive()["result"]["content"][0]["text"], "typed record")
        self.peer.send("tool/call", {"name": "typed", "arguments": self.record("cancel"), "context": {}}, 10)
        self.assertEqual(self.progress()["event"]["message"], "entered")
        self.peer.send("$/cancelRequest", {"id": 10})
        self.assertEqual(self.peer.receive()["error"]["code"], -32800)
        self.assertIn("result", self.peer.call(self.record("healthy"), tool="typed"))
        self.peer.shutdown()

    def test_A07_unnegotiated_progress_refused(self):
        selection = self.initialize(progress=False)
        self.assertNotIn("request_progress", selection["protocol"]["features"])
        self.assertEqual(self.peer.call(self.record("progress"), tool="typed")["error"]["code"], -32601)
        self.assertIn("result", self.peer.call(self.record("healthy"), tool="typed"))
        self.peer.shutdown()


if __name__ == "__main__":
    unittest.main(verbosity=2)
