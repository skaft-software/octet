#!/usr/bin/env python3
"""Reproducible, dependency-free systems measurements for local coding agents.

The default case measures subprocess creation through ``--version`` exit, not
cold-cache startup, UI readiness, or model TTFT. Additional commands are
``NAME=ARGV`` values (parsed with :mod:`shlex`, never a shell). Long-lived
commands supplied with ``--idle-command`` are sampled for direct-process
RSS/PSS and CPU; descendants and independently running servers are not sampled.
Commands inherit the caller's environment and configuration: isolation and
network policy are the campaign operator's responsibility.

Examples:

    python3 scripts/bench-systems.py \
      --binary ./target/release/octet --repetitions 9 \
      --output /tmp/octet-systems.json

    python3 scripts/bench-systems.py \
      --command sessions='./target/release/octet --offline sessions list' \
      --idle-command idle='./target/release/octet --plain --offline ...' \
      --concurrency 1,2,4 --telemetry /tmp/octet-telemetry.jsonl
"""

from __future__ import annotations

import argparse
import glob
import json
import math
import os
import platform
import shlex
import signal
import statistics
import subprocess
import sys
import time
from pathlib import Path
from typing import Any

SCHEMA = "octet.systems-benchmark.v2"
DEFAULT_REPETITIONS = 9
DEFAULT_TIMEOUT_SECONDS = 30.0
DEFAULT_IDLE_SECONDS = 1.0
DEFAULT_SETTLE_SECONDS = 0.25
SAMPLE_INTERVAL_SECONDS = 0.05


def parse_named_command(raw: str) -> tuple[str, list[str]]:
    name, separator, command = raw.partition("=")
    if not separator or not name.strip():
        raise argparse.ArgumentTypeError("expected NAME=COMMAND")
    try:
        argv = shlex.split(command)
    except ValueError as error:
        raise argparse.ArgumentTypeError(str(error)) from error
    if not argv or not argv[0]:
        raise argparse.ArgumentTypeError(f"command for {name!r} is empty")
    if any("\0" in argument for argument in argv):
        raise argparse.ArgumentTypeError("command arguments cannot contain NUL")
    return name.strip(), argv


def finite_number(value: float) -> float | None:
    return value if math.isfinite(value) else None


def quantile(values: list[float], fraction: float) -> float | None:
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return ordered[0]
    position = fraction * (len(ordered) - 1)
    lower = math.floor(position)
    upper = math.ceil(position)
    if lower == upper:
        return ordered[lower]
    weight = position - lower
    return ordered[lower] * (1.0 - weight) + ordered[upper] * weight


def summarize(values: list[float]) -> dict[str, float | int | None]:
    return {
        "count": len(values),
        "min": min(values) if values else None,
        "median": statistics.median(values) if values else None,
        "p95": quantile(values, 0.95),
        "max": max(values) if values else None,
    }


def read_cpu_percent(pid: int) -> float | None:
    try:
        result = subprocess.run(
            ["ps", "-o", "pcpu=", "-p", str(pid)],
            check=True,
            capture_output=True,
            text=True,
            timeout=2,
        )
        return finite_number(float(result.stdout.strip()))
    except (FileNotFoundError, OSError, subprocess.SubprocessError, ValueError):
        return None


def read_memory(pid: int) -> dict[str, float | int | None]:
    """Return best-effort process memory and CPU data for one PID."""

    if sys.platform.startswith("linux"):
        rss: int | None = None
        try:
            for line in Path(f"/proc/{pid}/status").read_text().splitlines():
                if line.startswith("VmRSS:"):
                    rss = int(line.split()[1]) * 1024
                    break
        except (FileNotFoundError, OSError, ValueError):
            pass
        pss: int | None = None
        try:
            for line in Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines():
                if line.startswith("Pss:"):
                    pss = int(line.split()[1]) * 1024
                    break
        except (FileNotFoundError, OSError, ValueError):
            pass
        cpu = read_cpu_percent(pid)
        return {"rss_bytes": rss, "pss_bytes": pss, "cpu_percent": cpu}

    # macOS and BSD expose RSS/CPU portably through ps. PSS is intentionally
    # reported as null rather than confused with RSS.
    try:
        result = subprocess.run(
            ["ps", "-o", "rss=,pcpu=", "-p", str(pid)],
            check=True,
            capture_output=True,
            text=True,
            timeout=2,
        )
        fields = result.stdout.split()
        if len(fields) >= 2:
            return {
                "rss_bytes": int(float(fields[0]) * 1024),
                "pss_bytes": None,
                "cpu_percent": finite_number(float(fields[1])),
            }
    except (FileNotFoundError, OSError, subprocess.SubprocessError, ValueError):
        pass
    return {"rss_bytes": None, "pss_bytes": None, "cpu_percent": None}


