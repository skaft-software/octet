#!/usr/bin/env bash
set -euo pipefail

script_directory=$(cd "$(dirname "$0")" && pwd)
source_installer="$script_directory/install.sh"
work_directory=$(mktemp -d "${TMPDIR:-/tmp}/octet-installer-test.XXXXXX")
trap 'rm -rf "$work_directory"' EXIT
assets="$work_directory/assets"
fake_bin="$work_directory/fake-bin"
installer="$work_directory/install-octet.sh"
version=0.7.1
identity_version=${version//./\\.}
expected_identity="^https://github\\.com/skaft-software/octet/\\.github/workflows/release-octet\\.yml@refs/tags/(v${identity_version}|octet-binaries-v${identity_version})$"
package="octet-$version-aarch64-apple-darwin"
archive_name="$package.tar.gz"
release_commit=0123456789abcdef0123456789abcdef01234567
mkdir -p "$assets" "$fake_bin"

sha256_file() {
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum "$1" | awk '{print $1}'
    else
        shasum -a 256 "$1" | awk '{print $1}'
    fi
}

cat > "$fake_bin/cosign-template" <<'EOF'
#!/bin/sh
set -eu
if [ "${OCTET_TEST_BAD_SIGNATURE:-0}" = 1 ]; then
    exit 1
fi
[ "${1:-}" = verify-blob ] || exit 2
shift
identity=${OCTET_TEST_EXPECTED_IDENTITY:?}
expected_sha=0123456789abcdef0123456789abcdef01234567
saw_bundle=false
saw_identity=false
saw_issuer=false
saw_name=false
saw_repository=false
saw_sha=false
blob=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --bundle)
            [ -f "$2" ] || exit 2
            case "$2" in
                /dev/fd/*) exit 2 ;;
            esac
            saw_bundle=true
            shift 2
            ;;
        --certificate-identity-regexp)
            [ "$2" = "$identity" ] || exit 2
            saw_identity=true
            shift 2
            ;;
        --certificate-oidc-issuer)
            [ "$2" = https://token.actions.githubusercontent.com ] || exit 2
            saw_issuer=true
            shift 2
            ;;
        --certificate-github-workflow-name)
            [ "$2" = 'octet binary release' ] || exit 2
            saw_name=true
            shift 2
            ;;
        --certificate-github-workflow-repository)
            [ "$2" = skaft-software/octet ] || exit 2
            saw_repository=true
            shift 2
            ;;
        --certificate-github-workflow-sha)
            [ "$2" = "$expected_sha" ] || exit 2
            saw_sha=true
            shift 2
            ;;
        --*) exit 2 ;;
        *)
            [ -z "$blob" ] || exit 2
            blob=$1
            shift
            ;;
    esac
done
[ "$saw_bundle" = true ]
[ "$saw_identity" = true ]
[ "$saw_issuer" = true ]
[ "$saw_name" = true ]
[ "$saw_repository" = true ]
[ "$saw_sha" = true ]
[ -f "$blob" ]
printf '%s\n' verified >> "$OCTET_TEST_COSIGN_LOG"
EOF
chmod 0755 "$fake_bin/cosign-template"

cosign_sha256=$(sha256_file "$fake_bin/cosign-template")
python3 - "$source_installer" "$installer" "$release_commit" "$cosign_sha256" <<'PY'
import pathlib
import sys

source = pathlib.Path(sys.argv[1]).read_text(encoding="utf-8")
commit = sys.argv[3]
digest = sys.argv[4]
placeholder = 'release_source_commit="__OCTET_RELEASE_SOURCE_COMMIT__"'
cosign = 'cosign_darwin_arm64_sha256="5cf948c2f4dfe59687bdd0b8523709067383e03982cc543475c8a7dc70e92a76"'
if source.count(placeholder) != 1 or source.count(cosign) != 1:
    raise SystemExit("installer release placeholders changed unexpectedly")
source = source.replace(placeholder, f'release_source_commit="{commit}"')
source = source.replace(cosign, f'cosign_darwin_arm64_sha256="{digest}"')
pathlib.Path(sys.argv[2]).write_text(source, encoding="utf-8")
PY
chmod 0755 "$installer"

make_archive() {
    variant=$1
    rm -rf "$assets"
    mkdir -p "$assets"
    cp "$fake_bin/cosign-template" "$assets/cosign-darwin-arm64"
    printf '%s\n' 'test sigstore bundle' > "$assets/OCTET_SHA256SUMS.sigstore.json"
    python3 - "$assets/$archive_name" "$package" "$variant" <<'PY'
import gzip
import io
import pathlib
import sys
import tarfile

archive = pathlib.Path(sys.argv[1])
package = sys.argv[2]
variant = sys.argv[3]

files = {
    "LICENSE": b"test license\n",
    "README.md": b"# octet\n",
    "octet": b'''#!/bin/sh
case "${1:-}" in
    --version) printf '%s\\n' 'octet 0.7.1' ;;
    --help) printf '%s\\n' 'fake octet help' ;;
    *) exit 0 ;;
esac
''',
    "octet-host": b'''#!/bin/sh
IFS= read -r request
case "$request" in
    *'"request_id":"installer-probe"'*)
        printf '%s\\n' '{"protocol_version":1,"request_id":"installer-probe","seq":1,"type":"hello","data":{"sdk_version":"0.7.1"}}'
        ;;
    *) exit 2 ;;
esac
''',
    "docs/index.md": b"# Docs\n",
    "docs/current-reference.md": b"# octet current reference\n",
    "examples/README.md": b"# Example\n",
    "sdk/README.md": b"# SDK\n",
}
directories = [package, f"{package}/docs", f"{package}/examples", f"{package}/sdk"]

class Zeros(io.RawIOBase):
    def __init__(self, size):
        self.remaining = size
    def readable(self):
        return True
    def readinto(self, target):
        count = min(len(target), self.remaining)
        if count == 0:
            return 0
        target[:count] = b"\0" * count
        self.remaining -= count
        return count


def add_directory(output, name):
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    output.addfile(info)


def add_file(output, name, data, mode=0o644):
    info = tarfile.TarInfo(name)
    info.size = len(data)
    info.mode = mode
    output.addfile(info, io.BytesIO(data))

with archive.open("wb") as raw:
    with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
        with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as output:
            for directory in directories:
                add_directory(output, directory)
            for name, data in files.items():
                if variant == "link" and name == "octet":
                    info = tarfile.TarInfo(f"{package}/octet")
                    info.type = tarfile.SYMTYPE
                    info.linkname = "LICENSE"
                    output.addfile(info)
                else:
                    add_file(
                        output,
                        f"{package}/{name}",
                        data,
                        0o755 if name in {"octet", "octet-host"} else 0o644,
                    )
            if variant == "duplicate":
                add_file(output, f"{package}/README.md", b"duplicate\n")
            elif variant == "portable-collision":
                add_file(output, f"{package}/docs/INDEX.md", b"collision\n")
            elif variant == "traversal":
                add_file(output, f"{package}/docs/../escape", b"escape\n")
            elif variant == "device-name":
                add_file(output, f"{package}/docs/CON.txt", b"device\n")
            elif variant == "unexpected":
                add_file(output, f"{package}/private.txt", b"private\n")
            elif variant == "special":
                info = tarfile.TarInfo(f"{package}/docs/device")
                info.type = tarfile.CHRTYPE
                info.devmajor = 1
                info.devminor = 3
                output.addfile(info)
            elif variant == "many":
                for index in range(4096):
                    add_file(output, f"{package}/docs/member-{index:04d}", b"")
            elif variant == "expanded":
                for index in range(3):
                    size = 44 * 1024 * 1024
                    info = tarfile.TarInfo(f"{package}/docs/large-{index}")
                    info.size = size
                    info.mode = 0o644
                    output.addfile(info, io.BufferedReader(Zeros(size), buffer_size=1024 * 1024))

if variant == "concatenated":
    with archive.open("ab") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w") as output:
                add_file(output, "second-archive", b"unexpected\n")
PY
    {
        printf '%064d  ./install-octet.sh\n' 0
        printf '%s  ./%s\n' "$(sha256_file "$assets/$archive_name")" "$archive_name"
        printf '%064d  ./octet-0.7.1-x86_64-apple-darwin.tar.gz\n' 0
        printf '%064d  ./octet-0.7.1-x86_64-unknown-linux-gnu.tar.gz\n' 0
    } > "$assets/OCTET_SHA256SUMS"
}

cat > "$fake_bin/uname" <<'EOF'
#!/bin/sh
case "${1:-}" in
    -s) printf '%s\n' Darwin ;;
    -m) printf '%s\n' arm64 ;;
    *) exit 2 ;;
esac
EOF
cat > "$fake_bin/cargo" <<'EOF'
#!/bin/sh
echo 'binary installer unexpectedly invoked Cargo' >&2
exit 99
EOF
cat > "$fake_bin/curl" <<'EOF'
#!/bin/sh
set -eu
output=
headers=
url=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --proto|--proto-redir|--max-redirs|--retry|--retry-delay|--connect-timeout|--max-time|--write-out)
            shift 2
            ;;
        --dump-header)
            headers=$2
            shift 2
            ;;
        --output)
            output=$2
            shift 2
            ;;
        --tlsv1.2|--location|--fail|--silent|--show-error)
            shift
            ;;
        https://*)
            url=$1
            shift
            ;;
        *)
            printf 'unexpected curl argument: %s\n' "$1" >&2
            exit 2
            ;;
    esac
done
name=${url##*/}
source="$OCTET_TEST_ASSETS/$name"
if [ "${OCTET_TEST_HARDLINK_ARCHIVE:-0}" = 1 ] && [ "$name" = octet-0.7.1-aarch64-apple-darwin.tar.gz ]; then
    ln "$source" "$output"
