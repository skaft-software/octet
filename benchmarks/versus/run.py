#!/usr/bin/env python3
"""Head-to-head latency of octet and fx (vercel-labs/fx) on the same machine.

Every scenario runs both agents with hyperfine (no shell, same runs and
warmups), each from its own private HOME with a cleared environment, against
the same loopback OpenAI Chat Completions mock (mock_openai.py). Fixtures are
built by driving each agent itself, so each one reads its own native session
format:

  startup        bench boundary: OCTET_BENCH=1 octet / FX_BENCH=1 fx
  version        --version
  help           octet --help / fx help
  doctor         octet doctor / fx doctor --json
  models         octet --list-models / fx models --json        (model picking)
  sessions-N     octet sessions list / fx sessions --json     (N saved sessions)
  ask            one print-mode round trip, no session saved   (API round trip)
  ask-saved      one print-mode round trip into a new saved session
  resume-long    one round trip resuming a T-turn session      (long session resume)

Usage:
  benchmarks/versus/run.py --octet target/release/octet --fx /path/to/fx \\
      [--runs 30] [--warmup 3] [--sessions 500] [--turns 200] [--only a,b]

Requires hyperfine and python3. Prints a table of medians and octet/fx ratios
and writes raw hyperfine JSON under --results (default: a temporary directory).
"""

import argparse
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile
import time

HERE = pathlib.Path(__file__).resolve().parent
MODEL = "bench-model"
CONTEXT = 4_000_000
PROMPT = "Summarize the module layout and explain how the session store works. " * 20


