#!/usr/bin/env python3
"""Reproducible, credential-free Pi runtime evidence harness.

The checked-in fixture driver measures the pinned compatibility bridge without
starting a model provider or reading user homes. It is intentionally a release
*input*, not a release approval: wire an actual runtime-manager adapter before
using the resulting schema as candidate-release evidence.
"""

from __future__ import annotations

import argparse
import hashlib
import importlib.util
import json
import math
import os
from pathlib import Path
import platform
import queue
import re
import shutil
import signal
import statistics
import subprocess
import sys
import tempfile
import threading
import time
from typing import Any


SCHEMA = "octet.pi.runtime.evidence.v1"
DRIVER_SCHEMA = "octet.pi.runtime.benchmark-driver.v1"
DECISION_SCHEMA = "octet.pi.runtime.decision.v1"
PROFILES = ("no_extension", "legacy_eager", "lazy", "shared_workspace", "pi_aggregate")
MAX_REPETITIONS = 31
MAX_RESOURCE_SAMPLES = 256
MAX_STDERR_BYTES = 16 * 1024
MIN_DECISION_REPETITIONS = 5

# Documented, bounded release thresholds. Every metric is derived from the
# profiles above; a metric that cannot be measured on this platform is
# `unavailable` (decision `incomplete`), never estimated. Override an
# individual limit with `--threshold NAME=VALUE`.
THRESHOLD_DEFAULTS: dict[str, dict[str, Any]] = {
    "aggregate_startup_overhead_median_ms": {
        "limit": 250.0,
        "unit": "ms",
        "about": "pi_aggregate median startup readiness minus no_extension median startup readiness",
    },
    "aggregate_startup_readiness_p95_ms": {
        "limit": 1000.0,
        "unit": "ms",
        "about": "pi_aggregate startup readiness p95",
    },
    "aggregate_first_activation_p95_ms": {
        "limit": 1500.0,
        "unit": "ms",
        "about": "pi_aggregate first activation p95",
    },
    "aggregate_warm_call_p95_ms": {
        "limit": 250.0,
        "unit": "ms",
        "about": "pi_aggregate warm call p95",
    },
    "aggregate_restart_readiness_p95_ms": {
        "limit": 1500.0,
        "unit": "ms",
        "about": "pi_aggregate process-replacement readiness p95",
    },
    "aggregate_peak_rss_delta_kib": {
        "limit": 262144.0,
        "unit": "KiB",
        "about": "pi_aggregate minus no_extension p95 peak RSS in the active-extension phase",
    },
}
RELEASE_ADAPTER = "runtime_manager"
PUBLISH_ROOT = Path("docs/benchmarks")


class EvidenceError(RuntimeError):
    """An expected harness failure with a terse, non-secret diagnostic."""


def repository_root() -> Path:
    return Path(__file__).resolve().parents[1]


def load_identity_helpers(root: Path) -> Any:
    path = root / "extensions/octet-pi-compat/tests/helpers.py"
    spec = importlib.util.spec_from_file_location("octet_pi_bench_identity", path)
    if spec is None or spec.loader is None:
        raise EvidenceError("cannot load the hermetic Pi identity helper")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def command_output(command: list[str], *, timeout: float = 5.0) -> str | None:
    try:
        completed = subprocess.run(
            command,
            check=True,
            capture_output=True,
            text=True,
            timeout=timeout,
        )
    except (OSError, subprocess.CalledProcessError, subprocess.TimeoutExpired):
        return None
    return completed.stdout.strip()


def scrubbed_environment(work: Path) -> dict[str, str]:
    """Do not pass provider credentials, user homes, npm config, or octet config."""
    home = work / "home"
    home.mkdir(parents=True, exist_ok=True)
    environment = {
        "HOME": str(home),
        "XDG_CONFIG_HOME": str(home / ".config"),
        "XDG_CACHE_HOME": str(home / ".cache"),
        "XDG_DATA_HOME": str(home / ".local/share"),
        "PATH": os.defpath,
        "LANG": "C",
        "LC_ALL": "C",
        "TZ": "UTC",
    }
    # Node occasionally needs this on Windows; it is harmless and does not
    # carry a credential. Do not inherit any other environment variable.
    if os.name == "nt" and "SYSTEMROOT" in os.environ:
        environment["SYSTEMROOT"] = os.environ["SYSTEMROOT"]
    return environment


def safe_float(value: float | int | None) -> float | None:
    return None if value is None else round(float(value), 3)


