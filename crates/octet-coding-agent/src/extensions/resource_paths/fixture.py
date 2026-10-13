"""Real bounded wire peer, not CLM and not an adapter-parity oracle."""
import json
import sys
from pathlib import Path

root = Path(sys.argv[1])
# Opt-in regression mode used only by the genuine App constructor tests.
reverse_mode = (root / "reverse-requests").read_text() if (root / "reverse-requests").exists() else None
pending = {}

def send(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)

def finish_hook(request):
    params = request["params"]
    if params["hook"] == "resources_discover":
        if reverse_mode:
            paths = json.loads((root / "reply.json").read_text())
        else:
            assert params["context"]["resource_owner"]["session_id"] == "resource-path-owner"
            paths = {} if params["payload"]["reason"] == "reload" else {
                "skill_paths": [str(root / "assets" / "skills")],
                "prompt_paths": [str(root / "assets" / "prompts")],
                "theme_paths": [str(root / "assets" / "themes")],
            }
        send(request["id"], {"resource_paths": paths})
    else:
        send(request["id"], {"disposition": {"action": "continue"}})

for line in sys.stdin:
    request = json.loads(line)
    if "method" not in request:
        held = pending.pop(request["id"])
        with (root / "reverse-replies.jsonl").open("a") as log:
            log.write(json.dumps({"hook": held["params"]["hook"], "reply": request}) + "\n")
        if reverse_mode == "interactive":
            assert request["result"]["entry_id"], request
        else:
            assert "no foreground session" in request["error"]["message"], request
        # The hook cannot finish, and discovery cannot be successful, without
        # an actual durable append or an explicit headless refusal from host.
        finish_hook(held)
        continue
    method = request["method"]
    params = request.get("params", {})
    if method == "initialize":
        offer = params["protocol"]
        assert "resource_paths_v1" in offer["optional_features"]
        features = offer["required_features"] + ["resource_paths_v1"]
        if reverse_mode:
            assert "session_entries" in offer["optional_features"]
            features.append("session_entries")
        commands = [{"name": "release-start", "description": "fixture command"}] if reverse_mode else []
        send(request["id"], {"api_version": "0.4", "tools": [], "commands": commands,
            "protocol": {"version": "0.4", "features": features,
                "limits": {"max_concurrent_requests": 1}}})
    elif method == "hook/run":
        with (root / "calls.jsonl").open("a") as log:
            log.write(json.dumps(params) + "\n")
        if reverse_mode and params["hook"] in ("session_start", "resources_discover"):
            child_id = "reverse-" + params["hook"]
            pending[child_id] = request
            print(json.dumps({"jsonrpc": "2.0", "id": child_id, "method": "session/append_entry",
                "params": {"parent_request_id": request["id"], "entry_type": "resource-phase-proof",
                    "data": {"hook": params["hook"]}}}), flush=True)
        else:
            finish_hook(request)
    elif method == "shutdown":
        send(request["id"], {})
        break
    elif "id" in request:
        raise AssertionError(method)
