#!/bin/sh
set -eu

# Exact-version 0.10.3 release tooling. Published assets must match this version.
# See the version-pinned release record for publication and installation evidence.
repository="skaft-software/octet"
version="0.10.3"
tag="v$version"
release_source_commit="3ada5545684a28632ca6b2325a860b3e322b217b"
release_workflow_commit="3ada5545684a28632ca6b2325a860b3e322b217b"
release_base="https://github.com/$repository/releases/download/$tag"
checksum_asset="OCTET_SHA256SUMS"
checksum_bundle_asset="$checksum_asset.sigstore.json"
cosign_version="3.1.3"
cosign_linux_amd64_sha256="4629c757b7618056f8ddd7e2625ae9fdd94c0372a65049520bc7d9df9efc7f71"
cosign_darwin_amd64_sha256="2347488e5d5b25336644024dfeca5601b190e91197a71a917bda44744aff106c"
cosign_darwin_arm64_sha256="5cf948c2f4dfe59687bdd0b8523709067383e03982cc543475c8a7dc70e92a76"
mode="binary"

validate_release_source_commit() {
    case "$release_source_commit" in
        [0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f][0-9a-f]) ;;
        *)
            printf 'installer is not bound to an immutable octet release commit\n' >&2
            return 1
            ;;
    esac
}

verified_archive_sha256() {
    checksums=$1
    bundle=$2
    archive_name=$3
    validate_release_source_commit
    install_pinned_cosign
    ui_stage "Verifying release signature and checksums"

    # The published v0.7.0 checksum signature predates the repository rename.
    # Download from the current repository without changing historical trust.
    signing_repository="$repository"
    case "$version:$release_source_commit" in
        0.7.0:6dcde0620314c554b11719b0bc97835b104f0e47)
            signing_repository="skaft-software/ygg" ;;
    esac
    identity_version=$(printf '%s' "$version" | sed 's/\./\\./g')
    identity="^https://github\\.com/${signing_repository}/\\.github/workflows/release-octet\\.yml@refs/tags/(v${identity_version}|octet-binaries-v${identity_version})$"
    python3 - \
        "$checksums" \
        "$bundle" \
        "$cosign_path" \
        "$identity" \
        "$signing_repository" \
        "$release_workflow_commit" \
        "$archive_name" <<'PY'
import os
import re
import stat
import subprocess
import sys

manifest_path, bundle_path, cosign_path = sys.argv[1:4]
identity, repository, source_commit, archive_name = sys.argv[4:8]
expected_names = {
    "install-octet.ps1",
    "install-octet.sh",
    "octet-0.10.3-aarch64-apple-darwin.tar.gz",
    "octet-0.10.3-x86_64-apple-darwin.tar.gz",
    "octet-0.10.3-x86_64-pc-windows-msvc.zip",
    "octet-0.10.3-x86_64-unknown-linux-gnu.tar.gz",
}
line_pattern = re.compile(r"^([0-9A-Fa-f]{64})  (?:\./)?([A-Za-z0-9_.-]+)$")


def open_private_regular(path, maximum):
    if not hasattr(os, "O_NOFOLLOW"):
        raise RuntimeError("descriptor-relative verification is unavailable")
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW)
    metadata = os.fstat(descriptor)
    if (
        not stat.S_ISREG(metadata.st_mode)
        or metadata.st_nlink != 1
        or metadata.st_size <= 0
        or metadata.st_size > maximum
    ):
        os.close(descriptor)
        raise RuntimeError("release provenance object is unsafe")
    return descriptor


manifest = bundle = None
try:
    manifest = open_private_regular(manifest_path, 1024 * 1024)
    bundle = open_private_regular(bundle_path, 1024 * 1024)
    manifest_reference = f"/dev/fd/{manifest}"
    # Cosign's Darwin build cannot reliably reopen a Sigstore bundle through a
    # shared /dev/fd descriptor. The bundle lives in the installer's private
    # work directory and was opened above with O_NOFOLLOW and a single-link
    # regular-file check, so pass that private path while retaining the validated
    # descriptor until verification completes.
    bundle_reference = bundle_path
    if not os.path.exists(manifest_reference):
        raise RuntimeError("descriptor-relative verification is unavailable")
    command = [
        cosign_path,
        "verify-blob",
        "--bundle", bundle_reference,
        "--certificate-identity-regexp", identity,
        "--certificate-oidc-issuer", "https://token.actions.githubusercontent.com",
        "--certificate-github-workflow-name", "octet binary release",
        "--certificate-github-workflow-repository", repository,
        "--certificate-github-workflow-sha", source_commit,
        manifest_reference,
    ]
    result = subprocess.run(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
        pass_fds=(manifest, bundle),
        timeout=120,
        check=False,
    )
    if result.returncode != 0:
        raise RuntimeError("release checksum provenance verification failed")

    os.lseek(manifest, 0, os.SEEK_SET)
    with os.fdopen(os.dup(manifest), "rb") as source:
        contents = source.read(1024 * 1024 + 1)
    if len(contents) > 1024 * 1024:
        raise RuntimeError("release checksum manifest exceeds its size limit")
    try:
        lines = contents.decode("ascii").splitlines()
    except UnicodeDecodeError as error:
        raise RuntimeError("release checksum manifest is malformed") from error
    entries = {}
    for line in lines:
        match = line_pattern.fullmatch(line)
        if match is None:
            raise RuntimeError("release checksum manifest is malformed")
        digest, name = match.groups()
        if name in entries:
            raise RuntimeError("release checksum manifest contains duplicate entries")
        entries[name] = digest.lower()
    if set(entries) != expected_names:
        raise RuntimeError("release checksum manifest has an unexpected asset set")
    print(entries[archive_name])
except (KeyError, OSError, subprocess.SubprocessError, RuntimeError) as error:
    message = str(error)
    allowed = {
        "descriptor-relative verification is unavailable",
        "release provenance object is unsafe",
        "release checksum provenance verification failed",
        "release checksum manifest exceeds its size limit",
        "release checksum manifest is malformed",
        "release checksum manifest contains duplicate entries",
        "release checksum manifest has an unexpected asset set",
    }
    print(message if message in allowed else "release checksum provenance verification failed", file=sys.stderr)
    raise SystemExit(1)
finally:
    if manifest is not None:
        os.close(manifest)
    if bundle is not None:
        os.close(bundle)
PY
}

