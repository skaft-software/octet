#!/usr/bin/env python3
"""Pi extensions eval: real Pi 1.4.0 vs octet (shipped octet-pi-compat bundle), side by side on a PTY.

    python3 scripts/pi-eval/run.py --legs cold,warm --reps 5 --variant baseline

See README.md. Rows land in `<out>/results.jsonl` as each leg finishes; rerunning
resumes at (case, rep). Harness failures and ceiling hits go to `errors.jsonl`.
"""

from __future__ import annotations

import argparse
import glob
import json
import os
import re
import shutil
import sys
import time
import traceback
import types
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent.parent
sys.path.insert(0, str(HERE))
sys.path.insert(0, str(REPO / "scripts" / "e2e"))

import core  # noqa: E402
import fixture as fx  # noqa: E402
from octet_e2e import session as session_module  # noqa: E402
from octet_e2e.fixtures import Environment, octet_args  # noqa: E402
from octet_e2e.provider import MockProvider  # noqa: E402
from octet_e2e.session import OctetSession  # noqa: E402

COLUMNS, ROWS = 120, 40
DEFAULT_CEILING_S = 180.0
EXIT_GRACE_S = 15.0
SETTLE_S = 1.0


class EvalSession(OctetSession):
    """OctetSession whose stderr goes to a file instead of the PTY.

    `OCTET_STARTUP_TRACE` lines are written to stderr; on the PTY they would
    corrupt the screen model. The parent class hard-wires `stderr=slave`, so
    the spawn runs against a `subprocess` shim for the duration of __init__.
    """

    def __init__(self, *args, stderr_path: Path, **kwargs):
        self.stderr_path = Path(stderr_path)
        self._stderr_handle = open(self.stderr_path, "wb")
        real = session_module.subprocess
        handle = self._stderr_handle

        def popen(*popen_args, **popen_kwargs):
            popen_kwargs["stderr"] = handle
            return real.Popen(*popen_args, **popen_kwargs)

        shim = types.SimpleNamespace(**{name: getattr(real, name) for name in dir(real) if not name.startswith("__")})
        shim.Popen = popen
        session_module.subprocess = shim
        try:
            super().__init__(*args, **kwargs)
        finally:
            session_module.subprocess = real

    def stderr_text(self) -> str:
        self._stderr_handle.flush()
        return self.stderr_path.read_text(errors="replace")

    def close(self, **kwargs) -> int:
        try:
            return super().close(**kwargs)
        finally:
            self._stderr_handle.close()

    def kill(self) -> None:
        super().kill()
        if not self._stderr_handle.closed:
            self._stderr_handle.close()


class LegError(Exception):
    """A leg that produced no scorable result; `klass` goes to errors.jsonl."""

    def __init__(self, klass: str, message: str, extra: dict | None = None):
        super().__init__(message)
        self.klass = klass
        self.extra = extra or {}


# --------------------------------------------------------------------------- #
# One launch
# --------------------------------------------------------------------------- #

def leg_tmp(env: Environment) -> Path:
    """TMPDIR of a (target, rep): shared by the cold launch and the warm launch that follows it.

    Pi's jiti transform cache (`$TMPDIR/jiti`) and Node's compile cache live here, so this
    directory must survive cold -> warm and be wiped with the home on the next cold leg.
    (It used to be per-leg, which made Pi's warm leg start with an empty jiti cache.)
    """
    return env.root / "tmp"


def reset_adapter_cache(extensions_dir: Path) -> None:
    """octet-pi-compat keeps its startup caches in `<extension dir>/.cache` (startup-cache.mjs; the
    host strips OCTET_PI_COMPAT_CACHE from the adapter's environment). That directory is outside the
    leg's HOME, so a cold leg must delete it or it would inherit the previous run's caches."""
    shutil.rmtree(extensions_dir / "octet-pi-compat" / ".cache", ignore_errors=True)