def start_mock(work: pathlib.Path, name: str, reply_bytes: int, log: pathlib.Path):
    port_file = work / f"{name}.port"
    process = subprocess.Popen(
        [sys.executable, str(HERE / "mock_openai.py"), "--port-file", str(port_file),
         "--log", str(log), "--model", MODEL, "--reply-bytes", str(reply_bytes)],
        stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    for _ in range(200):
        if port_file.exists() and port_file.read_text():
            return process, int(port_file.read_text())
        time.sleep(0.02)
    raise SystemExit("mock server did not start")


def private_write(path: pathlib.Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    os.chmod(path.parent, 0o700)
    fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_TRUNC, 0o600)
    with os.fdopen(fd, "w") as file:
        file.write(text)


class Agent:
    def __init__(self, name, binary, work, port):
        self.name = name
        self.binary = str(binary)
        self.home = work / f"home-{name}"
        self.home.mkdir(parents=True)
        self.port = port
        self.configure(port)

    def env(self, extra=None):
        env = {"PATH": "/usr/bin:/bin", "HOME": str(self.home), "TERM": "dumb", "NO_COLOR": "1"}
        env.update(self.base_env)
        env.update(extra or {})
        return env

    def workspace(self, name):
        path = self.home / "work" / name
        path.mkdir(parents=True, exist_ok=True)
        return path

    def run(self, args, cwd, extra=None, port=None):
        if port is not None and port != self.port:
            self.configure(port)
        result = subprocess.run([self.binary, *args], cwd=cwd, env=self.env(extra),
                                stdin=subprocess.DEVNULL, capture_output=True, text=True)
        if result.returncode != 0:
            raise SystemExit(f"{self.name} {' '.join(args[:3])} failed: {result.stderr[-2000:]}")
        return result


class Octet(Agent):
    base_env = {"OCTET_OFFLINE": "true"}

    def configure(self, port):
        self.port = port
        registry = {"version": 1, "providers": {"bench": {
            "label": "mock", "base_url": f"http://127.0.0.1:{port}/v1/", "auth": {"kind": "none"},
            "auto_discover": False,
            "models": [{"api_name": MODEL, "context_window": CONTEXT, "max_output_tokens": 8192,
                        "tools": True, "parallel_tool_calls": False, "vision": False,
                        "structured_output": False, "reasoning": False}]}}}
        private_write(self.home / ".octet" / "credentials" / "custom.json", json.dumps(registry))
        (self.home / ".octet" / "config.toml").write_text('theme = "dark"\n')

    model = ["--model", f"custom/bench/{MODEL}"]

    def commands(self):
        return {
            "startup": ([], {"OCTET_BENCH": "1"}),
            "version": (["--version"], {}),
            "help": (["--help"], {}),
            "doctor": (["doctor"], {}),
            "models": (["--list-models"], {}),
            "sessions": (["sessions", "list"], {}),
            "ask": ([*self.model, "--no-session", "-p", "hi"], {}),
            "ask-saved": ([*self.model, "-p", "hi"], {}),
            "resume-long": ([*self.model, "--continue", "-p", "hi"], {}),
        }

    def new_turn(self, cwd, prompt):
        self.run([*self.model, "-p", prompt], cwd)

    def next_turn(self, cwd, prompt):
        self.run([*self.model, "--continue", "-p", prompt], cwd)


class Fx(Agent):
    base_env = {"FX_AUTO_UPGRADE": "0", "FX_SOUND": "0"}

    def configure(self, port):
        self.port = port
        settings = {
            "provider": "local", "auto_upgrade": False,
            "providers": {"local": {
                "protocol": "openai-chat-completions", "base_url": f"http://127.0.0.1:{port}/v1",
                "auth": {"type": "none"},
                "model_metadata": {MODEL: {"context_window": CONTEXT, "max_output_tokens": 8192,
                                           "supports_tool_use": True}}}},
            "models": {"local": MODEL},
        }
        private_write(self.home / ".fx" / "settings.json", json.dumps(settings))

    def commands(self):
        return {
            "startup": ([], {"FX_BENCH": "1"}),
            "version": (["--version"], {}),
            "help": (["help"], {}),
            "doctor": (["doctor", "--json"], {}),
            "models": (["models", "--json"], {}),
            "sessions": (["sessions", "--json"], {}),
            "ask": (["ask", "--full-access", "--no-save", "hi"], {}),
            "ask-saved": (["ask", "--full-access", "hi"], {}),
            "resume-long": (["ask", "--full-access", "--resume", "last", "hi"], {}),
        }

    def new_turn(self, cwd, prompt):
        self.run(["ask", "--full-access", prompt], cwd)

    def next_turn(self, cwd, prompt):
        self.run(["ask", "--full-access", "--resume", "last", prompt], cwd)


def hyperfine(agent, scenario, args, extra, cwd, results, runs, warmup):
    out = results / f"{scenario}-{agent.name}.json"
    command = " ".join([agent.binary, *[a if " " not in a else repr(a) for a in args]])
    subprocess.run(
        ["hyperfine", "-N", "--style", "none", "--runs", str(runs), "--warmup", str(warmup),
         "--export-json", str(out), "--command-name", f"{scenario}-{agent.name}", command],
        cwd=cwd, env={**agent.env(extra), "PATH": os.environ["PATH"]}, check=True,
        stdout=subprocess.DEVNULL)
    return json.loads(out.read_text())["results"][0]


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--octet", required=True)
    parser.add_argument("--fx")
    parser.add_argument("--runs", type=int, default=30)
    parser.add_argument("--warmup", type=int, default=3)
    parser.add_argument("--sessions", type=int, default=500)
    parser.add_argument("--turns", type=int, default=200)
    parser.add_argument("--only", default="")
    parser.add_argument("--results")
    args = parser.parse_args()
    if not shutil.which("hyperfine"):
        raise SystemExit("hyperfine is not installed")
    only = {name for name in args.only.split(",") if name}
    work = pathlib.Path(tempfile.mkdtemp(prefix="octet-versus-"))
    results = pathlib.Path(args.results) if args.results else work / "results"
    results.mkdir(parents=True, exist_ok=True)
    wire = work / "wire.jsonl"
    mock, port = start_mock(work, "short", 2, wire)
    fixture_log = work / "fixture.jsonl"
    long_mock, long_port = start_mock(work, "long", 4096, fixture_log)
    try:
        agents = [Octet("octet", pathlib.Path(args.octet).resolve(), work, port)]
        if args.fx:
            agents.append(Fx("fx", pathlib.Path(args.fx).resolve(), work, port))
        wanted = lambda name: not only or name in only
        rows = {}
        for agent in agents:
            commands = agent.commands()
            plain = agent.workspace("plain")
            if wanted("sessions"):
                many = agent.workspace("many")
                started = time.monotonic()
                for index in range(args.sessions):
                    agent.new_turn(many, f"session {index}: {PROMPT[:200]}")
                print(f"{agent.name}: {args.sessions} sessions built in {time.monotonic() - started:.1f}s",
                      file=sys.stderr)
            if wanted("resume-long"):
                long = agent.workspace("long")
                agent.configure(long_port)
                started = time.monotonic()
                agent.new_turn(long, PROMPT)
                for index in range(args.turns - 1):
                    agent.next_turn(long, f"turn {index}: {PROMPT}")
                print(f"{agent.name}: {args.turns}-turn session built in {time.monotonic() - started:.1f}s",
                      file=sys.stderr)
            for scenario, (argv, extra) in commands.items():
                if not wanted(scenario):
                    continue
                cwd = {"sessions": "many", "resume-long": "long"}.get(scenario, "plain")
                label = f"sessions-{args.sessions}" if scenario == "sessions" else scenario
                if scenario == "resume-long":
                    label = f"resume-{args.turns}"
                # A saved session is bound to the endpoint that created it (fx
                # refuses to resume across a changed base URL), so the resume
                # scenario talks to the long-reply mock that built its fixture.
                agent.configure(long_port if scenario == "resume-long" else port)
                result = hyperfine(agent, label, argv, extra, agent.workspace(cwd), results,
                                   args.runs, args.warmup)
                rows.setdefault(label, {})[agent.name] = result
                print(f"{agent.name:<6} {label:<14} median {result['median'] * 1000:8.2f} ms",
                      file=sys.stderr)
        print()
        print(f"{'scenario':<16}{'octet ms':>10}{'fx ms':>10}{'octet/fx':>10}   (median wall, {args.runs} runs)")
        for label, by_agent in rows.items():
            octet = by_agent.get("octet")
            fx = by_agent.get("fx")
            o = f"{octet['median'] * 1000:.2f}" if octet else "-"
            f = f"{fx['median'] * 1000:.2f}" if fx else "-"
            ratio = f"{octet['median'] / fx['median']:.2f}x" if octet and fx else "-"
            print(f"{label:<16}{o:>10}{f:>10}{ratio:>10}")
        print(f"raw hyperfine JSON: {results}")
    finally:
        mock.terminate()
        long_mock.terminate()


if __name__ == "__main__":
    main()
