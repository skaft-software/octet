#!/usr/bin/env python3
"""Check that a published octet release carries its complete asset set.

This command is the *completeness* half of the repository's "complete assets
before Latest" release ordering: it answers "is every required asset present,
byte-identical to its manifest entry, and accounted for?" It does **not**
establish that the manifests are authentic. Nothing automates promotion yet; see
`docs/release/complete-assets-before-latest.md` for the blocked design and the
signature chain a promotion gate would still need.

Inputs, all local files:

* the GitHub **REST** release document for the tag
  (`gh api repos/{repo}/releases/tags/{tag}`), read with its REST field names
  `tag_name`, `draft`, `prerelease` and `assets[].digest`;
* the exact bytes of the `OCTET_SHA256SUMS`, `SHA256SUMS` and
  `OCTET_RELEASE_METADATA.json` assets attached to that release, each of which
  must match its own published `assets[].digest`;
* `extensions/release-catalog.txt`.

Digest equality against the published asset list is what makes "complete" mean
"these exact bytes are on the release". It is still **not** authentication: a
sender who controls the release document can satisfy every check here without a
signature. Nothing in this command runs cosign.

Exit status is 0 only when the whole union is complete. There is no flag,
environment variable or partial mode that skips a check.
"""

from __future__ import annotations

import argparse
import hashlib
import json
import pathlib
import re
import sys
from typing import Any

SCHEMA = "octet.release.completeness.v1"
METADATA_SCHEMA = "octet.release.metadata.v1"
NATIVE_MANIFEST = "OCTET_SHA256SUMS"
BUNDLE_MANIFEST = "SHA256SUMS"
RELEASE_METADATA = "OCTET_RELEASE_METADATA.json"
SIGNATURE_SUFFIX = ".sigstore.json"
CODEMODE_ID = "octet-codemode"
# `scripts/package-octet-extension-release.sh` accepts exactly these codemode
# targets and `release-serve.yml`'s `build-codemode` matrix builds exactly these.
# `scripts/tests/test_release_completeness.py` fails if the matrix drifts.
CODEMODE_TARGETS = (
    "aarch64-apple-darwin",
    "x86_64-apple-darwin",
    "x86_64-pc-windows-gnu",
    "x86_64-pc-windows-msvc",
    "x86_64-unknown-linux-gnu",
)
VERSION_PATTERN = re.compile(r"[0-9]+\.[0-9]+\.[0-9]+")
TAG_PATTERN = re.compile(r"v[0-9]+\.[0-9]+\.[0-9]+")
COMMIT_PATTERN = re.compile(r"[0-9a-f]{40}")
# `sha256sum ./*.tar.gz` renders `<digest><two spaces>./<name>`; a manifest
# rewritten without the `./` prefix describes the same bytes, so accept both
# spellings and nothing else.
MANIFEST_ENTRY = re.compile(r"^(?P<digest>[0-9a-f]{64}) {2}(?:\./)?(?P<name>[A-Za-z0-9_.-]+)$")


class CompletenessError(Exception):
    """An input cannot be read or interpreted; the asset set is untestable."""


def read_text(path: pathlib.Path, label: str) -> str:
    try:
        return path.read_text(encoding="utf-8")
    except OSError as error:
        raise CompletenessError(f"{label} is not readable: {path}: {error}") from error


def read_bytes(path: pathlib.Path, label: str) -> bytes:
    try:
        return path.read_bytes()
    except OSError as error:
        raise CompletenessError(f"{label} is not readable: {path}: {error}") from error


def read_json(path: pathlib.Path, label: str) -> Any:
    try:
        return json.loads(read_text(path, label))
    except json.JSONDecodeError as error:
        raise CompletenessError(f"{label} is not valid JSON: {path}: {error}") from error


def sha256(payload: bytes) -> str:
    return hashlib.sha256(payload).hexdigest()


def parse_manifest(payload: bytes, label: str) -> dict[str, str]:
    """Map asset name -> sha256 from a `sha256sum` manifest, refusing duplicates."""
    try:
        text = payload.decode("ascii")
    except UnicodeDecodeError as error:
        raise CompletenessError(f"{label} is not ASCII: {error}") from error
    entries: dict[str, str] = {}
    for line in text.splitlines():
        match = MANIFEST_ENTRY.fullmatch(line)
        if match is None:
            raise CompletenessError(f"{label} has a malformed line: {line!r}")
        name = match.group("name")
        if name in entries:
            raise CompletenessError(f"{label} lists {name} twice")
        entries[name] = match.group("digest")
    if not entries:
        raise CompletenessError(f"{label} lists no assets")
    return entries