else
    cp "$source" "$output"
fi
if [ "${OCTET_TEST_TAMPER_ARCHIVE:-0}" = 1 ] && [ "$name" = octet-0.7.1-aarch64-apple-darwin.tar.gz ]; then
    printf 'tampered' >> "$output"
fi
if [ "${OCTET_TEST_TAMPER_COSIGN:-0}" = 1 ] && [ "$name" = cosign-darwin-arm64 ]; then
    printf 'tampered' >> "$output"
fi
host=${OCTET_TEST_REDIRECT_HOST:-release-assets.githubusercontent.com}
effective="https://$host/test/$name"
printf 'HTTP/1.1 302 Found\r\nLocation: %s\r\n\r\nHTTP/1.1 200 OK\r\n\r\n' \
    "$effective" > "$headers"
printf '%s' "$effective"
EOF
chmod 0755 "$fake_bin/uname" "$fake_bin/cargo" "$fake_bin/curl"

run_installer() {
    test_home=$1
    shift
    mkdir -p "$test_home"
    env \
        HOME="$test_home" \
        SHELL=/bin/sh \
        PATH="$fake_bin:$PATH" \
        OCTET_INSTALL_DIR="$test_home/bin" \
        OCTET_NO_MODIFY_PATH=1 \
        OCTET_TEST_ASSETS="$assets" \
        OCTET_TEST_COSIGN_LOG="$work_directory/cosign.log" \
        OCTET_TEST_EXPECTED_IDENTITY="$expected_identity" \
        "$@" \
        sh "$installer"
}

