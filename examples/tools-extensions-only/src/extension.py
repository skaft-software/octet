#!/usr/bin/env python3
"""Offline protocol peer: no provider, adapter, credentials or network."""
import json
import sys

for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "initialize":
        protocol = request["params"]["protocol"]
        assert "agent_sessions" not in protocol["optional_features"]
        assert "session_owner_routes_v1" not in protocol["optional_features"]
        result = {
            "api_version": "0.4",
            "tools": [{"name": "echo", "description": "Echo", "parameters": {"type": "object"}}],
            "commands": [],
            "protocol": {"version": "0.4", "features": protocol["required_features"],
                         "limits": {"max_concurrent_requests": 1}},
        }
    elif method == "tool/call":
        result = {"content": [{"type": "text", "text": request["params"]["arguments"]["text"]}], "is_error": False}
    elif method == "shutdown":
        result = {}
    else:
        continue
    print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)
    if method == "shutdown":
        break