def catalog_extensions(path: pathlib.Path) -> list[str]:
    """The official extension catalog, comment and blank lines removed."""
    extensions = [
        line.strip()
        for line in read_text(path, "extension catalog").splitlines()
        if line.strip() and not line.lstrip().startswith("#")
    ]
    if not extensions:
        raise CompletenessError("extension catalog lists no extensions")
    return extensions


def native_assets(version: str) -> set[str]:
    """The native publication contract: four platform archives and two installers."""
    return {
        "install-octet.sh",
        "install-octet.ps1",
        f"octet-{version}-aarch64-apple-darwin.tar.gz",
        f"octet-{version}-x86_64-apple-darwin.tar.gz",
        f"octet-{version}-x86_64-unknown-linux-gnu.tar.gz",
        f"octet-{version}-x86_64-pc-windows-msvc.zip",
    }


def bundle_assets(extensions: list[str], version: str) -> set[str]:
    """The exact bundle archive inventory the extension release workflow builds.

    Every catalogued extension publishes one version-named archive. Codemode is
    the exception: it ships a target-native runner, so it must publish exactly
    one archive per supported target and never the historical generic launcher
    bundle.
    """
    archives: set[str] = set()
    for extension in extensions:
        if extension == CODEMODE_ID:
            archives.update(f"{extension}-{version}-{target}.tar.gz" for target in CODEMODE_TARGETS)
        else:
            archives.add(f"{extension}-{version}.tar.gz")
    return archives


def evidence_assets(version: str, tag: str) -> dict[str, list[str]]:
    """Closed, version-derived evidence sets that are not part of the manifest union.

    Both sets are produced by protected jobs in `release-octet.yml` and are named
    from the canonical tag and version, never matched by wildcard. Their presence
    is optional, because each is a post-publication repair or evidence channel;
    when present, every payload must still carry its signature bundle, which is
    what keeps an exact asset-set comparison meaningful.
    """
    installer = f"install-octet-{tag}.sh"
    npm = [
        *(f"octet-{platform}-{version}.tgz" for platform in (
            "darwin-arm64", "darwin-x64", "linux-x64-gnu", "win32-x64",
        )),
        f"octet-{version}.tgz",
        "OCTET_NPM_SHA256SUMS",
        "OCTET_NPM_MANIFEST.json",
    ]
    return {"installer-repair": [installer], "npm-evidence": npm}


def release_state(release: Any) -> tuple[str, bool, bool, dict[str, str]]:
    """Read the GitHub **REST** release document.

    `gh api` returns REST field names. `gh release view --json` returns camelCase
    names instead, so a document without `tag_name` is rejected rather than
    silently compared as null.
    """
    if not isinstance(release, dict):
        raise CompletenessError("release document is not a JSON object")
    if "tag_name" not in release:
        raise CompletenessError(
            "release document is not the REST release document: no tag_name. "
            "Use `gh api repos/{repo}/releases/tags/{tag}`."
        )
    assets: dict[str, str] = {}
    for asset in release.get("assets") or []:
        if not isinstance(asset, dict) or not isinstance(asset.get("name"), str):
            raise CompletenessError("release document carries an asset without a name")
        name = asset["name"]
        if name in assets:
            raise CompletenessError(f"release document lists {name} twice")
        published_digest = asset.get("digest")
        assets[name] = published_digest if isinstance(published_digest, str) else ""
    return (
        release["tag_name"],
        release.get("draft") is False,
        release.get("prerelease") is False,
        assets,
    )


def verify_metadata(
    metadata: Any,
    *,
    version: str,
    tag: str,
    source_commit: str,
    native_entries: dict[str, str],
    native_manifest_payload: bytes,
    problems: list[str],
) -> None:
    if not isinstance(metadata, dict):
        problems.append("release metadata is not a JSON object")
        return
    if metadata.get("schema") != METADATA_SCHEMA:
        problems.append(
            f"release metadata schema is {metadata.get('schema')!r}, expected {METADATA_SCHEMA!r}"
        )
    for field, expected in (("tag", tag), ("version", version)):
        if metadata.get(field) != expected:
            problems.append(
                f"release metadata {field} is {metadata.get(field)!r}, expected {expected!r}"
            )
    if metadata.get("source_commit") != source_commit:
        problems.append(
            f"release metadata source_commit is {metadata.get('source_commit')!r}, "
            f"expected {source_commit!r}"
        )
    checksum_manifest = metadata.get("checksum_manifest")
    if not isinstance(checksum_manifest, dict):
        problems.append("release metadata carries no checksum manifest record")
    else:
        if checksum_manifest.get("name") != NATIVE_MANIFEST:
            problems.append(
                f"release metadata checksum manifest name is {checksum_manifest.get('name')!r}, "
                f"expected {NATIVE_MANIFEST!r}"
            )
        if checksum_manifest.get("sha256") != sha256(native_manifest_payload):
            problems.append("release metadata checksum manifest digest disagrees with the manifest")
    listed: dict[str, str] = {}
    for asset in metadata.get("assets") or []:
        if not isinstance(asset, dict) or not isinstance(asset.get("name"), str):
            problems.append("release metadata carries a malformed asset record")
            continue
        if asset["name"] in listed:
            problems.append(f"release metadata lists the asset twice: {asset['name']}")
            continue
        listed[asset["name"]] = asset.get("sha256") if isinstance(asset.get("sha256"), str) else ""
    if listed != native_entries:
        for name in sorted(set(native_entries) - set(listed)):
            problems.append(f"release metadata omits the native asset: {name}")
        for name in sorted(set(listed) - set(native_entries)):
            problems.append(f"release metadata publishes an unmanifested native asset: {name}")
        for name in sorted(set(listed) & set(native_entries)):
            if listed[name] != native_entries[name]:
                problems.append(f"release metadata digest disagrees with the manifest for: {name}")


