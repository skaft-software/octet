"""Bulk SDK process tests: payloads are files, every JSON frame stays small."""
import hashlib
import json
from pathlib import Path
import tempfile
import unittest
from test_process import Peer, ROOT, offer
from test_resource_process import OWNER, FEATURES, LIMITS

BINARY = ROOT / "sdk/rust/target/debug/examples/bulk-probe"
BULK_LIMITS = {"object_bytes": 8 * 1024 * 1024, "owner_bytes": 16 * 1024 * 1024,
               "write_tickets_per_generation": 8, "read_leases_per_generation": 32, "blobs_per_owner": 256}


class BulkProcess(unittest.TestCase):
    def setUp(self):
        self.directory = tempfile.TemporaryDirectory()
        self.workspace = Path(self.directory.name)
        self.transfer = self.workspace / "transfer"
        self.transfer.mkdir()
        self.addCleanup(self.directory.cleanup)
        self.peer = Peer([str(BINARY), str(self.workspace)])
        self.addCleanup(self.peer.close)
        self.serial = 0

    def initialize(self, bulk=True):
        params = offer()
        params["contributes"]["tools"] = ["write", "read", "joint", "rewrite_saved"]
        params["workspace"] = str(self.workspace)
        params["protocol"]["optional_features"] += FEATURES
        params["protocol"]["limits"]["resource_refs_v1"] = LIMITS
        if bulk:
            params["protocol"]["optional_features"].append("bulk_objects_v1")
            params["protocol"]["bulk_objects_v1"] = {
                "profile": "local-file.v1", "transfer_directory": str(self.transfer), "limits": BULK_LIMITS}
        self.peer.send("initialize", params, 1)
        return self.peer.receive()

    def frame(self):
        frame = self.peer.frames.get(timeout=3)
        self.assertIsNotNone(frame, bytes(self.peer.diagnostics))
        self.assertLess(len(frame), 2048, "payload leaked into control JSON")
        return json.loads(frame)

    def reply(self, request, result):
        self.peer.raw(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}).encode() + b"\n")

    def call(self, name, args, request_id=10):
        self.peer.send("tool/call", {"name": name, "arguments": args, "context": {"resource_owner": OWNER}}, request_id)

    def descriptor(self, size, token="blob-id"):
        return {"$blob": token, "bytes": size, "media_type": "application/octet-stream",
                "digest": {"algorithm": "sha256", "value": hashlib.sha256(b"\xa5" * size).hexdigest()}}

    def write(self, size, mode=""):
        self.call("write", {"bytes": size, "mode": mode})
        request = self.frame()
        self.assertEqual(request["method"], "bulk/write")
        self.assertEqual(request["params"], {"parent_request_id": 10, "capacity": size, "media_type": "application/octet-stream", "profile": "local-file.v1"})
        self.serial += 1
        locator = f"opaque-transfer-{self.serial}"
        path = self.transfer / locator
        path.touch(mode=0o600)
        self.reply(request, {"ticket": f"ticket-{self.serial}", "profile": "local-file.v1", "locator": locator, "capacity": size})
        request = self.frame()
        if mode == "overflow":
            self.assertEqual(request["method"], "bulk/release")
            self.reply(request, {"released": True})
            self.assertLessEqual(path.stat().st_size, size)
            return None, self.peer.receive()
        self.assertEqual(request["method"], "bulk/commit")
        descriptor = self.descriptor(size, f"blob-{self.serial}")
        self.assertEqual(path.stat().st_size, size)
        self.assertEqual(hashlib.sha256(path.read_bytes()).hexdigest(), descriptor["digest"]["value"])
        self.assertEqual(request["params"], {"parent_request_id": 10, "ticket": f"ticket-{self.serial}", "bytes": size, "digest": descriptor["digest"]})
        path.unlink()  # Actual host consumes the ticket after its verified snapshot.
        self.reply(request, descriptor)
        return descriptor, self.peer.receive()

    def read(self, descriptor, locator="read-copy", payload=None):
        if payload is not None:
            (self.transfer / locator).write_bytes(payload)
        self.call("read", {"data": descriptor}, 20)
        request = self.frame()
        self.assertEqual(request["method"], "bulk/read")
        self.assertEqual(request["params"], {"parent_request_id": 20, "profile": "local-file.v1", "blob": descriptor})
        self.reply(request, {"lease": "lease-id", "profile": "local-file.v1", "locator": locator, "bytes": descriptor["bytes"]})
        release = self.frame()
        self.assertEqual(release["method"], "bulk/release")
        self.assertEqual(release["params"], {"parent_request_id": 20, "id": "lease-id"})
        self.reply(release, {"released": True})
        return self.peer.receive()

    def test_large_stream_typed_metadata_and_explicit_lease_release(self):
        catalog = self.initialize()["result"]
        self.assertIn("bulk_objects_v1", catalog["protocol"]["features"])
        write = next(tool for tool in catalog["tools"] if tool["name"] == "write")
        schema = write["output_schema"]["properties"]["data"]
        self.assertFalse(schema["additionalProperties"])
        self.assertEqual(schema["properties"]["digest"]["properties"]["algorithm"]["enum"], ["sha256"])
        size = 3 * 1024 * 1024 + 17
        descriptor, result = self.write(size)
        self.assertEqual(result["result"]["structured_content"], {"data": descriptor})
        self.assertNotIn(str(self.transfer), json.dumps(result))
        result = self.read(descriptor, payload=b"\xa5" * size)
        self.assertEqual(result["result"]["structured_content"], {"bytes": size})
        self.peer.shutdown()

    def test_zero_bytes_and_bounded_write_overflow(self):
        self.initialize()
        descriptor, result = self.write(0)
        self.assertIn("result", result)
        self.assertEqual(self.read(descriptor, payload=b"")["result"]["structured_content"], {"bytes": 0})
        _, result = self.write(4, "overflow")
        self.assertEqual(result["error"]["code"], -32000)
        self.peer.shutdown()

    def test_integrity_failure_releases_lease_and_never_returns_success(self):
        self.initialize()
        descriptor = self.descriptor(4)
        descriptor["digest"]["value"] = "0" * 64
        self.assertEqual(self.read(descriptor, payload=b"\xa5" * 4)["error"]["code"], -32602)
        self.assertEqual(self.read(self.descriptor(4), payload=b"\xa5" * 3)["error"]["code"], -32602)
        self.peer.shutdown()

    def test_locator_traversal_symlinks_directories_and_hardlinks_refused(self):
        self.initialize()
        outside = self.workspace / "outside"
        outside.write_bytes(b"\xa5")
        (self.transfer / "symlink").symlink_to(outside)
        (self.transfer / "directory").mkdir()
        (self.transfer / "hardlink").hardlink_to(outside)
        for locator in ["../outside", str(outside), "symlink", "directory", "hardlink", "back\\slash"]:
            with self.subTest(locator=locator):
                self.assertIn("error", self.read(self.descriptor(1), locator=locator))
        self.assertEqual(outside.read_bytes(), b"\xa5")
        self.peer.shutdown()

    def test_invalid_blob_codec_zero_handler_entry_and_unoffered_feature(self):
        self.initialize()
        descriptor = self.descriptor(1)
        descriptor["digest"]["value"] = "z" * 64
        self.call("read", {"data": descriptor})
        self.assertEqual(self.peer.receive()["error"]["code"], -32602)
        self.assertFalse((self.workspace / "bulk.jsonl").exists())
        self.peer.shutdown()
        with Peer([str(BINARY), str(self.workspace)]) as peer:
            self.peer = peer
            self.assertEqual(self.initialize(False)["error"]["code"], -32000)

    def test_host_can_cancel_one_correlated_reverse_request(self):
        self.initialize()
        self.call("write", {"bytes": 16}, 31)
        request = self.frame()
        self.peer.send("$/cancelRequest", {"id": request["id"]})
        self.assertEqual(self.peer.receive()["error"]["code"], -32800)
        self.peer.shutdown()

    def test_cancelled_reverse_bulk_wait_uses_existing_cancellation(self):
        self.initialize()
        self.call("write", {"bytes": 16}, 30)
        request = self.frame()
        self.assertEqual(request["method"], "bulk/write")
        self.peer.send("$/cancelRequest", {"id": 30})
        cancellation = self.frame()
        self.assertEqual(cancellation["method"], "$/cancelRequest")
        self.assertEqual(cancellation["params"]["id"], request["id"])
        self.assertEqual(self.peer.receive()["error"]["code"], -32800)
        self.peer.shutdown()


if __name__ == "__main__":
    unittest.main(verbosity=2)
