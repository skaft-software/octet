#!/usr/bin/env bash
# Long-session soak: real octet + pi-compat + local loopback fake provider.
# See fuzz/soak/README.md. Deterministic per --seed.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
root="$(cd "$here/../.." && pwd)"
target_dir="${CARGO_TARGET_DIR:-$root/../target-fuzz}"
turns="${SOAK_TURNS:-300}"
seed="${SOAK_SEED:-0}"
rss_limit="${SOAK_RSS_SLOPE_LIMIT:-0.20}"

octet_bin="${SOAK_OCTET:-$target_dir/release/octet}"
if [[ ! -x "$octet_bin" ]]; then
  octet_bin="$target_dir/debug/octet"
fi
if [[ ! -x "$octet_bin" ]]; then
  echo "octet binary not found; build it first:" >&2
  echo "  CARGO_TARGET_DIR=$target_dir cargo build -p octet-coding-agent --bin octet" >&2
  exit 2
fi

# Run directories live outside the repository (a temp dir inside a git
# worktree causes false failures); override with TMPDIR or SOAK_RUN_ROOT.
export CARGO_TARGET_DIR="$target_dir" TMPDIR="${TMPDIR:-/tmp}"
python3 "$here/guard.py" "${SOAK_RUN_ROOT:-$TMPDIR}" || exit 2
mkdir -p "$TMPDIR"
exec python3 "$here/driver.py" \
  --octet "$octet_bin" \
  --turns "$turns" \
  --seed "$seed" \
  --rss-slope-limit "$rss_limit" \
  --keep "$@"
