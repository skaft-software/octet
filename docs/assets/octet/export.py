#!/usr/bin/env python3
"""Validate pinned native octet assets and reproduce their public ZIP."""

import argparse
import hashlib
import io
import json
from pathlib import Path
import struct
import xml.etree.ElementTree as ET
import zipfile
import zlib

ROOT = Path(__file__).resolve().parent
BITS = "01101111"


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def validate_svg(data, name):
    root = ET.fromstring(data)
    elements = list(root.iter())
    require(not any(e.tag.rsplit("}", 1)[-1] in
                    {"text", "script", "image", "foreignObject", "use"}
                    for e in elements), f"{name}: non-outlined or external content")
    columns = [e for e in elements if "data-bit" in e.attrib]
    if name.startswith("wordmark-"):
        require(not columns and any(e.tag.endswith("}path") for e in elements),
                f"{name}: expected original outlined wordmark")
        return
    require(len(columns) == 8, f"{name}: expected eight columns")
    require([e.get("data-bit") for e in columns] == list(map(str, range(8))),
            f"{name}: column order")
    require("".join(e.get("data-value", "") for e in columns) == BITS,
            f"{name}: wrong byte")
    widths = [float(e.get("width")) for e in columns]
    heights = [float(e.get("height")) for e in columns]
    require(widths[0] > 0 and len(set(widths)) == 1, f"{name}: column widths")
    full = max(heights)
    baseline = float(columns[0].get("y")) + heights[0]
    start = float(columns[0].get("x"))
    for index, (column, bit) in enumerate(zip(columns, BITS)):
        require(float(column.get("x")) == start + index * widths[0],
                f"{name}: non-contiguous columns")
        require(heights[index] == full * (0.5 if bit == "0" else 1),
                f"{name}: incorrect height")
        require(float(column.get("y")) + heights[index] == baseline,
                f"{name}: unaligned baseline")
    require(full == widths[0] * 4, f"{name}: distorted profile")


def validate_png(data, name):
    require(data.startswith(b"\x89PNG\r\n\x1a\n"), f"{name}: PNG signature")
    cursor, compressed, dimensions = 8, bytearray(), None
    while cursor < len(data):
        length, = struct.unpack_from(">I", data, cursor)
        kind = data[cursor + 4:cursor + 8]
        payload = data[cursor + 8:cursor + 8 + length]
        crc, = struct.unpack_from(">I", data, cursor + 8 + length)
        require(zlib.crc32(kind + payload) == crc, f"{name}: PNG CRC")
        if kind == b"IHDR":
            dimensions = struct.unpack_from(">II", payload)
        if kind == b"IDAT":
            compressed.extend(payload)
        cursor += 12 + length
        if kind == b"IEND":
            break
    require(cursor == len(data) and kind == b"IEND" and dimensions,
            f"{name}: incomplete PNG")
    require(bool(zlib.decompress(compressed)), f"{name}: empty PNG image")
    if name.startswith("icon-"):
        size = int(name.removesuffix(".png").rsplit("-", 1)[1])
        require(dimensions == (size, size), f"{name}: icon dimensions")
    return dimensions


def validate_ico(data):
    reserved, kind, count = struct.unpack_from("<HHH", data)
    require(reserved == 0 and kind == 1 and count > 0, "invalid ICO header")
    sizes = []
    for index in range(count):
        width, height, _, _, _, _, size, offset = struct.unpack_from(
            "<BBBBHHII", data, 6 + 16 * index)
        width, height = width or 256, height or 256
        require(width == height and offset + size <= len(data), "invalid ICO entry")
        sizes.append(width)
    require(sizes == [16, 32, 48, 64, 128, 256], "ICO size inventory differs")


def validated_payload(root=ROOT):
    manifest = json.loads((root / "manifest.json").read_text())
    require(manifest["byte"]["bits"] == BITS and manifest["product"] == "octet",
            "invalid identity contract")
    payload = {}
    for asset in manifest["assets"]:
        name = asset["path"]
        path = root / name
        require(not Path(name).is_absolute() and ".." not in Path(name).parts,
                "invalid asset path")
        require(name not in payload and not path.is_symlink(), "duplicate or symlink asset")
        data = path.read_bytes()
        require(len(data) == asset["bytes"] and sha256(data) == asset["sha256"],
                f"{name}: pinned source digest differs")
        if name.endswith(".svg"):
            validate_svg(data, path.name)
        elif name.endswith(".png"):
            validate_png(data, path.name)
        elif name.endswith(".ico"):
            validate_ico(data)
        payload[name] = data
    actual_marks = {"marks/" + p.name for p in (root / "marks").iterdir()}
    require(actual_marks == {p for p in payload if p.startswith("marks/")},
            "unmanifested marks")
    for name in ["README.md", "manifest.json", "export.py", "test_export.py"]:
        payload[name] = (root / name).read_bytes()
    return payload


def artifacts(payload):
    sums = "".join(f"{sha256(data)}  {name}\n"
                   for name, data in sorted(payload.items())).encode()
    output = io.BytesIO()
    with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_STORED) as archive:
        for name, data in sorted({**payload, "SHA256SUMS": sums}.items()):
            info = zipfile.ZipInfo("octet-identity/" + name, (1980, 1, 1, 0, 0, 0))
            info.create_system = 3
            info.external_attr = 0o100644 << 16
            archive.writestr(info, data)
    package = output.getvalue()
    return {"SHA256SUMS": sums, "octet-identity.zip": package,
            "octet-identity.zip.sha256":
                f"{sha256(package)}  octet-identity.zip\n".encode()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    mode = parser.add_mutually_exclusive_group(required=True)
    mode.add_argument("--check", action="store_true")
    mode.add_argument("--write", action="store_true")
    args = parser.parse_args()
    payload = validated_payload()
    for name, data in artifacts(payload).items():
        path = ROOT / name
        if args.write:
            path.write_bytes(data)
        else:
            require(path.read_bytes() == data, f"{name}: export is stale")
    print(f"octet: {len(payload) - 4} pinned assets validated; export "
          + ("written" if args.write else "reproduced exactly"))


if __name__ == "__main__":
    main()
