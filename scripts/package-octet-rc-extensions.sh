#!/usr/bin/env bash
# Exact-commit, unsigned dogfood bundles. Never install or enable extensions.
set -euo pipefail

if [[ $# -ne 4 ]]; then
    printf 'usage: %s TARGET OUTPUT_DIRECTORY VERSION SOURCE_DIRECTORY\n' "$0" >&2
    exit 2
fi
target=$1
output=$2
version=$3
source=$(cd "$4" && pwd)
case "$target" in
    x86_64-unknown-linux-gnu|x86_64-apple-darwin|aarch64-apple-darwin) suffix= ;;
    x86_64-pc-windows-gnu) suffix=.exe ;;
    *) printf 'unsupported dogfood target: %s\n' "$target" >&2; exit 2 ;;
esac
if ! git -C "$source" diff-index --quiet HEAD --; then
    printf 'dogfood extension source has tracked changes; use a clean commit\n' >&2
    exit 1
fi
native="$source/extensions/octet-codemode/target/$target/release/octet-codemode$suffix"
if [[ ! -f "$native" || ! -s "$native" || -L "$native" ]]; then
    printf 'missing regular native codemode executable: %s\n' "$native" >&2
    exit 1
fi
stage=$(mktemp -d "${TMPDIR:-/tmp}/octet-rc-extensions.XXXXXX")
trap 'rm -rf "$stage"' EXIT
# Tracked files only: no workstation node_modules, caches, or factory settings.
git -C "$source" archive HEAD -- extensions | tar -xf - -C "$stage"
if [[ -n "$suffix" ]]; then
    python3 - "$stage/extensions/octet-codemode/extension.toml" <<'PY'
import pathlib
import sys
path = pathlib.Path(sys.argv[1])
text = path.read_text(encoding="utf-8")
old = 'command = "bin/codemode"'
if text.count(old) != 1:
    raise SystemExit("unexpected codemode entrypoint contract")
path.write_text(text.replace(old, 'command = "bin/codemode.exe"'), encoding="utf-8", newline="\n")
PY
    rm "$stage/extensions/octet-codemode/bin/codemode"
fi
cp "$native" "$stage/extensions/octet-codemode/bin/codemode$suffix"
chmod 0755 "$stage/extensions/octet-codemode/bin/codemode$suffix"
mkdir -p "$output"
while IFS= read -r extension; do
    [[ -n "$extension" ]] || continue
    "$source/scripts/package-octet-extension-release.sh" \
        "$extension" "$output" "$version" "$stage/extensions/$extension"
done < <(grep -Ev '^[[:space:]]*(#|$)' "$stage/extensions/release-catalog.txt")
python3 - "$source" "$output" "$version" "$target" <<'PY'
import hashlib
import json
import pathlib
import subprocess
import sys
source, output, version, target = sys.argv[1:]
output = pathlib.Path(output)
commit = subprocess.check_output(["git", "-C", source, "rev-parse", "HEAD"], text=True).strip()
tree = subprocess.check_output(["git", "-C", source, "rev-parse", "HEAD^{tree}"], text=True).strip()
catalog = subprocess.check_output(["git", "-C", source, "show", "HEAD:extensions/release-catalog.txt"], text=True)
ids = [line.strip() for line in catalog.splitlines() if line.strip() and not line.lstrip().startswith("#")]
archives = [f"{extension}-{version.removeprefix('v')}.tar.gz" for extension in ids]
info = {
    "kind": "unsigned dogfood release candidate; not a signed release",
    "version": version.removeprefix("v"), "target": target,
    "source_commit": commit, "source_tree": tree, "extensions": ids,
    "factory_enablement": "none; installation, review, and consent remain separate",
}
(output / "BUILD-INFO.json").write_text(json.dumps(info, indent=2) + "\n", encoding="utf-8")
paths = sorted(archives + ["BUILD-INFO.json"])
(output / "SHA256SUMS").write_text("".join(
    f"{hashlib.sha256((output / name).read_bytes()).hexdigest()}  {name}\n" for name in paths
), encoding="utf-8")
PY
