"""Local child-session command peer; no providers, Pi imports or network."""
import json
import sys

pending = None
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        offer = request["params"]["protocol"]
        result = {
            "api_version": "0.4", "tools": [],
            "commands": [{"name": "delegate", "description": "test child sessions"}],
            "protocol": {"version": "0.4",
                         "features": offer["required_features"] + ["agent_sessions"],
                         "limits": {"max_concurrent_requests": 1}},
        }
    elif method == "command/execute":
        params = request["params"]
        operation, arguments = params["arguments"]
        arguments = json.loads(arguments)
        arguments["parent_request_id"] = request["id"]
        pending = request["id"]
        print(json.dumps({"jsonrpc": "2.0", "id": f"child-call-{pending}",
                          "method": operation, "params": arguments}), flush=True)
        continue
    elif method == "shutdown":
        result = {}
    elif method is None and request.get("id") == f"child-call-{pending}":
        assert pending is not None
        result = {"text": json.dumps(request)}
        request = {"id": pending}
        pending = None
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    if method == "shutdown":
        break