expect_failure() {
    label=$1
    expected=$2
    shift 2
    test_home="$work_directory/$label-home"
    if run_installer "$test_home" "$@" \
        > "$work_directory/$label.out" 2> "$work_directory/$label.err"; then
        printf 'installer accepted invalid input: %s\n' "$label" >&2
        exit 1
    fi
    grep -F "$expected" "$work_directory/$label.err" >/dev/null
    test ! -e "$test_home/bin/octet"
}

make_archive valid
positive_home="$work_directory/positive-home"
run_installer "$positive_home" > "$work_directory/positive.out"
test -x "$positive_home/bin/octet"
test -x "$positive_home/bin/octet-host"
printf '%s\n' '{"protocol_version":1,"request_id":"installer-probe","command":"hello"}' \
    | "$positive_home/bin/octet-host" \
    | grep -F '"sdk_version":"0.7.1"' >/dev/null
test "$("$positive_home/bin/octet" --version)" = 'octet 0.7.1'
test -f "$positive_home/share/octet/README.md"
test -f "$positive_home/share/octet/docs/index.md"
test -f "$positive_home/share/octet/docs/current-reference.md"
test ! -e "$positive_home/bin/ygg"
test ! -e "$positive_home/bin/ygg-host"
test -f "$positive_home/share/octet/examples/README.md"
test -f "$positive_home/share/octet/sdk/README.md"
test "$(cat "$positive_home/share/octet/.octet-version")" = "$version"
grep -Fx verified "$work_directory/cosign.log" >/dev/null

upgrade_home="$work_directory/upgrade-home"
mkdir -p \
    "$upgrade_home/bin" \
    "$upgrade_home/.octet/sessions" \
    "$upgrade_home/share/octet/docs"
cat > "$upgrade_home/bin/octet" <<'EOF'
#!/bin/sh
printf '%s\n' 'octet 0.4.0'
EOF
cat > "$upgrade_home/bin/octet-host" <<'EOF'
#!/bin/sh
printf '%s\n' 'legacy octet-host 0.4.0'
EOF
chmod 0755 "$upgrade_home/bin/octet" "$upgrade_home/bin/octet-host"
printf '%s\n' 'keep helper' > "$upgrade_home/bin/unrelated-helper"
printf '%s\n' 'keep config' > "$upgrade_home/.octet/config.toml"
printf '%s\n' 'keep session' > "$upgrade_home/.octet/sessions/session.jsonl"
printf '%s\n' 'remove old docs' > "$upgrade_home/share/octet/docs/old.md"
run_installer "$upgrade_home" > "$work_directory/upgrade.out"
test "$("$upgrade_home/bin/octet" --version)" = 'octet 0.7.1'
printf '%s\n' '{"protocol_version":1,"request_id":"installer-probe","command":"hello"}' \
    | "$upgrade_home/bin/octet-host" \
    | grep -F '"sdk_version":"0.7.1"' >/dev/null