def launch(target: str, env: Environment, fixture: fx.Fixture, octet_bin: Path, work: Path, *, ceiling_s: float, extensions_dir: Path = REPO / "extensions") -> dict:
    """Start one process on a PTY, wait for typeable / full UI, exit it, and collect observations."""
    workspace, tmp = env.workspace, leg_tmp(env)
    tmp.mkdir(parents=True, exist_ok=True)
    for stale in glob.glob(str(tmp / "octet-pi-mirror-*")):  # a previous launch's bridge.json must not be read as this one's
        shutil.rmtree(stale, ignore_errors=True)
    workspace.mkdir(parents=True, exist_ok=True)
    env.sessions.mkdir(parents=True, exist_ok=True)
    if target == "octet":
        binary = octet_bin
        args = octet_args(
            env, name=None, agent=False,
            extension_dirs=[extensions_dir], enable_extensions=["octet-pi-compat"],
        )
        child_env = env.child_env(REPO, extra={"OCTET_STARTUP_TRACE": "1", "TMPDIR": str(tmp)})
        description = "octet + shipped octet-pi-compat bundle (OCTET_PI_AUTO_MIRROR)"
    else:
        binary = Path("node")
        args = [str(fixture.pi_cli), "--model", f"{fx.PROVIDER}/{fx.MODEL}"]
        child_env = {k: v for k, v in os.environ.items() if not k.startswith(("PI_", "OCTET_"))}
        child_env.update(
            HOME=str(env.home), TERM="xterm-256color", COLORTERM="truecolor", LANG="C.UTF-8", LC_ALL="C.UTF-8",
            PI_OFFLINE="1", PI_SKIP_VERSION_CHECK="1", PI_TELEMETRY="0", TMPDIR=str(tmp),
        )
        description = "Pi 1.4.0 (managed release, interactive TUI)"
    session = EvalSession(
        binary, args=args, env=child_env, cwd=workspace, columns=COLUMNS, rows=ROWS,
        log_path=work / "transcript.ansi", stderr_path=work / "stderr.log",
    )
    started = session.started_at
    typeable = full = None
    bridge_bytes: bytes | None = None
    last_bridge_poll = 0.0
    failure: str | None = None
    try:
        while True:
            now = time.monotonic()
            elapsed = now - started
            text = session.screen_text()
            if typeable is None and core.MODEL_LABEL in text:
                typeable = elapsed
            if full is None and core.READY_MARKER in text:
                full = elapsed
            if target == "octet" and bridge_bytes is None and now - last_bridge_poll > 0.25:
                last_bridge_poll = now
                for candidate in glob.glob(str(tmp / "octet-pi-mirror-*" / "octet-pi-compat" / "bridge.json")):
                    try:
                        bridge_bytes = Path(candidate).read_bytes()
                    except OSError:
                        pass
            if full is not None:
                break
            if session.process.poll() is not None:
                failure = f"process exited with {session.process.returncode} before PI-EVAL-READY"
                break
            if elapsed > ceiling_s:
                failure = f"no PI-EVAL-READY within the {ceiling_s:.0f}s ceiling"
                break
            time.sleep(0.01)
        if failure is None:
            deadline = time.monotonic() + SETTLE_S
            while time.monotonic() < deadline:
                time.sleep(0.1)
                if target == "octet" and bridge_bytes is None:
                    for candidate in glob.glob(str(tmp / "octet-pi-mirror-*" / "octet-pi-compat" / "bridge.json")):
                        try:
                            bridge_bytes = Path(candidate).read_bytes()
                        except OSError:
                            pass
        screen = session.screen()
        screen_text, scrollback = screen.text(), screen.scrollback_text()
        exit_code, exit_s = None, None
        if failure is None:
            before = time.monotonic()
            exit_code = session.close(timeout=EXIT_GRACE_S)
            exit_s = time.monotonic() - before
        else:
            session.kill()
            exit_code = session.process.returncode
    except BaseException:
        session.kill()
        raise
    stderr_text = (work / "stderr.log").read_text(errors="replace")
    return {
        "description": description,
        "command": [str(binary), *args],
        "typeable_s": typeable,
        "full_ui_s": full,
        "failure": failure,
        "exit_code": exit_code,
        "exit_s": exit_s,
        "screen": screen_text,
        "scrollback": scrollback,
        "transcript": session.transcript(),
        "stderr": stderr_text,
        "bridge": json.loads(bridge_bytes) if bridge_bytes else None,
        "wall_s": time.monotonic() - started,
    }


# --------------------------------------------------------------------------- #
# Scoring one launch
# --------------------------------------------------------------------------- #