def terminate_process(process: subprocess.Popen[bytes]) -> None:
    """Close input and reap the child; clean its owned POSIX group as well.

    A group can outlive its leader, including after a zero exit. Detached
    descendants are not discoverable here; non-POSIX cleanup is direct-only.
    """
    if process.stdin is not None:
        try:
            process.stdin.close()
        except (BrokenPipeError, OSError):
            pass
    try:
        process.wait(timeout=1)
    except subprocess.TimeoutExpired:
        pass
    try:
        if os.name == "posix":
            os.killpg(process.pid, signal.SIGTERM)
        elif process.poll() is None:
            process.terminate()
        process.wait(timeout=5)
    except (ProcessLookupError, subprocess.TimeoutExpired):
        pass
    finally:
        try:
            if os.name == "posix":
                os.killpg(process.pid, signal.SIGKILL)
            elif process.poll() is None:
                process.kill()
        except ProcessLookupError:
            pass
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            pass


def launch_kwargs() -> dict[str, Any]:
    kwargs: dict[str, Any] = {
        "stdin": subprocess.DEVNULL,
        "stdout": subprocess.DEVNULL,
        "stderr": subprocess.DEVNULL,
    }
    if os.name == "posix":
        kwargs["start_new_session"] = True
    return kwargs


def idle_launch_kwargs() -> dict[str, Any]:
    kwargs = launch_kwargs()
    kwargs["stdin"] = subprocess.PIPE
    return kwargs


def benchmark_startup(
    name: str,
    argv: list[str],
    repetitions: int,
    timeout_seconds: float,
) -> dict[str, Any]:
    durations: list[float] = []
    return_codes: list[int | None] = []
    errors: list[str] = []
    runs: list[dict[str, Any]] = []
    for _ in range(repetitions):
        started = time.monotonic()
        process: subprocess.Popen[bytes] | None = None
        return_code: int | None = None
        error_name: str | None = None
        try:
            process = subprocess.Popen(argv, **launch_kwargs())
            remaining = timeout_seconds - (time.monotonic() - started)
            if remaining <= 0:
                raise subprocess.TimeoutExpired(argv, timeout_seconds)
            return_code = process.wait(timeout=remaining)
        except (OSError, subprocess.TimeoutExpired) as error:
            error_name = type(error).__name__
            errors.append(error_name)
        finally:
            elapsed_ms = (time.monotonic() - started) * 1000
            if process is not None:
                terminate_process(process)
        return_codes.append(return_code)
        runs.append({
            "duration_ms": elapsed_ms,
            "return_code": return_code,
            "error": error_name,
        })
        if return_code == 0 and error_name is None:
            durations.append(elapsed_ms)
    return {
        "kind": "startup",
        "phase": "command_completion",
        "summary_population": "zero-exit runs only; all attempts retained in runs",
        "timeout_seconds": timeout_seconds,
        "name": name,
        "argv": argv,
        "repetitions": repetitions,
        "successful_runs": len(durations),
        "failed_runs": repetitions - len(durations),
        "duration_ms": summarize(durations),
        "return_codes": return_codes,
        "errors": errors,
        "runs": runs,
    }


