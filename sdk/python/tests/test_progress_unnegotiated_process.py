"""Supplementary fixture check; production-host A07 acceptance lives in Rust."""
import unittest

from test_typed_process import ProcessHarness


class UnnegotiatedProgressProcessTests(ProcessHarness, unittest.TestCase):
    fixture_name = "progress_unnegotiated_fixture.py"
    tools = ["unnegotiated_progress", "healthy"]

    def test_A07_declines_offered_progress_and_remains_healthy(self):
        catalog = self.initialize(progress=True)
        self.assertEqual(sorted(catalog["protocol"]["features"]),
                         ["content_parts", "request_cancellation"])
        error = {"code": -32601, "message": "API 0.2 feature is not negotiated: request_progress"}
        self.assertEqual(self.call("unnegotiated_progress", {"value": 41})["error"], error)
        result = self.call("healthy", {"value": 41})["result"]
        self.assertEqual(result["structured_content"], 42)
        self.assertFalse(result["is_error"])
        self.assertEqual(result["content"], [{"type": "text", "text": "Healthy typed result."}])
        rows = self.events()
        self.assertEqual([r["event"] for r in rows], ["started", "attempt", "refused", "healthy"])
        self.assertTrue(rows[1]["host_offered_progress"])
        self.assertEqual(rows[1]["negotiated_features"], ["content_parts", "request_cancellation"])
        self.assertEqual(rows[2]["error"], error)
        self.assertEqual({r["pid"] for r in rows}, {self.process.pid})
        self.shutdown()
        self.reader.join(timeout=3)
        self.assertTrue(self.messages.empty(), "unexpected progress or duplicate terminal frame")


if __name__ == "__main__":
    unittest.main()