def percentile(values: list[float], point: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    index = max(0, math.ceil(point * len(ordered)) - 1)
    return safe_float(ordered[index])


def summary(values: list[float]) -> dict[str, float | int | None]:
    return {
        "count": len(values),
        "median": safe_float(statistics.median(values)) if values else None,
        "p95": percentile(values, 0.95),
        "min": safe_float(min(values)) if values else None,
        "max": safe_float(max(values)) if values else None,
    }


def linux_process_tree(root_pid: int) -> dict[str, int | float | None]:
    records: dict[int, tuple[int, int, int, int]] = {}
    proc = Path("/proc")
    try:
        entries = list(proc.iterdir())
    except OSError:
        return unavailable_resource_sample()
    for entry in entries:
        if not entry.name.isdigit():
            continue
        try:
            raw = (entry / "stat").read_text(encoding="utf-8")
            after = raw[raw.rfind(")") + 2 :].split()
            # Linux proc(5): state, ppid, ..., utime, stime, ..., num_threads.
            records[int(entry.name)] = (
                int(after[1]),
                int(after[11]),
                int(after[12]),
                int(after[17]),
            )
        except (OSError, ValueError, IndexError):
            continue
    descendants = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, (ppid, _utime, _stime, _threads) in records.items():
            if ppid in descendants and pid not in descendants:
                descendants.add(pid)
                changed = True
    rss_kib = 0
    pss_kib: int | None = 0
    fds = 0
    cpu_ticks = 0
    threads = 0
    page_kib = os.sysconf("SC_PAGE_SIZE") // 1024
    for pid in descendants:
        try:
            fields = (proc / str(pid) / "statm").read_text(encoding="utf-8").split()
            rss_kib += int(fields[1]) * page_kib
        except (OSError, ValueError, IndexError):
            pass
        try:
            for line in (proc / str(pid) / "smaps_rollup").read_text(encoding="utf-8").splitlines():
                if line.startswith("Pss:"):
                    pss_kib = (pss_kib or 0) + int(line.split()[1])
                    break
        except (OSError, ValueError, IndexError):
            pss_kib = None
        try:
            fds += len(list((proc / str(pid) / "fd").iterdir()))
        except OSError:
            pass
        if pid in records:
            _ppid, utime, stime, count = records[pid]
            cpu_ticks += utime + stime
            threads += count
    return {
        "rss_kib": rss_kib,
        "pss_kib": pss_kib,
        "cpu_ticks": cpu_ticks,
        "processes": len(descendants & records.keys()),
        "threads": threads,
        "fd_count": fds,
    }


# Darwin `ps` field support varies by release: macOS 27 (Darwin 27) rejects the
# `thcount` keyword outright. Probe once, cache the result, and keep RSS/CPU/
# process fields measured even when the thread column is unsupported.
MAC_PS_FORMATS: tuple[tuple[str, bool], ...] = (
    ("pid=,ppid=,rss=,pcpu=,thcount=", True),
    ("pid=,ppid=,rss=,pcpu=", False),
)
_MAC_PS_FORMAT: tuple[str, bool] | None = None


def mac_ps_format() -> tuple[str, bool] | None:
    global _MAC_PS_FORMAT
    if _MAC_PS_FORMAT is None:
        for fields, has_threads in MAC_PS_FORMATS:
            if command_output(["ps", "-axo", fields]) is not None:
                _MAC_PS_FORMAT = (fields, has_threads)
                break
    return _MAC_PS_FORMAT


def mac_process_tree(root_pid: int) -> dict[str, int | float | None]:
    selected_format = mac_ps_format()
    if selected_format is None:
        return unavailable_resource_sample()
    fields, has_threads = selected_format
    output = command_output(["ps", "-axo", fields])
    if output is None:
        return unavailable_resource_sample()
    records: dict[int, tuple[int, int, float, int | None]] = {}
    for line in output.splitlines():
        parts = line.split()
        if len(parts) != len(fields.split(",")):
            continue
        try:
            pid, ppid, rss, cpu = (int(parts[0]), int(parts[1]), int(parts[2]), float(parts[3]))
            threads = int(parts[4]) if has_threads else None
        except ValueError:
            continue
        records[pid] = (ppid, rss, cpu, threads)
    descendants = {root_pid}
    changed = True
    while changed:
        changed = False
        for pid, (ppid, _rss, _cpu, _threads) in records.items():
            if ppid in descendants and pid not in descendants:
                descendants.add(pid)
                changed = True
    selected = [records[pid] for pid in descendants if pid in records]
    thread_values = [record[3] for record in selected if record[3] is not None]
    return {
        "rss_kib": sum(record[1] for record in selected),
        "pss_kib": None,
        "cpu_ticks": None,
        "cpu_percent_snapshot": safe_float(sum(record[2] for record in selected)),
        "processes": len(selected),
        "threads": sum(thread_values) if thread_values else None,
        "fd_count": None,
    }


def unavailable_resource_sample() -> dict[str, int | float | None]:
    return {
        "rss_kib": None,
        "pss_kib": None,
        "cpu_ticks": None,
        "processes": None,
        "threads": None,
        "fd_count": None,
    }


def process_tree_sample(pid: int) -> dict[str, int | float | None]:
    if sys.platform.startswith("linux"):
        return linux_process_tree(pid)
    if sys.platform == "darwin":
        return mac_process_tree(pid)
    return unavailable_resource_sample()


class ResourceSampler:
    def __init__(self, pid: int, interval_ms: int, maximum: int) -> None:
        self.pid = pid
        self.interval = interval_ms / 1000
        self.maximum = maximum
        self.samples: list[dict[str, Any]] = []
        self._samples_lock = threading.Lock()
        self._finish_lock = threading.Lock()
        self._finished = False
        self._origin_ns = time.monotonic_ns()
        self._stop = threading.Event()
        self._thread = threading.Thread(target=self._run, daemon=True)

    def start(self) -> None:
        self._capture()
        self._thread.start()

    def _capture(self) -> None:
        with self._samples_lock:
            if len(self.samples) >= self.maximum:
                return
            sample = process_tree_sample(self.pid)
            sample["t_ms"] = round((time.monotonic_ns() - self._origin_ns) / 1_000_000, 3)
            self.samples.append(sample)

    def _run(self) -> None:
        while not self._stop.wait(self.interval):
            self._capture()

    def finish(self) -> list[dict[str, Any]]:
        with self._finish_lock:
            if self._finished:
                return self.samples
            self._stop.set()
            self._thread.join(timeout=max(0.2, self.interval * 2))
            self._capture()
            self._finished = True
            return self.samples


def peak_resource(samples: list[dict[str, Any]]) -> dict[str, int | float | None]:
    result: dict[str, int | float | None] = {}
    for field in ("rss_kib", "pss_kib", "processes", "threads", "fd_count"):
        values = [sample[field] for sample in samples if isinstance(sample.get(field), (int, float))]
        result[f"peak_{field}"] = max(values) if values else None
    cpu_ticks = [sample["cpu_ticks"] for sample in samples if isinstance(sample.get("cpu_ticks"), int)]
    result["cpu_ticks_total"] = (max(cpu_ticks) - min(cpu_ticks)) if len(cpu_ticks) > 1 else 0
    if len(samples) > 1 and cpu_ticks and sys.platform.startswith("linux"):
        elapsed = (samples[-1]["t_ms"] - samples[0]["t_ms"]) / 1000
        ticks = os.sysconf(os.sysconf_names["SC_CLK_TCK"])
        result["cpu_percent_interval"] = safe_float((result["cpu_ticks_total"] / ticks) / elapsed * 100) if elapsed > 0 else 0
    else:
        result["cpu_percent_interval"] = None
    return result


class RpcPeer:
    """Minimal line-JSON RPC peer; retains bounded diagnostics only."""

    def __init__(self, command: list[str], environment: dict[str, str], work: Path) -> None:
        self.process = subprocess.Popen(
            command,
            cwd=work,
            env=environment,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
            encoding="utf-8",
            bufsize=1,
            start_new_session=True,
        )
        self._messages: queue.Queue[dict[str, Any]] = queue.Queue()
        self._pending: dict[int, dict[str, Any]] = {}
        self._next_id = 1
        self._write_lock = threading.Lock()
        self.stderr = ""
        self._stderr_lock = threading.Lock()
        self._stdout_thread = threading.Thread(target=self._read_stdout, daemon=True)
        self._stderr_thread = threading.Thread(target=self._read_stderr, daemon=True)
        self._stdout_thread.start()
        self._stderr_thread.start()

    def _read_stdout(self) -> None:
        assert self.process.stdout is not None
        for line in self.process.stdout:
            try:
                message = json.loads(line)
            except json.JSONDecodeError:
                continue
            if isinstance(message, dict):
                self._messages.put(message)

    def _read_stderr(self) -> None:
        assert self.process.stderr is not None
        for line in self.process.stderr:
            with self._stderr_lock:
                if len(self.stderr.encode("utf-8")) < MAX_STDERR_BYTES:
                    self.stderr += line[:1024]

    def request(self, method: str, params: dict[str, Any], timeout: float = 10.0) -> dict[str, Any]:
        request_id = self._next_id
        self._next_id += 1
        assert self.process.stdin is not None
        payload = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}
        with self._write_lock:
            self.process.stdin.write(json.dumps(payload, separators=(",", ":")) + "\n")
            self.process.stdin.flush()
        deadline = time.monotonic() + timeout
        while True:
            cached = self._pending.pop(request_id, None)
            if cached is not None:
                return cached
            if self.process.poll() is not None:
                raise EvidenceError("fixture runtime exited before its protocol response")
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise EvidenceError("fixture runtime timed out waiting for its protocol response")
            try:
                message = self._messages.get(timeout=min(remaining, 0.1))
            except queue.Empty:
                continue
            if message.get("id") == request_id and "method" not in message:
                return message
            if isinstance(message.get("id"), int) and "method" not in message:
                self._pending[message["id"]] = message

    def close(self) -> None:
        if self.process.poll() is None:
            try:
                self.request("shutdown", {}, timeout=1.0)
            except EvidenceError:
                try:
                    os.killpg(self.process.pid, signal.SIGTERM)
                except (OSError, ProcessLookupError):
                    pass
        try:
            self.process.wait(timeout=2.0)
        except subprocess.TimeoutExpired:
            try:
                os.killpg(self.process.pid, signal.SIGKILL)
            except (OSError, ProcessLookupError):
                pass
            self.process.wait(timeout=2.0)
        self._stdout_thread.join(timeout=0.2)
        self._stderr_thread.join(timeout=0.2)
        for stream in (self.process.stdin, self.process.stdout, self.process.stderr):
            if stream is not None:
                stream.close()


