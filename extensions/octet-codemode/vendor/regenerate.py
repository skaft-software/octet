#!/usr/bin/env python3
"""Regenerate/check vendored published runtimes offline; never installs or runs npm."""
import argparse
import base64
import hashlib
import io
import json
from pathlib import Path, PurePosixPath
import sys
import tarfile

ROOT = Path(__file__).resolve().parent
PACKAGES = (
    {
        "name": "@earendil-works/pi-codemode", "version": "1.0.0", "directory": "pi-codemode",
        "archive": "pi-codemode-1.0.0.tgz",
        "url": "https://registry.npmjs.org/@earendil-works/pi-codemode/-/pi-codemode-1.0.0.tgz",
        "integrity": "sha512-LPpFI4+T9NzDnhBDs15izWAolaoM8xnwqdziRd6Zx8BQeoEPvzefD2vMMzSyF0rOtq34TfQqkZ0ki16f6cGdMg==",
        "sha256": "05ea491bc1f7adf9fac438c6de9fd313d7a258beb0801724485cf8915b7f8186",
        "repository": "https://github.com/earendil-works/pi", "git_head": "a13d35a742c6ef8462812a28fbe1d8c8b7431c32",
    },
    {
        "name": "quickjs-wasi", "version": "3.6.2", "directory": "quickjs-wasi",
        "archive": "quickjs-wasi-3.6.2.tgz",
        "url": "https://registry.npmjs.org/quickjs-wasi/-/quickjs-wasi-3.6.2.tgz",
        "integrity": "sha512-FCqGtGOrMgzUiIrMNMA2YnsOxCNwo31dzqXvclXUC6xeT35NJLKXQJsvbeCTjvoFAwZgEAPg8U6+KAPDGXn8Mg==",
        "sha256": "f1f4349f19a2d849e33ea0ae9bec2e7062b8839f4eceb17c9051ddbaa2720982",
        "repository": "https://github.com/vercel-labs/quickjs-wasi",
        "source_tag": "quickjs-wasi@3.6.2", "git_head": "5a7a0eeda87c99542f8cf3095b6d61ecfa755977",
    },
)
LICENSE_SHA256 = "0457f5bcec3b3b211605dfb5d1a49042fd638f3686a410fe099c24a25af13c48"
LICENSE_URL = "https://raw.githubusercontent.com/earendil-works/pi/a13d35a742c6ef8462812a28fbe1d8c8b7431c32/LICENSE"
IMPORT_OLD = b'from "quickjs-wasi";'
IMPORT_NEW = b'from "../../../quickjs-wasi/dist/index.js";'
# Supplemental engine/runtime notices omitted from npm, plus notices for optional
# modules retained only inside the unmodified source archive. Published source
# pins: quickjs-wasi@3.6.2 / QuickJS-NG submodule and its wasi-sdk-32 toolchain.
SUPPLEMENTAL_LICENSES = (
    ("QuickJS-NG", "quickjs-ng-LICENSE",
     "https://raw.githubusercontent.com/quickjs-ng/quickjs/6d46d07d04041b40f4f49eaa7fdebe44c314c699/LICENSE",
     "96f73f9d2a16c21a36b418f06073be26e7d6d5e7c1bc99756b21a4f2c74ef171"),
    ("wasi-libc", "wasi-libc-LICENSE",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/LICENSE",
     "2711a8b5a5cdfef0e639f96c1aca12ae23d7d64a02d0507f1bdf14d2b27bbc3a"),
    ("wasi-libc", "wasi-libc-LICENSE-APACHE-LLVM",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/LICENSE-APACHE-LLVM",
     "268872b9816f90fd8e85db5a28d33f8150ebb8dd016653fb39ef1f94f2686bc5"),
    ("wasi-libc", "wasi-libc-LICENSE-APACHE",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/LICENSE-APACHE",
     "a60eea817514531668d7e00765731449fe14d059d3249e0bc93b36de45f759f2"),
    ("wasi-libc", "wasi-libc-LICENSE-MIT",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/LICENSE-MIT",
     "23f18e03dc49df91622fe2a76176497404e46ced8a715d9d2b67a7446571cca3"),
    ("wasi-libc", "wasi-libc-libc-bottom-half-cloudlibc-LICENSE",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/libc-bottom-half/cloudlibc/LICENSE",
     "c8b789cf5a746611e6300a0cc7750dbf92b61912a709d04e639245f7290656d0"),
    ("wasi-libc", "wasi-libc-libc-top-half-musl-COPYRIGHT",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/libc-top-half/musl/COPYRIGHT",
     "f9bc4423732350eb0b3f7ed7e91d530298476f8fec0c6c427a1c04ade22655af"),
    ("wasi-libc", "wasi-libc-fts-musl-fts-COPYING",
     "https://raw.githubusercontent.com/WebAssembly/wasi-libc/2fc32bc81b9f07f8d9525edea59bfbaf760c06d6/fts/musl-fts/COPYING",
     "55af87e4017668f54467a3380e7ebbac5e672d8c763bfe95e6fc882a6fdc4046"),
    ("LLVM runtime", "llvm-LICENSE.TXT",
     "https://raw.githubusercontent.com/llvm/llvm-project/4434dabb69916856b824f68a64b029c67175e532/llvm/LICENSE.TXT",
     "8d85c1057d742e597985c7d4e6320b015a9139385cff4cbae06ffc0ebe89afee"),
    ("Ada URL (archive only; MIT alternative)", "ada-LICENSE-MIT",
     "https://raw.githubusercontent.com/ada-url/ada/v3.4.3/LICENSE-MIT",
     "af0d7d2cef91fc243cf4ad98570b03d1f26f5e0227cad0f5f4a7376e2feb3160"),
    ("Mbed TLS (archive only; Apache-2.0 alternative)", "mbedtls-LICENSE",
     "https://raw.githubusercontent.com/vercel-labs/quickjs-wasi/5a7a0eeda87c99542f8cf3095b6d61ecfa755977/extensions/crypto/mbedtls/LICENSE",
     "9b405ef4c89342f5eae1dd828882f931747f71001cfba7d114801039b52ad09b"),
)


