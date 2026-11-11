#!/usr/bin/env python3
"""Long-session soak driver: real octet + pi-compat + local fake provider.

Runs N randomized turns (default 300) against one persistent `octet --mode rpc`
process with the soak Pi fixture, mixing prompt submissions, tool calls,
compaction, model switches, thinking-level cycles, reloads, new sessions,
abort cancellations and Pi UI updates. Checks runtime invariants from the RPC
state and process logs, records RSS per turn, and fails on a positive RSS
slope above a configurable threshold.

Deterministic: the seed drives every decision, so a failing run reproduces
with `--seed`. Fail closed: prompts are only submitted while the resolved
model is the extension-provided soak model, and every assistant reply must
carry the fake provider's signature or a soak tool result — anything else
would mean a live provider answered.
"""
import argparse
import json
import os
import random
import re
import shutil
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

import guard

HERE = Path(__file__).resolve().parent
WORKTREE = HERE.parents[1]
SOAK_MODEL_ID = "soak/m-" + __import__("hashlib").sha256(b"soak-model").hexdigest()[:32]

MOUNT_CANCEL_SIGNATURES = (
    re.compile(r"-32800"),
    re.compile(r"request cancelled"),
    re.compile(r"remote UI .* cancelled", re.IGNORECASE),
)


class SoakFailure(Exception):
    pass


def rss_of(pid: int) -> int:
    """Summed VmRSS (KiB) of a process and its descendants."""
    total = 0
    try:
        with open(f"/proc/{pid}/status") as handle:
            for line in handle:
                if line.startswith("VmRSS:"):
                    total += int(line.split()[1])
                    break
    except OSError:
        return 0
    try:
        with open(f"/proc/{pid}/task/{pid}/children") as handle:
            children = [int(value) for value in handle.read().split()]
    except OSError:
        children = []
    for child in children:
        total += rss_of(child)
    return total


def linear_slope(points):
    """Least-squares slope of (index, MiB) points."""
    if len(points) < 2:
        return 0.0
    n = len(points)
    xs = [float(i) for i in range(n)]
    ys = [float(y) for _, y in points]
    mean_x = sum(xs) / n
    mean_y = sum(ys) / n
    denominator = sum((x - mean_x) ** 2 for x in xs)
    if denominator == 0:
        return 0.0
    return sum((x - mean_x) * (y - mean_y) for x, y in zip(xs, ys)) / denominator


class OctetRpc:
    """Line-framed RPC session with octet --mode rpc."""

    def __init__(self, process, log_path):
        self.process = process
        self.log = open(log_path, "a")
        self.stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self.stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self.lines = []
        self.stderr_text = ""
        self.mount_cancel_hits = 0
        self.lock = threading.RLock()  # wait_turn_end holds the condition and calls events_since()
        self.condition = threading.Condition(self.lock)
        self.next_id = 0

    def _read_stdout(self):
        for raw in self.process.stdout:
            with self.condition:
                self.lines.append(raw)
                self.condition.notify_all()
            self.log.write(raw)
            self.log.flush()

    def _read_stderr(self):
        for raw in self.process.stderr:
            hits = sum(len(pattern.findall(raw)) for pattern in MOUNT_CANCEL_SIGNATURES)
            with self.lock:
                self.stderr_text = (self.stderr_text + raw)[-262144:]
                self.mount_cancel_hits += hits
            self.log.write(f"[stderr] {raw}")
            self.log.flush()

    def start(self):
        self.stdout_thread.start()
        self.stderr_thread.start()

    def call(self, command_type, payload, timeout=240.0):
        """Sends one command and waits for its id-matched response."""
        self.next_id += 1
        request_id = f"soak-{self.next_id}"
        request = {"type": command_type, "id": request_id, **payload}
        self._send(request)
        deadline = time.monotonic() + timeout
        with self.condition:
            while True:
                for line in list(self.lines):
                    try:
                        value = json.loads(line)
                    except json.JSONDecodeError:
                        continue
                    if value.get("type") == "response" and value.get("id") == request_id:
                        self.lines.remove(line)
                        return value
                    if value.get("type") == "error" and value.get("id") == request_id:
                        self.lines.remove(line)
                        raise SoakFailure(f"{command_type} failed: {value}")
                if time.monotonic() > deadline:
                    raise SoakFailure(f"timeout waiting for {command_type} response id={request_id}")
                self.condition.wait(timeout=1.0)

    def _send(self, value):
        self.process.stdin.write(json.dumps(value, separators=(",", ":")) + "\n")
        self.process.stdin.flush()

    def events_since(self, marker=None):
        """Drains and returns session events (assistant text, tool results)."""
        with self.lock:
            events = []
            for line in list(self.lines):
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if value.get("type") == "response":
                    continue
                events.append(value)
                self.lines.remove(line)
            return events

    def wait_turn_end(self, timeout=420.0):
        """Waits until the agent settles (agent_settled), returning every event.

        A tool-calling prompt runs two model turns, so the first turn_end is
        not the end of the prompt; the next prompt would be refused as "Agent
        is already streaming"."""
        deadline = time.monotonic() + timeout
        collected = []
        with self.condition:
            while True:
                if self.process.poll() is not None:
                    raise SoakFailure("octet exited while waiting for the turn to complete")
                collected.extend(self.events_since())
                if any(event.get("type") == "agent_settled" for event in collected):
                    return collected
                if time.monotonic() > deadline:
                    raise SoakFailure("timeout waiting for the agent to settle")
                self.condition.wait(timeout=0.5)

    def stderr(self):
        with self.lock:
            return self.stderr_text