def measure_resources(
    argv: list[str],
    count: int,
    idle_seconds: float,
    settle_seconds: float,
    timeout_seconds: float,
) -> dict[str, Any]:
    """One sampling window; one monotonic seconds clock for every deadline.

    The timeout includes launch, settle and observation, but not cleanup. OS
    process creation and a resource probe cannot be interrupted by this loop;
    overruns are retained as failed trials, never successful measurements.
    """
    started = time.monotonic()
    timeout_deadline = started + timeout_seconds
    processes: list[subprocess.Popen[bytes]] = []
    samples: list[dict[str, Any]] = []
    errors: list[str] = []
    launch_ms: float | None = None
    status = "completed"
    exit_codes: list[int | None] = []
    try:
        for _ in range(count):
            if time.monotonic() >= timeout_deadline:
                status = "timeout"
                break
            processes.append(subprocess.Popen(argv, **idle_launch_kwargs()))
        if status == "completed":
            launch_ms = (time.monotonic() - started) * 1000
            time.sleep(min(settle_seconds, max(0.0, timeout_deadline - time.monotonic())))
            sample_deadline = time.monotonic() + idle_seconds
            while True:
                now = time.monotonic()
                if now >= timeout_deadline:
                    status = "timeout"
                    break
                exit_codes = [process.poll() for process in processes]
                if any(code is not None for code in exit_codes):
                    status = "early_exit"
                    errors.append("process_exited_before_window_end")
                    break
                if now >= sample_deadline:
                    break
                process_samples = [
                    {"pid": process.pid, **read_memory(process.pid)}
                    for process in processes
                ]
                sample: dict[str, Any] = {
                    "elapsed_ms": (now - started) * 1000,
                    "sample_finished_ms": (time.monotonic() - started) * 1000,
                    "processes": process_samples,
                }
                for source, target, divisor in (
                    ("rss_bytes", "rss_total_kib", 1024),
                    ("pss_bytes", "pss_total_kib", 1024),
                    ("cpu_percent", "cpu_total_percent", 1),
                ):
                    values = [item[source] for item in process_samples]
                    sample[target] = (
                        sum(values) / divisor if all(value is not None for value in values) else None
                    )
                samples.append(sample)
                remaining = min(sample_deadline, timeout_deadline) - time.monotonic()
                if remaining > 0:
                    time.sleep(min(SAMPLE_INTERVAL_SECONDS, remaining))
        if status == "timeout":
            errors.append("measurement_timeout")
    except OSError as error:
        status = "launch_error"
        errors.append(type(error).__name__)
    finally:
        exit_codes = [process.poll() for process in processes]
        observation_ms = (time.monotonic() - started) * 1000
        for process in processes:
            terminate_process(process)
    run = {
        "launch_ms": launch_ms,
        "observation_ms": observation_ms,
        "status": status,
        "launched_processes": len(processes),
        "sample_count": len(samples),
        "exit_codes_before_cleanup": exit_codes,
        "errors": errors,
        "samples": samples,
    }
    for metric, peak in (
        ("rss_total_kib", "rss_peak_kib"),
        ("pss_total_kib", "pss_peak_kib"),
        ("cpu_total_percent", "cpu_peak_percent"),
    ):
        values = [sample[metric] for sample in samples if sample[metric] is not None]
        run[peak] = max(values) if values else None
    return run


def resource_summary(runs: list[dict[str, Any]]) -> dict[str, Any]:
    completed = [run for run in runs if run["status"] == "completed"]
    result: dict[str, Any] = {
        "completed_runs": len(completed),
        "failed_runs": len(runs) - len(completed),
        "summary_population": "resource summaries: completed windows only; launch_ms: all fully launched trials; failed trials retained in runs",
        "launch_ms": summarize([run["launch_ms"] for run in runs if run["launch_ms"] is not None]),
    }
    for metric in ("rss_peak_kib", "pss_peak_kib", "cpu_peak_percent"):
        result[metric] = summarize([run[metric] for run in completed if run[metric] is not None])
    return result


def benchmark_idle(
    name: str,
    argv: list[str],
    repetitions: int,
    idle_seconds: float,
    settle_seconds: float,
    timeout_seconds: float,
) -> dict[str, Any]:
    runs = [
        measure_resources(argv, 1, idle_seconds, settle_seconds, timeout_seconds)
        for _ in range(repetitions)
    ]
    # Preserve the single-process sample shape; concurrency retains each PID and
    # the complete total so missing members cannot masquerade as lower overhead.
    for run in runs:
        run["exit_code_before_cleanup"] = next(iter(run.pop("exit_codes_before_cleanup")), None)
        run["samples"] = [
            {
                "elapsed_ms": sample["elapsed_ms"],
                "sample_finished_ms": sample["sample_finished_ms"],
                **sample["processes"][0],
            }
            for sample in run["samples"]
        ]
    samples = [sample for run in runs if run["status"] == "completed" for sample in run["samples"]]
    return {
        "kind": "idle_memory",
        "phase": "settled_direct_process_sampling",
        "resource_scope": "direct_process_only",
        "name": name,
        "argv": argv,
        "repetitions": repetitions,
        "idle_seconds": idle_seconds,
        "settle_seconds": settle_seconds,
        "timeout_seconds": timeout_seconds,
        **resource_summary(runs),
        "sample_count": len(samples),
        "rss_kib": summarize([sample["rss_bytes"] / 1024 for sample in samples if sample["rss_bytes"] is not None]),
        "pss_kib": summarize([sample["pss_bytes"] / 1024 for sample in samples if sample["pss_bytes"] is not None]),
        "cpu_percent": summarize([sample["cpu_percent"] for sample in samples if sample["cpu_percent"] is not None]),
        "exit_codes": [run["exit_code_before_cleanup"] for run in runs],
        "errors": [error for run in runs for error in run["errors"]],
        "runs": runs,
        "memory_metric_notes": "RSS/PSS/CPU sample summaries pool available samples independently; peak summaries use one peak per completed run. PSS is not RSS. Descendants are excluded.",
    }


