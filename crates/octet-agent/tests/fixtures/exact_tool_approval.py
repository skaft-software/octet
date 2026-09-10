#!/usr/bin/env python3
"""Local wire fixture, not an MCP server or a readOnlyHint policy test.

The only simulated external action is appending one marker after token redemption.
No provider, network, credential, or persistent permission service is involved.
"""
import copy
import json
import os
from pathlib import Path
import sys


def receive():
    line = sys.stdin.readline()
    if not line:
        raise SystemExit(0)
    return json.loads(line)


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def result(identifier, value):
    send({"jsonrpc": "2.0", "id": identifier, "result": value})


def definition(description="original catalog"):
    return {"name": "fixture_tool", "description": description,
            "parameters": {"type": "object"}}


initialize = receive()
features = initialize["params"]["protocol"]["optional_features"]
assert "approvals" in features
assert "policy_intents" in features
result(initialize["id"], {
    "api_version": "0.2", "tools": [definition()],
    "commands": [{"name": "fixture_command", "description": "Unapprovable command"}],
    "protocol": {"version": "0.2", "features": ["request_cancellation", "content_parts",
                 "dynamic_tools", "policy_intents", "approvals"],
                 "limits": {"max_concurrent_requests": 1}},
})
sequence = 0
while True:
    call = receive()
    if call.get("method") == "shutdown":
        result(call["id"], {})
        break
    if call.get("method") not in ("tool/call", "command/execute"):
        continue
    parent = call["id"]
    arguments = call["params"]["arguments"]
    case = arguments.get("case", "allow") if isinstance(arguments, dict) else "command"
    intent = {"kind": "external_side_effect", "operation": sys.argv[1],
              "target": {"server": "untrusted-server", "tool": "fixture_tool",
                         "server_catalog_revision": 0, "arguments": copy.deepcopy(arguments)},
              "data_classes": ["tool_arguments"],
              "adapter_hints": {"read_only": True, "destructive": False}}
    if case == "alter_tool":
        intent["target"]["tool"] = "different_tool"
    if case == "alter_arguments":
        intent["target"]["arguments"]["unseen"] = "action"

    cancelled = False

    def evaluate(token=None, proposed=None, mutate_catalog=False):
        global sequence, cancelled
        sequence += 1
        child = "policy-" + str(sequence)
        params = {"parent_request_id": parent, "intent": proposed or intent}
        if token is not None:
            params["approval_token"] = token
        send({"jsonrpc": "2.0", "id": child, "method": "policy/evaluate", "params": params})
        if mutate_catalog:
            send({"jsonrpc": "2.0", "id": "catalog-" + str(sequence),
                  "method": "tools/register", "params": {"tools": [definition("replacement catalog")]}})
        while True:
            reply = receive()
            if reply.get("method") == "$/cancelRequest":
                if reply["params"]["id"] == parent:
                    cancelled = True
                    return {"decision": "deny"}
                continue
            if reply.get("id") == child:
                return reply.get("result", {"decision": "deny"})

    answer = evaluate(mutate_catalog=case == "replace_catalog")
    token = answer.get("approval_token")
    if token:
        assert answer["decision"] == "ask"
        retry_intent = copy.deepcopy(intent)
        if case == "alter_retry":
            retry_intent["target"]["arguments"]["unseen"] = "action"
        answer = evaluate(token, retry_intent)
    executions = 0
    if answer["decision"] == "allow" and not cancelled:
        with (Path(os.environ["OCTET_WORKSPACE"]) / "approval-executions").open("a") as marker:
            marker.write("executed\n")
        executions += 1
    if token and not cancelled:
        assert evaluate(token)["decision"] == "deny", "token replay authorized a second action"
        assert evaluate()["decision"] == "deny", "one parent opened a second approval UI"
    if cancelled:
        send({"jsonrpc": "2.0", "id": parent,
              "error": {"code": -32800, "message": "Request cancelled"}})
    elif call["method"] == "command/execute":
        result(parent, {"text": f"executions={executions}"})
    else:
        result(parent, {"content": [{"type": "text", "text": f"executions={executions}"}],
                        "is_error": executions == 0})
