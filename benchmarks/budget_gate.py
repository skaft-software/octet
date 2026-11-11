#!/usr/bin/env python3
"""Long-session performance budget gate (stdlib only).

Runs the short scale of long_session_bench.py (100 turns, offline, loopback
fake provider) against one binary and asserts the checked-in budget in
`performance_budget.json`. Any breach fails with its measured value and the
allowed bound, so a regression names the per-turn cost that grew instead of a
generic timeout.

The budget expresses octet's own behaviour only. Absolute ceilings carry
headroom over a calibrated quiet-machine run; growth checks are relative so
runner noise cancels. Re-calibrate on a new runner before trusting
absolute numbers: run the gate once per binary with --calibrate to print the
proposed values beside the committed threshold, then review the numbers and
edit the file by hand.
"""
from __future__ import annotations

import argparse
import importlib.util
import json
import math
from pathlib import Path
import subprocess
import sys

HERE = Path(__file__).resolve().parent
BUDGET_PATH = HERE / "performance_budget.json"
BENCH_PATH = HERE / "long_session_bench.py"

SPEC = importlib.util.spec_from_file_location("long_session_bench", BENCH_PATH)
BENCH = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(BENCH)

SHORT_SCALE = 100
BUDGET_SCHEMA = "octet.long-session.budget.v1"


def load_budget(path: Path = BUDGET_PATH):
    budget = json.loads(path.read_text())
    if budget.get("schema") != BUDGET_SCHEMA:
        raise SystemExit(f"{path}: schema is not {BUDGET_SCHEMA}")
    if not budget.get("checks"):
        raise SystemExit(f"{path}: no budget checks")
    return budget


def require(condition: bool, message: str) -> None:
    if not condition:
        raise SystemExit(f"budget file unusable: {message}")


def finite(value) -> bool:
    return isinstance(value, (int, float)) and not isinstance(value, bool) and math.isfinite(value)


def observed(result: dict) -> dict[str, float]:
    """Flatten every metric the budget books can reference."""
    summary = result["cells"][-1]["summary"]
    turns = result["cells"][-1]["turns"]
    cells = {cell["turns"]: cell for cell in result["cells"]}
    metrics = {
        "turns": float(turns),
        "turn_latency_p50_ms": summary["turn_latency_ms"]["p50"],
        "turn_latency_p95_ms": summary["turn_latency_ms"]["p95"],
        "turn_latency_max_ms": summary["turn_latency_ms"]["max"],
        "reply_frame_p50_ms": summary["reply_frame_ms"]["p50"],
        "reply_frame_p95_ms": summary["reply_frame_ms"]["p95"],
        "settled_p95_ms": summary["settled_ms"]["p95"],
        "settled_max_ms": summary["settled_ms"]["max"],
        "turn_latency_growth_ratio": summary["late_small_p50_ms"] / summary["early_small_p50_ms"]
        if summary["early_small_p50_ms"] > 0 else math.inf,
        "late_small_p50_ms": summary["late_small_p50_ms"],
        "rss_growth_mib_per_100_turns": summary["rss_growth_bytes_per_100_turns"] / (1 << 20),
        "peak_rss_mib": summary["peak_rss_bytes"] / (1 << 20),
        "end_rss_mib": summary["end_rss_bytes"] / (1 << 20),
        "cpu_ms_per_turn": summary["cpu_ms"] / turns,
        "append_bytes_p50_per_turn": summary["append_bytes_per_turn"]["p50"],
        "append_persist_p95_ms": (summary["append_persist_ms_per_turn"] or {}).get("p95", math.inf),
        "session_mib_per_turn": summary["session_bytes"] / turns / (1 << 20),
    }
    for turns_at, cell in cells.items():
        label = f"resume_{turns_at}_ready_ms"
        metrics[label] = cell["resume"]["ready_frame_ms"]
        metrics[f"resume_{turns_at}_rss_mib"] = cell["resume"]["rss_bytes"] / (1 << 20)
    stream = summary["stream"]
    metrics["stream_turn_latency_ms"] = stream["wall_ms"]
    metrics["stream_reply_frame_ms"] = stream["reply_frame_ms"]
    metrics["stream_cpu_ms"] = stream["cpu_ms"]
    return metrics


