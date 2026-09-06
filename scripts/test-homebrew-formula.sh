#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: %s\n' "$0" >&2
}
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
if [[ $# -ne 0 ]]; then
    usage
    exit 2
fi

repository_directory=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
script_directory="$repository_directory/scripts"
fixture_directory="$script_directory/fixtures/homebrew"
generator="$script_directory/generate-homebrew-formula.py"
for command in python3 ruby; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'required Homebrew formula test command is unavailable: %s\n' "$command" >&2
        exit 1
    }
done
for path in \
    "$generator" \
    "$fixture_directory/OCTET_RELEASE_METADATA.json" \
    "$fixture_directory/expected-octet.rb" \
    "$fixture_directory/assets/OCTET_SHA256SUMS"; do
    [[ -f "$path" && ! -L "$path" ]] || {
        printf 'Homebrew formula fixture is missing or linked: %s\n' "$path" >&2
        exit 1
    }
done

work_directory=$(mktemp -d "${TMPDIR:-/tmp}/octet-homebrew-test.XXXXXX")
trap 'rm -rf "$work_directory"' EXIT
formula="$work_directory/octet.rb"
repeat="$work_directory/octet-repeat.rb"

python3 "$generator" \
    "$fixture_directory/OCTET_RELEASE_METADATA.json" \
    --assets-dir "$fixture_directory/assets" \
    --output "$formula"
python3 "$generator" \
    --metadata "$fixture_directory/OCTET_RELEASE_METADATA.json" \
    --assets-dir "$fixture_directory/assets" \
    --output "$repeat"
cmp "$fixture_directory/expected-octet.rb" "$formula"
cmp "$formula" "$repeat"
ruby -c "$formula"

python3 - "$formula" <<'PY'
import pathlib
import sys

formula = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
required = (
    'class Octet < Formula',
    'on_arm do',
    'on_intel do',
    'depends_on :macos',
    'depends_on "ripgrep"',
    'bin.install File.join(root, "octet")',
    'bin.install File.join(root, "octet-host")',
    'sha256 "',
)
for marker in required:
    if marker not in formula:
        raise SystemExit(f"formula is missing required Homebrew contract: {marker}")
if "Cargo.toml" in formula or "api.github.com" in formula:
    raise SystemExit("formula contains a mutable release source")
PY

# Recompute the immutable fixture handoff, not just its display formula. Archive
# filenames alone cannot prove the executable members were cleanly renamed.
python3 "$script_directory/generate-octet-release-metadata.py" \
    0.7.0 v0.7.0 \
    0123456789abcdef0123456789abcdef01234567 \
    abcdef0123456789abcdef0123456789abcdef01 \
    skaft-software/ygg/.github/workflows/release-octet.yml@refs/tags/octet-binaries-v0.7.0 \
    skaft-software/ygg \
    "$fixture_directory/assets/OCTET_SHA256SUMS" \
    "$work_directory/recomputed.json"
cmp "$fixture_directory/OCTET_RELEASE_METADATA.json" "$work_directory/recomputed.json"
python3 - "$fixture_directory/assets" <<'PY'
import pathlib
import sys
import tarfile

assets = pathlib.Path(sys.argv[1])
for target in ("aarch64-apple-darwin", "x86_64-apple-darwin", "x86_64-unknown-linux-gnu"):
    root = f"octet-0.7.0-{target}"
    with tarfile.open(assets / f"{root}.tar.gz", "r:gz") as archive:
        members = archive.getmembers()
        assert {member.name for member in members} == {
            f"{root}/{name}" for name in ("octet", "octet-host", "LICENSE", "README.md")
        }
        assert len(members) == 4 and all(member.isfile() for member in members)
        for name in ("octet", "octet-host"):
            assert archive.getmember(f"{root}/{name}").mode == 0o755
        assert archive.extractfile(f"{root}/octet").read() == b'#!/bin/sh\nprintf "octet 0.7.0\\n"\n'
        assert archive.extractfile(f"{root}/octet-host").read() == b"#!/bin/sh\nexit 0\n"
PY

# A metadata digest or local asset mismatch must stop formula generation before
# it can produce a formula that points at a different native release.
cp "$fixture_directory/OCTET_RELEASE_METADATA.json" "$work_directory/bad.json"
python3 - "$work_directory/bad.json" <<'PY'
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
value = json.loads(path.read_text(encoding="utf-8"))
value["assets"][1]["sha256"] = "0" * 64
path.write_text(json.dumps(value, sort_keys=True), encoding="utf-8")
PY
if python3 "$generator" "$work_directory/bad.json" --assets-dir "$fixture_directory/assets" --output "$work_directory/bad.rb"; then
    echo "formula generator accepted a mismatched immutable asset" >&2
    exit 1
fi

if rg -n 'Cargo\.toml|api\.github\.com|releases/latest|gh release|curl ' "$generator"; then
    echo "formula generator contains a mutable release lookup" >&2
    exit 1
fi

printf 'Homebrew formula generation and offline validation passed\n'
