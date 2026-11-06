"""Pure parsing, scoring and reporting logic for the Pi extensions eval.

Nothing here spawns a process or touches the network, so `selftest.py` can
exercise all of it on synthetic data.
"""

from __future__ import annotations

import json
import os
import re
import statistics
from pathlib import Path
from typing import Iterable

# --------------------------------------------------------------------------- #
# Constants shared by the harness
# --------------------------------------------------------------------------- #

READY_MARKER = "PI-EVAL-READY"
MODEL_LABEL = "mock-1"
GRADE_METRICS = ("full_ui_s", "typeable_s", "packages_parity", "command_parity", "clean_exit")
#: (id, label, better, unit, description); first entry is the headline metric.
METRIC_DEFS = (
    ("full_ui_s", "full UI s", "lower", "s", "Seconds from process spawn until PI-EVAL-READY is on screen (every configured extension live, session_start delivered)."),
    ("typeable_s", "typeable s", "lower", "s", "Seconds from spawn until the editor is usable (octet: composer/model label; Pi: footer model label)."),
    ("packages_parity", "pkg parity", "higher", "", "Fraction of locked packages whose load status (loaded / not loaded) matches Pi 1.4.0."),
    ("command_parity", "cmd parity", "higher", "", "Jaccard similarity of extension slash-command names against Pi 1.4.0."),
    ("clean_exit", "clean exit", "higher", "", "1 when Ctrl+D exits with code 0 within the grace period and the terminal modes are restored."),
)
PERF_FIELDS = (
    ("latency_s", "leg wall s", "s"),
    ("typeable_s", "typeable s", "s"),
    ("full_ui_s", "full UI s", "s"),
    ("exit_s", "exit s", "s"),
    ("prepare_s", "mirror prepare s", "s"),
    ("extensions_attached_s", "attached s", "s"),
    ("frame_ready_s", "first frame s", "s"),
    ("adapter_load_ready_s", "adapter ready s", "s"),
)


# --------------------------------------------------------------------------- #
# OCTET_STARTUP_TRACE parsing
# --------------------------------------------------------------------------- #

_TRACE_RE = re.compile(
    r"^octet-startup: (?P<phase>\S+) (?:elapsed=(?P<us>\d+)us|count=(?P<count>\d+))(?: (?P<rest>.*))?$"
)


def parse_trace_lines(text: str) -> list[dict]:
    """Parse every `octet-startup:` line; other lines (and partial lines) are ignored."""
    events: list[dict] = []
    for raw in text.splitlines():
        line = raw.strip().replace("\r", "")
        match = _TRACE_RE.match(line)
        if not match:
            continue
        event: dict = {"phase": match["phase"], "raw": line}
        if match["us"] is not None:
            event["elapsed_us"] = int(match["us"])
        else:
            event["count"] = int(match["count"])
        rest = match["rest"] or ""
        event["fields"] = dict(re.findall(r"(\w+)=(\S+)", rest))
        events.append(event)
    return events