grep -Fx 'keep helper' "$upgrade_home/bin/unrelated-helper" >/dev/null
grep -Fx 'keep config' "$upgrade_home/.octet/config.toml" >/dev/null
grep -Fx 'keep session' "$upgrade_home/.octet/sessions/session.jsonl" >/dev/null
test ! -e "$upgrade_home/share/octet/docs/old.md"
test -f "$upgrade_home/share/octet/docs/index.md"
test "$(cat "$upgrade_home/share/octet/.octet-version")" = "$version"

# Clean-break installation must neither consume old environment overrides nor
# touch an unrelated, populated earlier first-party installation/data tree.
legacy_home="$work_directory/legacy-trap-home"
mkdir -p "$legacy_home/bin" "$legacy_home/.ygg/sessions" "$legacy_home/share/ygg"
printf '%s\n' 'untouched old binary' > "$legacy_home/bin/ygg"
printf '%s\n' 'untouched old host' > "$legacy_home/bin/ygg-host"
printf '%s\n' 'untouched old config' > "$legacy_home/.ygg/config.toml"
printf '%s\n' 'untouched old session' > "$legacy_home/.ygg/sessions/session.jsonl"
printf '%s\n' 'untouched old docs' > "$legacy_home/share/ygg/README.md"
run_installer "$legacy_home" \
    YGG_INSTALL_DIR="$work_directory/forbidden-old-bin" \
    YGG_DATA_DIR="$work_directory/forbidden-old-docs" > "$work_directory/legacy-trap.out"
test "$("$legacy_home/bin/octet" --version)" = 'octet 0.7.1'
grep -Fx 'untouched old binary' "$legacy_home/bin/ygg" >/dev/null
grep -Fx 'untouched old host' "$legacy_home/bin/ygg-host" >/dev/null
grep -Fx 'untouched old config' "$legacy_home/.ygg/config.toml" >/dev/null
grep -Fx 'untouched old session' "$legacy_home/.ygg/sessions/session.jsonl" >/dev/null
grep -Fx 'untouched old docs' "$legacy_home/share/ygg/README.md" >/dev/null
test ! -e "$work_directory/forbidden-old-bin"
test ! -e "$work_directory/forbidden-old-docs"

override_home="$work_directory/override-home"
override_data="$work_directory/override-data"
run_installer "$override_home" OCTET_DATA_DIR="$override_data" > "$work_directory/override.out"
test -x "$override_home/bin/octet"
test -x "$override_home/bin/octet-host"
test -f "$override_data/README.md"
test -f "$override_data/docs/index.md"
test -f "$override_data/examples/README.md"
test -f "$override_data/sdk/README.md"
test ! -e "$override_home/share/octet"

expect_failure untrusted 'redirected to an untrusted host' OCTET_TEST_REDIRECT_HOST=example.com
expect_failure signature 'release checksum provenance verification failed' OCTET_TEST_BAD_SIGNATURE=1
expect_failure cosign-tamper 'checksum mismatch for the pinned cosign verifier' OCTET_TEST_TAMPER_COSIGN=1
expect_failure archive-tamper 'checksum mismatch for release archive' OCTET_TEST_TAMPER_ARCHIVE=1
expect_failure hardlink 'downloaded archive is not a private regular file' OCTET_TEST_HARDLINK_ARCHIVE=1

printf '%s  ./%s\n' "$(sha256_file "$assets/$archive_name")" "$archive_name" \
    >> "$assets/OCTET_SHA256SUMS"
expect_failure duplicate-checksum 'release checksum manifest contains duplicate entries'

for case_spec in \
    'link|links or unexpected entry types' \
    'duplicate|duplicate member' \
    'portable-collision|colliding portable paths' \
    'traversal|unsafe or non-portable path' \
    'device-name|unsafe or non-portable path' \
    'unexpected|unexpected layout' \
    'special|links or unexpected entry types' \
    'many|too many members' \
    'expanded|expanded-size limit' \
    'concatenated|trailing or concatenated data'; do
    variant=${case_spec%%|*}
    expected=${case_spec#*|}
    make_archive "$variant"
    expect_failure "$variant" "$expected"
done

printf '%s\n' 'binary installer tests passed'