def generated():
    files = {}
    for package in PACKAGES:
        archive_path = f"sources/{package['archive']}"
        archive = (ROOT / archive_path).read_bytes()
        integrity = "sha512-" + base64.b64encode(hashlib.sha512(archive).digest()).decode()
        if integrity != package["integrity"] or hashlib.sha256(archive).hexdigest() != package["sha256"]:
            raise ValueError(f"{archive_path}: published npm integrity/SHA256 mismatch")
        files[archive_path] = archive
        with tarfile.open(fileobj=io.BytesIO(archive), mode="r:gz") as tar:
            seen = set()
            for member in tar:
                path = PurePosixPath(member.name)
                if not member.isfile() or path.parts[0] != "package" or ".." in path.parts or member.name in seen:
                    raise ValueError(f"unsafe or duplicate archive member: {member.name}")
                seen.add(member.name)
                relative = path.relative_to("package").as_posix()
                # Only published JS and its declarations/maps, metadata, license/docs,
                # and the core WASM. No optional .so modules are extracted or loaded.
                keep = relative in {"package.json", "README.md", "LICENSE", "quickjs.wasm"} or relative.startswith("dist/")
                if not keep:
                    continue
                if member.size > 8 * 1024 * 1024:
                    raise ValueError(f"oversized source member: {member.name}")
                data = tar.extractfile(member).read()
                if package["directory"] == "pi-codemode" and relative == "dist/runtime/worker.js":
                    if data.count(IMPORT_OLD) != 1:
                        raise ValueError("published worker import changed")
                    data = data.replace(IMPORT_OLD, IMPORT_NEW)
                files[f"{package['directory']}/{relative}"] = data
        metadata = json.loads(files[f"{package['directory']}/package.json"])
        if (metadata["name"], metadata["version"], metadata["license"]) != (package["name"], package["version"], "MIT"):
            raise ValueError("published package metadata mismatch")
    license = (ROOT / "sources/pi-LICENSE").read_bytes()
    if hashlib.sha256(license).hexdigest() != LICENSE_SHA256:
        raise ValueError("pinned Pi MIT license mismatch")
    files["sources/pi-LICENSE"] = license
    files["pi-codemode/LICENSE"] = license
    supplemental = []
    for component, name, url, digest in SUPPLEMENTAL_LICENSES:
        source = f"sources/{name}"
        destination = f"quickjs-wasi/licenses/{name}"
        data = (ROOT / source).read_bytes()
        if hashlib.sha256(data).hexdigest() != digest:
            raise ValueError(f"{source}: pinned component license mismatch")
        files[source] = files[destination] = data
        supplemental.append({"component": component, "source": source,
                             "destination": destination, "url": url, "sha256": digest})
    provenance = {
        "schema": "octet.codemode.vendor.v1", "packages": list(PACKAGES),
        "pi_license": {"url": LICENSE_URL, "sha256": LICENSE_SHA256,
                       "note": "Pi's npm tarball omits LICENSE; supplied from its exact published gitHead."},
        "supplemental_licenses": supplemental,
        "patches": [{"path": "pi-codemode/dist/runtime/worker.js", "old": IMPORT_OLD.decode(), "new": IMPORT_NEW.decode(),
                     "reason": "Offline relative import relocation only; engine/prelude otherwise unchanged."}],
        "omitted": ["quickjs-wasi optional native WASM .so extensions (not used by Pi codemode)"],
    }
    files["PROVENANCE.json"] = (json.dumps(provenance, indent=2) + "\n").encode()
    files["SHA256SUMS"] = "".join(f"{hashlib.sha256(data).hexdigest()}  {path}\n" for path, data in sorted(files.items())).encode()
    return files


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--check", action="store_true", help="verify all vendored bytes without writing or networking")
    args = parser.parse_args()
    try:
        files = generated()
        errors = []
        for path, data in files.items():
            target = ROOT / path
            if args.check:
                if not target.is_file() or target.is_symlink() or target.read_bytes() != data:
                    errors.append(path)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                target.write_bytes(data)
        for directory in ("pi-codemode", "quickjs-wasi", "sources"):
            for target in (ROOT / directory).rglob("*"):
                if target.is_file() and target.relative_to(ROOT).as_posix() not in files:
                    errors.append(f"unexpected file: {target.relative_to(ROOT)}")
        if errors:
            raise ValueError("vendor drift: " + ", ".join(errors) + "; run python3 vendor/regenerate.py and review the changes")
        print(f"{'Verified' if args.check else 'Regenerated'} {len(files)} vendored files offline (npm integrity + SHA256)")
        return 0
    except (OSError, ValueError, tarfile.TarError) as error:
        print(f"codemode vendor check failed: {error}", file=sys.stderr)
        return 1


if __name__ == "__main__":
    raise SystemExit(main())
