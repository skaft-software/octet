#!/usr/bin/env python3
"""Mock-provider agent-loop benchmark for octet and other coding agents.

Every agent under test talks to one loopback OpenAI-compatible Chat Completions
server that scripts the same turns for all of them. No paid model is called, the
model's latency is a fixed knob, and each scenario fixes the work the model asks
for. What differs between agents is startup, request construction, tool
scheduling and loop overhead, which is exactly what this measures.

Scenarios:

  answer          one request, no tools: startup plus one round trip
  parallel-shell  one response with three 1-second shell calls, then an answer;
                  ~1 s of tool time when the agent overlaps them, ~3 s when not
  wide-shell      one response with eight 1-second shell calls, then an answer;
                  shows whether overlap is capped below the batch size
  parallel-read   one response reading three workspace files, then an answer
  large-output    one shell call printing ~3.4 MB, then an answer: capture,
                  truncation and the larger follow-up request
  shell-chain     --chain-rounds rounds (default 5) of one quick shell call
                  each, then an answer; isolates per-round loop overhead

Per run the server records every request, so the report separates model
requests from auxiliary ones (for example title generation), counts tool calls
per response, and splits wall time into startup (spawn to first request),
continuations (response end to the next request: tool execution plus loop
overhead) and shutdown (last response to exit).

Agents run one at a time with a persistent per-agent HOME (so first-run caches
stay warm across repetitions; one discarded warm-up run per scenario fills
them) and a fresh workspace per run. Workspaces are created under --work-root,
which must not have a `.git` ancestor.

Example:

    python3 scripts/bench-agent-loop.py \\
      --octet ./target/release/octet \\
      --pi ~/agents/pi/node_modules/.bin/pi \\
      --opencode ~/agents/opencode/node_modules/.bin/opencode \\
      --repetitions 5 --output /tmp/agent-loop.json
"""

from __future__ import annotations

import argparse
import json
import os
import shutil
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from dataclasses import dataclass, field
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

MODEL_ID = "mock-1"
SHELL_TOOL = "bash"
READ_TOOL = "read"
PATH_ARGUMENTS = ("path", "filePath", "file_path")
SCENARIOS = ("answer", "parallel-shell", "wide-shell", "parallel-read", "large-output", "shell-chain")
PARALLEL_SHELL_COMMANDS = (
    "sleep 1; echo one",
    "sleep 1; echo two",
    "sleep 1; echo three",
)
WIDE_SHELL_COMMANDS = tuple(f"sleep 1; echo wide-{index}" for index in range(8))
LARGE_OUTPUT_COMMAND = "seq 1 500000"
READ_FILES = tuple(f"bench-{index}.txt" for index in range(3))
READ_FILE_LINES = 2000
FINAL_TEXT = "done"
PROMPT = "Run the benchmark steps, then reply done."


@dataclass
class Exchange:
    """One request/response pair seen by the mock provider."""

    received: float
    finished: float = 0.0
    auxiliary: bool = False
    tool_results: int = 0
    tool_calls: int = 0
    parallel_tool_calls: Any = None
    stream: bool = False


@dataclass
class Script:
    scenario: str = "answer"
    delay: float = 0.0
    chain_rounds: int = 5
    workspace: str = ""
    exchanges: list[Exchange] = field(default_factory=list)
    lock: threading.Lock = field(default_factory=threading.Lock)

    def reset(self, scenario: str, delay: float, chain_rounds: int, workspace: str) -> None:
        with self.lock:
            self.scenario = scenario
            self.delay = delay
            self.chain_rounds = chain_rounds
            self.workspace = workspace
            self.exchanges = []


def tool_arguments(tool: dict, primary: dict[str, Any]) -> dict:
    """Arguments for an agent's tool, filling its other required fields."""
    parameters = tool.get("parameters") or {}
    properties = parameters.get("properties") or {}
    arguments = dict(primary)
    for name in parameters.get("required") or []:
        if name in arguments:
            continue
        kind = (properties.get(name) or {}).get("type")
        if kind in ("integer", "number"):
            arguments[name] = 60_000
        elif kind == "boolean":
            arguments[name] = False
        else:
            arguments[name] = "benchmark step"
    return arguments


def read_arguments(tool: dict, path: str) -> dict:
    properties = (tool.get("parameters") or {}).get("properties") or {}
    name = next((name for name in PATH_ARGUMENTS if name in properties), "path")
    return tool_arguments(tool, {name: path})


