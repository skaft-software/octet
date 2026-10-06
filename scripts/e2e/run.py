#!/usr/bin/env python3
"""Entry point: `python3 scripts/e2e/run.py [options]` (or scripts/e2e/run.sh)."""

from __future__ import annotations

import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from octet_e2e.runner import main  # noqa: E402

if __name__ == "__main__":
    raise SystemExit(main())
