#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: %s [--fixture] PACKAGE_DIRECTORY VERSION\n' "$0" >&2
}
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
fixture=false
if [[ "${1:-}" == "--fixture" ]]; then
    fixture=true
    shift
fi
if [[ $# -ne 2 ]]; then
    usage
    exit 2
fi

package_directory=$1
version=$2
repository_directory=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
if [[ ! -d "$package_directory" || -L "$package_directory" ]]; then
    printf 'npm package directory must be a real directory: %s\n' "$package_directory" >&2
    exit 1
fi
package_directory=$(CDPATH= cd -- "$package_directory" && pwd -P)
for command in npm uname python3; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'required npm install test command is unavailable: %s\n' "$command" >&2
        exit 1
    }
done

case "$(uname -s):$(uname -m)" in
    Darwin:arm64|Darwin:aarch64) platform_artifact="octet-darwin-arm64-$version.tgz" ;;
    Darwin:x86_64)
        if command -v sysctl >/dev/null 2>&1 \
            && [[ "$(sysctl -in sysctl.proc_translated 2>/dev/null || true)" == 1 ]]; then
            platform_artifact="octet-darwin-arm64-$version.tgz"
        else
            platform_artifact="octet-darwin-x64-$version.tgz"
        fi
        ;;
    Linux:x86_64|Linux:amd64) platform_artifact="octet-linux-x64-gnu-$version.tgz" ;;
    *)
        printf 'npm install smoke does not support this test host: %s %s\n' "$(uname -s)" "$(uname -m)" >&2
        exit 1
        ;;
esac

launcher="$package_directory/octet-$version.tgz"
platform="$package_directory/$platform_artifact"
[[ -f "$launcher" && -f "$platform" ]] || {
    printf 'required local npm tarballs are missing\n' >&2
    exit 1
}

work_directory=$(mktemp -d "${TMPDIR:-/tmp}/octet-npm-install-test.XXXXXX")
trap 'rm -rf "$work_directory"' EXIT
prefix="$work_directory/prefix"
cache="$work_directory/cache"
home="$work_directory/home"
probe_directory="$work_directory/probe"
sentinel="$home/user-data-sentinel"
mkdir -p "$prefix" "$cache" "$home" "$probe_directory" "$work_directory/tmp" \
    "$home/.config" "$home/.local/share" "$home/.cache" "$home/.codex" "$home/.pi"

# Candidate processes inherit no provider credentials, configuration overrides or
# model-selection environment. --offline alone is not inference isolation.
run_isolated() (
    cd "$probe_directory"
    env -i PATH="$PATH" HOME="$home" LANG=C.UTF-8 TERM=dumb \
        XDG_CONFIG_HOME="$home/.config" XDG_DATA_HOME="$home/.local/share" \
        XDG_CACHE_HOME="$home/.cache" CODEX_HOME="$home/.codex" \
        PI_CODING_AGENT_DIR="$home/.pi" TMPDIR="$work_directory/tmp" \
        NPM_CONFIG_USERCONFIG="$home/.npmrc" NPM_CONFIG_GLOBALCONFIG="$home/.npm-globalrc" \
        NPM_CONFIG_CACHE="$cache" NPM_CONFIG_OFFLINE=true NPM_CONFIG_IGNORE_SCRIPTS=true \
        NPM_CONFIG_REGISTRY=http://127.0.0.1:9 NPM_CONFIG_AUDIT=false NPM_CONFIG_FUND=false "$@"
)
printf '%s\n' 'must survive npm uninstall' > "$sentinel"

