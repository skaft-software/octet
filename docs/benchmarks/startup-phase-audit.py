#!/usr/bin/env python3
"""Compare offline startup phase boundaries in disposable, credential-free homes.

This is not a PTY/frame or extension-enabled benchmark. Pass two prebuilt binaries;
the script never builds, contacts a provider, or submits a successful inference.
"""

import argparse
import hashlib
import os
from pathlib import Path
import re
import statistics
import subprocess
import tempfile
import time


PHASE = re.compile(r"octet-startup: ([\w.]+) elapsed=(\d+)us")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("before", type=Path)
    parser.add_argument("after", type=Path)
    parser.add_argument("--trials", type=int, default=9)
    args = parser.parse_args()
    if args.trials < 1:
        parser.error("--trials must be positive")
    binaries = {"before": args.before.resolve(), "after": args.after.resolve()}
    records = {name: [] for name in binaries}
    with tempfile.TemporaryDirectory() as root:
        homes = {name: Path(root, name) for name in binaries}
        for home in homes.values():
            (home / ".octet").mkdir(parents=True)
            (home / ".octet/config.toml").write_text(
                'model = "gpt-4o-mini"\noffline = true\n'
            )
        # First process per cell primes its home and is excluded. Alternate
        # order to reduce the effect of machine drift on the two medians.
        for trial in range(args.trials + 1):
            order = list(binaries) if trial % 2 == 0 else list(reversed(binaries))
            for name in order:
                env = {
                    key: os.environ[key]
                    for key in ("PATH", "LANG", "TERM", "TMPDIR")
                    if key in os.environ
                }
                env.update(HOME=str(homes[name]), OCTET_STARTUP_TRACE="1")
                start = time.monotonic_ns()
                result = subprocess.run(
                    [str(binaries[name]), "--offline", "--color", "never", "--print", "hi"],
                    env=env,
                    capture_output=True,
                    text=True,
                    timeout=15,
                    check=False,
                )
                elapsed_us = (time.monotonic_ns() - start) // 1000
                phases = {phase: int(us) for phase, us in PHASE.findall(result.stderr)}
                if "bootstrap.ready" not in phases or result.returncode != 1:
                    raise RuntimeError(
                        f"{name} trial {trial}: exit {result.returncode}, "
                        f"phases {sorted(phases)} (expected offline inference failure)"
                    )
                records[name].append((phases, elapsed_us))
    for name, binary in binaries.items():
        rows = records[name][1:]
        print(f"{name} sha256={hashlib.sha256(binary.read_bytes()).hexdigest()}")
        for earlier, later in (
            ("catalog.copilot", "bootstrap.ready"),
            ("catalog.copilot", "session.marker"),
            ("session.marker", "catalog.client"),
        ):
            if later not in rows[0][0]:
                continue
            samples = [phase[later] - phase[earlier] for phase, _ in rows]
            print(f"  {earlier} -> {later} us: {samples}; median={statistics.median(samples)}")
        samples = [wall for _, wall in rows]
        print(f"  spawn-to-exit us: {samples}; median={statistics.median(samples)}")


if __name__ == "__main__":
    main()
