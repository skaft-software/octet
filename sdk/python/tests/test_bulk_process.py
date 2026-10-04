"""Raw stdio/file-transfer evidence, distinct from production host admission."""
import hashlib
import json
import unittest

from test_bulk import LIMITS
from test_typed_process import ProcessHarness


class BulkProcessTests(ProcessHarness, unittest.TestCase):
    fixture_name = "bulk_fixture.py"
    tools = ["publish", "measure", "invalid_output", "failed_parent", "cancel_publish", "cancel_read"]

    def initialize(self):
        self.send("initialize", {"api_version": "0.4", "contributes": {"tools": self.tools}, "protocol": {
            "version": "0.4", "required_features": ["request_cancellation", "content_parts"],
            "optional_features": ["bulk_objects_v1"], "limits": {"max_concurrent_requests": 1},
            "bulk_objects_v1": {"profile": "local-file.v1", "transfer_directory": str(self.root), "limits": LIMITS}}})
        return self.receive()["result"]

    def reply(self, request, value):
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": value}) + "\n")
        self.process.stdin.flush()

    def publish(self, name="publish", length=512 * 1024):
        parent = self.send("tool/call", {"name": name, "arguments": {"length": length}})
        ticket = self.receive()
        self.assertEqual(ticket["method"], "bulk/write")
        self.assertEqual(ticket["params"], {"parent_request_id": parent, "profile": "local-file.v1", "capacity": length, "media_type": "application/octet-stream"})
        path = self.root / "opaque-ticket"
        path.touch()
        self.reply(ticket, {"ticket": "t1", "profile": "local-file.v1", "locator": path.name, "capacity": length})
        commit = self.receive()
        self.assertEqual(commit["method"], "bulk/commit")
        self.assertEqual(set(commit["params"]), {"parent_request_id", "ticket", "bytes", "digest"})
        self.assertEqual(commit["params"]["parent_request_id"], parent)
        self.data = path.read_bytes()
        self.assertEqual(len(self.data), length)
        self.assertEqual(commit["params"]["digest"], {"algorithm": "sha256", "value": hashlib.sha256(self.data).hexdigest()})
        reference = {"$blob": "b1", "bytes": length, "digest": commit["params"]["digest"], "media_type": "application/octet-stream"}
        self.reply(commit, reference)
        return reference, self.receive()

    def read(self, reference, name="measure"):
        parent = self.send("tool/call", {"name": name, "arguments": {"data": reference}})
        request = self.receive()
        self.assertEqual(request["method"], "bulk/read")
        self.assertEqual(request["params"], {"parent_request_id": parent, "profile": "local-file.v1", "blob": reference})
        (self.root / "opaque-lease").write_bytes(self.data)
        self.reply(request, {"lease": "l1", "profile": "local-file.v1", "locator": "opaque-lease", "bytes": len(self.data)})
        return parent

    def release(self, parent):
        request = self.receive()
        self.assertEqual(request["method"], "bulk/release")
        self.assertEqual(request["params"], {"parent_request_id": parent, "id": "l1"})
        self.reply(request, {"released": True})

    def test_large_binary_roundtrip_has_only_bounded_descriptor_frames(self):
        catalog = self.initialize()
        self.assertIn("bulk_objects_v1", catalog["protocol"]["features"])
        self.assertNotIn("operation_descriptors_v1", catalog["protocol"]["features"])
        reference, result = self.publish()
        self.assertEqual(result["result"]["structured_content"], {"data": reference})
        self.assertLess(len(json.dumps(result)), 2048)
        parent = self.read(reference)
        self.release(parent)
        output = self.receive()["result"]
        self.assertEqual(output["structured_content"], {"bytes": len(self.data), "sha256": reference["digest"]["value"]})
        self.assertTrue(next(e for e in self.events() if e["event"] == "read_closed")["closed"])
        self.shutdown()

    def test_cancelled_read_releases_lease_and_closes_snapshot(self):
        self.initialize()
        reference, _ = self.publish(length=3)
        parent = self.read(reference, name="cancel_read")
        self.barrier("entered")
        self.send("$/cancelRequest", {"id": parent}, notification=True)
        self.release(parent)
        self.assertEqual(self.receive()["error"]["code"], -32800)
        self.assertTrue(next(e for e in self.events() if e["event"] == "cancel_read_closed")["closed"])
        self.shutdown()

    def test_committed_blob_is_not_smuggled_in_invalid_or_error_result(self):
        self.initialize()
        for name in ("invalid_output", "failed_parent"):
            _, result = self.publish(name, length=3)
            if name == "invalid_output":
                self.assertIn("error", result)
            else:
                self.assertTrue(result["result"]["is_error"])
                self.assertNotIn("structured_content", result["result"])
            self.assertNotIn("$blob", json.dumps(result))
        self.shutdown()


if __name__ == "__main__":
    unittest.main()
