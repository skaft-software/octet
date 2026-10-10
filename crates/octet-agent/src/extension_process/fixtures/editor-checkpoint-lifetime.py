#!/usr/bin/env python3
"""Real wire barriers for native editor checkpoint lifetime tests; no sleeps."""
import json
import sys


def receive():
    line = sys.stdin.readline()
    assert line, "unexpected host EOF"
    return json.loads(line)


def send(**value):
    print(json.dumps(dict(jsonrpc="2.0", **value)), flush=True)


def notice(message):
    send(method="notification", params=dict(message=message))


def main():
    init = receive()
    send(id=init["id"], result=dict(
        api_version="0.4", tools=[], commands=[dict(name="probe", description="probe")],
        protocol=dict(version="0.4", features=init["params"]["protocol"]["required_features"]
                      + ["composer", "remote_ui"], limits=dict(max_concurrent_requests=2))))
    call = receive()
    if call.get("method") == "shutdown":
        send(id=call["id"], result={})
        return
    assert call["method"] == "command/execute", call
    mode = call["params"]["arguments"][0]
    parent = call["id"]
    owner = call["params"]["context"]["resource_owner"]
    send(id="open", method="ui/open", params=dict(
        parent_request_id=parent, resource_owner=owner, surface_id="editor",
        title="Editor", placement="editor"))
    opened = receive()
    assert opened["id"] == "open", opened
    mount = opened["result"]["editor_mount_id"]
    params = dict(parent_request_id=parent, resource_owner=owner, text="complete draft 🦀")
    if mode != "plain":
        params["editor_checkpoint"] = dict(surface_id="editor", mount_id=mount,
                                            input_revision=0, checkpoint_revision=1)
    send(id="checkpoint", method="composer/set", params=params)
    # The host releases this ordinary child only after observing checkpoint admission.
    send(id="barrier", method="composer/get", params=dict(
        parent_request_id=parent, resource_owner=owner))
    barrier = receive()
    assert barrier["id"] == "barrier" and "result" in barrier, barrier
    early_checkpoint = None
    if mode in ("cancel", "cancel-first", "commit-first"):
        while True:
            cancelled = receive()
            if cancelled.get("id") == "checkpoint":
                assert mode == "commit-first" and early_checkpoint is None, cancelled
                early_checkpoint = cancelled
                continue
            assert cancelled["method"] == "$/cancelRequest", cancelled
            assert cancelled["params"]["id"] == parent, cancelled
            break
        send(id=parent, error=dict(code=-32800, message="cancelled"))
        notice("parent-cancelled")
    elif mode == "cancelled-terminal":
        send(id=parent, error=dict(code=-32800, message="cancelled"))
    else:
        if mode == "child-cancel":
            send(method="$/cancelRequest", params=dict(id="checkpoint"))
        send(id=parent, result=dict(text="settled"))
    answered = False
    while True:
        message = early_checkpoint if early_checkpoint is not None else receive()
        early_checkpoint = None
        method = message.get("method")
        if method == "shutdown":
            send(id=message["id"], result={})
            return
        if method in ("ui/closed", "context/updated"):
            continue
        if mode == "plain" and method == "$/cancelRequest":
            assert message["params"]["id"] == "checkpoint", message
            assert message["params"]["reason"] == "parent settled", message
            assert not answered, message
            answered = True
            notice("plain-cancelled")
            continue
        assert method is None and message["id"] == "checkpoint", message
        assert not answered, "duplicate checkpoint response"
        answered = True
        if mode in ("success", "writer-full", "commit-first"):
            assert message["result"] == dict(input_revision=0, checkpoint_revision=1), message
            notice("checkpoint-ack")
        else:
            assert message["error"]["message"].startswith("not_foreground_owner"), message
            notice("checkpoint-refused")


main()
