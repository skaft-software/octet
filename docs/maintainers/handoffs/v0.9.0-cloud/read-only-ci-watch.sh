#!/bin/bash
# Read-only, bounded fallback for gh run watch's failing jobs endpoint.
set -uo pipefail
RUN=$1
SHA=$2
PREFIX=$3
COUNT=${4:-240}
finish() { code=$?; printf '%s\n' "$code" > "$PREFIX.exit"; }
trap finish EXIT
printf '%s\n' "$$" > "$PREFIX.pid"
for ((i=0; i<COUNT; i++)); do
  if gh api "repos/skaft-software/octet/actions/runs/$RUN" --jq '{id,head_sha,status,conclusion,html_url}' > "$PREFIX.tmp.json" 2> "$PREFIX.stderr"; then
    mv "$PREFIX.tmp.json" "$PREFIX.snapshot.json"
    values=$(python3 - "$PREFIX.snapshot.json" <<'PY'
import json,sys
v=json.load(open(sys.argv[1]))
print(v['head_sha'],v['status'],v['conclusion'] or 'pending')
PY
)
    read -r head status conclusion <<< "$values"
    test "$head" = "$SHA" || { printf 'FAIL unexpected source %s\n' "$head"; exit 3; }
    if test "$values" != "${previous:-}"; then
      printf '%s %s\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')" "$values"
      previous=$values
    fi
    if test "$status" = completed; then
      test "$conclusion" = success && exit 0
      exit 1
    fi
  else
    printf '%s transport error; no CI conclusion inferred\n' "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  fi
  sleep 30
done
printf 'Observation budget exhausted; reconcile the same run.\n'
exit 124
