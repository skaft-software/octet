#!/bin/sh
# Inspect only local Cargo dependency metadata; never invoke a provider.
set -eu
root=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
tree=$(cargo tree --manifest-path "$root/Cargo.toml" --locked --prefix none)
if printf '%s\n' "$tree" | grep -E '^(octet-agent|octet-coding-agent|sexy-tui-rs) '; then
    echo 'FAIL: provider-only dependency boundary' >&2
    exit 1
fi
printf '%s\n' "$tree" | grep '^octet-ai '
echo 'PASS: no agent, frontend or terminal renderer dependency'
