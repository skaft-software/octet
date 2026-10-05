"""Handwritten local protocol peer. No Pi imports, factories or network."""
import json
import sys
from pathlib import Path
root = Path(sys.argv[1])
held_start = None
def send(i, result):
    print(json.dumps({"jsonrpc": "2.0", "id": i, "result": result}), flush=True)
for line in sys.stdin:
    req = json.loads(line)
    method = req["method"]
    params = req.get("params", {})
    if method == "initialize":
        with (root / "initialize.jsonl").open("a") as log:
            log.write(json.dumps(params) + "\n")
        offer = params["protocol"]
        assert "resource_paths_v1" in offer["optional_features"]
        send(req["id"], {"api_version": "0.4", "tools": [], "commands": [{"name": "release-start", "description": "release test barrier"}],
            "protocol": {"version": "0.4", "features": offer["required_features"] + ["resource_paths_v1"],
                "limits": {"max_concurrent_requests": 1}}})
    elif method == "hook/run":
        with (root / "consumer-calls.jsonl").open("a") as log:
            log.write(json.dumps(params) + "\n")
        if params["hook"] == "session_start" and (root / "hold-start").exists():
            held_start = req["id"]
            continue
        if params["hook"] == "resources_discover":
            send(req["id"], {"resource_paths": json.loads((root / "reply.json").read_text())})
        else:
            send(req["id"], {"disposition": {"action": "continue"}})
    elif method == "command/execute":
        assert params["name"] == "release-start"
        assert held_start is not None
        send(held_start, {"disposition": {"action": "continue"}})
        send(req["id"], {"text": "late-start-sent"})
    elif method == "shutdown":
        send(req["id"], {})
        break
    elif "id" in req:
        raise AssertionError(method)
