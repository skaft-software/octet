"""Opt-in live observe-act-verify smoke through the computer-use extension.

This drives a real desktop, so it is deliberately not collected by the unit
suite (the file name does not match ``test_*.py``) and refuses to run unless a
person asks for it:

* ``--i-have-an-unlocked-desktop`` must be passed;
* standard input must be an interactive terminal, and the tester must type
  ``yes`` at the prompt;
* it refuses to run when ``CI`` or ``GITHUB_ACTIONS`` is set.

It never provisions or downloads the driver, never grants an operating-system
permission, and never enters credentials. It uses an already provisioned
driver (``/computer-use setup`` inside octet, or ``OCTET_CUA_DRIVER_BINARY``).

The sequence goes through the same extension tools the model uses: status,
list windows, launch an application (Notepad by default), find its window,
type a unique marker into the focused field, read the window state back to
verify the marker, then select-all and delete it and verify it is gone. The
application is left open for the tester to close.

Run from ``extensions/octet-computer-use`` with the repository's test path::

    python tests/live_smoke.py --i-have-an-unlocked-desktop
"""

from __future__ import annotations

import argparse
import json
import os
import sys
import time
import uuid
from pathlib import Path
from typing import Any, Dict, Iterable, List, Mapping, Optional, Tuple

ROOT = Path(__file__).resolve().parents[1]
for entry in (ROOT, ROOT / "vendor"):
    if str(entry) not in sys.path:
        sys.path.insert(0, str(entry))

CONFIRMATION_FLAG = "--i-have-an-unlocked-desktop"
POLL_SECONDS = 20.0


class SmokeRefused(Exception):
    """The environment does not permit an attended live run."""


def refusal_reason(argv_confirmed: bool, environment: Mapping[str, str], interactive: bool) -> Optional[str]:
    """Why this run must not drive the desktop, or ``None`` when it may ask."""

    if environment.get("CI") or environment.get("GITHUB_ACTIONS"):
        return "refusing to drive a desktop from CI; this smoke is for an attended session"
    if not argv_confirmed:
        return f"pass {CONFIRMATION_FLAG} to confirm an unlocked desktop you are watching"
    if not interactive:
        return "standard input is not an interactive terminal; an attended confirmation is required"
    return None


def _text(result: Mapping[str, Any]) -> str:
    parts = result.get("content") or []
    return "\n".join(str(part.get("text", "")) for part in parts if isinstance(part, Mapping))


def _dicts(value: Any) -> Iterable[Dict[str, Any]]:
    if isinstance(value, dict):
        yield value
        for child in value.values():
            yield from _dicts(child)
    elif isinstance(value, list):
        for child in value:
            yield from _dicts(child)


def find_window(structured: Any, needle: str) -> Optional[Tuple[int, int, str]]:
    """The first window record naming ``needle`` in its app or title."""

    needle = needle.lower()
    for record in _dicts(structured):
        pid, window_id = record.get("pid"), record.get("window_id")
        if not isinstance(pid, int) or not isinstance(window_id, int):
            continue
        label = " ".join(
            str(record.get(key, "")) for key in ("app_name", "app", "name", "title", "process_name")
        )
        if needle in label.lower():
            return pid, window_id, label.strip()
    return None


