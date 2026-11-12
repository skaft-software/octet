#!/usr/bin/env bash
# Pre-push gate, starting from the owner's gate-reference.sh.
# Update this script and its contract test whenever .github/workflows/ci.yml changes.
set -uo pipefail

usage() {
    cat <<'HELP'
Usage: scripts/gate.sh [--quick] [--base <rev>]

Full mode mirrors the Linux quality/test jobs in .github/workflows/ci.yml.
Only the full gate qualifies a push. Run it on the exact commit being pushed.
--quick       Run fmt, workspace clippy and tests for crates touched since base.
--base <rev>  Quick-mode diff base (default: origin/main). No fetch is performed.
--help        Show this help.

Run on Linux with CI's Rust 1.89.0 (rustfmt/clippy), Rust 1.90.0 (packaging),
Node 22.19.0, Python 3 and ripgrep installed. Full mode installs npm dependencies
as CI does; pre-cache dependencies for offline use. No toolchains are installed.
CARGO_TARGET_DIR and CARGO_BUILD_JOBS are honored. Quick mode is not a push gate.
HELP
}

# Read this workflow's run blocks without requiring a YAML package. Select either
# a named step or a single-line run command. Fail closed if CI changes its shape.
ci_script() {
    python3 - "$1" "$2" <<'PY'
import pathlib
import re
import sys

job, selector = sys.argv[1:]
workflow = pathlib.Path('.github/workflows/ci.yml').read_text()
jobs = dict(re.findall(r'(?ms)^  ([\w-]+):\n(.*?)(?=^  [\w-]+:\n|\Z)',
                       workflow.split('\njobs:\n', 1)[1]))
name = None
matches = []
lines = jobs[job].splitlines()
for i, line in enumerate(lines):
    if line.startswith('      - '):
        name = line.removeprefix('      - name: ') if line.startswith('      - name: ') else None
    match = re.match(r'^(?:      - |        )run: (.*)$', line)
    if not match:
        continue
    command = match[1]
    if command == '|':
        body = []
        for following in lines[i + 1:]:
            if following.strip() and not following.startswith('          '):
                break
            body.append(following[10:])
        command = '\n'.join(body)
    if selector == name or selector == command:
        matches.append(command)
if len(matches) != 1:
    sys.exit(f'CI step missing or ambiguous: {job}: {selector}')
print(matches[0])
PY
}

ci_step() {
    local script
    script=$(ci_script "$1" "$2") || return
    bash -euo pipefail -c "$script"
}