def bridge_spec(
    root: Path,
    identity: Any,
    work: Path,
    sources: list[Path],
    command_name: str,
) -> tuple[list[str], dict[str, Any], str]:
    node = shutil.which("node")
    if node is None:
        raise EvidenceError("node is required for the Pi runtime evidence harness")
    bridge = root / "extensions/octet-pi-compat/bridge.mjs"
    fake_pi = root / "extensions/octet-pi-compat/tests/fixtures/fake-pi"
    agent_dir = work / "agent"
    manifest = work / "manifest" / "extension.toml"
    manifest.parent.mkdir(parents=True, exist_ok=True)
    manifest.write_text(
        (
            'name = "pi-fixture"\n'
            'version = "0.0.0"\n'
            'api_version = "0.2"\n\n'
            '[entrypoint]\n'
            'command = "node"\n'
            'args = []\n'
        ),
        encoding="utf-8",
    )
    source_hashes = [identity.compute_source_fingerprint(source) for source in sources]
    lock_hashes = [identity.source_lock_fingerprint(source) for source in sources]
    runtime_hash = identity.runtime_integrity(fake_pi)
    aggregate_digest = hashlib.sha256(
        ("fixture-aggregate\0" + command_name + "\0" + "\0".join(source_hashes)).encode("utf-8")
    ).hexdigest()
    link = identity.link_identity(
        extensions=sources,
        source_hashes=source_hashes,
        lock_hashes=lock_hashes,
        pi_package=fake_pi,
        pi_runtime_integrity=runtime_hash,
        aggregate_digest=aggregate_digest,
        manifest_path=manifest,
        command_name=command_name,
        octet_version="0.7.0",
        agent_dir=agent_dir,
    )
    command = [node, str(bridge)]
    for source, source_hash, lock_hash in zip(sources, source_hashes, lock_hashes, strict=True):
        command.extend(
            [
                "--extension",
                str(source),
                "--source-fingerprint",
                source_hash,
                "--source-lock-fingerprint",
                lock_hash,
            ]
        )
    command.extend(
        [
            "--agent-dir",
            str(agent_dir),
            "--pi-package",
            str(fake_pi),
            "--pi-runtime-integrity",
            runtime_hash,
            "--aggregate-digest",
            aggregate_digest,
            "--link-manifest",
            str(manifest),
            "--link-identity",
            link,
            "--octet-version",
            "0.7.0",
            "--command",
            command_name,
        ]
    )
    params = {
        "workspace": str(root),
        "host": {},
        "protocol": {"optional_features": ["lifecycle_events"]},
        "octet_version": "0.7.0",
        "extension": {
            "name": command_name,
            "version": "fixture",
            "manifest_path": str(manifest),
            "source": "explicit",
        },
    }
    return command, params, "aggregate_state" if len(sources) > 1 else "fixture_echo"


