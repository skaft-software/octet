#!/usr/bin/env python3
"""Stage an inert, source-only bundle; optionally build a reproducible tar.gz.

Never downloads, installs, opens devices, invokes Git, or reads user preferences.
"""
from __future__ import annotations

import argparse
import gzip
from pathlib import Path
import shutil
import stat
import tarfile
import tomllib

ROOT = Path(__file__).resolve().parent
FILES = (
    "extension.toml", "launcher.py", "extension.py", "settings.py", "service.py",
    "audio.py", "portable_audio.py", "realtime.py", "voice_ui.py", "README.md",
    "requirements.in", "requirements.lock", "LICENSE", "SDK-LICENSE", "SDK-PROVENANCE.txt",
    "HOST-INTEGRATION.md", "SECURITY.md",
)
# Reviewed SDK module allowlist, not a recursive copy of the workstation.
SDK_FILES = (
    "__init__.py", "api_v03.py", "bulk.py", "cache_warming.py", "diagnostics.py",
    "event_bus.py", "extension.py", "protocol.py", "resources.py", "typed.py",
)


def regular_source(relative):
    path = ROOT / relative
    for ancestor in (path, *path.parents):
        if ancestor == ROOT:
            break
        if ancestor.is_symlink():
            raise ValueError(f"linked package source: {relative}")
    if not stat.S_ISREG(path.stat().st_mode):
        raise ValueError(f"package source is not a regular file: {relative}")
    return path


def build(output, archive=None):
    manifest = tomllib.loads(regular_source("extension.toml").read_text(encoding="utf-8"))
    if manifest["name"] != "octet-voice" or manifest["entrypoint"]["command"] != "launcher.py":
        raise ValueError("unexpected package identity or entrypoint")
    relative_files = [Path(name) for name in FILES]
    relative_files += [Path("vendor/octet_extension") / name for name in SDK_FILES]
    sources = [(relative, regular_source(relative)) for relative in relative_files]
    output.mkdir(parents=True, exist_ok=False)
    for relative, source in sources:
        target = output / relative
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        target.chmod(0o755 if relative == Path("launcher.py") else 0o644)
    if archive is not None:
        # Exclusive creation: never overwrite a reviewed artifact. Fixed metadata
        # makes identical source produce identical archive bytes on every platform.
        archive.parent.mkdir(parents=True, exist_ok=True)
        with archive.open("xb") as raw:
            with gzip.GzipFile(filename="", fileobj=raw, mode="wb", mtime=0) as compressed:
                with tarfile.open(fileobj=compressed, mode="w", format=tarfile.GNU_FORMAT) as bundle:
                    root = tarfile.TarInfo("octet-voice")
                    root.type, root.mode, root.mtime = tarfile.DIRTYPE, 0o755, 0
                    bundle.addfile(root)
                    for directory in ("vendor", "vendor/octet_extension"):
                        info = tarfile.TarInfo(f"octet-voice/{directory}")
                        info.type, info.mode, info.mtime = tarfile.DIRTYPE, 0o755, 0
                        bundle.addfile(info)
                    for relative in sorted(relative_files):
                        path = output / relative
                        info = tarfile.TarInfo(f"octet-voice/{relative.as_posix()}")
                        info.size = path.stat().st_size
                        info.mode = 0o755 if relative == Path("launcher.py") else 0o644
                        with path.open("rb") as contents:
                            bundle.addfile(info, contents)
    return output


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, help="new staging directory")
    parser.add_argument("--archive", type=Path, help="new .tar.gz path")
    args = parser.parse_args()
    print(build(args.output, args.archive))


if __name__ == "__main__":
    main()