def summarize_trace(events: list[dict]) -> dict:
    """Group trace events by origin.

    host:    micros since the octet process entered its runtime
    process: durations of host-side process steps (spawn, initialize)
    adapter: micros since the Node adapter process started (a different origin)
    """
    host: dict[str, float] = {}
    process: dict[str, float] = {}
    adapter: dict[str, float] = {}
    counts: dict[str, int] = {}
    imports: dict[int, float] = {}
    registered: set[int] = set()
    load_ready: dict = {}
    for event in events:
        phase = event["phase"]
        if "count" in event:
            counts[phase] = event["count"]
            continue
        seconds = event["elapsed_us"] / 1e6
        if phase.startswith("adapter."):
            adapter.setdefault(phase, seconds)
            match = re.fullmatch(r"adapter\.factory\.(import|registered)\.(\d+)", phase)
            if match:
                index = int(match[2])
                if match[1] == "import":
                    imports.setdefault(index, seconds)
                else:
                    registered.add(index)
            if phase == "adapter.load.ready":
                load_ready = {
                    "elapsed_s": seconds,
                    "factories": int(event["fields"].get("factories", -1)),
                    "failures": int(event["fields"].get("failures", -1)),
                }
        elif phase.startswith("extensions.process."):
            process.setdefault(phase, seconds)
        else:
            host.setdefault(phase, seconds)
    ordered = [imports[index] for index in sorted(imports)]
    derived: dict[str, float] = {}
    begin = host.get("extensions.pi_mirror.prepare.begin")
    end = host.get("extensions.pi_mirror.prepare.end")
    if begin is not None and end is not None:
        derived["prepare_s"] = end - begin
    if "extensions.attached" in host:
        derived["extensions_attached_s"] = host["extensions.attached"]
    if "frame.ready" in host:
        derived["frame_ready_s"] = host["frame.ready"]
    if load_ready:
        derived["adapter_load_ready_s"] = load_ready["elapsed_s"]
    if ordered:
        derived["adapter_import_span_s"] = ordered[-1] - ordered[0]
    return {
        "host": host,
        "process": process,
        "adapter": adapter,
        "counts": counts,
        "factory_import_s": {str(k): v for k, v in sorted(imports.items())},
        "factory_registered": sorted(registered),
        "load_ready": load_ready,
        "derived": derived,
        "events": len(events),
    }


# --------------------------------------------------------------------------- #
# Terminal restore
# --------------------------------------------------------------------------- #

_PRIVATE_MODE_RE = re.compile(r"\x1b\[\?([0-9;]+)([hl])")
_KITTY_RE = re.compile(r"\x1b\[([<>=])([0-9;]*)u")
_MODIFY_OTHER_RE = re.compile(r"\x1b\[>4(?:;([0-9]+))?m")
_MODE_NAMES = {
    1049: "alt-screen", 1047: "alt-screen", 47: "alt-screen",
    1000: "mouse", 1002: "mouse", 1003: "mouse", 1005: "mouse", 1006: "mouse", 1015: "mouse",
    2004: "bracketed-paste", 1004: "focus-events", 2026: "synchronized-output",
}


def terminal_restore(transcript: bytes | str) -> dict:
    """Check that every terminal mode the process turned on was turned off again.

    Final-state analysis over the raw PTY bytes: the last toggle of each mode
    wins. Cursor must end visible; kitty keyboard pushes must be popped.
    """
    text = transcript.decode("utf-8", "replace") if isinstance(transcript, bytes) else transcript
    # Order matters across kinds only per mode, so one pass over each regex is enough.
    last: dict[int, bool] = {}
    for match in _PRIVATE_MODE_RE.finditer(text):
        for mode in match[1].split(";"):
            if mode.isdigit():
                last[int(mode)] = match[2] == "h"
    leftover: list[str] = []
    seen = set()
    for mode, on in last.items():
        name = _MODE_NAMES.get(mode)
        if name and on and name not in seen:
            seen.add(name)
            leftover.append(f"{name} (?{mode}h never reset)")
    if last.get(25) is False:
        leftover.append("cursor hidden (?25l never undone)")
    depth = 0
    for match in _KITTY_RE.finditer(text):
        kind, args = match[1], match[2]
        if kind == ">":
            depth += 1
        elif kind == "<":
            depth = max(0, depth - (int(args) if args.isdigit() else 1))
    if depth > 0:
        leftover.append(f"kitty keyboard protocol ({depth} push(es) never popped)")
    modify = None
    for match in _MODIFY_OTHER_RE.finditer(text):
        modify = int(match[1]) if match[1] else 0
    if modify:
        leftover.append(f"modifyOtherKeys level {modify} never reset")
    return {"restored": not leftover, "leftover": leftover}


# --------------------------------------------------------------------------- #
# Startup error / issue lines
# --------------------------------------------------------------------------- #

_ISSUE_HEADERS = re.compile(
    r"\[(Extension issues|Extension errors?|Skill conflicts|Prompt conflicts|Theme conflicts|Context issues)\]"
)
_ISSUE_LINES = re.compile(
    r"\[pi-compat\]|octet-pi-compat (?:Error|did not start)|\[octet-pi-compat (?:Error|Warning)[^\]]*\]"
    r"|Pi setup mirror could not start|Failed to load extension|Extension (?:error|failed)"
    r"|\bskipped [^\n]*: |mirror review refused|installed_pi_unavailable"
)


