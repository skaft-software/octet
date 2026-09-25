"""Stdio fixture recording actual calls, including mutations with no response."""

from __future__ import annotations

import json
from pathlib import Path
import sys


ANNOTATIONS = {
    "read_only": {"readOnlyHint": True},
    "unknown": {"readOnlyHint": False},
    "missing": None,
    "mutation": {"destructiveHint": True},
    "contradictory": {"readOnlyHint": True, "destructiveHint": True},
}


def reply(request_id, result):
    print(json.dumps({"jsonrpc": "2.0", "id": request_id, "result": result}), flush=True)


def main():
    journal = Path(sys.argv[1])
    for line in sys.stdin:
        message = json.loads(line)
        method = message.get("method")
        request_id = message.get("id")
        if method == "initialize":
            reply(request_id, {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {"listChanged": False}},
                "serverInfo": {"name": "policy-fixture", "version": "1"},
            })
        elif method == "tools/list":
            tools = []
            for name, annotations in ANNOTATIONS.items():
                tool = {
                    "name": name,
                    "inputSchema": {
                        "type": "object",
                        "properties": {"hold": {"type": "boolean"}},
                        "additionalProperties": False,
                    },
                }
                if annotations is not None:
                    tool["annotations"] = annotations
                tools.append(tool)
            reply(request_id, {"tools": tools})
        elif method in {"tools/call", "notifications/cancelled"}:
            # Persist before replying (or withholding a reply): a real upstream
            # mutation may already have happened when timeout/cancel is observed.
            with journal.open("a", encoding="utf-8") as stream:
                stream.write(json.dumps(message) + "\n")
            if method == "tools/call" and not message["params"]["arguments"].get("hold"):
                reply(request_id, {
                    "content": [{"type": "text", "text": "executed"}],
                    "isError": False,
                })


if __name__ == "__main__":
    main()