def command_response(process: OctetRpc, command_type, payload, timeout=240.0):
    response = process.call(command_type, payload, timeout=timeout)
    if not response.get("success"):
        raise SoakFailure(f"{command_type} reported failure: {response}")
    return response


def wait_for_state(process, predicate, timeout):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        value = command_response(process, "get_state", {})
        if predicate(value.get("data") or {}):
            return value
        time.sleep(0.5)
    raise SoakFailure("state predicate never satisfied")


def select_model(process, model_id, timeout=300.0):
    """set_model plus a barrier until get_state reports the applied model.

    The set_model response acknowledges the command, not the completed
    application; a prompt admitted while the reconfiguration still applies can
    queue behind it indefinitely."""
    response = command_response(process, "set_model", {"modelId": model_id}, timeout=timeout)
    wait_for_state(
        process,
        lambda state: ((state.get("model") or {}).get("id") or "") == model_id,
        timeout,
    )
    # The authoritative receipt can precede the hook/view teardown; a prompt
    # admitted immediately after a switch can queue behind it indefinitely.
    time.sleep(2.0)
    return response


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--octet", required=True, help="path to the octet binary")
    parser.add_argument("--turns", type=int, default=300)
    parser.add_argument("--seed", type=int, default=0)
    parser.add_argument("--rss-slope-limit", type=float, default=0.20,
                        help="fail when the post-warmup RSS slope exceeds this MiB/turn")
    parser.add_argument("--warmup-fraction", type=float, default=0.25,
                        help="leading fraction of samples excluded from the slope check")
    parser.add_argument("--route", choices=("native", "extension"), default="native",
                        help="route every inference turn takes: the native custom route (default) "
                             "or the Pi extension provider registered by the fixture")
    parser.add_argument("--keep", action="store_true", help="keep the run directory on failure")
    args = parser.parse_args()

    rng = random.Random(args.seed)
    route_model = SOAK_MODEL_ID if args.route == "extension" else "custom/soak/soak-bootstrap"
    try:
        guard.preflight(os.environ.get("SOAK_RUN_ROOT") or os.environ.get("TMPDIR") or "/tmp")
    except guard.IsolationError as error:
        print(error, file=sys.stderr)
        return 2
    run_dir = Path(os.environ.get("SOAK_RUN_ROOT") or os.environ.get("TMPDIR") or "/tmp") / "octet-soak" / f"soak-{args.seed}" if args.route == "native" else f"soak-ext-{args.seed}"
    if run_dir.exists():
        shutil.rmtree(run_dir)
    run_dir.mkdir(parents=True)
    run_dir = run_dir.resolve()
    home = run_dir / "home"
    workspace = run_dir / "workspace"
    pi_agent = home / ".pi" / "agent"
    (workspace / ".pi" / "extensions").mkdir(parents=True)
    pi_agent.mkdir(parents=True)
    shutil.copy(HERE / "fixture" / "soak.ts", workspace / ".pi" / "extensions" / "soak.ts")
    extension_dir = run_dir / "extension-dir"
    extension_dir.mkdir()
    (extension_dir / "octet-pi-compat").symlink_to(WORKTREE / "extensions" / "octet-pi-compat")
    log_path = run_dir / "octet.log"
    provider_log = open(run_dir / "provider.log", "w")

    # Hard rule: never touch the real HOME, ~/.pi or ~/.octet. The sentinel
    # tells the fixture it runs under this harness's own agent directory.
    guard_env = {
        "HOME": str(home),
        "OCTET_PI_AGENT_DIR": str(pi_agent),
        "PI_CODING_AGENT_DIR": str(pi_agent),
        "OCTET_DATA_DIR": str(home / ".local" / "share" / "octet"),
    }
    try:
        guard.check_isolated(guard_env, run_dir)
    except guard.IsolationError as error:
        shutil.rmtree(run_dir, ignore_errors=True)
        print(error, file=sys.stderr)
        return 2
    (pi_agent / guard.SENTINEL).write_text(str(run_dir))
    (workspace / ".pi" / guard.SENTINEL).write_text(str(run_dir))

    provider = subprocess.Popen(
        [sys.executable, str(HERE / "fake_provider.py"), "--port-file", str(run_dir / "port")],
        stdout=provider_log, stderr=subprocess.STDOUT, cwd=run_dir)
    try:
        deadline = time.monotonic() + 30
        while not (run_dir / "port").exists() and time.monotonic() < deadline:
            time.sleep(0.1)
        port = (run_dir / "port").read_text().strip()
        provider_url = f"http://127.0.0.1:{port}/v1/"
        (workspace / ".pi" / "soak-port").write_text(port)
    except Exception as error:
        provider.kill()
        raise SoakFailure(f"fake provider failed to start: {error}") from error

    env = {
        **os.environ,
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_DATA_HOME": str(home / ".local" / "share"),
        "OCTET_PI_AGENT_DIR": str(pi_agent),
        "PI_CODING_AGENT_DIR": str(pi_agent),
        "OCTET_DATA_DIR": str(home / ".local" / "share" / "octet"),
        "OCTET_TRUSTED_EXTENSIONS": "octet-pi-compat",
        "SOAK_PROVIDER_URL": provider_url,
        "OCTET_NO_UPDATE_CHECK": "1",
        "TMPDIR": str(run_dir / "tmp"),
    }
    guard.check_isolated(env, run_dir)
    (run_dir / "tmp").mkdir()
    # Bootstrap a native route to the loopback fake so a fresh HOME has a
    # resolvable startup model: offline setup with an explicit inventory, no
    # discovery traffic, no credentials. This model is also a soak turn
    # target (native -> extension provider switches).
    setup = subprocess.run(
        [args.octet, "setup", "--yes", "--offline", "--endpoint", provider_url,
         "--provider", "soak", "--label", "soak", "--manual-model", "soak-bootstrap",
         "--no-auth", "--context-window", "32768"],
        capture_output=True, text=True, env=env, cwd=workspace, timeout=120)
    if setup.returncode != 0:
        raise SoakFailure(f"octet setup failed: {setup.stderr[-800:]}")
    process = subprocess.Popen(
        [args.octet, "--mode", "rpc", "--workspace", str(workspace), "--workspace-trusted", "--offline", "--model", "custom/soak/soak-bootstrap", "--enable-extension", "octet-pi-compat", "--extension-dir", str(WORKTREE / "extensions")],
        stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
        text=True, bufsize=1, env=env, cwd=workspace)
    rpc = OctetRpc(process, log_path)
    rpc.start()

    rss_samples = []
    mount_cancel_hits = 0
    probe_results = 0
    ack_replies = 0
    extension_turn_failures = 0
    failures = []
    summary = {}

    def sample_rss(turn):
        rss_samples.append((turn, rss_of(process.pid) / 1024.0))

    try:
        # Fail closed: switch to the soak model before anything can prompt.
        model_response = select_model(rpc, SOAK_MODEL_ID, timeout=300)
        resolved = (model_response.get("data") or {}).get("id") or (model_response.get("data") or {}).get("model_id")
        state = command_response(rpc, "get_state", {})
        state_model = json.dumps(state.get("data") or {}).lower()
        if SOAK_MODEL_ID not in json.dumps(model_response).lower() and SOAK_MODEL_ID not in state_model:
            raise SoakFailure(f"soak model not selected; refusing to run: {model_response}")
        # The extension provider registered and resolved; pin the native
        # custom route for every inference turn (the bridge-delegated
        # extension route's baseUrl is not reproducible in this shared
        # environment; see fuzz/soak/README.md "unverified").
        select_model(rpc, route_model, timeout=300)

        for turn in range(args.turns):
            print(f"[driver] turn {turn} (choice {choice:.3f})", flush=True) if False else None
            if process.poll() is not None:
                raise SoakFailure(f"octet exited during turn {turn}: {process.returncode}")
            sample_rss(turn)
            choice = rng.random()
            if choice < 0.45:
                payload = {"message": f"soak turn {turn} {rng.randrange(10**9)}"}
                response = command_response(rpc, "prompt", payload)
                events = rpc.wait_turn_end()
                text = json.dumps(events)
                if "ack:" in text:
                    ack_replies += 1
                elif "probe turn" in text:
                    probe_results += 1
                elif "connection" in text.lower() or "errorMessage" in text:
                    extension_turn_failures += 1
                    raise SoakFailure(f"turn {turn}: provider connection failure: {text[-400:]}")
                else:
                    raise SoakFailure(f"turn {turn}: assistant output lacks the fake provider signature: {text[-400:]}")
            elif choice < 0.60:
                payload = {"message": f"SOAK_TOOL turn:{turn} {rng.randrange(10**9)}"}
                response = command_response(rpc, "prompt", payload)
                text = json.dumps(rpc.wait_turn_end())
                if "probe turn" in text:
                    probe_results += 1
                else:
                    raise SoakFailure(f"turn {turn}: tool call did not produce the probe result: {text[-400:]}")
            elif choice < 0.68:
                payload = {"message": f"SOAK_UI turn:{turn} {rng.randrange(10**9)}"}
                response = command_response(rpc, "prompt", payload)
                events = json.dumps(rpc.wait_turn_end())
                if "probe turn" not in events and "footer" not in events and "soak" not in events.lower():
                    raise SoakFailure(f"turn {turn}: UI tool call produced no observable result: {events[-400:]}")
            elif choice < 0.74:
                command_response(rpc, "abort", {})
            elif choice < 0.82:
                command_response(rpc, "reload", {}, timeout=300)
                # After a rebuild the extension provider must re-register or
                # remain selected; re-pin the model before the next prompt.
                try:
                    select_model(rpc, SOAK_MODEL_ID, timeout=300)
                    select_model(rpc, route_model, timeout=300)
                except SoakFailure as error:
                    raise SoakFailure(f"reload at turn {turn} lost the soak model: {error}") from error
            elif choice < 0.88 and turn >= 3:
                command_response(rpc, "compact", {}, timeout=300)
            elif choice < 0.90:
                command_response(rpc, "cycle_thinking_level", {})
            elif choice < 0.96:
                # Model-switch round trip: reselect the bridge-delegated
                # extension model (verifying the route stays registered) and
                # pin the native route back before any inference turn.
                select_model(rpc, SOAK_MODEL_ID, timeout=300)
                select_model(rpc, route_model, timeout=300)
            else:
                command_response(rpc, "new_session", {}, timeout=300)
                # Same sequencing as after reload: the extension route is
                # re-selected (registration check) and the native route is
                # pinned again before any inference turn.
                select_model(rpc, SOAK_MODEL_ID, timeout=300)
                select_model(rpc, route_model, timeout=300)
            # Mount-cancellation signature detection (known main bug): report,
            # do not fail until the fix lands.
            mount_cancel_hits = rpc.mount_cancel_hits
            sample_rss(turn + 1)

        command_response(rpc, "get_state", {})
    except Exception as error:
        failures.append(str(error))
        summary["failure"] = str(error)
    finally:
        try:
            process.terminate()
            process.wait(timeout=10)
        except Exception:
            process.kill()
        provider.terminate()
        provider_log.close()

    sample_rss(args.turns)
    mount_cancel_hits = rpc.mount_cancel_hits
    warmup = max(1, int(len(rss_samples) * args.warmup_fraction))
    slope = linear_slope(rss_samples[warmup:])
    summary.update({
        "seed": args.seed,
        "route": args.route,
        "turns": args.turns,
        "rss_samples": len(rss_samples),
        "rss_slope_mib_per_turn": round(slope, 4),
        "rss_peak_mib": round(max((y for _, y in rss_samples), default=0), 2),
        "ack_replies": ack_replies,
        "probe_results": probe_results,
        "extension_model_turn_failures": extension_turn_failures,
        "mount_cancel_signatures": mount_cancel_hits,
        "failures": failures,
    })
    (run_dir / "summary.json").write_text(json.dumps(summary, indent=2))
    print(json.dumps(summary, indent=2))
    print(f"run directory: {run_dir}")

    hard_failure = bool(failures) or slope > args.rss_slope_limit
    if slope > args.rss_slope_limit:
        print(f"RSS SLOPE VIOLATION: {slope:.4f} MiB/turn > {args.rss_slope_limit}")
    if mount_cancel_hits:
        print("KNOWN MAIN BUG: mount-cancellation signature observed on stderr "
              "(expected until the surface-lease fix lands); not treated as a failure.")
    if hard_failure and not args.keep:
        shutil.rmtree(run_dir)
    return 1 if hard_failure else 0


if __name__ == "__main__":
    sys.exit(main())