def extract_issue_lines(text: str, *, limit: int = 60) -> list[str]:
    """Startup error/issue lines visible on screen (or in scrollback).

    A Pi `[Extension issues]` style header keeps its block (up to the next
    blank line, 12 lines); loose lines match the octet/adapter diagnostics.
    """
    lines = [line.rstrip() for line in text.splitlines()]
    out: list[str] = []
    seen: set[str] = set()

    def add(item: str) -> None:
        key = item.strip()
        if key and key not in seen and len(out) < limit:
            seen.add(key)
            out.append(item.strip())

    index = 0
    while index < len(lines):
        line = lines[index]
        if _ISSUE_HEADERS.search(line):
            block = [line.strip()]
            index += 1
            blank_run = 0
            while index < len(lines) and len(block) < 12 and blank_run < 2:
                if lines[index].strip():
                    blank_run = 0
                    block.append(lines[index].strip())
                else:
                    blank_run += 1
                index += 1
            add("\n".join(block))
            continue
        if _ISSUE_LINES.search(line):
            add(line)
        index += 1
    return out


# --------------------------------------------------------------------------- #
# Package / command parity
# --------------------------------------------------------------------------- #

_NODE_MODULES = "/node_modules/"


def package_from_path(path: str) -> str | None:
    """`.../node_modules/@scope/name/src/x.ts` -> `@scope/name`; None outside node_modules."""
    normalized = path.replace("\\", "/")
    position = normalized.rfind(_NODE_MODULES)
    if position < 0:
        return None
    parts = [part for part in normalized[position + len(_NODE_MODULES):].split("/") if part]
    if not parts:
        return None
    if parts[0].startswith("@") and len(parts) > 1:
        return f"{parts[0]}/{parts[1]}"
    return parts[0]


def _command_names(commands: Iterable) -> set[str]:
    names = set()
    for command in commands or []:
        name = command.get("name") if isinstance(command, dict) else command
        if isinstance(name, str) and name:
            names.add(name.lstrip("/"))
    return names


def _skipped_reason(entry: str, name: str, skipped: dict[str, str]) -> str | None:
    """The `[pi-compat] skipped <entry>: <why>` reason for this entry, if any.

    Screen text can wrap or truncate the path, so also accept a suffix match or
    the same file name inside the same package.
    """
    base = entry.rsplit("/", 1)[-1]
    for key, reason in skipped.items():
        if key == entry or entry.endswith(key) or key.endswith(entry) or (name in key and key.endswith(base)):
            return reason
    return None


def octet_package_status(
    bridge: dict | None,
    locked: list[str],
    *,
    skipped_text: str = "",
    registered: Iterable[int] | None = None,
) -> dict:
    """Per-package load status for octet from the generated mirror bridge.

    A package is `loaded` when one of its extension entries is in the bridge,
    not listed in `mirror_excluded`, not reported `[pi-compat] skipped`, and (when
    the trace gave registration indices) its factory registered.
    Status values: loaded | excluded | skipped | unregistered | absent.
    """
    status = {name: {"status": "absent", "detail": ""} for name in locked}
    if not bridge:
        return status
    entries = list(bridge.get("extensions") or [])
    excluded = {}
    for item in bridge.get("mirror_excluded") or []:
        entry = item.get("entry") if isinstance(item, dict) else None
        if entry:
            excluded[entry] = str(item.get("reason", ""))[:300]
    skipped: dict[str, str] = {}
    for line in skipped_text.splitlines():
        match = re.search(r"\[pi-compat\] skipped (.*?): (.*)$", line)
        if match:
            skipped[match[1]] = match[2][:300]
    registered_set = None if registered is None else set(registered)
    for index, entry in enumerate(entries):
        name = package_from_path(entry)
        if name not in status:
            continue
        current = status[name]["status"]
        if entry in excluded:
            result = ("excluded", excluded[entry])
        elif (reason := _skipped_reason(entry, name, skipped)) is not None:
            result = ("skipped", reason)
        elif registered_set is not None and registered_set and index not in registered_set:
            result = ("unregistered", "no adapter.factory.registered trace for this factory")
        else:
            result = ("loaded", "")
        # A package with several entries is loaded only if all of them are.
        if current == "absent" or (current == "loaded" and result[0] != "loaded"):
            status[name] = {"status": result[0], "detail": result[1]}
    return status