def validate(
    *,
    release: Any,
    native_manifest_payload: bytes,
    bundle_manifest_payload: bytes,
    metadata_payload: bytes,
    extensions: list[str],
    version: str,
    tag: str,
    source_commit: str,
) -> tuple[dict[str, Any], list[str]]:
    """Return the receipt and the full list of completeness problems.

    The three control documents are supplied as the exact bytes attached to the
    release, and the caller cannot substitute already-parsed content: every
    digest comparison below is against those bytes.
    """
    for label, value, pattern in (
        ("version", version, VERSION_PATTERN),
        ("tag", tag, TAG_PATTERN),
        ("source commit", source_commit, COMMIT_PATTERN),
    ):
        if pattern.fullmatch(value) is None:
            raise CompletenessError(f"{label} is not canonical: {value!r}")
    if tag != f"v{version}":
        raise CompletenessError(f"tag {tag!r} does not name version {version!r}")
    try:
        metadata = json.loads(metadata_payload.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as error:
        raise CompletenessError(f"{RELEASE_METADATA} is not valid UTF-8 JSON: {error}") from error

    problems: list[str] = []

    release_tag, not_draft, not_prerelease, published = release_state(release)
    if release_tag != tag:
        problems.append(f"release tag is {release_tag!r}, expected {tag!r}")
    if not not_draft:
        problems.append("release is still a draft; a draft serves no public asset URL")
    if not not_prerelease:
        problems.append("release is marked as a prerelease")

    native_entries = parse_manifest(native_manifest_payload, NATIVE_MANIFEST)
    bundle_entries = parse_manifest(bundle_manifest_payload, BUNDLE_MANIFEST)

    required_native = native_assets(version)
    for name in sorted(required_native - set(native_entries)):
        problems.append(f"{NATIVE_MANIFEST} omits the native asset: {name}")
    for name in sorted(set(native_entries) - required_native):
        problems.append(f"{NATIVE_MANIFEST} carries an unexpected native asset: {name}")

    required_bundles = bundle_assets(extensions, version)
    for name in sorted(required_bundles - set(bundle_entries)):
        problems.append(f"{BUNDLE_MANIFEST} omits the required bundle: {name}")
    for name in sorted(set(bundle_entries) - required_bundles):
        if name == f"{CODEMODE_ID}-{version}.tar.gz":
            problems.append(
                f"{BUNDLE_MANIFEST} carries the historical generic {CODEMODE_ID} bundle; "
                "codemode must publish one archive per supported target: " + name
            )
        else:
            problems.append(f"{BUNDLE_MANIFEST} carries an unexpected bundle archive: {name}")

    payloads = set(native_entries) | set(bundle_entries)
    signed = payloads | {NATIVE_MANIFEST, BUNDLE_MANIFEST, RELEASE_METADATA}
    required = signed | {f"{name}{SIGNATURE_SUFFIX}" for name in signed}

    evidence = evidence_assets(version, tag)
    optional_payloads = {name for names in evidence.values() for name in names}
    optional = optional_payloads | {f"{name}{SIGNATURE_SUFFIX}" for name in optional_payloads}

    for name in sorted(required - set(published)):
        problems.append(f"release is missing the required asset: {name}")
    for name in sorted(set(published) - required - optional):
        problems.append(f"release carries an asset the signed manifests do not account for: {name}")
    for name in sorted(optional_payloads & set(published)):
        if f"{name}{SIGNATURE_SUFFIX}" not in published:
            problems.append(f"release carries an unsigned optional asset: {name}")
    for name in sorted((optional - optional_payloads) & set(published)):
        payload_name = name[: -len(SIGNATURE_SUFFIX)]
        if payload_name not in published:
            problems.append(f"release carries an orphan signature for an absent asset: {name}")

    for name, digest in sorted({**native_entries, **bundle_entries}.items()):
        if name not in published:
            continue
        published_digest = published[name]
        if not published_digest:
            problems.append(f"release asset has no immutable digest: {name}")
        elif published_digest != f"sha256:{digest}":
            problems.append(
                f"release digest disagrees with the signed manifest for {name}: "
                f"{published_digest} != sha256:{digest}"
            )

    # The control documents must be the bytes the release actually publishes.
    # Without this, a caller could feed a locally edited manifest or metadata
    # document and every check below would pass against the wrong bytes.
    for name, payload in (
        (NATIVE_MANIFEST, native_manifest_payload),
        (BUNDLE_MANIFEST, bundle_manifest_payload),
        (RELEASE_METADATA, metadata_payload),
    ):
        if name not in published:
            continue
        published_digest = published[name]
        if published_digest != f"sha256:{sha256(payload)}":
            problems.append(
                f"supplied {name} bytes are not the published asset: "
                f"{published_digest or 'no immutable digest'} != sha256:{sha256(payload)}"
            )

    verify_metadata(
        metadata,
        version=version,
        tag=tag,
        source_commit=source_commit,
        native_entries=native_entries,
        native_manifest_payload=native_manifest_payload,
        problems=problems,
    )

    fingerprint = sha256(
        "".join(f"{name}\t{published[name]}\n" for name in sorted(published)).encode("utf-8")
    )
    report = {
        "schema": SCHEMA,
        "tag": tag,
        "version": version,
        "source_commit": source_commit,
        "assets": {
            "count": len(published),
            "required": len(required),
            "optional": sorted(optional_payloads & set(published)),
            "fingerprint": fingerprint,
        },
        "native": {"manifest": NATIVE_MANIFEST, "entries": sorted(native_entries)},
        "bundles": {
            "manifest": BUNDLE_MANIFEST,
            "entries": sorted(bundle_entries),
            "extensions": sorted(
                extension for extension in extensions
                if any(name.startswith(f"{extension}-{version}") for name in bundle_entries)
            ),
            "codemode_targets": sorted(CODEMODE_TARGETS) if CODEMODE_ID in extensions else [],
        },
        "complete": not problems,
        "problems": problems,
    }
    return report, problems


def main(argv: list[str]) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--release-json", required=True, type=pathlib.Path,
                        help="GitHub REST release document for the tag")
    parser.add_argument("--native-manifest", required=True, type=pathlib.Path)
    parser.add_argument("--bundle-manifest", required=True, type=pathlib.Path)
    parser.add_argument("--metadata", required=True, type=pathlib.Path)
    parser.add_argument("--catalog", required=True, type=pathlib.Path)
    parser.add_argument("--version", required=True)
    parser.add_argument("--tag", required=True)
    parser.add_argument("--source-commit", required=True)
    parser.add_argument("--report", type=pathlib.Path, default=None)
    arguments = parser.parse_args(argv)

    try:
        report, problems = validate(
            release=read_json(arguments.release_json, "release document"),
            native_manifest_payload=read_bytes(arguments.native_manifest, NATIVE_MANIFEST),
            bundle_manifest_payload=read_bytes(arguments.bundle_manifest, BUNDLE_MANIFEST),
            metadata_payload=read_bytes(arguments.metadata, RELEASE_METADATA),
            extensions=catalog_extensions(arguments.catalog),
            version=arguments.version,
            tag=arguments.tag,
            source_commit=arguments.source_commit,
        )
    except CompletenessError as error:
        print(f"error: {error}", file=sys.stderr)
        return 1

    if arguments.report is not None:
        arguments.report.parent.mkdir(parents=True, exist_ok=True)
        arguments.report.write_text(
            json.dumps(report, indent=2, sort_keys=True) + "\n", encoding="utf-8"
        )
    if problems:
        for problem in problems:
            print(f"error: {problem}", file=sys.stderr)
        print(
            f"release {arguments.tag} is incomplete: {len(problems)} problem(s)",
            file=sys.stderr,
        )
        return 1
    print(
        f"release {arguments.tag} is complete: {report['assets']['count']} assets, "
        f"{len(report['native']['entries'])} native, "
        f"{len(report['bundles']['entries'])} bundles across "
        f"{len(report['bundles']['extensions'])} extensions, "
        f"fingerprint {report['assets']['fingerprint']}"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