class Smoke:
    def __init__(self, app: str) -> None:
        from octet_computer_use import driver, entrypoint

        override = os.environ.get("OCTET_CUA_DRIVER_BINARY")
        if override:
            binary = Path(override)
            if not binary.is_file():
                raise SmokeRefused(f"OCTET_CUA_DRIVER_BINARY is not a file: {binary}")
            driver.installed_binary = lambda paths: binary
        self.extension, self.computer = entrypoint.create_extension()
        # Screenshots normally go to the host's artifact store. Record their
        # size only; nothing is written to disk.
        self.screenshots: List[int] = []

        def record_artifact(**arguments: Any) -> str:
            self.screenshots.append(len(arguments.get("data") or b""))
            return f"live-smoke-{len(self.screenshots)}"

        self.extension.publish_artifact = record_artifact  # type: ignore[assignment]
        self.app = app
        self.context = {"host": {"model": "live-smoke"}}
        self.steps: List[Dict[str, Any]] = []

    def call(self, tool: str, arguments: Mapping[str, Any]) -> Dict[str, Any]:
        return self.extension._tools[tool].handler(dict(arguments), self.context)

    def step(self, name: str, passed: bool, detail: str) -> bool:
        self.steps.append({"step": name, "passed": passed, "detail": detail[:500]})
        print(f"[{'PASS' if passed else 'FAIL'}] {name}: {detail[:500]}")
        return passed

    def run(self) -> bool:
        status = self.call("computer_use_status", {})
        report = status.get("structured_content") or {}
        if not self.step("status", bool(report.get("installed")), json.dumps(report, sort_keys=True)):
            print("Provision the driver first: run `/computer-use setup` inside octet.")
            return False
        if report.get("permissions") not in (None, "granted"):
            self.step("permissions", False, "grant the driver's OS permissions yourself, then rerun")
            return False

        windows = self.call("computer_use_windows", {})
        if not self.step("observe windows", not windows.get("is_error"), _text(windows)[:200]):
            return False

        launched = self.call("computer_use_launch_app", {"name": self.app})
        if not self.step("act: launch", not launched.get("is_error"), _text(launched)):
            return False

        target = None
        deadline = time.monotonic() + POLL_SECONDS
        while target is None and time.monotonic() < deadline:
            listing = self.call("computer_use_windows", {})
            target = find_window(listing.get("structured_content"), self.app)
            if target is None:
                time.sleep(0.5)
        if not self.step("verify: window appeared", target is not None, repr(target)):
            return False
        pid, window_id, _ = target  # type: ignore[misc]
        window = {"pid": pid, "window_id": window_id}

        marker = f"octet-live-smoke-{uuid.uuid4().hex[:8]}"
        typed = self.call("computer_use_type_text", {**window, "text": marker})
        if not self.step("act: type marker", not typed.get("is_error"), _text(typed)):
            return False
        state = self.call("computer_use_window_state", window)
        seen = marker in json.dumps(state.get("structured_content"), default=str) or marker in _text(state)
        if not self.step("verify: marker visible in window state", seen, marker):
            return False

        self.call("computer_use_hotkey", {**window, "keys": ["ctrl", "a"]})
        cleared = self.call("computer_use_press_key", {**window, "key": "delete"})
        state = self.call("computer_use_window_state", window)
        gone = marker not in json.dumps(state.get("structured_content"), default=str) and marker not in _text(state)
        if not self.step("act+verify: marker removed", not cleared.get("is_error") and gone, marker):
            return False

        desktop = self.call("computer_use_desktop_state", {})
        self.step(
            "observe: desktop screenshot",
            not desktop.get("is_error") and bool(self.screenshots),
            f"{len(self.screenshots)} screenshot(s), bytes={self.screenshots}",
        )
        return all(step["passed"] for step in self.steps)


def main(argv: Optional[List[str]] = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument(CONFIRMATION_FLAG, dest="confirmed", action="store_true")
    parser.add_argument("--app", default="notepad", help="application to launch (default: notepad)")
    parser.add_argument("--report", type=Path, help="write the step results as JSON here")
    args = parser.parse_args(argv)
    reason = refusal_reason(args.confirmed, os.environ, sys.stdin.isatty())
    if reason:
        print(f"live smoke refused: {reason}", file=sys.stderr)
        return 2
    print(
        f"This will launch {args.app!r}, type into it, and delete what it typed, on the desktop "
        "you are looking at. Keep your hands off the keyboard and mouse until it finishes."
    )
    if input("Type yes to continue: ").strip().lower() != "yes":
        print("live smoke cancelled", file=sys.stderr)
        return 2
    smoke: Optional[Smoke] = None
    try:
        smoke = Smoke(args.app)
        passed = smoke.run()
    except SmokeRefused as error:
        print(f"live smoke refused: {error}", file=sys.stderr)
        return 2
    finally:
        if smoke is not None:
            smoke.computer.shutdown()
    if args.report:
        args.report.write_text(json.dumps({"passed": passed, "steps": smoke.steps}, indent=2) + "\n")
    print("live smoke " + ("passed" if passed else "FAILED") + f"; {args.app} is left open for you to close.")
    return 0 if passed else 1


if __name__ == "__main__":
    raise SystemExit(main())
