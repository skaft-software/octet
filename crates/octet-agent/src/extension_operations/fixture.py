#!/usr/bin/env python3
"""Handwritten real-process peer: D tests the host, not SDK syntax/parity."""
import json
import os
import socket
import sys

os.environ["HOME"] = os.getcwd()


def send(value):
    print(json.dumps(value, separators=(",", ":")), flush=True)


def reply(request, result):
    send({"jsonrpc": "2.0", "id": request["id"], "result": result})


def receive():
    request = json.loads(sys.stdin.readline())
    # Independent cleanup may interleave a parent-correlated reverse reply.
    while request.get("method") == "resource/dispose":
        dispose(request)
        request = json.loads(sys.stdin.readline())
    return request


def dispose(request):
    results = []
    for resource in request["params"]["resources"]:
        objects.pop(resource["$resource"], None)
        log("dispose", resource=resource)
        results.append({"resource": resource, "status": "completed"})
    reply(request, {"results": results})


def log(kind, **fields):
    with open("calls.jsonl", "a", encoding="utf-8") as stream:
        stream.write(json.dumps({"pid": os.getpid(), "kind": kind, **fields}) + "\n")


def record(properties, required=None):
    return {"type": "object", "properties": properties,
            "required": list(properties) if required is None else required,
            "additionalProperties": False}


def ref_schema(nominal):
    return record({"$resource": {"type": "string"},
                   "type": {"type": "string", "const": nominal}})


def operation(index, revision=0):
    nominal = "Circuit" if index < 20 else "Circuit2"
    properties = {"circuit": ref_schema(nominal)}
    inputs = [{"path": "/circuit", "type": nominal, "access": "exclusive"}]
    if index == 0:
        properties["secondary"] = ref_schema(nominal)
        inputs.append({"path": "/secondary", "type": nominal, "access": "exclusive"})
    required = list(properties)
    if revision:
        properties["revision_marker"] = {"type": "integer", "const": revision}
    descriptor = {"id": f"circuit.op{index:03}", "resource_inputs": inputs,
                  "resource_outputs": []}
    if index != 1:
        descriptor["receiver"] = "/circuit"
    return {"name": f"op_{index:03}", "description": f"Operation revision {revision}",
            "parameters": record(properties, required), "operation": descriptor}


create = {"name": "create", "description": "Create native circuit", "parameters": record({}),
          "output_schema": record({"circuit": ref_schema("Circuit")}),
          "operation": {"id": "circuit.create", "resource_inputs": [],
                        "resource_outputs": [{"path": "/circuit", "type": "Circuit"}]}}
plain = {"name": "plain", "description": "Unannotated ordinary tool", "parameters": record({})}
replace = {"name": "replace_catalog", "description": "Publish new exact schema", "parameters": record({})}
init = receive()
protocol = init["params"]["protocol"]
features = ["resource_refs_v1", "operation_descriptors_v1", "dynamic_tools"]
assert all(feature in protocol["optional_features"] for feature in features)
reply(init, {"api_version": "0.4", "commands": [],
             "tools": [create, plain, replace] + [operation(i) for i in reversed(range(100))],
             "protocol": {"version": "0.4", "features": protocol["required_features"] + features,
                          "limits": {"max_concurrent_requests": 2}}})


class Counter:
    def __init__(self):
        self.count = 0


objects = {}
revision = 0
handlers = {0: lambda count: f"handler revision 0; count {count}"}
for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if method == "shutdown":
        reply(request, {})
        break
    if method == "resource/dispose":
        dispose(request)
        continue
    if method != "tool/call":
        continue
    params = request["params"]
    name = params["name"]
    log("call", name=name, revision=params.get("catalog_revision"), args=params["arguments"])
    structured = None
    if name == "create":
        send({"jsonrpc": "2.0", "id": f"register-{request['id']}", "method": "resource/register",
              "params": {"parent_request_id": request["id"], "type": "Circuit"}})
        resource = receive()["result"]
        objects[resource["$resource"]] = Counter()
        structured = {"circuit": resource}
        text = "Created native circuit"
    elif name == "replace_catalog":
        revision += 1
        handlers[revision] = lambda count, version=revision: f"handler revision {version}; count {count}"
        if os.path.exists("remove-on-replace"):
            send({"jsonrpc": "2.0", "id": f"replace-{revision}", "method": "tools/unregister",
                  "params": {"names": ["op_000"]}})
        else:
            send({"jsonrpc": "2.0", "id": f"replace-{revision}", "method": "tools/register",
                  "params": {"tools": [operation(i, revision) for i in range(100)]}})
        response = receive()
        assert response["result"]["revision"] == revision, response
        text = "Replaced catalog"
    elif name.startswith("op_"):
        if name in ("op_001", "op_002", "op_019") and os.path.exists("barrier.json"):
            # Socket admission/acknowledgement is a deterministic execution
            # barrier, never a timing sleep. Host release must see the live pin.
            with open("barrier.json", encoding="utf-8") as stream:
                port = json.load(stream)
            with socket.create_connection(("127.0.0.1", port), timeout=5) as barrier:
                barrier.sendall(b"entered")
                assert barrier.recv(1) == b"x"
        resource = params["arguments"]["circuit"]
        counter = objects[resource["$resource"]]
        # Two otherwise identical inspection operations differ only in receiver
        # presentation. They allow an exact same-arguments/result comparison.
        if name not in ("op_001", "op_002"):
            counter.count += 1
        # This fixture retains both handler revisions, selected by the normal
        # tool/call catalog_revision; it never reroutes an old call to new code.
        text = handlers[params["catalog_revision"]](counter.count)
    else:
        text = "plain unchanged"
    result = {"content": [{"type": "text", "text": text}], "is_error": False, "metadata": {}}
    if structured is not None:
        result["structured_content"] = structured
    reply(request, result)
