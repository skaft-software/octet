#!/usr/bin/env python3
"""Deterministic API 0.4 stdio smoke test; no provider or network access."""

import json
import os
from pathlib import Path
import subprocess
import sys
import unittest


DIRECTORY = Path(__file__).resolve().parent
SDK_PATH = DIRECTORY.parents[2] / "sdk" / "python"


def request(request_id, method, params):
    return {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}


def hook(request_id, warm_cost):
    return request(request_id, "hook/run", {
        "hook": "cache_warming_decision",
        "payload": {"model": "cache-model", "decision": {
            "phase": "idle", "warm_cost_microdollars": warm_cost,
            "miss_cost_microdollars": 100_000, "continuation_probability": 0.15,
            "expected_savings_microdollars": 15_000 - warm_cost,
            "economics_available": True, "action": "warm",
        }},
        "context": {"resource_owner": {
            "session_id": "owner-one", "extension_instance_id": "instance-one",
            "process_generation": 1,
        }},
    })


class CacheWarmingExampleTests(unittest.TestCase):
    def test_stdio_negotiation_typed_stop_no_opinion_and_shutdown(self):
        messages = [request(1, "initialize", {
            "api_version": "0.4", "contributes": {
                "tools": [], "commands": [], "hooks": ["cache_warming_decision"],
            },
            "protocol": {
                "version": "0.4", "required_features": ["request_cancellation", "content_parts"],
                "optional_features": ["cache_warming_decision"],
                "limits": {"max_concurrent_requests": 1},
            },
        }), hook(2, 5_001), hook(3, 5_000), request(4, "shutdown", {})]
        env = dict(os.environ)
        env["PYTHONPATH"] = str(SDK_PATH)
        result = subprocess.run(
            [sys.executable, str(DIRECTORY / "extension.py")],
            input="".join(json.dumps(message) + "\n" for message in messages),
            text=True, capture_output=True, env=env, timeout=10, check=False,
        )
        self.assertEqual(result.returncode, 0, result.stderr)
        responses = {message["id"]: message for message in map(json.loads, result.stdout.splitlines())}
        self.assertEqual(set(responses), {1, 2, 3, 4})
        self.assertEqual(responses[1]["result"]["api_version"], "0.4")
        self.assertIn("cache_warming_decision", responses[1]["result"]["protocol"]["features"])
        self.assertEqual(responses[2]["result"]["cache_warming_decision"], "stop")
        self.assertIsNone(responses[3]["result"]["cache_warming_decision"])
        self.assertEqual(responses[2]["result"]["context"], [])
        self.assertIn("result", responses[4])


if __name__ == "__main__":
    unittest.main()