def pi_package_status(probe: dict | None, locked: list[str]) -> dict:
    """Per-package load status for Pi 1.4.0 from the SDK probe output."""
    status = {name: {"status": "absent", "detail": ""} for name in locked}
    if not probe:
        return status
    for extension in probe.get("extensions") or []:
        name = package_from_path(extension.get("resolvedPath") or extension.get("path") or "")
        if name in status and status[name]["status"] != "error":
            status[name] = {"status": "loaded", "detail": ""}
    for error in probe.get("errors") or []:
        name = package_from_path(error.get("path", ""))
        if name not in status:
            continue
        detail = str(error.get("error", ""))[:300]
        if status[name]["status"] == "loaded":
            # Pi lists the extension and reports e.g. a tool-name conflict: loaded, with an issue.
            status[name]["detail"] = (status[name]["detail"] + " " + detail).strip()
        else:
            status[name] = {"status": "error", "detail": detail}
    return status


def pi_command_names(probe: dict | None, *, exclude_path_suffixes: tuple[str, ...] = ("pi-eval-sentinel.ts",)) -> set[str]:
    names: set[str] = set()
    for extension in (probe or {}).get("extensions") or []:
        path = extension.get("resolvedPath") or extension.get("path") or ""
        if path.endswith(exclude_path_suffixes) or path.startswith("<") or path.startswith("builtin:"):
            continue
        names |= _command_names(extension.get("commands"))
    return names


def octet_command_names(bridge: dict | None) -> set[str]:
    registrations = (bridge or {}).get("registrations") or {}
    return _command_names(registrations.get("commands"))


def jaccard(a: set, b: set) -> float:
    if not a and not b:
        return 1.0
    return len(a & b) / len(a | b)


def packages_parity(pi_status: dict, octet_status: dict) -> tuple[float, list[str]]:
    """Fraction of packages whose loaded/not-loaded state matches Pi; plus the mismatches."""
    if not pi_status:
        return 1.0, []
    mismatched = [
        name
        for name in sorted(pi_status)
        if (pi_status[name]["status"] == "loaded") != (octet_status.get(name, {}).get("status") == "loaded")
    ]
    return 1 - len(mismatched) / len(pi_status), mismatched


def peer_excluded(manifests: dict[str, dict], pi_versions: dict[str, str], semver_satisfies) -> dict[str, list[str]]:
    """Packages whose declared pi peer ranges exclude the Pi 1.4.0 packages.

    `semver_satisfies(version, range) -> bool` is injected (the fixture uses the
    installed `semver` package through node) so this stays unit-testable.
    """
    excluded: dict[str, list[str]] = {}
    for name, manifest in manifests.items():
        reasons = []
        for peer, wanted in (manifest.get("peerDependencies") or {}).items():
            actual = pi_versions.get(peer)
            if actual is None:
                continue
            if not semver_satisfies(actual, wanted):
                reasons.append(f"{peer}@{actual} vs {wanted}")
        if reasons:
            excluded[name] = reasons
    return excluded


# --------------------------------------------------------------------------- #
# Result rows, grading, resume
# --------------------------------------------------------------------------- #

def case_id(target: str, leg: str) -> str:
    return f"{leg}.{target}"


def row_key(row: dict) -> tuple[str, int]:
    return (row["prompt_id"], int(row.get("rep", 0)))


