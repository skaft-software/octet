#!/usr/bin/env python3
"""Offline public-documentation inventory, producer and relative-link regression.

No reference file is executed. --embedded checks a newly built text archive;
--package checks an extracted native/npm documentation root against current source.
"""
import argparse
import pathlib
import posixpath
import re
import shutil
import stat
import subprocess
import sys
import tarfile
import tempfile
from urllib.parse import unquote, urlsplit

ROOT = pathlib.Path(__file__).resolve().parent.parent
INVENTORY = "docs/package-assets.txt"


def inventory():
    result = {}
    for line in (ROOT / INVENTORY).read_text().splitlines():
        if not line or line.startswith("#"):
            continue
        kind, name = line.split(" ")
        assert kind in {"text", "asset"}
        assert re.fullmatch(r"[A-Za-z0-9_./-]+", name)
        assert all(part not in {"", ".", ".."} for part in name.split("/"))
        assert name not in result, name
        result[name] = kind
    return result


def helper(script, marker):
    return (ROOT / script).read_text().split("<<'" + marker + "'\n", 1)[1].split("\n" + marker, 1)[0]


def invoke(code, *args, success=True):
    result = subprocess.run([sys.executable, "-c", code, *map(str, args)], capture_output=True, timeout=30)
    assert (result.returncode == 0) == success, result.stderr.decode()


def check_links(files):
    for name in files:
        if not name.endswith(".md"):
            continue
        text = (ROOT / name).read_text()
        # Bound this audit to written Markdown/HTML links, not example code,
        # bare prose paths, anchors, or external URLs.
        text = re.sub(r"(?ms)^(`{3,}|~{3,}).*?^\1[^\n]*$", "", text)
        targets = re.findall(r"\]\(\s*<?([^\s)>]+)", text)
        targets += re.findall(r"(?m)^\s*\[[^\]]+\]:\s*<?([^\s>]+)", text)
        targets += re.findall(r'(?:src|href)=["\']([^"\']+)', text)
        for target in targets:
            parsed = urlsplit(target)
            if parsed.scheme or parsed.netloc or not parsed.path:
                continue
            resolved = posixpath.normpath(posixpath.join(posixpath.dirname(name), unquote(parsed.path)))
            assert resolved in files or any(p.startswith(resolved.rstrip("/") + "/") for p in files), (name, target, resolved)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--embedded", type=pathlib.Path)
    parser.add_argument("--package", type=pathlib.Path)
    args = parser.parse_args()
    files = inventory()
    tracked = set(subprocess.check_output(["git", "-C", str(ROOT), "ls-files", "-z"]).decode().split("\0"))
    # This newly introduced manifest is allowed while testing the uncommitted
    # implementation. The production native producer requires it tracked too.
    assert set(files) <= tracked | {INVENTORY}, set(files) - tracked - {INVENTORY}
    assert {p for p in tracked if p.startswith(("docs/", "examples/", "sdk/"))} <= set(files)
    for name, kind in files.items():
        source = ROOT
        for part in name.split("/"):
            source /= part
            assert not source.is_symlink(), name
        assert stat.S_ISREG(source.stat().st_mode), name
        if kind == "text":
            source.read_bytes().decode("utf-8")
        else:
            assert name.startswith("docs/"), name
    check_links(files)
    extras = {name for name in files if name != "README.md" and not name.startswith(("docs/", "examples/", "sdk/"))}
    installer_extras = set(helper("scripts/install.sh", "OCTET_DOCUMENTATION_EXTRAS").splitlines())
    assert extras == installer_extras, (extras - installer_extras, installer_extras - extras)

    code = helper("scripts/package-octet-release.sh", "PYASSETS")
    with tempfile.TemporaryDirectory(prefix="octet-docs-test-") as temporary:
        root = pathlib.Path(temporary)
        source, package = root / "source", root / "package"
        for name in files:
            dest = source / name
            dest.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(ROOT / name, dest)
        listing = root / "tracked"
        listing.write_bytes(b"\0".join(name.encode() for name in files) + b"\0")
        (source / "docs/private.md").write_text("must not ship")
        (source / "sdk/private.so").write_bytes(b"\0must not ship")
        invoke(code, source, package, listing)
        assert {p.relative_to(package).as_posix() for p in package.rglob("*") if p.is_file()} == set(files)
        for name in files:
            assert (package / name).read_bytes() == (ROOT / name).read_bytes(), name
        # Container staging uses the same finite inventory and current bytes.
        invoke("import os; os.umask(0o077)\n" + helper("scripts/build-octet-image.sh", "PYDOCS"), source)
        container = source / ".octet-package-docs"
        assert {p.relative_to(container).as_posix() for p in container.rglob("*") if p.is_file()} == set(files)
        for name in files:
            assert (container / name).read_bytes() == (ROOT / name).read_bytes(), name
        assert all(stat.S_IMODE(path.stat().st_mode) == 0o755 for path in [container, *(p for p in container.rglob("*") if p.is_dir())])
        assert "COPY .octet-package-docs /usr/local/share/octet" in (ROOT / "deploy/Dockerfile.octet").read_text()
        # Production native copier rejects missing, untracked, symlinked and
        # traversal inventory members, including a symlinked parent directory.
        manifest = source / INVENTORY
        original = manifest.read_text()
        for bad in ("text ../escape\n", "text /escape\n", "text docs/private.md\n", "text README.md\n"):
            manifest.write_text(original + bad)
            invoke(code, source, package, listing, success=False)
        manifest.write_text(original)
        security = source / "SECURITY.md"
        security.unlink()
        invoke(code, source, package, listing, success=False)
        security.symlink_to(ROOT / "SECURITY.md")
        invoke(code, source, package, listing, success=False)
        security.unlink()
        shutil.copyfile(ROOT / "SECURITY.md", security)
        browse = source / "extensions/octet-browse"
        browse.rename(source / "extensions/browse-real")
        browse.symlink_to(source / "extensions/browse-real", target_is_directory=True)
        invoke(code, source, package, listing, success=False)

    if args.package:
        for name in files:
            assert (args.package / name).read_bytes() == (ROOT / name).read_bytes(), name
    if args.embedded:
        expected = {name for name, kind in files.items() if kind == "text"}
        with tarfile.open(args.embedded, "r:gz") as archive:
            members = archive.getmembers()
            assert len(members) == len(expected)
            assert {member.name for member in members} == expected
            for member in members:
                assert member.isfile(), member.name
                assert archive.extractfile(member).read() == (ROOT / member.name).read_bytes(), member.name
    print(f"packaged docs: {len(files)} public files, {len(extras)} extra references; links, producer bytes and negative boundaries passed")


if __name__ == "__main__":
    main()
