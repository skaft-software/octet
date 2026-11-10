#!/usr/bin/env python3
"""A valid API 0.4 reply envelope containing an invalid header patch."""
import json
import sys
from pathlib import Path


def receive():
    return json.loads(sys.stdin.readline())


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


init = receive()
protocol = init["params"]["protocol"]
assert "pipeline_hooks_v1" in protocol["optional_features"]
send({"jsonrpc": "2.0", "id": init["id"], "result": {
    "api_version": "0.4", "tools": [], "protocol": {
        "version": "0.4", "features": protocol["required_features"] + ["pipeline_hooks_v1"],
        "limits": {"max_concurrent_requests": 1}}}})

while True:
    request = receive()
    if request["method"] == "shutdown":
        send({"jsonrpc": "2.0", "id": request["id"], "result": {}})
        break
    assert request["method"] == "hook/run"
    case_file = Path("case.json")
    case = json.loads(case_file.read_text()) if case_file.exists() else {
        "hook": "before_provider_headers",
        # A header patch must contain strings, string arrays, or null. This
        # integer is valid JSON but cannot deserialize into ProviderPipelineReply.
        "reply": {"result": {"provider_headers": {
            "x-private": "PRIVATE_FIXTURE_SECRET", "x-invalid": 17}}},
    }
    assert request["params"]["hook"] == case["hook"]
    with Path("hook-called").open("a") as ledger:
        ledger.write(case["hook"] + "\n")
    send({"jsonrpc": "2.0", "id": request["id"], **case["reply"]})
