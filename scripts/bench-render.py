#!/usr/bin/env python3
"""Retain credential-free render_bench JSON with executable/environment identity.

Build separately, then run e.g.:
  python3 scripts/bench-render.py --binary target/profiling/examples/render_bench \
    --build-profile profiling --output /tmp/render-before.json -- \
    --bytes 131072 --chunk-bytes 1024 --width 80 --warmup 1 --repetitions 9

This is generic Markdown API evidence, never octet shell or terminal latency.
The Rust driver owns fresh-state trials, exact correctness and p50/p95. This
wrapper does not build, contact a provider, inherit credentials, or measure RSS.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import math
import os
from pathlib import Path
import platform
import subprocess
import sys
import tempfile
from typing import Any

SCHEMA = "octet.render-bench.evidence.v1"
DRIVER_SCHEMA = "octet.render-bench.v1"
MAX_STDOUT = 8 * 1024 * 1024
MAX_STDERR = 8 * 1024
PHASES = ("ingest", "render", "finalize", "final_render")
METRICS = ("elapsed_ns", "allocation_calls", "allocation_requested_bytes")


def sha256(path: Path) -> str:
    digest = hashlib.sha256()
    with path.open("rb") as stream:
        for chunk in iter(lambda: stream.read(1024 * 1024), b""):
            digest.update(chunk)
    return digest.hexdigest()


def scrubbed_environment(home: Path) -> dict[str, str]:
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
    if os.name == "nt" and "SYSTEMROOT" in os.environ:
        environment["SYSTEMROOT"] = os.environ["SYSTEMROOT"]
    return environment


def percentile(values: list[int], point: float) -> float:
    ordered = sorted(values)
    rank = (len(ordered) - 1) * point
    low, high = math.floor(rank), math.ceil(rank)
    return ordered[low] + (ordered[high] - ordered[low]) * (rank - low)


def validate_result(result: dict[str, Any]) -> None:
    """Fail closed on incompatible/incomplete evidence or incorrect aggregation."""
    if result.get("schema") != DRIVER_SCHEMA:
        raise ValueError("unexpected driver schema")
    if not result.get("cases"):
        raise ValueError("no benchmark cases")
    for case in result["cases"]:
        required_checks = ("live_exact_replay", "final_raw_exact", "final_semantics_exact",
                           "final_copy_exact", "final_output_exact")
        if any(case.get("correctness", {}).get(check) is not True for check in required_checks):
            raise ValueError("case correctness failed or missing")
        if case["mode"] not in ("static", "document", "lines", "tail"):
            raise ValueError("unknown benchmark mode")
        for field in ("source_bytes", "chunk_bytes", "chunk_count"):
            if type(case[field]) is not int or case[field] <= 0:
                raise ValueError("invalid fixture accounting")
        count = case["chunk_count"]
        if count != (case["source_bytes"] + case["chunk_bytes"] - 1) // case["chunk_bytes"]:
            raise ValueError("chunk accounting mismatch")
        trials = case["trials"]
        if len(trials) != result["repetitions"] or not trials:
            raise ValueError("trial count mismatch")
        for trial in trials:
            if trial.get("correctness_passed") is not True:
                raise ValueError("trial correctness failed")
            for metric in (*METRICS, "calls"):
                values = [trial["phases"][phase][metric] for phase in PHASES]
                if any(type(value) is not int or value < 0 for value in values):
                    raise ValueError("invalid raw metric")
                if sum(values) != trial["phases"]["total"][metric]:
                    raise ValueError("phase total mismatch")
            calls = trial["phases"]["ingest"]["calls"]
            if calls != (1 if case["mode"] == "static" else count):
                raise ValueError("ingest call count mismatch")
        for phase in (*PHASES, "total"):
            for metric in METRICS:
                values = [trial["phases"][phase][metric] for trial in trials]
                summary = case["summary"][phase][metric]
                if summary["count"] != len(trials):
                    raise ValueError("summary count mismatch")
                for name, point in (("p50", 0.5), ("p95", 0.95)):
                    # The Rust JSON rounds interpolated values to three decimals.
                    if not math.isclose(summary[name], percentile(values, point), rel_tol=1e-12, abs_tol=0.00051):
                        raise ValueError("percentile mismatch")


def command_output(command: list[str], cwd: Path) -> str | None:
    try:
        return subprocess.run(command, cwd=cwd, check=True, capture_output=True,
                              text=True, timeout=5).stdout.strip()
    except (OSError, subprocess.SubprocessError):
        return None


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path, help="new evidence file (never overwritten)")
    parser.add_argument("--build-profile", required=True, help="caller-supplied label, not inferred from the executable")
    parser.add_argument("--label", default="local", help="e.g. before or candidate")
    parser.add_argument("--timeout", type=float, default=300)
    parser.add_argument("driver_args", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if not math.isfinite(args.timeout) or args.timeout <= 0:
        parser.error("timeout must be positive and finite")
    binary = args.binary.resolve(strict=True)
    if args.output.exists():
        parser.error("output already exists")
    driver_args = args.driver_args
    if driver_args[:1] == ["--"]:
        driver_args = driver_args[1:]
    root = Path(__file__).resolve().parents[1]
    example = root / "crates/sexy-tui-rs/examples/render_bench.rs"
    evidence: dict[str, Any] = {
        "schema": SCHEMA,
        "label": args.label,
        "scope": "generic Markdown parser/layout APIs only; NOT octet shell/frame/terminal/provider latency or RSS",
        "binary_sha256": sha256(binary),
        "binary_name": binary.name,
        "driver_arguments": driver_args,
        "build_profile_label": args.build_profile,
        "identity_note": "binary digest is authoritative; build-profile label is caller-supplied; checkout/compiler observations do not prove binary build provenance",
        "observed_checkout": {
            "revision": command_output(["git", "rev-parse", "HEAD"], root),
            "dirty": bool(command_output(["git", "status", "--porcelain"], root)),
            "example_sha256": sha256(example),
            "runner_sha256": sha256(Path(__file__)),
        },
        "observed_rustc": command_output(["rustc", "--version", "--verbose"], root),
        "platform": {"system": platform.system(), "release": platform.release(), "machine": platform.machine(), "logical_cpus": os.cpu_count()},
        "timeout_seconds": args.timeout,
        "status": "failed",
    }
    with tempfile.TemporaryDirectory(prefix="octet-render-bench-") as directory:
        work = Path(directory)
        home = work / "home"
        home.mkdir()
        environment = scrubbed_environment(home)
        evidence["environment_keys"] = sorted(environment)
        with tempfile.TemporaryFile() as stdout, tempfile.TemporaryFile() as stderr:
            try:
                completed = subprocess.run([str(binary), *driver_args], cwd=work, env=environment,
                                           stdin=subprocess.DEVNULL, stdout=stdout, stderr=stderr,
                                           timeout=args.timeout, check=False)
                evidence["exit_code"] = completed.returncode
                if completed.returncode:
                    evidence["failure"] = "driver_nonzero_exit"
            except subprocess.TimeoutExpired:
                evidence["failure"] = "benchmark_timeout"
            except OSError as error:
                evidence["failure"] = "driver_launch_failed"
                evidence["launch_errno"] = error.errno
            stderr.seek(0)
            diagnostics = stderr.read(MAX_STDERR + 1)
            evidence["stderr"] = diagnostics[:MAX_STDERR].decode("utf-8", errors="replace")
            evidence["stderr_truncated"] = len(diagnostics) > MAX_STDERR
            stdout.seek(0)
            raw = stdout.read(MAX_STDOUT + 1)
            evidence["stdout_truncated"] = len(raw) > MAX_STDOUT
            if len(raw) > MAX_STDOUT:
                evidence["failure"] = "driver_output_limit"
            elif "failure" not in evidence:
                try:
                    result = json.loads(raw)
                    validate_result(result)
                    evidence["result"] = result
                    evidence["status"] = "passed"
                except (ValueError, KeyError, TypeError, AttributeError):
                    evidence["failure"] = "invalid_or_incomplete_driver_evidence"
    if sha256(binary) != evidence["binary_sha256"]:
        evidence["status"] = "failed"
        evidence["failure"] = "binary_changed_during_run"
    args.output.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(args.output, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
    with os.fdopen(descriptor, "w", encoding="utf-8") as output:
        json.dump(evidence, output, sort_keys=True, separators=(",", ":"), allow_nan=False)
        output.write("\n")
    print(json.dumps({"schema": SCHEMA, "status": evidence["status"], "evidence_sha256": sha256(args.output)}))
    return 0 if evidence["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
