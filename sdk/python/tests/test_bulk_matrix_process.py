"""Supplementary wire/file fault checks; these do not validate host storage."""
import hashlib
import json
import unittest

import test_bulk_process


class BulkMatrixProcessTests(test_bulk_process.BulkProcessTests):
    fixture_name = "bulk_matrix_fixture.py"
    tools = test_bulk_process.BulkProcessTests.tools + ["fault_publish", "measure_small"]

    def test_real_scratch_faults_and_sdk_abandonment(self):
        self.initialize()
        for mode in ("short", "declared_short", "long", "digest", "abandon", "cancel"):
            parent = self.send("tool/call", {"name": "fault_publish", "arguments": {"mode": mode}})
            ticket = self.receive()
            self.assertEqual(ticket["method"], "bulk/write")
            path = self.root / "fault-scratch"
            path.write_bytes(b"")
            self.reply(ticket, {"ticket": "fault-ticket", "profile": "local-file.v1", "locator": path.name, "capacity": 64})
            if mode == "cancel":
                # Observe the specific new barrier, never an earlier iteration.
                import time
                deadline = time.monotonic() + 3
                while not any(r["event"] == "commit_ready" and r["mode"] == mode for r in self.events()):
                    if time.monotonic() > deadline:
                        self.fail("cancel commit barrier missing")
                    time.sleep(0.002)
                self.send("$/cancelRequest", {"id": parent}, notification=True)
                # The helper explicitly abandons its ticket even on cancellation;
                # production host retirement remains authoritative if this races.
                release = self.receive()
                self.assertEqual(release["method"], "bulk/release")
                self.reply(release, {"released": True})
                result = self.receive()
                self.assertEqual(result["error"]["code"], -32800)
                continue
            if mode != "abandon":
                commit = self.receive()
                self.assertEqual(commit["method"], "bulk/commit")
                self.assertEqual(len(path.read_bytes()), {"short": 63, "declared_short": 64, "long": 65, "digest": 64}[mode])
                expected = "0" * 64 if mode == "digest" else hashlib.sha256(bytes(range(64))).hexdigest()
                self.assertEqual(commit["params"]["digest"]["value"], expected)
                self.assertEqual(commit["params"]["bytes"], 63 if mode == "declared_short" else 64)
                self.process.stdin.write(json.dumps({"jsonrpc": "2.0", "id": commit["id"], "error": {
                    "code": -32000, "message": "injected refusal", "data": {"code": "integrity_mismatch"}}}) + "\n")
                self.process.stdin.flush()
            release = self.receive()
            self.assertEqual(release["method"], "bulk/release")
            self.assertEqual(release["params"]["id"], "fault-ticket")
            self.reply(release, {"released": True})
            self.assertIn("error", self.receive())
        self.shutdown()


if __name__ == "__main__":
    unittest.main()