def load_jsonl(path: Path) -> list[dict]:
    rows = []
    if not Path(path).exists():
        return rows
    for line in Path(path).read_text().splitlines():
        line = line.strip()
        if not line:
            continue
        try:
            rows.append(json.loads(line))
        except json.JSONDecodeError:
            continue  # a torn last line from an interrupted run is not a result
    return rows


def append_jsonl(path: Path, row: dict) -> None:
    path = Path(path)
    path.parent.mkdir(parents=True, exist_ok=True)
    with open(path, "a", encoding="utf-8") as handle:
        handle.write(json.dumps(row, sort_keys=True) + "\n")
        handle.flush()
        os.fsync(handle.fileno())


def completed_keys(results_path: Path) -> set[tuple[str, int]]:
    return {row_key(row) for row in load_jsonl(results_path) if row.get("status") == "ok"}


def build_grade(
    *,
    target: str,
    full_ui_s: float,
    typeable_s: float,
    clean_exit: bool,
    packages_parity_value: float,
    command_parity_value: float,
) -> dict:
    return {
        "full_ui_s": round(full_ui_s, 4),
        "typeable_s": round(typeable_s, 4),
        "packages_parity": round(packages_parity_value, 4),
        "command_parity": round(command_parity_value, 4),
        "clean_exit": 1.0 if clean_exit else 0.0,
    }


def state_document(variant: str, harness_paths: list[str]) -> dict:
    return {
        "schema": "hillclimb/v2",
        "flow": "pi-extensions",
        "variant": variant,
        "headline_metric": "full_ui_s",
        "headline": "octet/Pi full_ui_s ratio per leg (cold, warm); lower is better, below 1.0 means octet is faster than Pi",
        "metrics": [
            {"id": ident, "label": label, "kind": "float", "better": better, **({"scale": 1} if ident.endswith("parity") or ident == "clean_exit" else {})}
            for ident, label, better, _unit, _desc in METRIC_DEFS
        ],
        "perf_fields": [{"id": ident, "label": label, "unit": unit} for ident, label, unit in PERF_FIELDS],
        "metrics_md": "\n".join(f"- `{ident}`: {desc} ({better} is better)" for ident, _l, better, _u, desc in METRIC_DEFS),
        "harness_paths": harness_paths,
    }


# --------------------------------------------------------------------------- #
# Statistics and report
# --------------------------------------------------------------------------- #

def median_iqr(values: list[float]) -> tuple[float, float]:
    """Median and interquartile range (inclusive quartiles); IQR 0 for n < 2."""
    if not values:
        raise ValueError("no values")
    median = statistics.median(values)
    if len(values) < 2:
        return median, 0.0
    quartiles = statistics.quantiles(values, n=4, method="inclusive")
    return median, quartiles[2] - quartiles[0]


def aggregate(rows: list[dict]) -> dict[tuple[str, str], dict[str, list[float]]]:
    """(target, leg) -> metric -> values, over status-ok rows."""
    out: dict[tuple[str, str], dict[str, list[float]]] = {}
    for row in rows:
        if row.get("status") != "ok":
            continue
        tags = row.get("tags") or []
        if len(tags) < 2:
            continue
        leg, target = tags[0], tags[1]
        bucket = out.setdefault((target, leg), {})
        for metric, value in (row.get("grade") or {}).items():
            if isinstance(value, (int, float)):
                bucket.setdefault(metric, []).append(float(value))
        for field in ("exit_s", "prepare_s", "extensions_attached_s", "frame_ready_s", "adapter_load_ready_s", "latency_s"):
            if isinstance(row.get(field), (int, float)):
                bucket.setdefault(field, []).append(float(row[field]))
    return out


def ratios(agg: dict, metric: str = "full_ui_s", pi_fallback: dict | None = None) -> dict[str, float | None]:
    """leg -> median(octet metric) / median(pi metric).

    `pi_fallback` (an aggregate, normally the baseline's) supplies Pi's medians for a leg
    this run did not measure, so an octet-only variant still gets ratios.
    """
    out: dict[str, float | None] = {}
    legs = sorted({leg for (_t, leg) in agg})
    for leg in legs:
        octet = agg.get(("octet", leg), {}).get(metric)
        pi = agg.get(("pi", leg), {}).get(metric)
        if not pi and pi_fallback:
            pi = pi_fallback.get(("pi", leg), {}).get(metric)
        if octet and pi and statistics.median(pi) > 0:
            out[leg] = statistics.median(octet) / statistics.median(pi)
        else:
            out[leg] = None
    return out