def benchmark_concurrency(
    name: str,
    argv: list[str],
    levels: list[int],
    repetitions: int,
    idle_seconds: float,
    settle_seconds: float,
    timeout_seconds: float = DEFAULT_TIMEOUT_SECONDS,
) -> dict[str, Any]:
    measurements: list[dict[str, Any]] = []
    for level in levels:
        runs = [
            measure_resources(argv, level, idle_seconds, settle_seconds, timeout_seconds)
            for _ in range(repetitions)
        ]
        measurements.append({
            "sessions": level,
            "repetitions": repetitions,
            **resource_summary(runs),
            "runs": runs,
        })
    return {
        "kind": "concurrency_memory",
        "phase": "settled_direct_process_sampling",
        "resource_scope": "direct_processes_only",
        "name": name,
        "argv": argv,
        "idle_seconds": idle_seconds,
        "settle_seconds": settle_seconds,
        "timeout_seconds": timeout_seconds,
        "levels": measurements,
        "memory_metric_notes": "Totals cover directly launched processes, not descendants. Each sweep reads PIDs sequentially, not atomically. Incomplete RSS/PSS/CPU totals are unavailable, never partial.",
    }


def telemetry_number(value: Any) -> float | None:
    # JSON permits booleans and Python's decoder accepts NaN/Infinity. Neither
    # is a measurement; missing or invalid metrics must not become fake zeros.
    if type(value) not in (int, float):
        return None
    try:
        number = float(value)
    except OverflowError:
        return None
    return number if math.isfinite(number) and number >= 0 else None


def telemetry_summary(paths: list[str]) -> dict[str, Any]:
    files: list[str] = []
    seen: set[Path] = set()
    records: list[dict[str, Any]] = []
    read_errors: list[dict[str, str]] = []
    ignored_lines = 0
    request_samples: list[dict[str, Any]] = []
    tool_samples: list[dict[str, Any]] = []
    request_metrics = ("elapsed_ms", "ttft_ms", "first_text_delta_ms", "first_reasoning_delta_ms")
    for pattern in paths:
        matches = sorted(glob.glob(pattern)) or [pattern]
        for match in matches:
            path = Path(match)
            try:
                identity = path.resolve()
                if identity in seen:
                    continue
                seen.add(identity)
                if not path.is_file():
                    read_errors.append({"file": str(path), "error": "not_regular_file"})
                    continue
                lines = path.read_text(encoding="utf-8").splitlines()
            except (OSError, UnicodeError) as error:
                read_errors.append({"file": str(path), "error": type(error).__name__})
                continue
            files.append(str(path))
            for line_number, line in enumerate(lines, 1):
                try:
                    value = json.loads(line)
                except json.JSONDecodeError:
                    ignored_lines += 1
                    continue
                if not isinstance(value, dict) or value.get("schema") != "octet.telemetry.v1":
                    ignored_lines += 1
                    continue
                records.append(value)
                if value.get("record") == "model_request_finished":
                    request_samples.append({
                        "file": str(path), "line": line_number,
                        **{key: telemetry_number(value.get(key)) for key in request_metrics},
                    })
                elif value.get("record") == "tool_finished":
                    tool_samples.append({
                        "file": str(path), "line": line_number,
                        "elapsed_ms": telemetry_number(value.get("elapsed_ms")),
                    })
    run_records = [record for record in records if record.get("record") == "run_finished"]
    tool_starts = [record for record in records if record.get("record") == "tool_started"]
    repeated = [telemetry_number(record.get("repeated_recently")) for record in tool_starts]
    result = {
        "kind": "agent_telemetry",
        "files": files,
        "read_errors": read_errors,
        "ignored_lines": ignored_lines,
        "records": len(records),
        "model_requests": len(request_samples),
        "tool_calls": len(tool_starts),
        "repeated_tool_calls": (
            sum(value > 0 for value in repeated)
            if repeated and all(value is not None for value in repeated) else None
        ),
        "runs": len(run_records),
        "completed_runs": sum(record.get("status") == "completed" for record in run_records),
        "request_samples": request_samples,
        "tool_samples": tool_samples,
        "tool_elapsed_ms": summarize([sample["elapsed_ms"] for sample in tool_samples if sample["elapsed_ms"] is not None]),
        "count_semantics": "Counts are observed records, not proof of complete runs or zero activity. Overlapping input paths are read once; copied records in different files are not deduplicated.",
        "timing_semantics": {
            "ttft_ms": "Agent observer: request-attempt start to first text OR reasoning output delta; not first visible text, wire TTFT, process startup, or UI readiness. Historical producers may count empty deltas.",
            "first_text_delta_ms": "Agent observer: request-attempt start to first nonempty text delta; unavailable in older telemetry, never inferred from ttft_ms.",
            "first_reasoning_delta_ms": "Agent observer: request-attempt start to first nonempty reasoning delta; unavailable when not emitted, never inferred from ttft_ms.",
            "population": "model_request_finished records only; discarded/retried attempts are not included in timing summaries",
        },
        "usage_semantics": "No usage aggregation. uncached_input_tokens + cache_read_tokens + cache_write_tokens = provider_input_tokens; cache_write_1h_tokens is a cache-write subset; reasoning_tokens is an output subset. total_tokens is octet's canonical total. Never sum run_cumulative snapshots or equate these buckets to another harness's input_tokens without normalization.",
    }
    for metric in request_metrics:
        key = "request_elapsed_ms" if metric == "elapsed_ms" else metric
        result[key] = summarize([sample[metric] for sample in request_samples if sample[metric] is not None])
    return result