def planned_calls(script: Script, tools: dict[str, dict], tool_results: int) -> list[tuple[str, dict]]:
    """The (tool, arguments) calls the scripted model makes for this request."""
    shell, read = tools.get(SHELL_TOOL), tools.get(READ_TOOL)
    if script.scenario == "parallel-shell" and tool_results == 0 and shell:
        return [(SHELL_TOOL, tool_arguments(shell, {"command": command})) for command in PARALLEL_SHELL_COMMANDS]
    if script.scenario == "wide-shell" and tool_results == 0 and shell:
        return [(SHELL_TOOL, tool_arguments(shell, {"command": command})) for command in WIDE_SHELL_COMMANDS]
    if script.scenario == "parallel-read" and tool_results == 0 and read:
        return [
            (READ_TOOL, read_arguments(read, os.path.join(script.workspace, name)))
            for name in READ_FILES
        ]
    if script.scenario == "large-output" and tool_results == 0 and shell:
        return [(SHELL_TOOL, tool_arguments(shell, {"command": LARGE_OUTPUT_COMMAND}))]
    if script.scenario == "shell-chain" and tool_results < script.chain_rounds and shell:
        return [(SHELL_TOOL, tool_arguments(shell, {"command": f"echo step-{tool_results}"}))]
    return []


class Handler(BaseHTTPRequestHandler):
    server_version = "agent-loop-mock/1"
    protocol_version = "HTTP/1.1"

    def log_message(self, *_: Any) -> None:
        return

    @property
    def script(self) -> Script:
        return self.server.script  # type: ignore[attr-defined]

    def _json(self, status: int, payload: Any) -> None:
        body = json.dumps(payload).encode()
        self.send_response(status)
        self.send_header("content-type", "application/json")
        self.send_header("content-length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_GET(self) -> None:  # noqa: N802 - http.server API
        if self.path.rstrip("/").endswith("/models"):
            self._json(200, {"object": "list", "data": [{"id": MODEL_ID, "object": "model"}]})
        else:
            self._json(404, {"error": {"message": "not found"}})

    def do_POST(self) -> None:  # noqa: N802 - http.server API
        received = time.perf_counter()
        length = int(self.headers.get("content-length") or 0)
        try:
            body = json.loads(self.rfile.read(length) or b"{}")
        except json.JSONDecodeError:
            self._json(400, {"error": {"message": "invalid json"}})
            return
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self._json(404, {"error": {"message": f"unsupported path {self.path}"}})
            return
        tools = {
            (entry.get("function") or {}).get("name"): entry.get("function") or {}
            for entry in body.get("tools") or []
            if isinstance(entry, dict)
        }
        main = SHELL_TOOL in tools
        messages = body.get("messages") or []
        tool_results = sum(1 for message in messages if message.get("role") == "tool")
        exchange = Exchange(
            received=received,
            auxiliary=not main,
            tool_results=tool_results,
            parallel_tool_calls=body.get("parallel_tool_calls"),
            stream=bool(body.get("stream")),
        )
        with self.script.lock:
            delay = self.script.delay
            index = len(self.script.exchanges)
            self.script.exchanges.append(exchange)
            planned = planned_calls(self.script, tools, tool_results) if main else []
        calls = [
            {
                "id": f"call_{index}_{position}",
                "type": "function",
                "function": {"name": name, "arguments": json.dumps(arguments)},
            }
            for position, (name, arguments) in enumerate(planned)
        ]
        exchange.tool_calls = len(calls)
        text = "" if calls else (FINAL_TEXT if main else "Benchmark")
        if delay:
            time.sleep(delay)
        usage = {"prompt_tokens": 100, "completion_tokens": 10, "total_tokens": 110}
        finish = "tool_calls" if calls else "stop"
        if exchange.stream:
            self._stream(body, calls, text, finish, usage)
        else:
            message: dict[str, Any] = {"role": "assistant", "content": text or None}
            if calls:
                message["tool_calls"] = calls
            self._json(
                200,
                {
                    "id": f"chatcmpl-{index}",
                    "object": "chat.completion",
                    "created": int(time.time()),
                    "model": MODEL_ID,
                    "choices": [{"index": 0, "message": message, "finish_reason": finish}],
                    "usage": usage,
                },
            )
        exchange.finished = time.perf_counter()

    def _stream(self, body: dict, calls: list, text: str, finish: str, usage: dict) -> None:
        self.send_response(200)
        self.send_header("content-type", "text/event-stream")
        self.send_header("cache-control", "no-cache")
        self.send_header("connection", "close")
        self.end_headers()
        base = {"id": "chatcmpl-mock", "object": "chat.completion.chunk", "created": int(time.time()), "model": MODEL_ID}

        def chunk(delta: dict, finish_reason: str | None = None) -> None:
            payload = dict(base, choices=[{"index": 0, "delta": delta, "finish_reason": finish_reason}])
            self.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())

        chunk({"role": "assistant", "content": ""})
        if text:
            chunk({"content": text})
        for position, call in enumerate(calls):
            chunk({"tool_calls": [dict(call, index=position)]})
        chunk({}, finish)
        if (body.get("stream_options") or {}).get("include_usage"):
            payload = dict(base, choices=[], usage=usage)
            self.wfile.write(f"data: {json.dumps(payload)}\n\n".encode())
        self.wfile.write(b"data: [DONE]\n\n")
        self.wfile.flush()
        self.close_connection = True


