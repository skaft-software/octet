#!/usr/bin/env python3
"""Deterministic loopback fake provider for the soak harness.

Serves the OpenAI Responses SSE surface octet's extension-provider route
expects (see crates/octet-ai/tests/fixtures/openai_responses/*.sse for the
accepted event vocabulary). Every response is a pure function of the request
body, so a run is reproducible from its seed.

No network: binds 127.0.0.1 only, refuses any other host header. No
credentials are read or sent. This is a fake, not a benchmark endpoint.
"""
import argparse
import hashlib
import json
import re
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

TOOL_MARKER = "SOAK_TOOL"
UI_MARKER = "SOAK_UI"
TOOL_NAME = "soak_probe"
UI_TOOL_NAME = "soak_ui"


def digest(text: str) -> str:
    return hashlib.sha256(text.encode()).hexdigest()[:12]


def last_user_text(body: dict) -> str:
    """The final user message text of an OpenAI Responses request body."""
    for item in reversed(body.get("input", [])):
        role = item.get("role")
        if role == "user" and isinstance(item.get("content"), str):
            return item["content"]
        if role == "user" and isinstance(item.get("content"), list):
            text = "".join(
                part.get("text", "")
                for part in item["content"]
                if isinstance(part, dict) and part.get("type") in ("input_text", "output_text", "text")
            )
            if text:
                return text
    return ""


def sse(event: str, data: dict) -> str:
    return f"event: {event}\ndata: {json.dumps(data, separators=(',', ':'))}\n\n"


def tool_stream(body: dict) -> bytes:
    text = last_user_text(body)
    ui = UI_MARKER in text
    match = re.search(r"turn[:=](\d+)", text)
    turn = int(match.group(1)) if match else 0
    name = UI_TOOL_NAME if ui else TOOL_NAME
    arguments = "{}" if ui else json.dumps({"turn": turn}, separators=(",", ":"))
    events = [
        sse("response.created", {"type": "response.created", "response": {"id": "resp_soak_tool"}}),
        sse("response.output_item.added", {"type": "response.output_item.added", "output_index": 0,
            "item": {"id": "fc_soak", "type": "function_call", "call_id": "call_soak", "name": name}}),
        sse("response.function_call_arguments.delta", {"type": "response.function_call_arguments.delta",
            "output_index": 0, "delta": arguments}),
        sse("response.function_call_arguments.done", {"type": "response.function_call_arguments.done", "output_index": 0}),
        sse("response.completed", {"type": "response.completed", "response": {
            "usage": {"input_tokens": 11, "output_tokens": 7, "total_tokens": 18}}}),
    ]
    return "".join(events).encode()


def text_stream(body: dict) -> bytes:
    text = last_user_text(body)
    reply = f"ack:{digest(text)}:{text[:48].replace(chr(10), ' ')}"
    events = [
        sse("response.created", {"type": "response.created", "response": {"id": "resp_soak_text"}}),
        sse("response.output_item.added", {"type": "response.output_item.added", "output_index": 0,
            "item": {"id": "msg_soak", "type": "message"}}),
        sse("response.output_text.delta", {"type": "response.output_text.delta", "output_index": 0, "delta": reply}),
        sse("response.output_text.done", {"type": "response.output_text.done", "output_index": 0}),
        sse("response.completed", {"type": "response.completed", "response": {
            "output": [{"id": "msg_soak", "type": "message", "status": "completed",
                        "content": [{"type": "output_text", "text": reply}]}],
            "usage": {"input_tokens": 9, "output_tokens": 5, "total_tokens": 14}}}),
    ]
    return "".join(events).encode()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *args):  # keep the soak log clean
        pass

    def do_GET(self):
        if self.path.startswith("/health"):
            self.send_response(200)
            self.send_header("Content-Type", "text/plain")
            self.send_header("Content-Length", "2")
            self.end_headers()
            self.wfile.write(b"ok")
            return
        self.send_response(404)
        self.end_headers()

    def chat_payload(self, body: dict) -> bytes:
        text = "".join(
            message.get("content") or ""
            for message in body.get("messages", [])
            if isinstance(message, dict) and message.get("role") == "user"
        )
        reply = f"ack:{digest(text)}:{text[:48]}"
        if body.get("stream"):
            events = [
                f'data: {json.dumps({"id": "chatcmpl-soak", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"role": "assistant"}, "finish_reason": None}]}, separators=(",", ":"))}\n\n',
                f'data: {json.dumps({"id": "chatcmpl-soak", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {"content": reply}, "finish_reason": None}]}, separators=(",", ":"))}\n\n',
                f'data: {json.dumps({"id": "chatcmpl-soak", "object": "chat.completion.chunk", "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}, separators=(",", ":"))}\n\n',
                "data: [DONE]\n\n",
            ]
            return "".join(events).encode()
        return json.dumps({
            "id": "chatcmpl-soak", "object": "chat.completion",
            "choices": [{"index": 0, "message": {"role": "assistant", "content": reply}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 9, "completion_tokens": 5, "total_tokens": 14},
        }).encode()

    def do_POST(self):
        # Fail closed: only the loopback fake is a legal destination, and the
        # endpoint paths are fixed by the provider routes.
        host = (self.headers.get("Host") or "").split(":")[0]
        if host not in ("127.0.0.1", "localhost"):
            self.send_response(403)
            self.end_headers()
            return
        path = self.path.rstrip("/")
        responses = path.endswith("/responses")
        chat = path.endswith("/chat/completions")
        if not (responses or chat):
            self.send_response(404)
            self.end_headers()
            return
        length = int(self.headers.get("Content-Length") or 0)
        body = json.loads(self.rfile.read(length) if length else b"{}")
        if chat:
            payload = self.chat_payload(body)
        else:
            payload = tool_stream(body) if (TOOL_MARKER in last_user_text(body) or UI_MARKER in last_user_text(body)) else text_stream(body)
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream" if not chat else ("text/event-stream" if body.get("stream") else "application/json"))
        self.send_header("Content-Length", str(len(payload)))
        self.end_headers()
        self.wfile.write(payload)


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", required=True, help="write the bound port here")
    args = parser.parse_args()
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    with open(args.port_file, "w") as handle:
        handle.write(str(server.server_address[1]))
    print(f"fake provider listening on 127.0.0.1:{server.server_address[1]}", flush=True)
    server.serve_forever()
    return 0


if __name__ == "__main__":
    sys.exit(main())