def _fmt(values: list[float], metric: str) -> str:
    median, iqr = median_iqr(values)
    if metric in ("packages_parity", "command_parity", "clean_exit"):
        return f"{median:.3f}±{iqr:.3f}"
    return f"{median:.2f}±{iqr:.2f}s"


def format_report(agg: dict, *, error_counts: dict | None = None, baseline: dict | None = None) -> str:
    lines: list[str] = []
    metrics = [m[0] for m in METRIC_DEFS] + ["exit_s", "prepare_s", "frame_ready_s"]
    header = f"{'target':<7}{'leg':<6}{'n':>3}  " + "  ".join(f"{m:>17}" for m in metrics)
    lines.append("median±IQR per target x leg x metric")
    lines.append(header)
    lines.append("-" * len(header))
    for (target, leg) in sorted(agg, key=lambda k: (k[1] != "cold", k[1], k[0])):
        bucket = agg[(target, leg)]
        n = max((len(v) for v in bucket.values()), default=0)
        cells = [f"{_fmt(bucket[m], m):>17}" if m in bucket else f"{'-':>17}" for m in metrics]
        lines.append(f"{target:<7}{leg:<6}{n:>3}  " + "  ".join(cells))
    lines.append("")
    pi_from_baseline = baseline is not None and not any(t == "pi" for (t, _l) in agg)
    lines.append("octet / Pi ratio of medians (lower is better; < 1.00 means octet is faster)"
                 + ("; Pi medians taken from the baseline run" if pi_from_baseline else ""))
    for metric in ("full_ui_s", "typeable_s", "exit_s"):
        for leg, value in ratios(agg, metric, baseline if pi_from_baseline else None).items():
            lines.append(f"  {metric:<11}{leg:<6}" + ("n/a" if value is None else f"{value:.2f}x"))
    phase_metrics = ("prepare_s", "extensions_attached_s", "frame_ready_s", "adapter_load_ready_s")
    phase_keys = [k for k in sorted(agg, key=lambda k: (k[1] != "cold", k[1], k[0])) if k[0] == "octet" and any(m in agg[k] for m in phase_metrics)]
    if phase_keys:
        lines.append("")
        lines.append("octet per-phase medians±IQR (host trace; prepare = extensions.pi_mirror.prepare.end-begin, attached/frame = host clock, adapter ready = adapter clock)")
        for key in phase_keys:
            lines.append(f"  {key[1]:<6}" + "  ".join(f"{m}={_fmt(agg[key][m], m)}" for m in phase_metrics if m in agg[key]))
    if error_counts:
        lines.append("")
        lines.append("not scored (errors.jsonl): " + ", ".join(f"{key}={count}" for key, count in sorted(error_counts.items())))
    if baseline is not None:
        lines.append("")
        lines.append("delta vs baseline (median now - median baseline; negative is faster for *_s)")
        for key in sorted(agg, key=lambda k: (k[1] != "cold", k[1], k[0])):
            if key not in baseline:
                continue
            parts = []
            for metric in metrics:
                if metric in agg[key] and metric in baseline[key]:
                    now = statistics.median(agg[key][metric])
                    before = statistics.median(baseline[key][metric])
                    parts.append(f"{metric}={now - before:+.3f}")
            lines.append(f"  {key[0]}.{key[1]}: " + (" ".join(parts) or "no common metrics"))
        base_ratios = ratios(baseline)
        for leg, value in ratios(agg, pi_fallback=baseline if pi_from_baseline else None).items():
            before = base_ratios.get(leg)
            if value is not None and before is not None:
                lines.append(f"  full_ui ratio {leg}: {before:.2f}x -> {value:.2f}x ({value - before:+.2f})")
    return "\n".join(lines)
