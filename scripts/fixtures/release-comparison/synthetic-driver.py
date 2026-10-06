#!/usr/bin/env python3
"""Offline contract fixture. Fabricated timestamps, NEVER performance evidence."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

SCHEMA = "octet.release-comparison.driver.v1"
mode = sys.argv[1] if len(sys.argv) > 1 else "pass"
if mode == "version":
    print("synthetic-1")
    sys.exit(0)
if mode == "echo":
    print(json.dumps({"argv": sys.argv[2:], "environment": dict(os.environ)}))
    sys.exit(0)
if mode == "timeout":
    child = subprocess.Popen([sys.executable, "-B", __file__, "sleep"], stdin=subprocess.DEVNULL)
    Path("child.pid").write_text(str(child.pid))
    print("partial-timeout-evidence", flush=True)
    time.sleep(60)
if mode == "sleep":
    time.sleep(60)
    sys.exit(0)
request = json.load(sys.stdin)
case = request["case"]
checks = {name: mode != "incorrect" for name in case["checks"]}
events = [
    {"schema": SCHEMA, "type": "event", "name": "subject_spawn", "ns": 1000000},
    {"schema": SCHEMA, "type": "event", "name": "complete_ready_frame_received", "ns": 3000000},
]
for event in events:
    print(json.dumps(event), flush=True)
metrics = {} if mode == "missing" else {"pty_ready_ms": {"events": [0, 1], "observer": "pty"}}
print(json.dumps({"schema": SCHEMA, "type": "result", "synthetic": True, "correctness": checks,
                  "setup": request["setup"], "metrics": metrics}), flush=True)
if mode == "nonzero":
    sys.exit(3)