def baseline_spec(root: Path) -> tuple[list[str], dict[str, Any], str]:
    node = shutil.which("node")
    if node is None:
        raise EvidenceError("node is required for the Pi runtime evidence harness")
    return [node, str(root / "extensions/octet-pi-compat/tests/fixtures/runtime-idle.mjs")], {"workspace": str(root)}, "activate"


def start_and_initialize(
    command: list[str],
    params: dict[str, Any],
    environment: dict[str, str],
    work: Path,
    interval_ms: int,
    maximum_samples: int,
) -> tuple[RpcPeer, ResourceSampler, float]:
    started = time.perf_counter_ns()
    peer = RpcPeer(command, environment, work)
    sampler = ResourceSampler(peer.process.pid, interval_ms, maximum_samples)
    sampler.start()
    try:
        response = peer.request("initialize", params)
        if "error" in response:
            raise EvidenceError("fixture runtime rejected initialization")
    except BaseException:
        peer.close()
        sampler.finish()
        raise
    return peer, sampler, (time.perf_counter_ns() - started) / 1_000_000


def activate(peer: RpcPeer, tool: str) -> float:
    started = time.perf_counter_ns()
    if tool == "activate":
        response = peer.request("activate", {})
    else:
        response = peer.request("tool/call", {"name": tool, "arguments": {}, "catalog_revision": 0})
    if "error" in response:
        raise EvidenceError("fixture runtime rejected activation")
    return (time.perf_counter_ns() - started) / 1_000_000


def one_profile(
    profile: str,
    root: Path,
    identity: Any,
    environment: dict[str, str],
    interval_ms: int,
    maximum_samples: int,
) -> dict[str, Any]:
    fixtures = root / "extensions/octet-pi-compat/tests/fixtures"
    extension = fixtures / "fixture-extension.mjs"
    aggregate = [fixtures / "aggregate/first.mjs", fixtures / "aggregate/second.mjs"]
    with tempfile.TemporaryDirectory(prefix="octet-pi-evidence-") as directory:
        work = Path(directory)
        if profile == "no_extension":
            command, params, tool = baseline_spec(root)
            lazy = False
        elif profile == "pi_aggregate":
            command, params, tool = bridge_spec(root, identity, work, aggregate, "pi-aggregate")
            lazy = False
        elif profile == "legacy_eager":
            command, params, tool = bridge_spec(root, identity, work, [extension], "pi-legacy")
            lazy = False
        elif profile == "shared_workspace":
            command, params, tool = bridge_spec(root, identity, work, [extension], "pi-shared")
            lazy = False
        elif profile == "lazy":
            command, params, tool = baseline_spec(root)
            lazy = True
        else:
            raise EvidenceError(f"unknown benchmark profile {profile}")

        peer, sampler, ready_ms = start_and_initialize(
            command, params, environment, work, interval_ms, maximum_samples
        )
        lazy_peer: RpcPeer | None = None
        lazy_sampler: ResourceSampler | None = None
        try:
            if lazy:
                lazy_command, lazy_params, lazy_tool = bridge_spec(root, identity, work / "lazy", [extension], "pi-lazy")
                activation_start = time.perf_counter_ns()
                lazy_peer, lazy_sampler, lazy_ready_ms = start_and_initialize(
                    lazy_command, lazy_params, environment, work, interval_ms, maximum_samples
                )
                first_ms = lazy_ready_ms + activate(lazy_peer, lazy_tool)
                first_ms = max(first_ms, (time.perf_counter_ns() - activation_start) / 1_000_000)
                warm_ms = activate(lazy_peer, lazy_tool)
                active_peer, active_tool = lazy_peer, lazy_tool
            else:
                first_ms = activate(peer, tool)
                warm_ms = activate(peer, tool)
                active_peer, active_tool = peer, tool

            shared_reuse_ms: float | None = None
            if profile == "shared_workspace":
                active_peer.request("session/started", {})
                activate(active_peer, active_tool)
                active_peer.request("session/settled", {"outcome": "completed"})
                started = time.perf_counter_ns()
                active_peer.request("session/started", {})
                activate(active_peer, active_tool)
                active_peer.request("session/settled", {"outcome": "completed"})
                shared_reuse_ms = (time.perf_counter_ns() - started) / 1_000_000

            # Reload is deliberately a process replacement fixture. It is not
            # claimed to be #254's future manager-driven hot reload.
            active_peer.close()
            if active_peer is lazy_peer:
                assert lazy_sampler is not None
                active_samples = lazy_sampler.finish()
                lazy_peer = None
                lazy_sampler = None
            else:
                active_samples = sampler.finish()
                peer = None  # type: ignore[assignment]
            reload_command, reload_params, _reload_tool = (
                bridge_spec(root, identity, work / "reload", aggregate if profile == "pi_aggregate" else [extension], "pi-reload")
                if profile not in ("no_extension", "lazy")
                else (bridge_spec(root, identity, work / "reload", [extension], "pi-reload") if profile == "lazy" else baseline_spec(root))
            )
            reload_peer, reload_sampler, reload_ms = start_and_initialize(
                reload_command, reload_params, environment, work, interval_ms, maximum_samples
            )
            reload_peer.close()
            reload_samples = reload_sampler.finish()
            base_samples = [] if peer is None else sampler.finish()
            return {
                "profile": profile,
                "driver": "hermetic_fixture",
                "lifecycle_profile": "pi_aggregate" if profile == "pi_aggregate" else profile,
                "startup_readiness_ms": safe_float(ready_ms),
                "first_activation_ms": safe_float(first_ms),
                "warm_call_ms": safe_float(warm_ms),
                "process_restart_readiness_ms": safe_float(reload_ms),
                "shared_workspace_reuse_ms": safe_float(shared_reuse_ms),
                "agent": {
                    "initial_process": peak_resource(base_samples),
                    "active_extension_process": peak_resource(active_samples),
                    "reload_process": peak_resource(reload_samples),
                },
                "raw_resource_samples": {
                    "initial_process": base_samples,
                    "active_extension_process": active_samples,
                    "reload_process": reload_samples,
                },
                "inference": {
                    "included": False,
                    "reason": "hermetic runtime fixture does not launch or contact an inference server",
                },
            }
        finally:
            if lazy_peer is not None:
                lazy_peer.close()
            if lazy_sampler is not None:
                lazy_sampler.finish()
            if peer is not None:
                peer.close()
            sampler.finish()


