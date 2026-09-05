#!/usr/bin/env bash
set -euo pipefail

# Rebuild the first-party worker bundle from this checkout and atomically install
# it into the same ~/.octet/extensions location used by cargo run. This is
# intentionally independent of the release packaging clean-tree gate.
repository_directory=$(cd "$(dirname "$0")/.." && pwd)
version=$(python3 - "$repository_directory/Cargo.toml" <<'PY'
import sys
import tomllib
with open(sys.argv[1], "rb") as handle:
    print(tomllib.load(handle)["workspace"]["package"]["version"])
PY
)
staging_directory=$(mktemp -d "${TMPDIR:-/tmp}/octet-subagents-reinstall.XXXXXX")
trap 'rm -rf "$staging_directory"' EXIT

"$repository_directory/scripts/package-octet-extension-release.sh" \
    octet-subagents "$staging_directory" "v$version" \
    "$repository_directory/extensions/octet-subagents"

command=(extension install)
if [[ -f "${HOME}/.octet/extensions/octet-subagents/extension.toml" ]]; then
    command=(extension update)
fi
cargo run --quiet --manifest-path "$repository_directory/Cargo.toml" \
    -p octet-coding-agent -- "${command[@]}" \
    --path "$staging_directory/octet-subagents-$version.tar.gz"

printf 'Reinstalled octet-subagents from %s\n' "$repository_directory/extensions/octet-subagents"
