#!/usr/bin/env python3
"""Unit coverage for the deterministic process benchmark fixture."""
import json
import unittest
from urllib.request import Request, urlopen

import session_bench as bench


class FixtureTests(unittest.TestCase):
    def test_tool_chain_uses_only_results_after_current_user(self):
        tools = [{"function": {"name": "bash"}}]
        messages = [{"role": "user", "content": "old"}, {"role": "tool"},
                    {"role": "user", "content": "BENCH_PROMPT_002"}, {"role": "tool"}]
        calls, _, results, index = bench.response_plan("tools", {"tools": tools, "messages": messages})
        self.assertEqual((results, index), (1, 2))
        self.assertEqual(calls, [("bash", {"command": "printf step-1"})])
        messages.extend({"role": "tool"} for _ in range(3))
        self.assertEqual(bench.response_plan("tools", {"tools": tools, "messages": messages})[0], [])

    def test_write_and_stream_payload_sizes(self):
        write, _, _, _ = bench.response_plan("write", {"tools": [{"function": {"name": "write"}}]})
        self.assertEqual(len(write[0][1]["content"]), 200_000)
        _, text, _, _ = bench.response_plan("stream", {})
        self.assertEqual(len(text), 60_000)

    def test_stream_output_must_match_exactly(self):
        expected = bench.STREAM_DATA.encode()
        bench.verify_stream_output(expected)
        for broken in (expected[:-1], expected + b"extra", b"x" + expected[1:]):
            with self.assertRaises(AssertionError):
                bench.verify_stream_output(broken)

    def test_exchange_validation_fails_closed(self):
        exchange = lambda n, call: {"tool_results": n, "tool_calls": call, "stream": True,
                                    "tool_outputs": [f"exit=0\nstep-{i}" for i in range(n)]}
        good = [exchange(n, int(n < 4)) for n in range(5)]
        bench.verify_exchanges("tools", good)
        for broken in (good[:-1], good[:1] + good[2:], [dict(x, stream=False) for x in good],
                       good[:-1] + [dict(good[-1], tool_outputs=[])],
                       good[:-1] + [dict(good[-1], tool_outputs=["exit=1\nstep-0"] * 4)]):
            with self.assertRaises(AssertionError):
                bench.verify_exchanges("tools", broken)
        with self.assertRaises(AssertionError):
            bench.verify_exchanges("startup", good)

    def test_streaming_provider_and_tool_calls(self):
        server = bench.Provider()
        try:
            server.reset("tools")
            body = {"stream": True, "messages": [{"role": "user", "content": "BENCH_PROMPT_001"}],
                    "tools": [{"type": "function", "function": {"name": "bash"}}]}
            req = Request(f"http://127.0.0.1:{server.server_port}/v1/chat/completions",
                          data=json.dumps(body).encode(), headers={"Content-Type": "application/json"})
            with urlopen(req, timeout=3) as response:
                wire = response.read()
            self.assertIn(b'"name": "bash"', wire)
            self.assertIn(b'"finish_reason": "tool_calls"', wire)
            self.assertTrue(wire.endswith(b"data: [DONE]\n\n"))
            self.assertEqual(server.snapshot()[0]["tool_results"], 0)
        finally:
            server.close()

    def test_slope_and_spread(self):
        points = [{"prompt": i, "private_bytes": 10 + 7 * i} for i in range(1, 6)]
        self.assertEqual(bench.slope(points), 7)
        self.assertIsNone(bench.slope(points[:1]))
        self.assertEqual(bench.summary([9, 1, 4]), {"median": 4, "min": 1, "max": 9})


if __name__ == "__main__":
    unittest.main()