def start_server() -> tuple[ThreadingHTTPServer, Script]:
    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    server.daemon_threads = True
    script = Script()
    server.script = script  # type: ignore[attr-defined]
    threading.Thread(target=server.serve_forever, daemon=True).start()
    return server, script


# ── Agents ──────────────────────────────────────────────────────────────────


class Agent:
    name = ""

    def __init__(self, binary: str, home: Path, base_url: str):
        self.binary = binary
        self.home = home
        self.base_url = base_url

    def environment(self) -> dict[str, str]:
        env = {
            key: value
            for key, value in os.environ.items()
            if not key.startswith(("OCTET_", "PI_", "OPENCODE_", "OPENAI_", "ANTHROPIC_"))
        }
        env.update(
            HOME=str(self.home),
            XDG_CONFIG_HOME=str(self.home / ".config"),
            XDG_DATA_HOME=str(self.home / ".local/share"),
            XDG_CACHE_HOME=str(self.home / ".cache"),
            XDG_STATE_HOME=str(self.home / ".local/state"),
            NO_COLOR="1",
            CI="1",
        )
        return env

    def configure(self) -> None:
        raise NotImplementedError

    def argv(self, prompt: str) -> list[str]:
        raise NotImplementedError

    def version(self) -> str:
        try:
            result = subprocess.run(
                [self.binary, "--version"],
                env=self.environment(),
                capture_output=True,
                text=True,
                timeout=60,
            )
        except (OSError, subprocess.TimeoutExpired) as error:
            return f"unavailable: {error}"
        text = (result.stdout or result.stderr).strip()
        return text.splitlines()[-1] if text else ""


class Octet(Agent):
    name = "octet"

    def configure(self) -> None:
        registry = self.home / ".octet/credentials/custom.json"
        registry.parent.mkdir(parents=True, exist_ok=True)
        registry.write_text(
            json.dumps(
                {
                    "version": 1,
                    "providers": {
                        "mock": {
                            "label": "Mock",
                            "base_url": self.base_url + "/",
                            "auth": {"kind": "none"},
                            "auto_discover": False,
                            "models": [
                                {
                                    "api_name": MODEL_ID,
                                    "context_window": 128_000,
                                    "max_output_tokens": 8192,
                                    "tools": True,
                                    "parallel_tool_calls": True,
                                    "vision": False,
                                    "structured_output": False,
                                    "reasoning": False,
                                }
                            ],
                        }
                    },
                }
            )
        )
        registry.chmod(0o600)

    def argv(self, prompt: str) -> list[str]:
        return [self.binary, "--offline", "--model", f"custom/mock/{MODEL_ID}", "--print", prompt]


class Pi(Agent):
    name = "pi"

    def configure(self) -> None:
        models = self.home / ".pi/agent/models.json"
        models.parent.mkdir(parents=True, exist_ok=True)
        models.write_text(
            json.dumps(
                {
                    "providers": {
                        "mock": {
                            "baseUrl": self.base_url,
                            "api": "openai-completions",
                            "apiKey": "mock",
                            "models": [
                                {
                                    "id": MODEL_ID,
                                    "name": "Mock",
                                    "reasoning": False,
                                    "input": ["text"],
                                    "contextWindow": 128_000,
                                    "maxTokens": 8192,
                                }
                            ],
                        }
                    }
                }
            )
        )

    def argv(self, prompt: str) -> list[str]:
        return [
            self.binary,
            "--offline",
            "--provider",
            "mock",
            "--model",
            MODEL_ID,
            "--print",
            prompt,
        ]