# Populate npm's local cache from the four produced files. The actual install
# remains offline, so optional dependency resolution cannot silently reach a
# registry and no lifecycle hook can become a network-running installer.
for artifact in "$package_directory"/*.tgz; do
    run_isolated npm cache add --offline --cache "$cache" --ignore-scripts "$artifact" >/dev/null
done
run_isolated npm install \
    --global \
    --prefix "$prefix" \
    --cache "$cache" \
    --offline \
    --ignore-scripts \
    --no-audit \
    --no-fund \
    "$launcher" \
    "$platform" >/dev/null

bin_directory="$prefix/bin"
for command in octet octet-host; do
    [[ -x "$bin_directory/$command" ]] || {
        printf 'npm did not install %s\n' "$command" >&2
        exit 1
    }
done
PATH="$bin_directory:$PATH"
export PATH

[[ ! -e "$bin_directory/ygg" && ! -e "$bin_directory/ygg-host" ]]
[[ "$(run_isolated octet --version)" == "octet $version" ]]
run_isolated octet --help >/dev/null
if $fixture; then
    # Only the synthetic launcher fixtures implement unsolicited hello, --probe
    # and --exit. They must never be mistaken for the real octet CLI contract.
    frame=$(run_isolated octet-host)
    [[ "$(printf '%s\n' "$frame" | awk 'END { print NR }')" == 1 ]]
    printf '%s\n' "$frame" | grep -F '"type":"hello"' >/dev/null

    probe_output=$(run_isolated env OCTET_NPM_TEST_ENV=preserved octet --probe argv-value)
    expected_probe_directory=$(CDPATH= cd -P "$probe_directory" && pwd -P)
    printf '%s\n' "$probe_output" | grep -F "cwd=$expected_probe_directory" >/dev/null
    printf '%s\n' "$probe_output" | grep -F 'arg=argv-value' >/dev/null
    printf '%s\n' "$probe_output" | grep -F 'env=preserved' >/dev/null

    set +e
    run_isolated octet --exit 37
    exit_status=$?
    set -e
    [[ "$exit_status" == 37 ]]
else
    # Real host1 is request-driven. These two commands do not create an App,
    # contact a provider, discover models, or import user credentials.
    run_isolated python3 - "$version" <<'PYHOST'
import json
import subprocess
import sys
requests = [
    {"protocol_version": 1, "request_id": command, "command": command}
    for command in ("hello", "shutdown")
]
response = subprocess.run(
    ["octet-host"], input="".join(json.dumps(request) + "\n" for request in requests),
    text=True, capture_output=True, check=True, timeout=10,
)
frames = [json.loads(line) for line in response.stdout.splitlines()]
assert len(frames) == 2
assert frames[0]["protocol_version"] == 1
assert frames[0]["request_id"] == "hello" and frames[0]["type"] == "hello"
assert frames[0]["data"]["sdk_version"] == sys.argv[1]
assert frames[1]["request_id"] == "shutdown" and frames[1]["type"] == "shutdown"
PYHOST
    run_isolated octet extension list >/dev/null
fi

# Exercise the npm-created symlink and the alternate hoisted optional-package
# layout. Both paths must still reach the same native executable without a
# JavaScript process in the execution path.
ln -s "$bin_directory/octet" "$work_directory/octet-symlink"
[[ "$(run_isolated "$work_directory/octet-symlink" --version)" == "octet $version" ]]
public_root="$prefix/lib/node_modules/@skaft-software/octet"
platform_name=${platform_artifact%-"$version".tgz}
platform_name=${platform_name#octet-}
nested_root="$public_root/node_modules/@skaft-software/$platform_name"
hoisted_root="$prefix/lib/node_modules/@skaft-software/$platform_name"
if [[ -d "$nested_root" && ! -L "$nested_root" && ! -e "$hoisted_root" ]]; then
    mv "$nested_root" "$hoisted_root"
    [[ "$(run_isolated octet --version)" == "octet $version" ]]
fi

run_isolated npm uninstall \
    --global \
    --prefix "$prefix" \
    --offline \
    --ignore-scripts \
    --no-audit \
    --no-fund \
    @skaft-software/octet >/dev/null
[[ ! -e "$bin_directory/octet" && ! -e "$bin_directory/octet-host" ]]
[[ "$(cat "$sentinel")" == 'must survive npm uninstall' ]]
printf 'npm local install, launcher, and uninstall tests passed for %s\n' "$version"
