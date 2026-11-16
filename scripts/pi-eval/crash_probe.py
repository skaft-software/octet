#!/usr/bin/env python3
"""Supervise the reduced Pi long-session probes; EOF is not an OOM diagnosis."""
from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import signal
import subprocess
import tempfile
import time

REPO = Path(__file__).resolve().parents[2]


def supervise(command, directory, timeout=180):
    """Retain bounded streams, sampled Linux RSS, and observed host wait status."""
    samples = []
    reason = None
    with (directory / "stdout.log").open("wb") as stdout, (directory / "stderr.log").open("wb") as stderr:
        process = subprocess.Popen(command, cwd=REPO, stdout=stdout, stderr=stderr,
                                   stdin=subprocess.DEVNULL, start_new_session=True)
        started = time.monotonic()
        peak = 0
        while process.poll() is None:
            try:
                status = Path(f"/proc/{process.pid}/status").read_text()
                rss = next(int(line.split()[1]) * 1024 for line in status.splitlines() if line.startswith("VmRSS:"))
                peak = max(peak, rss)
                if len(samples) < 64:
                    samples.append({"seconds": round(time.monotonic() - started, 3), "rss_bytes": rss})
            except (OSError, StopIteration):
                pass  # RSS is unavailable off Linux, not zero and not an OOM.
            if time.monotonic() - started > timeout:
                reason = "deadline"
            elif any(path.stat().st_size > 16 * 1024 * 1024 for path in (directory / "stdout.log", directory / "stderr.log")):
                reason = "output_limit"
            if reason:
                os.killpg(process.pid, signal.SIGKILL)
                break
            time.sleep(0.02)
        code = process.wait(timeout=10)
    report = {"command": command, "host_pid": process.pid,
              "host_exit_code": code if code >= 0 else None,
              "host_signal": signal.Signals(-code).name if code < 0 else None,
              "supervisor_action": reason, "host_sampled_peak_rss_bytes": peak or None,
              "host_rss_samples": samples,
              "stderr_tail": (directory / "stderr.log").read_bytes()[-65536:].decode(errors="replace"),
              "scope": "reduced offline fixtures only; child wait status and snapshot transitions are in stdout.log"}
    (directory / "result.json").write_text(json.dumps(report, indent=2) + "\n")
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--out", type=Path, required=True, help="new private evidence directory")
    args = parser.parse_args()
    args.out.mkdir(mode=0o700, parents=True, exist_ok=False)
    reports = []
    for name, files in [("oracle", ["scripts/fixtures/pi-reference/endurance.test.mjs"]),
                        ("adapter", ["--test-name-pattern=long", "extensions/octet-pi-compat/test/session-transport.test.mjs"])]:
        directory = args.out / name
        directory.mkdir(mode=0o700)
        # Never resolve user Pi state, caches, or credentials.
        with tempfile.TemporaryDirectory(prefix="octet-pi-probe-home-") as home, tempfile.TemporaryDirectory(prefix="octet-pi-probe-tmp-") as tmp:
            previous = {key: os.environ.get(key) for key in ("HOME", "TMPDIR")}
            try:
                os.environ.update(HOME=home, TMPDIR=tmp)
                reports.append(supervise(["node", "--test", *files], directory))
            finally:
                for key, value in previous.items():
                    if value is None:
                        os.environ.pop(key, None)
                    else:
                        os.environ[key] = value
    print(json.dumps({"passed": sum(r["host_exit_code"] == 0 for r in reports), "failed": sum(r["host_exit_code"] != 0 for r in reports), "evidence": str(args.out)}))
    return int(any(r["host_exit_code"] != 0 for r in reports))


if __name__ == "__main__":
    raise SystemExit(main())
