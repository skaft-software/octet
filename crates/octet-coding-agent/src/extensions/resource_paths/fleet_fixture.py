"""Offline fleet binding peer with a filesystem-controlled initialize barrier."""
import json
import sys
import time
from pathlib import Path

root = Path(sys.argv[1])
name = sys.argv[2]
pending = None
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    params = request.get("params", {})
    if method == "initialize":
        deadline = time.monotonic() + 20
        while not (root / f"release-{name}").exists():
            assert time.monotonic() < deadline, "initialize barrier timed out"
            time.sleep(0.01)
        offer = params["protocol"]
        features = ["agent_sessions", "agent_model_selection_v1", "resource_paths_v1"]
        result = {"api_version": "0.4", "tools": [],
                  "commands": [{"name": name, "description": "offline fleet probe"}],
                  "protocol": {"version": "0.4",
                               "features": list(dict.fromkeys(offer["required_features"] + features)),
                               "limits": {"max_concurrent_requests": 1}}}
    elif method == "hook/run":
        result = {"disposition": {"action": "continue"}}
        if params["hook"] == "resources_discover":
            result = {"resource_paths": {}}
    elif method == "command/execute":
        operation, arguments = params["arguments"]
        arguments = json.loads(arguments)
        arguments["parent_request_id"] = request["id"]
        pending = request["id"]
        print(json.dumps({"jsonrpc": "2.0", "id": f"child-{pending}",
                          "method": operation, "params": arguments}), flush=True)
        continue
    elif method is None and request.get("id") == f"child-{pending}":
        result = {"text": json.dumps(request)}
        request = {"id": pending}
        pending = None
    elif method == "shutdown":
        result = {}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    if method == "shutdown":
        break