class Opencode(Agent):
    name = "opencode"

    def config_path(self) -> Path:
        return self.home / "opencode-bench.json"

    def configure(self) -> None:
        self.config_path().parent.mkdir(parents=True, exist_ok=True)
        self.config_path().write_text(
            json.dumps(
                {
                    "$schema": "https://opencode.ai/config.json",
                    "provider": {
                        "mock": {
                            "npm": "@ai-sdk/openai-compatible",
                            "name": "Mock",
                            "options": {"baseURL": self.base_url, "apiKey": "mock"},
                            "models": {
                                MODEL_ID: {
                                    "name": "Mock",
                                    "tool_call": True,
                                    "limit": {"context": 128_000, "output": 8192},
                                }
                            },
                        }
                    },
                    "model": f"mock/{MODEL_ID}",
                    "small_model": f"mock/{MODEL_ID}",
                    "permission": {"bash": "allow", "edit": "allow", "read": "allow", "external_directory": "allow"},
                    "autoupdate": False,
                    "share": "disabled",
                }
            )
        )

    def environment(self) -> dict[str, str]:
        env = super().environment()
        env.update(
            OPENCODE_CONFIG=str(self.config_path()),
            OPENCODE_DISABLE_AUTOUPDATE="1",
            OPENCODE_DISABLE_MODELS_FETCH="1",
            OPENCODE_DISABLE_LSP_DOWNLOAD="1",
        )
        return env

    def argv(self, prompt: str) -> list[str]:
        return [self.binary, "run", "--model", f"mock/{MODEL_ID}", prompt]


AGENTS = {"octet": Octet, "pi": Pi, "opencode": Opencode}


# ── Runs ────────────────────────────────────────────────────────────────────


def git_ancestor(path: Path) -> Path | None:
    for candidate in (path, *path.parents):
        if (candidate / ".git").exists():
            return candidate
    return None


