#!/usr/bin/env bash
#
# Startup latency of octet, optionally beside fx (vercel-labs/fx), measured the
# way fx's own benchmarks/startup.sh measures: hyperfine without a shell, 100
# runs after 10 warmups, wall clock from process launch to exit.
#
#   A  bench boundary  OCTET_BENCH=1 octet / FX_BENCH=1 fx: arguments parsed and
#                      dispatched to the interactive frontend, then exit
#   B  real CLI        --version and --help through a normal exit
#   C  first frame     benchmarks/first_frame.py (PTY, separate script)
#
# Each command runs with an empty temporary HOME and a cleared environment, so
# no profile, config, session or network work is involved.
#
# Usage:
#   benchmarks/startup.sh [--quick] [OCTET_BINARY [FX_BINARY]]
#
# OCTET_BINARY defaults to target/release/octet (cargo build --release --locked
# -p octet-coding-agent --bin octet). FX_BINARY, when given, is measured with
# the same commands. Requires hyperfine (fx's CI uses 1.19.0) and python3.

set -euo pipefail

runs=100
warmup=10
if [[ "${1:-}" == "--quick" ]]; then
    runs=20
    warmup=3
    shift
fi
root=$(cd "$(dirname "$0")/.." && pwd)
octet=${1:-$root/target/release/octet}
fx=${2:-}

# Commands run from a temporary HOME, so resolve binaries before leaving here.
octet=$(realpath "$octet")
[[ -z "$fx" ]] || fx=$(realpath "$fx")
command -v hyperfine >/dev/null || { echo "error: hyperfine is not installed" >&2; exit 1; }
[[ -x "$octet" ]] || { echo "error: octet binary not found: $octet" >&2; exit 1; }
[[ -z "$fx" || -x "$fx" ]] || { echo "error: fx binary not found: $fx" >&2; exit 1; }

work=$(mktemp -d "${TMPDIR:-/tmp}/octet-startup.XXXXXX")
trap 'rm -rf "$work"' EXIT
results=${RESULTS_DIR:-$work/results}
mkdir -p "$results"

measure() {
    # measure NAME ENVIRONMENT COMMAND...: one hyperfine run from an empty HOME.
    local name=$1 extra=$2
    shift 2
    local home="$work/home-$name"
    rm -rf "$home" && mkdir -p "$home"
    # shellcheck disable=SC2086
    (cd "$home" && env -i PATH="$PATH" HOME="$home" TERM=dumb $extra \
        hyperfine -N --style none --runs "$runs" --warmup "$warmup" \
        --export-json "$results/$name.json" --command-name "$name" "$*" >/dev/null)
}

measure "true" "" "$(command -v true)"
measure "octet-bench" "OCTET_BENCH=1" "$octet"
measure "octet-version" "" "$octet --version"
measure "octet-help" "" "$octet --help"
if [[ -n "$fx" ]]; then
    measure "fx-bench" "FX_BENCH=1 FX_AUTO_UPGRADE=0" "$fx"
    measure "fx-version" "FX_AUTO_UPGRADE=0" "$fx --version"
    measure "fx-help" "FX_AUTO_UPGRADE=0" "$fx help"
fi

python3 - "$results" <<'PY'
import json, pathlib, statistics, sys

print(f"{'command':<16}{'median':>10}{'mean':>10}{'stddev':>10}{'p95':>10}{'min':>10}  runs")
for path in sorted(pathlib.Path(sys.argv[1]).glob("*.json")):
    result = json.loads(path.read_text())["results"][0]
    times = sorted(result["times"])
    p95 = times[min(len(times) - 1, round(0.95 * (len(times) - 1)))]
    ms = lambda value: f"{value * 1000:.3f}"
    print(f"{result['command']:<16}{ms(result['median']):>10}{ms(result['mean']):>10}"
          f"{ms(result['stddev']):>10}{ms(p95):>10}{ms(result['min']):>10}  {len(times)}")
print("milliseconds, wall clock from launch to exit")
PY
[[ -n "${RESULTS_DIR:-}" ]] && echo "raw hyperfine JSON: $results" || echo "set RESULTS_DIR to keep the raw hyperfine JSON"