def score_leg(target: str, obs: dict, fixture: fx.Fixture, probe: dict | None) -> tuple[dict, dict]:
    """Returns (grade, meta) for a launch that reached full UI."""
    locked = [entry["name"] for entry in fixture.lock["packages"]]
    fixture_meta = fixture.meta()
    restore = core.terminal_restore(obs["transcript"])
    clean = obs["exit_code"] == 0 and restore["restored"]
    all_text = obs["scrollback"] + "\n" + obs["screen"]
    issues = core.extract_issue_lines(all_text)
    events = core.parse_trace_lines(obs["stderr"])
    trace = core.summarize_trace(events)
    pi_status = core.pi_package_status(probe, locked)
    pi_commands = core.pi_command_names(probe)
    meta: dict = {
        "target": target,
        "description": obs["description"],
        "exit_code": obs["exit_code"],
        "exit_s": obs["exit_s"],
        "terminal_restored": restore["restored"],
        "terminal_leftover": restore["leftover"],
        "startup_issue_lines": issues,
        "peer_excluded_pi14": sorted(fixture_meta.get("peer_excluded", {})),
        "phases": {k: v for k, v in trace.items() if k != "events"},
        "trace_events": trace["events"],
        "fixture_hash": fixture.root.name,
        "pi_loaded_packages": sorted(n for n, s in pi_status.items() if s["status"] == "loaded"),
    }
    if target == "pi":
        packages_value = commands_value = 1.0
        meta["reference"] = True
        meta["pi_not_loaded"] = {n: s for n, s in pi_status.items() if s["status"] != "loaded"}
        meta["pi_command_count"] = len(pi_commands)
    else:
        if obs["bridge"] is None:
            raise LegError("no_bridge", "octet reached full UI but the generated mirror bridge.json was not captured")
        octet_status = core.octet_package_status(
            obs["bridge"], locked, skipped_text=all_text + "\n" + obs["stderr"],
            registered=trace["factory_registered"] or None,
        )
        octet_commands = core.octet_command_names(obs["bridge"])
        packages_value, mismatched = core.packages_parity(pi_status, octet_status)
        commands_value = core.jaccard(pi_commands, octet_commands)
        meta["package_status"] = {n: {"pi": pi_status[n]["status"], "octet": octet_status[n]["status"], "octet_detail": octet_status[n]["detail"], "pi_detail": pi_status[n]["detail"]} for n in locked}
        meta["packages_mismatched"] = mismatched
        meta["commands_pi_only"] = sorted(pi_commands - octet_commands)
        meta["commands_octet_only"] = sorted(octet_commands - pi_commands)
        meta["command_counts"] = {"pi": len(pi_commands), "octet": len(octet_commands)}
    grade = core.build_grade(
        target=target, full_ui_s=obs["full_ui_s"], typeable_s=obs["typeable_s"] if obs["typeable_s"] is not None else obs["full_ui_s"],
        clean_exit=clean, packages_parity_value=packages_value, command_parity_value=commands_value,
    )
    return grade, meta


def trace_turns(obs: dict, grade: dict, meta: dict) -> list[dict]:
    turns = [
        {"role": "system", "content": f"{obs['description']}\nPTY {COLUMNS}x{ROWS}; ready markers: model label {core.MODEL_LABEL!r}, {core.READY_MARKER!r}"},
        {"role": "user", "content": "$ " + " ".join(obs["command"])},
        {"role": "assistant", "content": f"typeable_s={grade['typeable_s']} full_ui_s={grade['full_ui_s']} exit_code={obs['exit_code']} exit_s={obs['exit_s']}\n\nFinal screen:\n{obs['screen']}"},
    ]
    if obs["scrollback"].strip():
        turns.append({"role": "tool_result", "name": "scrollback", "content": obs["scrollback"][-20000:]})
    trace_lines = [e["raw"] for e in core.parse_trace_lines(obs["stderr"])]
    turns.append({"role": "tool_result", "name": "octet-startup trace", "content": "\n".join(trace_lines) or "(no trace lines)"})
    other = "\n".join(l for l in obs["stderr"].splitlines() if not l.startswith("octet-startup: "))
    if other.strip():
        turns.append({"role": "tool_result", "name": "stderr", "content": other[-20000:]})
    if "package_status" in meta:
        table = "\n".join(f"{n}: pi={v['pi']} octet={v['octet']} {v['octet_detail']}" for n, v in meta["package_status"].items())
        turns.append({"role": "tool_result", "name": "package parity", "content": table})
    return turns


# --------------------------------------------------------------------------- #
# Orchestration
# --------------------------------------------------------------------------- #

def make_env(homes: Path, target: str, rep: int, port: int) -> Environment:
    return Environment(root=homes / f"{target}_rep{rep}", name=f"{target}_rep{rep}", provider_port=port, model=f"custom/{fx.MODEL}")


