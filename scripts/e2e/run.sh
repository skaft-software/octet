#!/usr/bin/env bash
# One command: run the real-binary E2E regression suite against an octet build.
#
#   scripts/e2e/run.sh [--binary PATH] [--checks core|compat|NAME,...] [--keep]
#
# Defaults to the integrator's published binary at ~/src/octet-release/bin/octet-latest.
set -euo pipefail

here="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"
exec python3 "$here/run.py" "$@"