def compact_raw(sample: dict[str, Any], max_samples: int) -> dict[str, Any]:
    """Defensively enforce the committed-artifact raw sample bound."""
    for group in sample.get("raw_resource_samples", {}).values():
        if isinstance(group, list) and len(group) > max_samples:
            del group[max_samples:]
    return sample


def aggregate_profile_runs(runs: list[dict[str, Any]]) -> dict[str, Any]:
    measurements = ("startup_readiness_ms", "first_activation_ms", "warm_call_ms", "process_restart_readiness_ms")
    result: dict[str, Any] = {"runs": runs, "summary": {}}
    for measurement in measurements:
        result["summary"][measurement] = summary(
            [float(run[measurement]) for run in runs if run.get(measurement) is not None]
        )
    values = [run.get("shared_workspace_reuse_ms") for run in runs]
    result["summary"]["shared_workspace_reuse_ms"] = summary([float(value) for value in values if value is not None])
    return result


def summary_point(profiles: dict[str, Any], profile: str, measurement: str, point: str) -> float | None:
    block = profiles.get(profile, {}).get("summary", {}).get(measurement) or {}
    value = block.get(point)
    return None if value is None else float(value)


def active_peak_rss_series(profiles: dict[str, Any], profile: str) -> list[float]:
    values: list[float] = []
    for run in profiles.get(profile, {}).get("runs", []):
        peak = ((run.get("agent") or {}).get("active_extension_process") or {}).get("peak_rss_kib")
        if isinstance(peak, (int, float)):
            values.append(float(peak))
    return values


def measured_metric(profiles: dict[str, Any], metric: str) -> float | None:
    """Return the measured value for one threshold metric, or None (unavailable)."""
    if metric == "aggregate_startup_overhead_median_ms":
        baseline = summary_point(profiles, "no_extension", "startup_readiness_ms", "median")
        aggregate = summary_point(profiles, "pi_aggregate", "startup_readiness_ms", "median")
        return None if baseline is None or aggregate is None else aggregate - baseline
    if metric == "aggregate_startup_readiness_p95_ms":
        return summary_point(profiles, "pi_aggregate", "startup_readiness_ms", "p95")
    if metric == "aggregate_first_activation_p95_ms":
        return summary_point(profiles, "pi_aggregate", "first_activation_ms", "p95")
    if metric == "aggregate_warm_call_p95_ms":
        return summary_point(profiles, "pi_aggregate", "warm_call_ms", "p95")
    if metric == "aggregate_restart_readiness_p95_ms":
        return summary_point(profiles, "pi_aggregate", "process_restart_readiness_ms", "p95")
    if metric == "aggregate_peak_rss_delta_kib":
        aggregate = percentile(active_peak_rss_series(profiles, "pi_aggregate"), 0.95)
        baseline = percentile(active_peak_rss_series(profiles, "no_extension"), 0.95)
        if aggregate is None or baseline is None:
            return None
        return float(aggregate) - float(baseline)
    raise EvidenceError(f"unknown threshold metric {metric}")


def threshold_settings(overrides: list[str]) -> dict[str, dict[str, Any]]:
    settings = {name: dict(spec) for name, spec in THRESHOLD_DEFAULTS.items()}
    for override in overrides:
        name, separator, raw = override.partition("=")
        if not separator or name not in settings:
            raise EvidenceError(
                f"--threshold must be NAME=VALUE with a known metric, got {override!r}; "
                f"known: {', '.join(sorted(settings))}"
            )
        try:
            value = float(raw)
        except ValueError as error:
            raise EvidenceError(f"--threshold {name} needs a numeric value, got {raw!r}") from error
        if not math.isfinite(value) or value < 0:
            raise EvidenceError(f"--threshold {name} needs a finite non-negative value, got {raw!r}")
        settings[name]["limit"] = value
    return settings


def threshold_observations(profiles: dict[str, Any], thresholds: dict[str, dict[str, Any]]) -> list[dict[str, Any]]:
    observations: list[dict[str, Any]] = []
    for metric in sorted(thresholds):
        spec = thresholds[metric]
        observed = measured_metric(profiles, metric)
        if observed is None:
            status = "unavailable"
        elif observed > spec["limit"]:
            status = "fail"
        else:
            status = "pass"
        observations.append(
            {
                "metric": metric,
                "about": spec["about"],
                "unit": spec["unit"],
                "observed": safe_float(observed),
                "limit": spec["limit"],
                "status": status,
            }
        )
    return observations