def evaluate(budget: dict, result: dict) -> tuple[int, list[str]]:
    """Return (breach count, human-readable breach lines), or (-1, ...) if the
    result itself is not the checkable short-scale run the budget expects."""
    if result.get("status") != "passed":
        return -1, [f"benchmark did not pass: {result.get('failure', 'no failure recorded')}"]
    scale_ok = bool(result.get("cells")) and result["cells"][-1]["turns"] == budget["scope"]["short_scale"]
    if not scale_ok:
        return -1, ["the result's largest cell is not the budget's short scale"]
    metrics = observed(result)
    breaches = []
    for label, rule in budget["checks"].items():
        require("cap" in rule, f"{label} has no cap")
        require(label in metrics, f"no observed metric for cap {label!r}")
        if rule["cap"] is None:
            continue
        delivered = (f"{label}: {_format(metrics[label])} "
                     f"(cap {_format(rule['cap'])}, floor {_format(rule.get('floor'))})")
        if not finite(metrics[label]):
            breaches.append(f"{label} unobserved in the result; expected cap {_format(rule['cap'])}")
        elif rule["cap"] < metrics[label]:
            breaches.append(delivered)
        elif rule.get("floor") is not None and metrics[label] < rule["floor"]:
            breaches.append(
                f"{label}: {_format(metrics[label])} fell below floor {_format(rule['floor'])}; "
                "a floor pins a fixture-driven metric against silent shrinkage")
    return len(breaches), breaches


def _format(value):
    if not finite(value):
        return "none"
    return f"{value:.4g}"


def calibrate_lines(budget: dict, result: dict) -> list[str]:
    require(result.get("status") == "passed", "calibration needs a passed result")
    metrics = observed(result)
    lines = []
    for label, rule in budget["checks"].items():
        value = metrics.get(label)
        lines.append(f"{label}: measured {_format(value)}; committed cap "
                     f"{_format(rule.get('cap'))}")
    return lines


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--binary", type=Path, help="octet binary to measure; required without --result")
    parser.add_argument("--result", type=Path, help="existing result JSON to check instead of running")
    parser.add_argument("--budget", type=Path, default=BUDGET_PATH)
    parser.add_argument("--output", type=Path, help="where to store the raw benchmark result JSON")
    parser.add_argument("--timeout", type=float, default=90,
                        help="per-frame settle timeout seconds passed to the benchmark")
    parser.add_argument("--calibrate", action="store_true",
                        help="print measured values beside the committed caps and fail on a breach")
    parser.add_argument("--work-dir", type=Path, help="retain benchmark sessions and diagnostics")
    args = parser.parse_args()

    budget = load_budget(args.budget)
    short_scale = budget["scope"]["short_scale"]
    require(isinstance(short_scale, int) and 10 <= short_scale <= 5000, "short scale out of range")

    if args.result is not None:
        result = json.loads(args.result.read_text())
    else:
        if args.binary is None:
            parser.error("--binary is required without --result")
        binary = args.binary.resolve(strict=True)
        output = args.output
        if output is None:
            output = Path("octet-budget-run.json")
            output.unlink(missing_ok=True)
        command = [sys.executable, str(BENCH_PATH), "--binary", str(binary),
                   "--output", str(output), "--scales", str(short_scale),
                   "--timeout", str(args.timeout)]
        if args.work_dir:
            command += ["--work-dir", str(args.work_dir)]
        print(" ".join(command), flush=True)
        subprocess.run(command, check=True)
        result = json.loads(output.read_text())

    count, breaches = evaluate(budget, result)
    if count < 0:
        for line in breaches:
            print(f"breach: {line}", flush=True)
        print("LONG-SESSION: failed (benchmark did not produce a checkable result)", flush=True)
        return 1
    if args.calibrate:
        print("calibration", flush=True)
        for line in calibrate_lines(budget, result):
            print(f"  {line}", flush=True)
    for line in breaches:
        print(f"breach: {line}", flush=True)
    if breaches:
        print(f"LONG-SESSION: failed ({count} of {len(budget['checks'])} caps breached)", flush=True)
        return 1
    print(f"LONG-SESSION: passed ({len(budget['checks'])} caps)", flush=True)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