def run_once(
    agent: Agent,
    script: Script,
    scenario: str,
    delay: float,
    chain_rounds: int,
    work_root: Path,
    timeout: float,
) -> dict:
    workspace = Path(tempfile.mkdtemp(prefix=f"{agent.name}-{scenario}-", dir=work_root))
    for name in READ_FILES:
        (workspace / name).write_text(
            "".join(f"{name} line {line}: the quick brown fox jumps over the lazy dog\n" for line in range(READ_FILE_LINES))
        )
    script.reset(scenario, delay, chain_rounds, str(workspace))
    started = time.perf_counter()
    try:
        result = subprocess.run(
            agent.argv(PROMPT),
            cwd=workspace,
            env=agent.environment(),
            stdin=subprocess.DEVNULL,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
        status, stdout, stderr = result.returncode, result.stdout, result.stderr
    except subprocess.TimeoutExpired as error:
        status = "timeout"
        stdout = error.stdout.decode(errors="replace") if isinstance(error.stdout, bytes) else (error.stdout or "")
        stderr = error.stderr.decode(errors="replace") if isinstance(error.stderr, bytes) else (error.stderr or "")
    ended = time.perf_counter()
    shutil.rmtree(workspace, ignore_errors=True)
    with script.lock:
        exchanges = sorted(script.exchanges, key=lambda exchange: exchange.received)
    model = [exchange for exchange in exchanges if not exchange.auxiliary]
    continuations = [
        round(following.received - previous.finished, 4)
        for previous, following in zip(model, model[1:])
        if previous.finished
    ]
    completed = bool(model) and model[-1].tool_calls == 0 and FINAL_TEXT in stdout
    return {
        "status": status,
        "completed": completed,
        "wall_s": round(ended - started, 4),
        "startup_s": round(exchanges[0].received - started, 4) if exchanges else None,
        "shutdown_s": round(ended - model[-1].finished, 4) if model and model[-1].finished else None,
        "continuations_s": continuations,
        "model_requests": len(model),
        "auxiliary_requests": len(exchanges) - len(model),
        "tool_calls_per_response": [exchange.tool_calls for exchange in model],
        "parallel_tool_calls_flag": model[0].parallel_tool_calls if model else None,
        "stdout_tail": stdout[-400:],
        "stderr_tail": stderr[-800:],
    }


def median(values: list[float]) -> float | None:
    values = [value for value in values if value is not None]
    return round(statistics.median(values), 4) if values else None


def summarize(runs: list[dict]) -> dict:
    ok = [run for run in runs if run["completed"]]
    return {
        "runs": len(runs),
        "completed": len(ok),
        "wall_median_s": median([run["wall_s"] for run in ok]),
        "wall_min_s": min((run["wall_s"] for run in ok), default=None),
        "startup_median_s": median([run["startup_s"] for run in ok]),
        "continuation_total_median_s": median([sum(run["continuations_s"]) for run in ok]),
        "shutdown_median_s": median([run["shutdown_s"] for run in ok]),
        "model_requests": sorted({run["model_requests"] for run in ok}),
        "auxiliary_requests": sorted({run["auxiliary_requests"] for run in ok}),
    }


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    for name in AGENTS:
        parser.add_argument(f"--{name}", metavar="BINARY", help=f"{name} executable to benchmark")
    parser.add_argument("--scenario", action="append", choices=SCENARIOS, help="scenario to run (repeatable; default: all)")
    parser.add_argument("--repetitions", type=int, default=5)
    parser.add_argument("--model-delay", type=float, default=0.0, help="seconds the mock waits before each response")
    parser.add_argument("--chain-rounds", type=int, default=5, help="tool rounds in the shell-chain scenario")
    parser.add_argument("--timeout", type=float, default=120.0, help="per-run timeout in seconds")
    parser.add_argument("--work-root", type=Path, default=None, help="directory for HOMEs and workspaces (no .git ancestor)")
    parser.add_argument("--output", type=Path, help="write the full JSON report here")
    args = parser.parse_args()

    selected = [(name, getattr(args, name)) for name in AGENTS if getattr(args, name)]
    if not selected:
        parser.error("name at least one agent binary, for example --octet ./target/release/octet")
    work_root = (args.work_root or Path(tempfile.mkdtemp(prefix="agent-loop-"))).resolve()
    work_root.mkdir(parents=True, exist_ok=True)
    ancestor = git_ancestor(work_root)
    if ancestor is not None:
        parser.error(f"--work-root {work_root} is inside the git checkout {ancestor}; choose a directory outside it")
    scenarios = args.scenario or list(SCENARIOS)

    server, script = start_server()
    base_url = f"http://127.0.0.1:{server.server_address[1]}/v1"
    report: dict[str, Any] = {
        "model_delay_s": args.model_delay,
        "chain_rounds": args.chain_rounds,
        "repetitions": args.repetitions,
        "host": {"platform": sys.platform, "cpus": os.cpu_count()},
        "agents": {},
    }
    try:
        for name, binary in selected:
            home = work_root / f"home-{name}"
            home.mkdir(parents=True, exist_ok=True)
            agent = AGENTS[name](str(Path(binary).expanduser().resolve()), home, base_url)
            agent.configure()
            entry: dict[str, Any] = {"version": agent.version(), "scenarios": {}}
            report["agents"][name] = entry
            for scenario in scenarios:
                def measure() -> dict:
                    return run_once(
                        agent, script, scenario, args.model_delay, args.chain_rounds, work_root, args.timeout
                    )

                measure()
                runs = [measure() for _ in range(args.repetitions)]
                entry["scenarios"][scenario] = {"summary": summarize(runs), "runs": runs}
                summary = entry["scenarios"][scenario]["summary"]
                print(
                    f"{name:9} {scenario:15} wall {summary['wall_median_s']}s (min {summary['wall_min_s']}) "
                    f"startup {summary['startup_median_s']}s continuations {summary['continuation_total_median_s']}s "
                    f"shutdown {summary['shutdown_median_s']}s requests {summary['model_requests']}"
                    f"+aux {summary['auxiliary_requests']} completed {summary['completed']}/{summary['runs']}",
                    flush=True,
                )
                if summary["completed"] < summary["runs"]:
                    failed = next(run for run in runs if not run["completed"])
                    print(f"  first incomplete run: status={failed['status']} stderr={failed['stderr_tail']!r}", flush=True)
    finally:
        server.shutdown()
    if args.output:
        args.output.write_text(json.dumps(report, indent=2) + "\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