def release_decision(
    profiles: dict[str, Any],
    thresholds: dict[str, dict[str, Any]],
    repetitions: int,
    adapter: str,
    inference_included: bool,
) -> dict[str, Any]:
    """Derive the decision from measured thresholds plus explicit evidence gates.

    `status` is a measurement verdict: `fail` when any threshold is exceeded,
    `incomplete` when a required metric or repetition count is missing, else
    `pass`. `release_approval` is separate and stays false while an evidence
    gate is unmet, so a hermetic fixture can never approve a release.
    """
    observations = threshold_observations(profiles, thresholds)
    failed = [row["metric"] for row in observations if row["status"] == "fail"]
    unavailable = [row["metric"] for row in observations if row["status"] == "unavailable"]
    gates: list[dict[str, Any]] = []
    if adapter != RELEASE_ADAPTER:
        gates.append(
            {
                "gate": "runtime_manager_adapter",
                "satisfied": False,
                "observed": adapter,
                "required": "a checked-in adapter backed by the real aggregate plan/evidence seam",
            }
        )
    # An externally supplied PID proves only that a process was sampled, not
    # its model/server identity or workload attribution. This harness has no
    # reviewed cross-platform release receipt either; never infer approval.
    gates.extend([
        {
            "gate": "inference_attribution",
            "satisfied": False,
            "observed": "external PID snapshot only" if inference_included else "no inference process was launched or sampled",
            "required": "separately retained inference server identity and resources",
        },
        {
            "gate": "cross_platform_review",
            "satisfied": False,
            "observed": "single-platform measurement; no release-review receipt",
            "required": "reviewed Linux and macOS candidate runs and explicit release approval",
        },
    ])
    if repetitions < MIN_DECISION_REPETITIONS:
        gates.append(
            {
                "gate": "repetitions",
                "satisfied": False,
                "observed": repetitions,
                "required": f"at least {MIN_DECISION_REPETITIONS} per-profile repetitions",
            }
        )
    if failed:
        status = "fail"
    elif unavailable or repetitions < MIN_DECISION_REPETITIONS:
        status = "incomplete"
    else:
        status = "pass"

    reasons = [
        f"threshold {row['metric']} exceeded: measured {row['observed']} {row['unit']} > limit {row['limit']} {row['unit']}"
        for row in observations
        if row["status"] == "fail"
    ]
    reasons.extend(
        f"threshold {row['metric']} unavailable on this platform: no measured value was recorded"
        for row in observations
        if row["status"] == "unavailable"
    )
    if repetitions < MIN_DECISION_REPETITIONS:
        reasons.append(
            f"{repetitions} repetition(s) recorded; {MIN_DECISION_REPETITIONS} are required before a decision"
        )
    if adapter != RELEASE_ADAPTER:
        reasons.append(
            f"adapter {adapter!r} is a hermetic fixture, not a candidate runtime-manager adapter"
        )
    if not inference_included:
        reasons.append("no inference server was launched; agent and inference resources stay separate")
    reasons.append("Linux and macOS candidate runs must both be reviewed before any release approval")
    if not reasons or status == "pass":
        reasons.insert(0, "all measured thresholds are within their documented limits")

    return {
        "schema": DECISION_SCHEMA,
        "status": status,
        "thresholds": observations,
        "repetitions": repetitions,
        "min_repetitions": MIN_DECISION_REPETITIONS,
        "baseline_attribution": {
            "baseline_profile": "no_extension",
            "pi_aggregate_profile": "pi_aggregate",
            "startup_median_overhead_ms": safe_float(measured_metric(profiles, "aggregate_startup_overhead_median_ms")),
        },
        "reasons": reasons,
        "release_approval": {
            "approved": status == "pass" and not gates,
            "gates": gates,
        },
    }


def inference_evidence(pid: int | None) -> dict[str, Any]:
    if pid is None:
        return {
            "included": False,
            "reason": "no --inference-pid supplied; no inference process was launched or contacted",
            "gpu": {"available": False, "reason": "no inference process was sampled"},
        }
    sample = process_tree_sample(pid)
    if sample.get("processes") in (None, 0):
        return {
            "included": False,
            "reason": "the explicitly requested inference pid was unavailable",
            "gpu": {"available": False, "reason": "inference pid was unavailable"},
        }
    return {
        "included": True,
        "sampling": "explicit_pid_process_tree_snapshot",
        "resource": peak_resource([sample]),
        "gpu": {
            "available": False,
            "reason": "this portable harness records CPU/process metrics only; attach a platform GPU collector when relevant",
        },
    }


