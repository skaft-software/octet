"""Fixture wire checks only; the Agent model-request oracle lives in Rust."""
import hashlib
import json
import unittest

from test_bulk import LIMITS, SECURE_TRANSPORT
from test_typed_process import ProcessHarness


@SECURE_TRANSPORT
class ProgressProcessTests(ProcessHarness, unittest.TestCase):
    fixture_name = "progress_fixture.py"
    tools = ["typed_progress_descriptor"]

    def initialize(self, *, progress=True):
        self.send("initialize", {"api_version": "0.4", "contributes": {"tools": self.tools}, "protocol": {
            "version": "0.4", "required_features": ["request_cancellation", "content_parts"],
            "optional_features": ["bulk_objects_v1"] + (["request_progress"] if progress else []),
            "limits": {"max_concurrent_requests": 1}, "bulk_objects_v1": {
                "profile": "local-file.v1", "transfer_directory": str(self.root), "limits": LIMITS}}})
        return self.receive()["result"]

    def reply(self, request, result):
        self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}) + "\n")
        self.process.stdin.flush()

    def progress(self, parent, sequence, marker):
        progress = self.receive()
        self.assertEqual(progress["method"], "$/progress")
        self.assertEqual(progress["params"]["request_id"], parent)
        self.assertEqual(progress["params"]["sequence"], sequence)
        self.assertEqual(progress["params"]["event"]["message"], marker)

    def test_A07_progress_descriptor_fixture_sequence_and_projection(self):
        self.initialize()
        parent = self.send("tool/call", {"name": self.tools[0], "arguments": {}})
        self.progress(parent, 1, "a07_ephemeral_step_one")
        ticket = self.receive()
        self.assertEqual(ticket["method"], "bulk/write")
        self.assertEqual(ticket["params"]["parent_request_id"], parent)
        payload = b"A07-private-bulk-payload" * 16384
        self.assertEqual(ticket["params"]["capacity"], len(payload))
        path = self.root / "opaque-write"
        path.touch()
        self.reply(ticket, {"ticket": "write", "profile": "local-file.v1", "locator": path.name, "capacity": len(payload)})
        commit = self.receive()
        self.assertEqual(commit["method"], "bulk/commit")
        self.assertEqual(path.read_bytes(), payload)
        digest = {"algorithm": "sha256", "value": hashlib.sha256(payload).hexdigest()}
        self.assertEqual(commit["params"]["digest"], digest)
        reference = {"$blob": "progress-blob", "bytes": len(payload), "digest": digest, "media_type": "application/octet-stream"}
        self.reply(commit, reference)
        self.progress(parent, 2, "a07_ephemeral_step_two")
        result = self.receive()["result"]
        self.assertEqual(result["structured_content"], {"data": reference})
        text = result["content"][0]["text"]
        self.assertEqual(json.loads(text.removeprefix("A07 published descriptor: ")), reference)
        self.assertNotIn("a07_ephemeral", json.dumps(result))
        self.assertNotIn("A07-private-bulk-payload", json.dumps(result))
        self.assertEqual([row["event"] for row in self.events()],
                         ["started", "entered", "progress_one", "progress_two", "returned"])
        self.assertEqual(len({row["pid"] for row in self.events()}), 1)
        self.shutdown()

    def test_A07_progress_descriptor_refuses_unnegotiated_progress_before_bulk(self):
        self.initialize(progress=False)
        result = self.call(self.tools[0], {})
        self.assertIn("error", result)
        self.assertEqual([row["event"] for row in self.events()], ["started", "entered"])
        self.shutdown()


if __name__ == "__main__":
    unittest.main()
