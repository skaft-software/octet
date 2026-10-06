#!/usr/bin/env python3
"""Private, frozen-release comparison recorder (stdlib/POSIX; never builds).

Commands: template --output NEW.json; validate --manifest FILE;
run --manifest FILE --output NEW_DIRECTORY; export --raw DIR --output NEW_DIR
  --replacements JSON_FILE

The template deliberately refuses unqualified UI/extension cells. Version exit
is built in, NOT TUI startup. An operator-reviewed driver may supply the JSONL
contract documented in fixtures/release-comparison/README.md. No live inference
or native terminal campaign is authorized by this script. Environment isolation
is not a network sandbox; an external driver's network policy needs approval.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
import platform
from pathlib import Path
import random
import re
import selectors
import signal
import statistics
import subprocess
import sys
import time

SCHEMA = "octet.release-comparison.v1"
DRIVER_SCHEMA = "octet.release-comparison.driver.v1"
LIMIT = 8 * 1024 * 1024
DIGEST = re.compile(r"[a-f0-9]{64}\Z")
# Metric identity is its boundary, not its convenient name in client logs.
CLOCKS = {
    "version_exit_ms": ("subject_spawn", "version_exit", "process"),
    "first_editable_ms": ("subject_spawn", "application_edit_presented", "pty"),
    "submission_ready_ms": ("subject_spawn", "submission_accepted", "agent"),
    "pty_ready_ms": ("subject_spawn", "complete_ready_frame_received", "pty"),
    "resume_history_ms": ("subject_spawn", "complete_history_received", "pty"),
    "session_picker_ms": ("session_picker_input", "session_picker_edit_presented", "pty"),
    "model_picker_ms": ("model_picker_input", "model_picker_edit_presented", "pty"),
    "extension_ready_ms": ("subject_spawn", "extension_ready", "extension"),
    "extension_teardown_ms": ("shutdown_input", "extension_teardown_complete", "extension"),
    "process_tree_settlement_ms": ("shutdown_input", "process_tree_settled", "process_tree"),
    "long_history_edit_ms": ("history_edit_input", "history_edit_presented", "pty"),
    "cancel_admission_ms": ("cancel_input", "cancel_signal_set", "agent"),
    "cancel_settlement_ms": ("cancel_input", "cancel_authoritative_tree_settled", "process_tree"),
    "input_to_paint_ms": ("terminal_input", "physical_frame_presented", "native_tern"),
}
RESOURCES = {"rss_peak_bytes": "bytes", "pss_peak_bytes": "bytes", "cpu_peak_percent": "percent",
             "allocation_calls": "calls", "allocation_requested_bytes": "bytes"}
METRICS = set(CLOCKS) | set(RESOURCES)
MATCHED = ("frontend", "provider", "model", "workload", "theme", "session", "policy", "extension_contract")


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def parse_json(text):
    def reject(value):
        raise ValueError("non-finite JSON: " + value)
    return json.loads(text, parse_constant=reject)


def load(path):
    return parse_json(Path(path).read_text())


def home_hashes(home):
    result = {}
    for path in home.rglob("*"):
        if path.is_symlink():
            raise ValueError("trial_home_symlink_refused")
        if path.is_file():
            result[str(path.relative_to(home))] = sha256(path)
    return result


def dump(path, value):
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "w") as stream:
        stream.write(json.dumps(value, sort_keys=True, allow_nan=False) + "\n")


def emit(stream, value):
    stream.write(json.dumps({"schema": SCHEMA, **value}, sort_keys=True, allow_nan=False) + "\n")
    stream.flush()


def number(value):
    return type(value) in (int, float) and 0 <= value <= sys.float_info.max


def environment(home):
    # No inherited PATH, proxies, API keys, SSH sockets, launch hooks, Node options,
    # telemetry or configuration variables. All child config roots are disposable.
    env = {"HOME": str(home), "USERPROFILE": str(home), "PATH": os.defpath,
           "XDG_CONFIG_HOME": str(home / ".config"), "XDG_CACHE_HOME": str(home / ".cache"),
           "XDG_DATA_HOME": str(home / ".local/share"), "CODEX_HOME": str(home / ".codex"),
           "PI_CODING_AGENT_DIR": str(home / ".pi/agent"), "PIG_CODING_AGENT_DIR": str(home / ".pig/agent"),
           "PI_OFFLINE": "1", "PI_TELEMETRY": "0", "AWS_EC2_METADATA_DISABLED": "true",
           "TERM": "xterm-256color", "COLORTERM": "truecolor", "LANG": "C", "LC_ALL": "C", "TZ": "UTC",
           "TMPDIR": str(home / "tmp")}
    for key in ("XDG_CONFIG_HOME", "XDG_CACHE_HOME", "XDG_DATA_HOME", "CODEX_HOME",
                "PI_CODING_AGENT_DIR", "PIG_CODING_AGENT_DIR", "TMPDIR"):
        Path(env[key]).mkdir(parents=True, exist_ok=True, mode=0o700)
    return env


def argv(value):
    if not isinstance(value, list) or not value or any(not isinstance(x, str) or "\0" in x for x in value):
        raise ValueError("commands must be nonempty argument vectors, never shell strings")
    if not Path(value[0]).is_absolute():
        raise ValueError("executable must be absolute")
    if Path(value[0]).name in ("sh", "bash", "zsh", "fish", "cmd", "powershell"):
        raise ValueError("shell command drivers are not accepted")


def artifact(value, check=False):
    if not isinstance(value, dict) or not DIGEST.fullmatch(value.get("sha256", "")):
        raise ValueError("missing artifact digest")
    path = Path(value["path"])
    if not path.is_absolute():
        raise ValueError("artifact paths must be absolute")
    if check and (not path.is_file() or sha256(path) != value["sha256"]):
        raise ValueError("absent or changed artifact: " + str(path))


def pins(client):
    return client["artifacts"] + client["provenance"]["artifacts"]


def validate(manifest, check_files=False):
    if manifest.get("schema") != SCHEMA or manifest.get("frozen") is not True:
        raise ValueError("campaign requires an explicitly frozen manifest")
    if type(manifest.get("trials")) is not int or manifest["trials"] < 9:
        raise ValueError("require >=9 independent attempted trials; >=9 completed to qualify")
    if type(manifest.get("warmups")) is not int or manifest["warmups"] < 1:
        raise ValueError("require labelled warmups")
    if not number(manifest.get("timeout_seconds")) or not 0 < manifest["timeout_seconds"] <= 3600:
        raise ValueError("timeout must be positive, finite, <=3600")
    if type(manifest.get("seed")) is not int or manifest.get("order") not in ("random", "alternating"):
        raise ValueError("record random/alternating order and integer seed")
    host = manifest.get("host", {})
    for key in ("hardware", "os", "dimensions", "tern", "cache_policy"):
        if not host.get(key):
            raise ValueError("missing matched condition: " + key)
    dimensions = host["dimensions"]
    if not isinstance(dimensions, list) or len(dimensions) != 2 or any(type(n) is not int or n <= 0 for n in dimensions):
        raise ValueError("dimensions must be [columns, rows]")
    artifact(host["tern"], check_files)
    if not host["tern"].get("version"):
        raise ValueError("Tern version missing")
    clients = manifest.get("clients", {})
    if not clients:
        raise ValueError("no clients")
    for name, client in clients.items():
        if not re.fullmatch(r"[a-z0-9_-]+", name):
            raise ValueError("unsafe client identifier")
        argv(client["argv"])
        if not client.get("version") or not client.get("package_identity"):
            raise ValueError("actual version/package identity required")
        if not client.get("artifacts") or not client.get("provenance", {}).get("artifacts"):
            raise ValueError("runtime/source/build provenance artifacts required; executable hash alone is insufficient")
        provenance = client["provenance"]
        if provenance.get("kind") not in ("source_build", "installed_distribution") or not provenance.get("note"):
            raise ValueError("explicit source-build vs installed-distribution provenance required")
        if provenance["kind"] == "source_build" and not provenance.get("source_revision"):
            raise ValueError("source-build provenance needs exact revision and retained build/source artifacts")
        for item in pins(client):
            artifact(item, check_files)
        if client["argv"][0] not in {item["path"] for item in client["artifacts"]}:
            raise ValueError("argv executable must be pinned (including launcher/runtime)")
        setup = client["setup"]
        if any(setup.get(key) is None for key in MATCHED):
            raise ValueError("unknown effective settings")
        if not isinstance(setup.get("extensions"), list) or type(setup.get("dependency_count")) is not int or setup["dependency_count"] < 0:
            raise ValueError("enabled extension set and dependency count required")
        if type(setup.get("expected_child_processes")) is not int or setup["expected_child_processes"] < 0:
            raise ValueError("child process count required")
        for item in client.get("home_files", []):
            path = Path(item["path"])
            if path.is_absolute() or ".." in path.parts or not path.parts or not isinstance(item["content"], str):
                raise ValueError("config files must be literal disposable-home relative files")
            if hashlib.sha256(item["content"].encode()).hexdigest() != item["sha256"]:
                raise ValueError("config content digest mismatch")
    if not manifest.get("cases"):
        raise ValueError("no cases")
    seen = set()
    for case in manifest["cases"]:
        if not re.fullmatch(r"[a-z0-9_-]+", case["id"]) or case["id"] in seen:
            raise ValueError("unsafe/duplicate case identifier")
        seen.add(case["id"])
        if (not case.get("metrics") or not set(case["metrics"]) <= METRICS
                or len(set(case["metrics"])) != len(case["metrics"])):
            raise ValueError("unknown/empty metrics; provider throughput and agentic success are separate campaigns")
        if not case.get("checks") or any(not isinstance(x, str) for x in case["checks"]):
            raise ValueError("explicit correctness checks required")
        if not case.get("members") or set(case["members"]) - set(clients):
            raise ValueError("unknown client")
        for member in case["members"].values():
            if member.get("mode") == "unavailable":
                if not member.get("reason"):
                    raise ValueError("unavailability needs reason")
            elif member.get("mode") == "version":
                if case["metrics"] != ["version_exit_ms"] or case["checks"] != ["version_matches"]:
                    raise ValueError("version exit cannot stand in for UI startup")
                if not member.get("version_pattern"):
                    raise ValueError("version correctness pattern required")
                re.compile(member["version_pattern"])
            elif member.get("mode") == "driver":
                argv(member["argv"])
                if not member.get("artifacts"):
                    raise ValueError("driver must be pinned")
                for item in member["artifacts"]:
                    artifact(item, check_files)
                if member["argv"][0] not in {x["path"] for x in member["artifacts"]}:
                    raise ValueError("driver runtime must be pinned")
                subject_args = member.get("subject_args", [])
                if not isinstance(subject_args, list) or any(not isinstance(x, str) or "\0" in x for x in subject_args):
                    raise ValueError("subject arguments must be explicit argument vectors")
                if (Path(member["argv"][0]).name in ("env", "sudo")
                        or member.get("approved") is not True or not member.get("network_policy")):
                    raise ValueError("external driver correctness/network setup requires operator approval")
                if "input_to_paint_ms" in case["metrics"] and not manifest.get("native_tern_approved"):
                    raise ValueError("physical Tern observation requires separate approval")
            else:
                raise ValueError("unsupported member mode")


def refusal(manifest, case):
    if case.get("comparative") is not True:
        return "explicitly noncomparative"
    if len(case["members"]) < 2:
        return "fewer than two clients"
    if any(x["mode"] == "unavailable" for x in case["members"].values()):
        return "feature/metric unavailable in at least one harness"
    setups = [manifest["clients"][name]["setup"] for name in case["members"]]
    for key in MATCHED:
        if len({json.dumps(s[key], sort_keys=True) for s in setups}) > 1:
            return "non-equivalent setup: " + key
    return None


def ordered(names, iteration, policy, rng):
    values = list(names)
    if policy == "random":
        rng.shuffle(values)
    else:
        # Balanced cyclic order for three clients, not permanently-middle Pi.
        shift = iteration % len(values)
        values = values[shift:] + values[:shift]
        if iteration % 2:
            values.reverse()
    return values


def cleanup(child):
    # Signal only the session/process group created by this Popen. Never enumerate
    # arbitrary live processes for killing. Detached descendants remain a driver
    # correctness responsibility; no direct-only total-overhead claim is made.
    for sig, delay in ((signal.SIGTERM, .15), (signal.SIGKILL, .15)):
        try:
            os.killpg(child.pid, sig)
        except ProcessLookupError:
            break
        except PermissionError:
            return False
        # Reap the leader promptly: on Darwin signaling a zombie-only orphaned
        # group before reaping can report EPERM rather than ESRCH.
        try:
            child.wait(timeout=delay)
        except subprocess.TimeoutExpired:
            pass
    try:
        child.wait(timeout=2)
    except subprocess.TimeoutExpired:
        return False
    return True


def execute(command, cwd, env, timeout, stdin, directory):
    """Bound retained captures, monotonic deadline, own process group cleanup."""
    argv(command)
    result = {"argv": command, "status": "failed", "started_monotonic_ns": time.monotonic_ns()}
    child = None
    counts = {"stdout": 0, "stderr": 0}
    with (directory / "stdout.bin").open("xb") as out, (directory / "stderr.bin").open("xb") as err:
        os.chmod(directory / "stdout.bin", 0o600)
        os.chmod(directory / "stderr.bin", 0o600)
        try:
            child = subprocess.Popen(command, cwd=cwd, env=env, stdin=stdin, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, start_new_session=True, shell=False)
            with selectors.DefaultSelector() as selector:
                for stream, name, target in ((child.stdout, "stdout", out), (child.stderr, "stderr", err)):
                    selector.register(stream, selectors.EVENT_READ, (name, target))
                deadline = result["started_monotonic_ns"] / 1e9 + timeout
                while selector.get_map():
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        result["reason"] = "benchmark_timeout"
                        break
                    for key, _ in selector.select(min(.05, remaining)):
                        chunk = os.read(key.fileobj.fileno(), 65536)
                        if not chunk:
                            selector.unregister(key.fileobj)
                            continue
                        name, target = key.data
                        available = max(0, LIMIT - counts[name])
                        target.write(chunk[:available])
                        counts[name] += len(chunk)
                        if counts[name] > LIMIT:
                            result["reason"] = "capture_limit"
                            break
                    if "reason" in result:
                        break
                if "reason" not in result:
                    try:
                        child.wait(timeout=max(.001, deadline - time.monotonic()))
                    except subprocess.TimeoutExpired:
                        result["reason"] = "benchmark_timeout"
                result["finished_monotonic_ns"] = time.monotonic_ns()
                result["exit_code"] = child.poll()
                if "reason" not in result:
                    if result["exit_code"] == 0:
                        result["status"] = "completed"
                    else:
                        result["reason"] = "driver_nonzero_exit"
        except OSError as error:
            result.update(reason="launch_failed", launch_errno=error.errno)
        finally:
            if child:
                result["cleanup_reaped"] = cleanup(child)
                if not result["cleanup_reaped"]:
                    result["status"] = "failed"
                    result["cleanup_failure"] = "owned_process_cleanup_failed"
                    result.setdefault("reason", "owned_process_cleanup_failed")
                child.stdout.close()
                child.stderr.close()
    result["capture_bytes"] = counts
    result["capture_truncated"] = any(n > LIMIT for n in counts.values())
    return result


def observation(records, case, setup):
    """One process run is the statistical unit; frames remain raw events."""
    result = {"metrics": {}, "status": "failed"}
    events = [x for x in records if x.get("type") == "event"]
    finals = [x for x in records if x.get("type") == "result"]
    if len(finals) != 1 or not records or records[-1].get("type") != "result":
        return {**result, "reason": "missing_or_duplicate_result"}
    final = finals[0]
    result["fabricated_observations"] = final.get("synthetic") is True
    if any(x.get("schema") != DRIVER_SCHEMA for x in records):
        return {**result, "reason": "incompatible_driver_schema"}
    if any(type(x.get("ns")) is not int or not 0 <= x["ns"] < 2**63 for x in events) or any(a["ns"] > b["ns"] for a, b in zip(events, events[1:])):
        return {**result, "reason": "invalid_monotonic_events"}
    result["correctness"] = final.get("correctness", {})
    if any(result["correctness"].get(key) is not True for key in case["checks"]):
        return {**result, "reason": "correctness_failed_or_missing"}
    observed = final.get("setup", {})
    required = (*MATCHED, "extensions", "dependency_count", "expected_child_processes")
    if any(observed.get(key) != setup.get(key) for key in required):
        return {**result, "reason": "effective_setup_mismatch"}
    for metric in case["metrics"]:
        item = final.get("metrics", {}).get(metric)
        if not isinstance(item, dict):
            result["metrics"][metric] = {"value": None, "reason": "missing_metric"}
            continue
        if item.get("value") is None and item.get("reason"):
            result["metrics"][metric] = item
            continue
        if metric in CLOCKS:
            start_name, end_name, observer = CLOCKS[metric]
            indices = item.get("events", [])
            if len(indices) != 2 or any(type(i) is not int or not 0 <= i < len(events) for i in indices):
                result["metrics"][metric] = {"value": None, "reason": "missing_clock_events"}
                continue
            start, end = [events[i] for i in indices]
            if (start.get("name"), end.get("name"), item.get("observer")) != (start_name, end_name, observer) or end["ns"] < start["ns"]:
                result["metrics"][metric] = {"value": None, "reason": "non_equivalent_clock"}
                continue
            result["metrics"][metric] = {"value": (end["ns"] - start["ns"]) / 1e6, "unit": "ms", "observer": observer}
        else:
            # Allocation traffic is not residency. Resource results need all-tree
            # samples from a qualified observer; direct-PID or incomplete samples
            # cannot establish total agent overhead.
            allocation = metric.startswith("allocation_")
            scope = "allocator_traffic" if allocation else "agent_and_all_descendants"
            samples = item.get("samples", [])
            if (item.get("scope") != scope or item.get("unit") != RESOURCES[metric] or not samples or not isinstance(item.get("method"), str) or not item["method"]
                    or any(not number(x.get("value")) or type(x.get("ns")) is not int or x["ns"] < 0 for x in samples)
                    or any(a["ns"] > b["ns"] for a, b in zip(samples, samples[1:]))
                    or (not allocation and (item.get("tree_complete") is not True or not item.get("method")
                        or any(not x.get("pids") or x.get("complete") is not True
                               or len(set(x["pids"])) != len(x["pids"])
                               or any(type(pid) is not int or pid <= 0 for pid in x["pids"])
                               or set(x.get("per_pid", {})) != {str(pid) for pid in x["pids"]}
                               or any(not number(v) for v in x.get("per_pid", {}).values())
                               or not math.isclose(sum(x["per_pid"].values()), x["value"], rel_tol=1e-9)
                               for x in samples)))):
                result["metrics"][metric] = {"value": None, "reason": "incomplete_or_non_tree_resource_evidence"}
                continue
            result["metrics"][metric] = {"value": max(x["value"] for x in samples) if not allocation else sum(x["value"] for x in samples),
                                         "unit": RESOURCES[metric], "scope": scope, "method": item["method"]}
    result["status"] = "completed"
    return result


def summary(runs, metric):
    measured = [r for r in runs if not r["warmup"]]
    values = [r["metrics"][metric]["value"] for r in measured if r["status"] == "completed"
              and number(r.get("metrics", {}).get(metric, {}).get("value"))]
    quantile = None
    if values:
        values.sort()
        rank = (len(values) - 1) * .95
        quantile = values[math.floor(rank)] + (values[math.ceil(rank)] - values[math.floor(rank)]) * (rank % 1)
    warmup_failures = sum(r["warmup"] and r["status"] == "failed" for r in runs)
    return {"attempted": len(measured), "completed_observations": len(values), "warmup_failures": warmup_failures,
            "failed": sum(r["status"] == "failed" for r in measured),
            "unavailable": sum(r["status"] == "unavailable" for r in measured),
            "missing": sum(r["status"] == "completed" and not number(r.get("metrics", {}).get(metric, {}).get("value")) for r in measured),
            "median": statistics.median(values) if values else None, "p95": quantile,
            "qualified": len(values) >= 9 and len(values) == len(measured) and not warmup_failures,
            "quantile_note": "linear interpolation over independent trials; nine-trial p95 is descriptive"}


def run(manifest, output):
    validate(manifest, check_files=True)
    output.mkdir(mode=0o700)  # atomic collision refusal before any launch
    dump(output / "manifest.json", manifest)
    rng = random.Random(manifest["seed"])
    all_runs = []
    fd = os.open(output / "runs.jsonl", os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "w") as evidence:
        emit(evidence, {"type": "campaign", "manifest_sha256": sha256(output / "manifest.json"),
                        "runner_sha256": sha256(__file__), "python": sys.version,
                        "observed_host": {"system": platform.system(), "release": platform.release(),
                                          "machine": platform.machine(), "logical_cpus": os.cpu_count()},
                        "isolation": "fresh disposable HOME per trial; allowlisted environment; no network sandbox"})
        for case in manifest["cases"]:
            why = refusal(manifest, case)
            emit(evidence, {"type": "case", "id": case["id"], "comparison_refused": why})
            for iteration in range(manifest["warmups"] + manifest["trials"]):
                order = ordered(case["members"], iteration, manifest["order"], rng)
                emit(evidence, {"type": "order", "case": case["id"], "iteration": iteration, "clients": order})
                for name in order:
                    member, client = case["members"][name], manifest["clients"][name]
                    row = {"type": "run", "case": case["id"], "client": name, "iteration": iteration,
                           "warmup": iteration < manifest["warmups"], "metrics": {}}
                    if member["mode"] == "unavailable":
                        row.update(status="unavailable", reason=member["reason"])
                    elif why and case.get("comparative") is True:
                        row.update(status="unavailable", reason="comparison_refused: " + why)
                    else:
                        directory = output / f'{case["id"]}-{iteration:03}-{name}'
                        directory.mkdir(mode=0o700)
                        home = directory / "home"
                        home.mkdir(mode=0o700)
                        env = environment(home.resolve())
                        workspace = directory / "workspace"
                        workspace.mkdir(mode=0o700)
                        row["environment"] = env
                        try:
                            for pin in pins(client) + member.get("artifacts", []) + [manifest["host"]["tern"]]:
                                artifact(pin, True)
                            for item in client.get("home_files", []):
                                path = home / item["path"]
                                path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
                                dump_content(path, item["content"])
                            # Actual config bytes used, not merely a settings label.
                            row["config_hashes"] = home_hashes(home)
                            if member["mode"] == "version":
                                row.update(execute(client["argv"] + ["--version"], workspace.resolve(), env,
                                                   manifest["timeout_seconds"], subprocess.DEVNULL, directory))
                                text = (directory / "stdout.bin").read_text(errors="replace")
                                row["correctness"] = {"version_matches": bool(re.fullmatch(member["version_pattern"], text.strip()))}
                                if row["status"] == "completed" and not row["correctness"]["version_matches"]:
                                    row.update(status="failed", reason="version_correctness_failed")
                                elif row["status"] == "completed":
                                    row["metrics"] = {"version_exit_ms": {"value": (row["finished_monotonic_ns"] - row["started_monotonic_ns"]) / 1e6,
                                                                            "unit": "ms", "observer": "process"}}
                            else:
                                request = {"schema": DRIVER_SCHEMA, "subject_argv": client["argv"] + member.get("subject_args", []), "case": case,
                                           "setup": client["setup"], "host": manifest["host"], "home": str(home.resolve()),
                                           "workspace": str(workspace.resolve()), "timeout_seconds": manifest["timeout_seconds"]}
                                dump(directory / "request.json", request)
                                with (directory / "request.json").open("rb") as request_file:
                                    process = execute(member["argv"], workspace.resolve(), env,
                                                      manifest["timeout_seconds"], request_file, directory)
                                row["process"] = process
                                # Parse even a nonzero/timeout capture: failed correctness
                                # and partial raw events are never discarded or retried.
                                try:
                                    records = [parse_json(line) for line in (directory / "stdout.bin").read_text().splitlines()]
                                    row.update(observation(records, case, client["setup"]))
                                except (ValueError, TypeError, KeyError, IndexError, AttributeError):
                                    row.update(status="failed", reason="invalid_driver_evidence")
                                if process["status"] != "completed":
                                    row.update(status="failed", reason=process["reason"])
                            for pin in pins(client) + member.get("artifacts", []) + [manifest["host"]["tern"]]:
                                artifact(pin, True)
                        except (OSError, ValueError) as error:
                            row.update(status="failed", reason=str(error))
                        try:
                            row["config_final_hashes"] = home_hashes(home)
                        except (OSError, ValueError) as error:
                            row.update(status="failed", reason=str(error))
                        row["artifact_directory"] = directory.name
                    all_runs.append(row)
                    emit(evidence, row)
            summaries = {name: {metric: summary([r for r in all_runs if r["case"] == case["id"] and r["client"] == name], metric)
                               for metric in case["metrics"]} for name in case["members"]}
            for metric in case["metrics"]:
                semantics = {tuple(r["metrics"][metric].get(key) for key in ("unit", "observer", "scope", "method"))
                             for r in all_runs if r["case"] == case["id"] and not r["warmup"] and r["status"] == "completed"
                             and number(r.get("metrics", {}).get(metric, {}).get("value"))}
                if len(semantics) > 1:
                    why = "non-equivalent observed metric semantics: " + metric
            fabricated = any(r.get("fabricated_observations") for r in all_runs if r["case"] == case["id"])
            qualified = why is None and not fabricated and all(s["qualified"] for values in summaries.values() for s in values.values())
            emit(evidence, {"type": "summary", "case": case["id"], "clients": summaries, "comparative_requested": case.get("comparative") is True, "comparative_qualified": qualified,
                            "comparison_refused": why or ("fabricated fixture observations" if fabricated else None if qualified else "insufficient completed/correct observations")})
    dump(output / "checksums.json", {str(p.relative_to(output)): sha256(p) for p in output.rglob("*") if not p.is_symlink() and p.is_file()})
    return output


def dump_content(path, content):
    fd = os.open(path, os.O_CREAT | os.O_EXCL | os.O_WRONLY, 0o600)
    with os.fdopen(fd, "w") as stream:
        stream.write(content)


def export(raw, output, replacements):
    """Separate optional public structural export; never copy homes/captures."""
    if not isinstance(replacements, dict) or any(not k or not isinstance(v, str) for k, v in replacements.items()):
        raise ValueError("replacement map must contain nonempty string keys/string placeholders")
    output.mkdir(mode=0o700)
    def sanitize(value):
        if isinstance(value, str):
            for key in sorted(replacements, key=len, reverse=True):
                value = value.replace(key, replacements[key])
            return value
        if isinstance(value, list):
            return [sanitize(x) for x in value]
        if isinstance(value, dict):
            return {sanitize(k): sanitize(v) for k, v in value.items()}
        return value
    rows = [json.loads(line) for line in (raw / "runs.jsonl").read_text().splitlines()]
    # Diagnostics may contain provider/session payloads: omit, do not redact in place.
    for row in rows:
        for key in ("process", "correctness", "argv", "environment"):
            row.pop(key, None)
        if "reason" in row and not re.fullmatch(r"[a-z_]+", row["reason"]):
            row["reason"] = "private_diagnostic_omitted"
    public_manifest = load(raw / "manifest.json")
    for client in public_manifest["clients"].values():
        for item in client.get("home_files", []):
            item.pop("content", None)
    dump(output / "manifest.json", sanitize(public_manifest))
    with (output / "runs.jsonl").open("x") as stream:
        for row in rows:
            emit(stream, sanitize(row))
    os.chmod(output / "runs.jsonl", 0o600)
    dump(output / "sanitization.json", {"replacement_count": len(replacements),
         "public_placeholders": sorted(set(replacements.values())), "measurements_changed": False,
         "omitted": "all homes, sessions, literal config contents, request/capture files, per-run diagnostics/correctness/environment; manually inspect before publishing"})
    dump(output / "checksums.json", {p.name: sha256(p) for p in output.iterdir() if p.is_file()})


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="action", required=True)
    template = sub.add_parser("template")
    template.add_argument("--output", type=Path, required=True)
    for command in ("validate", "run"):
        child = sub.add_parser(command)
        child.add_argument("--manifest", type=Path, required=True)
        if command == "run":
            child.add_argument("--output", type=Path, required=True)
    child = sub.add_parser("export")
    child.add_argument("--raw", type=Path, required=True)
    child.add_argument("--output", type=Path, required=True)
    child.add_argument("--replacements", type=Path, required=True)
    args = parser.parse_args()
    try:
        if os.name != "posix":
            raise ValueError("POSIX process-group isolation required")
        if args.action == "template":
            dump(args.output, load(Path(__file__).parent / "fixtures/release-comparison/manifest.template.json"))
        elif args.action == "export":
            export(args.raw, args.output, load(args.replacements))
        else:
            manifest = load(args.manifest)
            validate(manifest, check_files=True)
            if args.action == "run":
                run(manifest, args.output)
                records = [parse_json(line) for line in (args.output / "runs.jsonl").read_text().splitlines()]
                summaries = [row for row in records if row["type"] == "summary"]
                if (any(not value["qualified"] for row in summaries for metrics in row["clients"].values() for value in metrics.values())
                        or any(row["comparative_requested"] and not row["comparative_qualified"] for row in summaries)):
                    return 1  # evidence retained; one or more cells incomplete
        return 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(str(error), file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