def sha256_file(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def fixture_inputs(root: Path, identity: Any) -> dict[str, Any]:
    fixtures = root / "extensions/octet-pi-compat/tests/fixtures"
    runtime = fixtures / "fake-pi"
    try:
        package = json.loads((runtime / "package.json").read_text(encoding="utf-8"))
    except (OSError, json.JSONDecodeError) as error:
        raise EvidenceError("cannot read the checked-in fake Pi package identity") from error
    source_paths = {
        "single_extension": fixtures / "fixture-extension.mjs",
        "aggregate_first": fixtures / "aggregate/first.mjs",
        "aggregate_second": fixtures / "aggregate/second.mjs",
        "no_extension_idle": fixtures / "runtime-idle.mjs",
    }
    return {
        "adapter": "hermetic_fixture",
        "benchmark_driver_sha256": sha256_file(Path(__file__).resolve()),
        "bridge": {"path": "extensions/octet-pi-compat/bridge.mjs", "sha256": sha256_file(root / "extensions/octet-pi-compat/bridge.mjs")},
        "pi_runtime": {
            "kind": "checked_in_fake_pi",
            "name": package.get("name"),
            "version": package.get("version"),
            "integrity_sha256": identity.runtime_integrity(runtime),
        },
        "sources": {
            name: {
                "path": str(path.relative_to(root)).replace(os.sep, "/"),
                "source_sha256": identity.compute_source_fingerprint(path),
                "dependency_lock_sha256": identity.source_lock_fingerprint(path),
            }
            for name, path in source_paths.items()
        },
    }


def system_metadata(candidate: str, node: str | None) -> dict[str, Any]:
    memory_kib = None
    if sys.platform.startswith("linux"):
        try:
            for line in Path("/proc/meminfo").read_text(encoding="utf-8").splitlines():
                if line.startswith("MemTotal:"):
                    memory_kib = int(line.split()[1])
                    break
        except OSError:
            pass
    cpu_model = None
    if sys.platform.startswith("linux"):
        try:
            cpu_model = next(
                (line.split(":", 1)[1].strip() for line in Path("/proc/cpuinfo").read_text().splitlines() if line.startswith("model name")),
                None,
            )
        except OSError:
            pass
    if sys.platform == "darwin":
        cpu_model = command_output(["sysctl", "-n", "machdep.cpu.brand_string"])
        memory_bytes = command_output(["sysctl", "-n", "hw.memsize"])
        if memory_bytes and memory_bytes.isdigit():
            memory_kib = int(memory_bytes) // 1024
    return {
        "candidate": candidate,
        "captured_at_utc": time.strftime("%Y-%m-%dT%H:%M:%SZ", time.gmtime()),
        "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine()},
        "hardware": {"logical_cpus": os.cpu_count(), "memory_kib": memory_kib, "cpu_model": cpu_model},
        "toolchain": {"python": sys.version.split()[0], "node": node, "extension_api_evidence_version": "0.3"},
        "safety": {
            "network_calls": False,
            "provider_calls": False,
            "credentials_inherited": False,
            "home_is_temporary": True,
        },
    }


def write_json(path: Path, value: Any) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = path.with_name(f".{path.name}.tmp")
    temporary.write_text(json.dumps(value, indent=2, sort_keys=True) + "\n", encoding="utf-8")
    temporary.replace(path)


def measured_summary(profiles: dict[str, Any], profile: str, measurement: str) -> str:
    block = profiles.get(profile, {}).get("summary", {}).get(measurement) or {}
    median = block.get("median")
    p95 = block.get("p95")
    if median is None and p95 is None:
        return "unavailable"
    return f"{median} / {p95}"


def publication_readme(artifact: dict[str, Any], command: str) -> str:
    decision = artifact["release_decision"]
    metadata = artifact["metadata"]
    inputs = artifact["inputs"]
    lines = [
        f"# Pi runtime fixture evidence — {metadata['candidate']}",
        "",
        "Bounded, offline, credential-free capture from",
        "[`scripts/bench-pi-runtime.py`](../../../scripts/bench-pi-runtime.py). It runs the checked-in",
        "Pi compatibility fixture only: no model or provider request, no network call, no inherited",
        "credentials, and a temporary HOME. These are fixture representations of the lifecycle",
        "profiles, not a production runtime-manager measurement.",
        "",
        "## Reproduction",
        "",
        "```console",
        command,
        "```",
        "",
        "The method is deterministic (fixed profiles, fixed fixture identities, bounded samples);",
        "wall-clock timings still vary per run and host, so the recorded numbers are one capture.",
        "",
        "## Measured thresholds",
        "",
        "| Metric | Observed | Limit | Unit | Status |",
        "| --- | --- | --- | --- | --- |",
    ]
    for row in decision["thresholds"]:
        observed = "unavailable" if row["observed"] is None else row["observed"]
        lines.append(f"| `{row['metric']}` | {observed} | {row['limit']} | {row['unit']} | {row['status']} |")
    lines += [
        "",
        f"`status: {decision['status']}` over {decision['repetitions']} repetition(s) per profile",
        f"(minimum {decision['min_repetitions']}); release approval",
        f"`{'approved' if decision['release_approval']['approved'] else 'blocked'}`.",
        "",
        "## Release gates",
        "",
    ]
    for gate in decision["release_approval"]["gates"]:
        lines.append(f"- `{gate['gate']}` (unmet): observed {gate['observed']!r}; requires {gate['required']}.")
    lines += [
        "",
        "## Profiles",
        "",
        "| Profile | Startup median/p95 ms | First activation median/p95 ms | Warm call median/p95 ms | Restart readiness median/p95 ms | Active peak RSS p95 KiB |",
        "| --- | --- | --- | --- | --- | --- |",
    ]
    for profile in artifact["collection"]["profiles"]:
        rss = percentile(active_peak_rss_series(artifact["profiles"], profile), 0.95)
        lines.append(
            f"| `{profile}` | {measured_summary(artifact['profiles'], profile, 'startup_readiness_ms')} | "
            f"{measured_summary(artifact['profiles'], profile, 'first_activation_ms')} | "
            f"{measured_summary(artifact['profiles'], profile, 'warm_call_ms')} | "
            f"{measured_summary(artifact['profiles'], profile, 'process_restart_readiness_ms')} | "
            f"{'unavailable' if rss is None else rss} |"
        )
    lines += [
        "",
        "## Method and limits",
        "",
        f"- Driver: `{artifact['driver']['name']}` (reload semantics: `{artifact['driver']['reload_semantics']}`).",
        f"- API evidence version: `{artifact['api']['version']}`; bridge SHA-256 `{inputs['bridge']['sha256'][:16]}…`.",
        f"- Pi runtime: `{inputs['pi_runtime']['kind']}` {inputs['pi_runtime']['name']} {inputs['pi_runtime']['version']}.",
        f"- Platform: {metadata['platform']['system']} {metadata['platform']['release']} {metadata['platform']['machine']}.",
        f"- Samples: interval {artifact['collection']['resource_sample_interval_ms']} ms, at most",
        f"  {artifact['collection']['max_resource_samples_per_process']} per process, raw samples bounded.",
        "- Linux records `/proc` RSS/PSS/CPU ticks/threads/FDs. Darwin uses `ps` for RSS, an",
        "  instantaneous CPU percent and, where the local `ps` supports the keyword, threads; PSS and",
        "  FD count stay unavailable rather than estimated.",
        "- Agent process trees are always separate from inference resources; this capture launched no",
        "  inference server, so no GPU or model-server claim is present.",
        f"- Sanitization: {metadata['publication']['note']}.",
        "- Nothing here approves a release or claims production lazy activation, cross-workspace",
        "  sharing, reload policy, FD limits or multi-session governance.",
        "",
    ]
    return "\n".join(lines)


def publish_artifact(output: Path, artifact: dict[str, Any], command: str) -> None:
    publish_root = (repository_root() / PUBLISH_ROOT).resolve()
    if not (output == publish_root or publish_root in output.parents):
        raise EvidenceError(f"--publish requires an output directory inside {PUBLISH_ROOT}/")
    results = output / "results.json"
    if results.exists():
        raise EvidenceError(f"refusing to overwrite existing evidence at {results}")
    metadata = artifact["metadata"]
    metadata["hardware"]["cpu_model"] = None
    metadata["publication"] = {
        "sanitized": True,
        "omitted": ["hardware.cpu_model"],
        "note": "nonessential CPU brand string omitted; measured samples, thresholds and limits unchanged",
    }
    output.mkdir(parents=True, exist_ok=True)
    write_json(results, artifact)
    readme = output / "README.md"
    readme.write_text(publication_readme(artifact, command) + "\n", encoding="utf-8")
    sums = [(sha256_file(results), "results.json"), (sha256_file(readme), "README.md")]
    (output / "SHA256SUMS").write_text(
        "".join(f"{digest}  {name}\n" for digest, name in sums), encoding="utf-8"
    )


def reproduction_command(arguments: argparse.Namespace, output: Path) -> str:
    parts = [
        "python3 scripts/bench-pi-runtime.py",
        f"--candidate {arguments.candidate}",
        f"--repetitions {arguments.repetitions}",
        f"--sample-interval-ms {arguments.sample_interval_ms}",
        f"--max-resource-samples {arguments.max_resource_samples}",
    ]
    parts.extend(f"--threshold {override}" for override in arguments.threshold)
    parts.append("--publish")
    parts.append(f"--output {output}")
    return " ".join(parts)


def parse_arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--candidate", required=True, help="Exact candidate revision or immutable build identifier.")
    parser.add_argument("--output", type=Path, required=True, help="Directory for bounded JSON evidence.")
    parser.add_argument("--repetitions", type=int, default=5, help=f"Per-profile repetitions (1-{MAX_REPETITIONS}).")
    parser.add_argument("--sample-interval-ms", type=int, default=20)
    parser.add_argument("--max-resource-samples", type=int, default=64)
    parser.add_argument(
        "--threshold",
        action="append",
        default=[],
        metavar="NAME=VALUE",
        help="Override one documented threshold limit, e.g. aggregate_warm_call_p95_ms=200.",
    )
    parser.add_argument(
        "--publish",
        action="store_true",
        help=(
            "Write a publication-safe, self-describing evidence directory (results.json, README.md, "
            f"SHA256SUMS) below {PUBLISH_ROOT}/; refuses to overwrite existing evidence."
        ),
    )
    parser.add_argument(
        "--inference-pid",
        type=int,
        default=None,
        help="Explicit external inference pid to snapshot separately; it is never contacted or controlled.",
    )
    return parser.parse_args()


def main() -> int:
    arguments = parse_arguments()
    if not 1 <= arguments.repetitions <= MAX_REPETITIONS:
        raise EvidenceError(f"--repetitions must be in 1..{MAX_REPETITIONS}")
    if not 1 <= arguments.max_resource_samples <= MAX_RESOURCE_SAMPLES:
        raise EvidenceError(f"--max-resource-samples must be in 1..{MAX_RESOURCE_SAMPLES}")
    if arguments.sample_interval_ms < 5:
        raise EvidenceError("--sample-interval-ms must be at least 5")
    if not arguments.candidate.strip():
        raise EvidenceError("--candidate must be a non-empty immutable identifier")
    if arguments.inference_pid is not None and arguments.inference_pid <= 0:
        raise EvidenceError("--inference-pid must be positive")
    thresholds = threshold_settings(arguments.threshold)
    if arguments.publish and not re.fullmatch(r"[0-9a-f]{40}|[0-9a-f]{64}", arguments.candidate):
        raise EvidenceError(
            "--publish requires a full lowercase 40-character commit or 64-character build digest candidate"
        )
    root = repository_root()
    identity = load_identity_helpers(root)
    node = command_output([shutil.which("node") or "node", "--version"])
    if node is None:
        raise EvidenceError("node is required for the Pi runtime evidence harness")

    output = arguments.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    all_runs: dict[str, list[dict[str, Any]]] = {profile: [] for profile in PROFILES}
    with tempfile.TemporaryDirectory(prefix="octet-pi-evidence-home-") as environment_directory:
        environment = scrubbed_environment(Path(environment_directory))
        for profile in PROFILES:
            for repetition in range(arguments.repetitions):
                run = one_profile(
                    profile,
                    root,
                    identity,
                    environment,
                    arguments.sample_interval_ms,
                    arguments.max_resource_samples,
                )
                run["repetition"] = repetition + 1
                all_runs[profile].append(compact_raw(run, arguments.max_resource_samples))

    profiles = {profile: aggregate_profile_runs(runs) for profile, runs in all_runs.items()}
    inputs = fixture_inputs(root, identity)
    inference = inference_evidence(arguments.inference_pid)
    artifact = {
        "schema": SCHEMA,
        "schema_version": 1,
        "api": {"version": "0.3", "schema": "octet.extension.api/0.3"},
        "driver": {"schema": DRIVER_SCHEMA, "name": "hermetic_fixture", "reload_semantics": "process_restart"},
        "inputs": inputs,
        "metadata": system_metadata(arguments.candidate, node),
        "collection": {
            "profiles": list(PROFILES),
            "repetitions": arguments.repetitions,
            "resource_sample_interval_ms": arguments.sample_interval_ms,
            "max_resource_samples_per_process": arguments.max_resource_samples,
            "raw_samples_bounded": True,
        },
        "profiles": profiles,
        "inference_server": inference,
        "release_decision": release_decision(
            profiles,
            thresholds,
            arguments.repetitions,
            inputs["adapter"],
            bool(inference["included"]),
        ),
    }
    if arguments.publish:
        try:
            relative_output = output.relative_to(root)
        except ValueError:
            raise EvidenceError(f"--publish requires an output directory inside {PUBLISH_ROOT}/") from None
        publish_artifact(output, artifact, reproduction_command(arguments, relative_output))
    else:
        write_json(output / "results.json", artifact)
        digest = hashlib.sha256((output / "results.json").read_bytes()).hexdigest()
        (output / "SHA256SUMS").write_text(f"{digest}  results.json\n", encoding="utf-8")
    digest = hashlib.sha256((output / "results.json").read_bytes()).hexdigest()
    print(
        json.dumps(
            {
                "output": str(output),
                "sha256": digest,
                "decision": artifact["release_decision"]["status"],
                "release_approval": artifact["release_decision"]["release_approval"]["approved"],
            }
        )
    )
    return 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except EvidenceError as error:
        print(f"bench-pi-runtime: {error}", file=sys.stderr)
        raise SystemExit(2)
