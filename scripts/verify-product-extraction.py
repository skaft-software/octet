#!/usr/bin/env python3
"""Verify pinned product source snapshots offline; never downloads or executes them."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess

ROOT = Path(__file__).resolve().parent.parent


def git(root, *args):
    return subprocess.check_output(["git", "-C", str(root), *args])


def verify(source, products_root, manifest):
    checked = 0
    for product in manifest["products"]:
        checkout = products_root / product["name"]
        source_revision = product["source_revision"]
        revision = product["published_revision"]
        prefix = product["source_path"]
        if git(checkout, "rev-parse", "HEAD").decode().strip() != revision:
            raise ValueError(f"{product['name']}: checkout must be at pinned revision {revision}")
        upstream = {}
        for record in git(source, "ls-tree", "-rz", source_revision, "--", prefix).split(b"\0"):
            if not record:
                continue
            metadata, path = record.decode().split("\t")
            mode, kind, oid = metadata.split()
            if kind != "blob" or mode not in {"100644", "100755"}:
                raise ValueError(f"unsupported upstream entry: {path}")
            relative = str(Path(path).relative_to(prefix))
            upstream[relative] = (mode, git(source, "cat-file", "blob", oid))
        if len(upstream) != product["files"] or not upstream:
            raise ValueError(f"{product['name']}: upstream inventory count mismatch")
        provenance = json.loads(git(checkout, "show", f"{revision}:MIGRATION-SOURCE.json"))
        if provenance["revision"] != source_revision or provenance["source_path"] != prefix:
            raise ValueError(f"{product['name']}: provenance source mismatch")
        files = provenance["files"]
        if len(files) != len(upstream) or {f["path"] for f in files} != set(upstream):
            raise ValueError(f"{product['name']}: incomplete provenance inventory")
        for record in files:
            relative = record["path"]
            mode, original = upstream[relative]
            if record["source_path"] != f"{prefix}/{relative}":
                raise ValueError(f"{product['name']}: source path mismatch: {relative}")
            digest = hashlib.sha256(original).hexdigest()
            extracted = git(checkout, "show", f"{revision}:{relative}")
            metadata = git(checkout, "ls-tree", revision, "--", relative).decode()
            if not metadata.startswith(mode + " "):
                raise ValueError(f"{product['name']}: executable mode mismatch: {relative}")
            if record["mode"] != mode or record["sha256"] != digest or extracted != original:
                raise ValueError(f"{product['name']}: content mismatch: {relative}")
            checked += 1
        print(f"{product['name']}: {len(upstream)} original files verified")
    return checked


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--products-root", type=Path, required=True,
                        help="directory containing product Git checkouts at their pinned revisions")
    parser.add_argument("--source", type=Path, default=ROOT,
                        help="octet checkout containing the pinned source commits")
    parser.add_argument("--manifest", type=Path,
                        default=ROOT / "docs/migrations/external-products.json")
    args = parser.parse_args()
    checked = verify(args.source, args.products_root, json.loads(args.manifest.read_text()))
    print(f"Verified {checked} original source files; standalone functionality is not qualified.")


if __name__ == "__main__":
    main()