def print_summary(report: dict[str, Any]) -> None:
    def metric(summary: dict[str, Any], unit: str) -> str:
        if not summary["count"]:
            return "unavailable (n=0)"
        return f"median {summary['median']} {unit}, p95 {summary['p95']} {unit} (n={summary['count']})"

    print(f"systems benchmark {report['schema']} on {report['environment']['platform']}")
    for measurement in report["measurements"]:
        if measurement["kind"] == "startup":
            print(
                f"  {measurement['name']}: command completion {metric(measurement['duration_ms'], 'ms')}; "
                f"failed {measurement['failed_runs']}/{measurement['repetitions']}"
            )
        elif measurement["kind"] == "idle_memory":
            print(
                f"  {measurement['name']}: direct-process RSS peak {metric(measurement['rss_peak_kib'], 'KiB')}; "
                f"failed {measurement['failed_runs']}/{measurement['repetitions']}"
            )
        elif measurement["kind"] == "concurrency_memory":
            for level in measurement["levels"]:
                print(
                    f"  {measurement['name']} x{level['sessions']}: direct-process RSS total peak "
                    f"{metric(level['rss_peak_kib'], 'KiB')}; failed {level['failed_runs']}/{level['repetitions']}"
                )
        elif measurement["kind"] == "agent_telemetry":
            print(
                f"  telemetry observed: {measurement['runs']} run finishes, "
                f"{measurement['model_requests']} request finishes, {measurement['tool_calls']} tool starts; "
                f"read errors {len(measurement['read_errors'])}, ignored lines {measurement['ignored_lines']}"
            )


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", default="octet", help="default executable for the --version command-completion case")
    parser.add_argument("--repetitions", type=int, default=DEFAULT_REPETITIONS)
    parser.add_argument("--timeout-seconds", type=float, default=DEFAULT_TIMEOUT_SECONDS)
    parser.add_argument("--idle-seconds", type=float, default=DEFAULT_IDLE_SECONDS)
    parser.add_argument("--settle-seconds", type=float, default=DEFAULT_SETTLE_SECONDS)
    parser.add_argument("--command", action="append", type=parse_named_command, default=[], metavar="NAME=ARGV")
    parser.add_argument("--idle-command", type=parse_named_command, metavar="NAME=ARGV")
    parser.add_argument("--concurrency", default="1,2,4", help="comma-separated process counts for --idle-command")
    parser.add_argument("--skip-startup", action="store_true", help="skip startup command measurements")
    parser.add_argument("--skip-idle", action="store_true", help="skip the single-process idle measurement")
    parser.add_argument("--skip-concurrency", action="store_true", help="skip concurrency measurements")
    parser.add_argument("--telemetry", action="append", default=[], metavar="PATH_OR_GLOB")
    parser.add_argument("--output", type=Path, help="write the complete JSON report to this path")
    args = parser.parse_args()
    if args.repetitions < 1:
        parser.error("--repetitions must be positive")
    for option, value, positive in (
        ("--timeout-seconds", args.timeout_seconds, True),
        ("--idle-seconds", args.idle_seconds, True),
        ("--settle-seconds", args.settle_seconds, False),
    ):
        if not math.isfinite(value) or value < 0 or (positive and value == 0):
            parser.error(f"{option} must be finite and {'positive' if positive else 'non-negative'}")
    if not args.binary or "\0" in args.binary:
        parser.error("--binary must be a nonempty executable without NUL")
    # Validate the entire invocation before launching even the version command.
    try:
        levels = [int(value) for value in args.concurrency.split(",")]
    except ValueError:
        parser.error("--concurrency must be comma-separated positive integers")
    if any(level < 1 for level in levels):
        parser.error("--concurrency values must be positive")

    measurements: list[dict[str, Any]] = []
    if not args.skip_startup:
        command_cases = [("version_command", [args.binary, "--version"]), *args.command]
        for name, argv in command_cases:
            measurements.append(benchmark_startup(name, argv, args.repetitions, args.timeout_seconds))

    if args.idle_command:
        name, argv = args.idle_command
        if not args.skip_idle:
            measurements.append(
                benchmark_idle(
                    name,
                    argv,
                    args.repetitions,
                    args.idle_seconds,
                    args.settle_seconds,
                    args.timeout_seconds,
                )
            )
        if not args.skip_concurrency:
            measurements.append(
                benchmark_concurrency(
                    name,
                    argv,
                    levels,
                    args.repetitions,
                    args.idle_seconds,
                    args.settle_seconds,
                    args.timeout_seconds,
                )
            )

    if args.telemetry:
        measurements.append(telemetry_summary(args.telemetry))

    report = {
        "schema": SCHEMA,
        "created_unix_ms": int(time.time() * 1000),
        "environment": {
            "platform": platform.platform(),
            "system": platform.system(),
            "os_release": platform.release(),
            "kernel_version": platform.version(),
            "machine": platform.machine(),
            "python": platform.python_version(),
            "cpu_count": os.cpu_count(),
            "cwd": str(Path.cwd()),
        },
        "measurements": measurements,
        "methodology": {
            "startup": "command_completion: subprocess creation through exit, stdout/stderr discarded; default version_command executes --version. No cold-cache, UI-readiness or TTFT claim.",
            "launch_ms": "Popen return latency only (all Popen calls at concurrency); not application or UI readiness; includes all fully launched trials, even incomplete windows",
            "memory": "direct launched PIDs only, never descendants or external servers; RSS and best-effort Linux PSS; inference in a measured PID cannot be separated automatically",
            "cpu": "ps pcpu OS-defined percentage, not interval CPU time; precision/averaging varies by OS; observed 0 is not zero instructions",
            "sampling": "settle then window; 50 ms sleep between sequential OS sweeps, shortened at deadlines; timestamps retain probe duration; this is sampled, not lifetime, peak memory/CPU",
            "timeout": "monotonic seconds from launch through settle and observation; checked between launches/resource sweeps, which can overrun; not a hard real-time limit; cleanup excluded",
            "cleanup": "stdin close then terminate/kill owned POSIX process groups; detached descendants are not discovered; non-POSIX cleanup is direct-process only",
            "statistics": "linear interpolated p95 at (n-1)*0.95; resource peaks use completed runs only; null means unavailable, count is available observations, failed trials retained",
            "concurrency": "complete sums of directly launched processes; missing any PID's metric makes that total unavailable; individual PID samples retained",
            "telemetry": "octet.telemetry.v1 observer timings only, separated by text/reasoning where available; no cross-harness TTFT or token equivalence assumed",
            "environment": "commands inherit environment/config; no cache reset, isolation, inference or network policy is enforced by this runner",
            "history": "v2 methodology does not revise or validate previously captured v1 figures",
        },
    }
    encoded = json.dumps(report, indent=2, sort_keys=True, allow_nan=False) + "\n"
    if args.output:
        args.output.parent.mkdir(parents=True, exist_ok=True)
        args.output.write_text(encoded, encoding="utf-8")
    print_summary(report)
    if args.output:
        print(f"  report: {args.output}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