# Give each Python check its own result, even when CI groups them in one step.
ci_checks() {
    local script line rest=
    script=$(ci_script "$1" "$2") || { step "$2" false; return; }
    while IFS= read -r line; do
        if [[ "$line" == python3\ * ]]; then
            step "$line" bash -euo pipefail -c "$line"
        else
            rest+="$line"$'\n'
        fi
    done <<< "$script"
    if [[ -n ${rest//[[:space:]]/} ]]; then
        step "$2" bash -euo pipefail -c "$rest"
    fi
}

step() {
    local label=$1 rc
    shift
    printf '=== %s\n' "$label"
    if "$@"; then
        printf 'PASS %s\n' "$label"
        passed=$((passed + 1))
    else
        rc=$?
        printf 'FAIL %s (exit %s)\n' "$label" "$rc"
        failed=$((failed + 1))
    fi
}

pi_fixture() {
    # CI starts clean; allow reruns without replacing an unrelated fixture link.
    local link=extensions/octet-pi-compat/node_modules/@earendil-works/pi-ai
    if [[ -L "$link" && $(readlink "$link") == "$PWD/scripts/fixtures/pi-reference/node_modules/@earendil-works/pi-ai" ]]; then
        rm "$link" || return
    fi
    ci_step test 'Provide the pinned Pi provider package that installed-Pi acceptance tests resolve'
}

quick_tests() {
    local packages package
    packages=$(python3 - "$base" <<'PY'
import json
from pathlib import Path
import subprocess
import sys

files = subprocess.check_output(['git', 'diff', '--name-only', '-z', sys.argv[1], '--']).decode().split('\0')
metadata = json.loads(subprocess.check_output(['cargo', 'metadata', '--no-deps', '--format-version', '1', '--locked']))
root = Path.cwd()
all_crates = any(f in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain', 'rust-toolchain.toml') or f.startswith('.cargo/') for f in files)
for package in metadata['packages']:
    if package['id'] not in metadata['workspace_members']:
        continue
    directory = Path(package['manifest_path']).parent.relative_to(root).as_posix() + '/'
    if all_crates or any(f.startswith(directory) for f in files):
        print(package['name'])
PY
    ) || return
    if [[ -z "$packages" ]]; then
        echo 'No touched workspace crates; no crate tests selected.'
        return 0
    fi
    local result=0
    while IFS= read -r package; do
        cargo test -p "$package" --all-targets --all-features --profile ci-test --locked --no-fail-fast || result=1
    done <<< "$packages"
    return "$result"
}

main() {
    local quick=0 base=origin/main passed=0 failed=0
    while (($#)); do
        case "$1" in
            --help|-h) usage; return 0 ;;
            --quick) quick=1; shift ;;
            --base)
                if (($# < 2)) || [[ -z "$2" || "$2" == -* ]]; then
                    echo '--base requires a revision' >&2; return 2
                fi
                base=$2; shift 2 ;;
            *) echo "Unknown argument: $1" >&2; usage >&2; return 2 ;;
        esac
    done
    cd "$(dirname "${BASH_SOURCE[0]}")/.." || return 2
    # Keep toolchain/cache locations while isolating tests from the real ~/.octet.
    export CARGO_HOME=${CARGO_HOME:-$HOME/.cargo}
    export RUSTUP_HOME=${RUSTUP_HOME:-$HOME/.rustup}
    export npm_config_cache=${npm_config_cache:-$HOME/.npm}
    export RUSTUP_TOOLCHAIN=${RUSTUP_TOOLCHAIN:-1.89.0}
    export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-$PWD/target}
    export OCTET_PTY_TRACE=1 PYTHONDONTWRITEBYTECODE=1
    local scratch
    scratch=$(mktemp -d /tmp/octet-gate.XXXXXXXX) || return 2
    # The EXIT trap runs after this function returns.
    trap "rm -rf -- '$scratch'" EXIT
    export HOME=$scratch/home TMPDIR=$scratch/tmp RUNNER_TEMP=$scratch/runner
    mkdir -p "$HOME" "$TMPDIR" "$RUNNER_TEMP" || return 2
    # Tests that run cargo/rustc with a scrubbed environment resolve rustup from
    # HOME, as on a CI runner; keep the real toolchains visible from the scratch HOME.
    ln -sfn "$RUSTUP_HOME" "$HOME/.rustup"
    ln -sfn "$CARGO_HOME" "$HOME/.cargo"

    if (( ! quick )); then
        step npm-pi ci_step test 'Install reviewed Pi adapter test dependencies'
        step npm-pi-ref pi_fixture
    fi
    step fmt cargo fmt --all -- --check
    step clippy cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
    if ((quick)); then
        echo 'Quick mode: does not qualify a push.'
        step touched-crate-tests quick_tests
    else
        step test-build cargo test --workspace --all-targets --all-features --profile ci-test --locked --no-run
        step test cargo test --workspace --all-targets --all-features --profile ci-test --locked --no-fail-fast
        step doctest cargo test --workspace --doc --profile ci-test --locked
        step editor-host cargo build -p octet-coding-agent --example native-editor-test-host --profile ci-test --locked
        local target=$CARGO_TARGET_DIR
        [[ "$target" == /* ]] || target=$PWD/$target
        export OCTET_NATIVE_EDITOR_TEST_HOST=$target/ci-test/examples/native-editor-test-host
        local extension root
        while IFS= read -r extension; do
            root="extensions/$extension"
            [[ -f "$root/package.json" && ! -d "$root/tests" ]] || continue
            if [[ "$extension" != octet-pi-compat ]]; then
                step "npm-$extension" npm ci --prefix "$root" --ignore-scripts --no-audit --no-fund
            fi
            step "node-$extension" npm test --prefix "$root"
        done < <(grep -Ev '^[[:space:]]*(#|$)' extensions/release-catalog.txt)
        ci_checks quality 'Test models.dev metadata tooling (offline)'
        ci_checks quality 'Test binary release tooling'
        # Includes changelog policy and current release-note synchronization.
        ci_checks quality 'Test measurement and repository-tooling contracts (no live clients)'
        step package cargo +1.90.0 package --workspace --exclude octet-coding-agent --locked --no-verify
        step no-default cargo check --workspace --all-targets --no-default-features --locked
    fi
    printf 'GATE: %s PASS, %s FAIL\n' "$passed" "$failed"
    ((failed == 0))
}

if [[ ${BASH_SOURCE[0]} == "$0" ]]; then
    main "$@"
fi
