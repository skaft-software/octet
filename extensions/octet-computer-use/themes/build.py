#!/usr/bin/env python3
"""Rebuild the bundled, model-colored Cua cursor artifacts (developer only).

Source: trycua/cua commit 11c4647128a99b2879f31a3f4eedc6b08d52c079,
libs/cua-driver/rust/crates/cursor-overlay/assets/cua.default.lottie (MIT).
The semantic animations are retained; the pointer silhouette, scale, and color
are Octet-specific. Cua's bounded v2 compiler validates every output.
"""

from __future__ import annotations

import json
import math
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SOURCE = HERE / "cua.default.lottie"
SOURCES = {
    "openai": "#1f1f1f", "anthropic": "#cc785c", "google": "#34a853",
    "xai": "#736cd3", "meta": "#0089f4", "mistral": "#fd6f00",
    "deepseek": "#2243e6", "alibaba": "#ff7018", "minimax": "#eb3568",
    "kimi": "#047afe", "zai": "#1c7ff8", "nvidia": "#86b737",
    "xiaomi": "#ff6900", "cohere": "#d18ee2", "amazon": "#ff9900",
    "microsoft": "#0078d5", "ai21": "#d63864", "bytedance": "#3c8bff",
    "perplexity": "#1b818e", "ibm": "#0f62fe", "baidu": "#2436d8",
    "tencent": "#5cb9ff", "allenai": "#f0529c", "unknown": "#16876d",
}
# tui/theme.rs balances model prompt colors to this universal luminance.
TARGET = 0.179
SCALE = 0.88
HOTSPOT = (103, 64)
CUA_BLUE = [94 / 255, 192 / 255, 232 / 255]


def luminance(rgb: tuple[int, ...]) -> float:
    def channel(value: int) -> float:
        value /= 255
        return value / 12.92 if value <= 0.04045 else ((value + 0.055) / 1.055) ** 2.4
    return sum(weight * channel(value) for weight, value in zip((0.2126, 0.7152, 0.0722), rgb))


def prompt_color(source: str) -> str:
    rgb = tuple(bytes.fromhex(source[1:]))
    if abs(luminance(rgb) - TARGET) <= 0.002:
        return source
    lighten = luminance(rgb) < TARGET
    destination = 255 if lighten else 0
    low, high = 0.0, 1.0
    for _ in range(20):
        amount = (low + high) / 2
        candidate = tuple(math.floor(value + (destination - value) * amount + 0.5) for value in rgb)
        reached = luminance(candidate) >= TARGET if lighten else luminance(candidate) <= TARGET
        if reached:
            high = amount
        else:
            low = amount
    result = tuple(math.floor(value + (destination - value) * high + 0.5) for value in rgb)
    return "#" + bytes(result).hex()


def map_property(prop: dict, transform) -> None:
    if not prop["a"]:
        prop["k"] = transform(prop["k"])
    else:
        for frame in prop["k"]:
            for key in ("s", "e"):
                if key in frame:
                    frame[key] = transform(frame[key])


def recolor_and_resize(animation: dict, color: str) -> None:
    accent = [value / 255 for value in bytes.fromhex(color[1:])]
    for layer in animation["layers"]:
        ks = layer["ks"]
        map_property(ks["p"], lambda p: [round(HOTSPOT[i] + SCALE * (p[i] - HOTSPOT[i]), 5)
                                       for i in range(2)])
        map_property(ks["s"], lambda s: [round(SCALE * value, 5) for value in s])
        for shape in layer.get("shapes", []):
            paint = shape.get("c")
            if paint and not paint["a"] and all(abs(paint["k"][i] - CUA_BLUE[i]) < 0.00001 for i in range(3)):
                paint["k"] = [*accent, 1]
            # Replace the stock pointer silhouette with a rounded, rightward
            # Octet-inspired pointer. The action marks and white outline remain.
            geometry = shape.get("ks", {}).get("k", {})
            if shape.get("nm") == "Cursor" and isinstance(geometry, dict) and "v" in geometry:
                geometry["v"] = [[31, 28], [100, 56], [107, 63], [101, 71], [82, 79],
                                 [76, 84], [67, 101], [59, 105], [52, 100], [27, 42]]
                geometry["i"] = [[0, 0], [-16, -8], [0, -4], [5, -3], [8, -3],
                                 [3, -2], [5, -12], [4, 0], [4, 5], [-4, -8]]
                geometry["o"] = [[3, -4], [6, 3], [0, 5], [-6, 4], [-4, 2],
                                 [-4, 4], [-3, 5], [-5, 0], [-4, -5], [-3, -7]]


def build(binary: str) -> None:
    palette = {}
    with zipfile.ZipFile(SOURCE) as original, tempfile.TemporaryDirectory() as temp:
        for lab, source in SOURCES.items():
            color = prompt_color(source)
            theme = f"com.octet.computeruse.{lab}"
            palette[lab] = {"color": color, "id": theme}
            source_path = Path(temp) / f"{lab}.lottie"
            with zipfile.ZipFile(source_path, "w", compression=zipfile.ZIP_DEFLATED) as output:
                for name in original.namelist():
                    payload = original.read(name)
                    if name == "cua/theme.json":
                        manifest = json.loads(payload)
                        manifest.update(id=theme, name=f"Octet {lab} cursor", author="Octet / Cua",
                                        version="1.0.0")
                        manifest["hotspot"] = {"x": HOTSPOT[0], "y": HOTSPOT[1]}
                        payload = json.dumps(manifest, separators=(",", ":")).encode()
                    elif name.startswith("a/") and name.endswith(".json"):
                        animation = json.loads(payload)
                        recolor_and_resize(animation, color)
                        payload = json.dumps(animation, separators=(",", ":")).encode()
                    output.writestr(name, payload)
            artifact = HERE / f"{lab}.cua-theme"
            subprocess.run([binary, "cursor-theme", "build", str(source_path),
                            "--output", str(artifact)], check=True, capture_output=True)
    (HERE / "palette.json").write_text(json.dumps(palette, sort_keys=True, indent=2) + "\n")
    print(f"Built {len(palette)} themes")


if __name__ == "__main__":
    build(sys.argv[1] if len(sys.argv) > 1 else "cua-driver")