def prepare_home(fixture: fx.Fixture, env: Environment, *, cold: bool) -> bool:
    """Cold: a fresh copy of the fixture home. Warm: the existing home; returns False if it must be primed."""
    if cold or not env.home.exists():
        if env.root.exists():
            shutil.rmtree(env.root)
        env.root.mkdir(parents=True)
        fx.copy_tree(fixture.home, env.home)
        return cold
    return True


def run_case(target: str, leg: str, rep: int, *, args, fixture: fx.Fixture, probe, probe_error, out: Path, homes: Path) -> None:
    cid = core.case_id(target, leg)
    provider = MockProvider(model_id=fx.MODEL)
    provider.default_reply("PI-EVAL-UNUSED")
    started = time.monotonic()
    try:
        env = make_env(homes, target, rep, provider.port)
        primed = prepare_home(fixture, env, cold=(leg == "cold"))
        if target == "octet" and (leg == "cold" or not primed):
            reset_adapter_cache(args.extensions_dir)
        fx.write_provider_config(env.home, provider.port)
        work = out / "work" / f"{cid}_rep{rep}"
        work.mkdir(parents=True, exist_ok=True)
        if leg == "warm" and not primed:
            print(f"  (priming a warm home for {target} rep{rep}; this launch is not measured)")
            (work / "prime").mkdir(parents=True, exist_ok=True)
            launch(target, env, fixture, args.octet, work / "prime", ceiling_s=args.ceiling, extensions_dir=args.extensions_dir)
        obs = launch(target, env, fixture, args.octet, work, ceiling_s=args.ceiling, extensions_dir=args.extensions_dir)
        if obs["failure"]:
            raise LegError(
                "ceiling" if "ceiling" in obs["failure"] else "no_full_ui", obs["failure"],
                {"screen_tail": core.extract_issue_lines(obs["screen"] + obs["scrollback"])[:5], "last_screen": obs["screen"][-1500:], "stderr_tail": obs["stderr"][-1500:]},
            )
        if probe is None:
            raise LegError("probe", f"Pi probe unavailable: {probe_error}")
        grade, meta = score_leg(target, obs, fixture, probe)
        derived = meta["phases"]["derived"]
        traces = out / "traces"
        traces.mkdir(parents=True, exist_ok=True)
        (traces / f"{cid}_rep{rep}.json").write_text(json.dumps(trace_turns(obs, grade, meta), indent=1))
        row = {
            "prompt_id": cid,
            "prompt": f"{obs['description']}: start on a {COLUMNS}x{ROWS} PTY with the locked Pi package set, {leg} start; measure time to typeable and to {core.READY_MARKER}, then Ctrl+D.",
            "tags": [leg, target],
            "rep": rep,
            "variant": args.variant,
            "stop_reason": "exit",
            "status": "ok",
            "model": fx.MODEL,
            "grade": grade,
            "latency_s": round(obs["wall_s"], 3),
            "exit_s": obs["exit_s"],
            **{k: round(v, 4) for k, v in derived.items() if k in ("prepare_s", "extensions_attached_s", "frame_ready_s", "adapter_load_ready_s")},
            "meta": meta,
        }
        core.append_jsonl(out / "results.jsonl", row)
        print(f"  {cid} rep{rep}: typeable={grade['typeable_s']:.2f}s full_ui={grade['full_ui_s']:.2f}s clean_exit={int(grade['clean_exit'])} pkg_parity={grade['packages_parity']:.2f} cmd_parity={grade['command_parity']:.2f}")
    except LegError as error:
        core.append_jsonl(out / "errors.jsonl", {"prompt_id": cid, "rep": rep, "variant": args.variant, "class": error.klass, "message": str(error), **error.extra})
        print(f"  {cid} rep{rep}: NOT SCORED ({error.klass}): {error}")
    except Exception as error:  # harness bug: never a result row
        core.append_jsonl(out / "errors.jsonl", {"prompt_id": cid, "rep": rep, "variant": args.variant, "class": "harness", "message": f"{type(error).__name__}: {error}", "traceback": traceback.format_exc()[-3000:]})
        print(f"  {cid} rep{rep}: HARNESS ERROR {type(error).__name__}: {error}")
    finally:
        provider.close()
        _ = started


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--octet", type=Path, default=REPO / "target" / "release" / "octet")
    parser.add_argument("--extensions-dir", type=Path, default=REPO / "extensions", help="extensions root holding octet-pi-compat/ (default: this checkout; use a git worktree to measure another adapter revision)")
    parser.add_argument("--legs", default="cold,warm")
    parser.add_argument("--reps", type=int, default=1)
    parser.add_argument("--targets", default="pi,octet")
    parser.add_argument("--variant", default="baseline", help="baseline or vN")
    parser.add_argument("--out", type=Path, default=None)
    parser.add_argument("--baseline", type=Path, default=None, help="results dir of an earlier variant to print deltas against")
    parser.add_argument("--ceiling", type=float, default=DEFAULT_CEILING_S, help="hard per-leg ceiling in seconds")
    parser.add_argument("--cache", type=Path, default=fx.default_cache())
    parser.add_argument("--scratch", type=Path, default=None, help="where leg homes live (default <cache>/scratch)")
    parser.add_argument("--keep", action="store_true", help="keep leg homes after the run")
    parser.add_argument("--report-only", action="store_true", help="print the report for --out and exit")
    parser.add_argument("--force-fixture", action="store_true")
    args = parser.parse_args(argv)

    if not re.fullmatch(r"baseline|v\d+", args.variant):
        parser.error("--variant must be 'baseline' or 'vN'")
    legs, targets = args.legs.split(","), args.targets.split(",")
    if not set(legs) <= {"cold", "warm"} or not set(targets) <= {"pi", "octet"}:
        parser.error("legs are cold,warm; targets are pi,octet")
    args.extensions_dir = args.extensions_dir.resolve()
    if not (args.extensions_dir / "octet-pi-compat" / "extension.toml").exists():
        parser.error(f"--extensions-dir {args.extensions_dir} has no octet-pi-compat/extension.toml")
    out = args.out or REPO / ".claude" / "hillclimb" / "pi-extensions" / args.variant
    out.mkdir(parents=True, exist_ok=True)
    harness = ["scripts/pi-eval/run.py", "scripts/pi-eval/core.py", "scripts/pi-eval/fixture.py", "scripts/pi-eval/pi_probe.mjs", "scripts/pi-eval/sentinel/pi-eval-sentinel.ts", "scripts/pi-eval/packages.lock.json"]
    (out / "_state.json").write_text(json.dumps(core.state_document(args.variant, harness), indent=2))

    if not args.report_only:
        if "octet" in targets and not args.octet.exists():
            print(f"octet binary not found: {args.octet}", file=sys.stderr)
            return 2
        fixture = fx.build_fixture(args.cache, force=args.force_fixture)
        probe = probe_error = None
        try:
            probe = fx.ensure_probe(fixture)
        except Exception as error:
            probe_error = f"{type(error).__name__}: {error}"
            print(f"Pi probe unavailable (octet legs will not be scored): {probe_error}", file=sys.stderr)
        homes = (args.scratch or args.cache / "scratch") / f"run-{os.getpid()}"
        homes.mkdir(parents=True, exist_ok=True)
        done = core.completed_keys(out / "results.jsonl")
        try:
            for rep in range(args.reps):
                # Alternate who goes first so neither target always runs on the warmer machine.
                order = targets if rep % 2 == 0 else list(reversed(targets))
                for leg in legs:
                    for target in order:
                        if (core.case_id(target, leg), rep) in done:
                            continue
                        print(f"rep{rep} {leg} {target}")
                        run_case(target, leg, rep, args=args, fixture=fixture, probe=probe, probe_error=probe_error, out=out, homes=homes)
                if not args.keep:
                    for target in targets:
                        shutil.rmtree(homes / f"{target}_rep{rep}", ignore_errors=True)
        finally:
            if not args.keep:
                shutil.rmtree(homes, ignore_errors=True)
            if "octet" in targets and not args.report_only:
                reset_adapter_cache(args.extensions_dir)  # leave no caches in the checkout

    rows = core.load_jsonl(out / "results.jsonl")
    errors = core.load_jsonl(out / "errors.jsonl")
    counts: dict[str, int] = {}
    for item in errors:
        key = f"{item.get('prompt_id')}:{item.get('class')}"
        counts[key] = counts.get(key, 0) + 1
    baseline = core.aggregate(core.load_jsonl(args.baseline / "results.jsonl")) if args.baseline else None
    print()
    print(core.format_report(core.aggregate(rows), error_counts=counts, baseline=baseline))
    print(f"\nresults: {out / 'results.jsonl'}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
