#!/usr/bin/env python3
"""Loopback OpenAI Chat Completions mock for head-to-head agent benchmarks.

Every POST to /v1/chat/completions streams the same short assistant reply as
SSE, so the time an agent spends on a round trip is the agent's own work. GET
/v1/models lists one model. Request sizes are appended to --log as JSON lines,
so wire bytes per turn can be compared too.

Usage: mock_openai.py --port-file PATH [--log PATH] [--model NAME] [--reply-bytes N]
"""

import argparse
import json
import socketserver
import threading
from http.server import BaseHTTPRequestHandler, HTTPServer


def stream(model: str, reply_bytes: int) -> bytes:
    # A long reply is several prose lines, the way a model's answer arrives.
    line = "The quick brown fox jumps over the lazy dog while the agent reads code. "
    text = "ok" if reply_bytes <= 2 else (line * (reply_bytes // len(line) + 1))[:reply_bytes]
    chunks = [
        {"id": "c", "object": "chat.completion.chunk", "model": model,
         "choices": [{"index": 0, "delta": {"role": "assistant", "content": text}, "finish_reason": None}]},
        {"id": "c", "object": "chat.completion.chunk", "model": model,
         "choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
        {"id": "c", "object": "chat.completion.chunk", "model": model, "choices": [],
         "usage": {"prompt_tokens": 12, "completion_tokens": 1, "total_tokens": 13}},
    ]
    body = "".join(f"data: {json.dumps(chunk, separators=(',', ':'))}\n\n" for chunk in chunks)
    return (body + "data: [DONE]\n\n").encode()


class Server(socketserver.ThreadingMixIn, HTTPServer):
    daemon_threads = True
    allow_reuse_address = True


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--port-file", required=True)
    parser.add_argument("--log")
    parser.add_argument("--model", default="bench-model")
    parser.add_argument("--reply-bytes", type=int, default=2)
    args = parser.parse_args()
    lock = threading.Lock()
    log = open(args.log, "a", buffering=1) if args.log else None
    reply = stream(args.model, args.reply_bytes)
    models = json.dumps({"object": "list", "data": [{"id": args.model, "object": "model"}]}).encode()

    class Handler(BaseHTTPRequestHandler):
        protocol_version = "HTTP/1.1"

        def log_message(self, *_):
            pass

        def _send(self, status, body, content_type):
            self.send_response(status)
            self.send_header("content-type", content_type)
            self.send_header("content-length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def do_GET(self):
            if self.path.rstrip("/").endswith("/models"):
                self._send(200, models, "application/json")
            else:
                self._send(404, b"{}", "application/json")

        def do_POST(self):
            length = int(self.headers.get("content-length", "0"))
            body = self.rfile.read(length)
            if log:
                with lock:
                    log.write(json.dumps({"path": self.path, "bytes": len(body)}) + "\n")
            if not self.path.rstrip("/").endswith("/chat/completions"):
                self._send(404, b"{}", "application/json")
                return
            self._send(200, reply, "text/event-stream")

    server = Server(("127.0.0.1", 0), Handler)
    with open(args.port_file, "w") as file:
        file.write(str(server.server_address[1]))
    server.serve_forever()


if __name__ == "__main__":
    main()
