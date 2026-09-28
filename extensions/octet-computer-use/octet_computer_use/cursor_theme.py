"""Bundled Cua cursor themes matching Octet's stable model-prompt palette.

Theme installation is a trusted local operation, never an agent-facing Cua
tool, and it only ever installs the reviewed artifacts bundled here. Custom Cua
themes are static; selecting a different installed theme on the next Octet tool
boundary follows a model switch without recompilation.
"""

from __future__ import annotations

import json
import os
import platform
import subprocess
from pathlib import Path
from typing import Any, Mapping, Optional

THEMES = Path(__file__).resolve().parent.parent / "themes"
PALETTE = json.loads((THEMES / "palette.json").read_text(encoding="utf-8"))


def model_lab(model: str, provider: str = "") -> str:
    """Mirror the stable family assignment in tui/theme.rs."""

    text = model.strip().lower()
    if any(mark in text for mark in ("claude", "anthropic")):
        return "anthropic"
    if "deepseek" in text:
        return "deepseek"
    if any(mark in text for mark in ("gemini", "gemma", "google")):
        return "google"
    if any(mark in text for mark in ("grok", "x.ai", "x-ai", "spacexai")):
        return "xai"
    if any(mark in text for mark in ("llama", "meta-ai", "meta/")):
        return "meta"
    if any(mark in text for mark in ("mistral", "mixtral", "codestral", "ministral", "devstral")):
        return "mistral"
    if any(mark in text for mark in ("qwen", "qwq", "alibaba", "dashscope")):
        return "alibaba"
    for lab, marks in (
        ("minimax", ("minimax",)), ("kimi", ("kimi", "moonshot")),
        ("zai", ("z-ai", "zhipu", "chatglm", "glm-")),
        ("nvidia", ("nvidia", "nemotron")), ("xiaomi", ("xiaomi", "mimo-")),
        ("cohere", ("cohere", "command-r", "command-a")),
        ("amazon", ("amazon", "bedrock", "nova-")),
        ("microsoft", ("microsoft", "azure", "phi-", "mai-")),
        ("ai21", ("ai21", "jamba")), ("bytedance", ("bytedance", "doubao", "seed-")),
        ("perplexity", ("perplexity", "sonar-")),
        ("ibm", ("ibm", "granite")), ("baidu", ("baidu", "ernie")),
        ("tencent", ("tencent", "hunyuan")),
        ("allenai", ("allenai", "allen-ai", "olmo")),
    ):
        if any(mark in text for mark in marks):
            return lab
    if (any(mark in text for mark in ("openai", "chatgpt", "codex", "gpt-"))
            or text.startswith(("o1", "o3", "o4"))):
        return "openai"
    provider = provider.strip().lower()
    provider_aliases = {"x-ai": "xai", "meta-ai": "meta", "z-ai": "zai",
                        "zhipu": "zai", "aws": "amazon", "amazon-bedrock": "amazon",
                        "ai2": "allenai", "allen-ai": "allenai"}
    if provider in provider_aliases:
        return provider_aliases[provider]
    if provider and provider != text:
        return model_lab(provider)
    return "unknown"


def theme_for_host(context: Mapping[str, Any]) -> str:
    host = context.get("host")
    if not isinstance(host, Mapping):
        return "unknown"
    model = host.get("model")
    view = host.get("model_view")
    provider = view.get("provider", "") if isinstance(view, Mapping) else ""
    return model_lab(model if isinstance(model, str) else "", provider if isinstance(provider, str) else "")


def theme_id(lab: str) -> str:
    return PALETTE[lab]["id"]


# The compiled-artifact header Cua Driver accepts: an 8-byte magic followed by
# a little-endian u16 artifact version. The driver fully re-validates every
# artifact when it loads one; this check only keeps a stale bundle from being
# written into its store.
_ARTIFACT_MAGIC = b"CUATHEM3"
_ARTIFACT_VERSION = 3
_MAX_ARTIFACT_BYTES = 24 * 1024 * 1024 + 46


def _sidecar(binary: Path) -> Optional[Path]:
    """Cua's ``cua-cursor-theme`` compiler beside the driver, if shipped.

    Every ``cua-driver cursor-theme`` subcommand delegates to it. Cua's app
    bundles ship it; the ``cua-driver`` wheels do not.
    """

    name = "cua-cursor-theme.exe" if platform.system() == "Windows" else "cua-cursor-theme"
    candidate = Path(binary).parent / name
    return candidate if candidate.is_file() else None


def theme_store_root() -> Optional[Path]:
    """Where Cua Driver loads installed themes from, mirroring its own lookup.

    Returns None where the store is not resolvable from this process.
    """

    override = os.environ.get("CUA_DRIVER_CURSOR_THEME_DIR")
    if override:
        path = Path(override)
        return path if path.is_absolute() else None
    system = platform.system()
    if system == "Windows":
        root = os.environ.get("LOCALAPPDATA")
        return Path(root) / "Cua Driver" / "cursor-themes" if root else None
    home = os.environ.get("HOME")
    if system == "Darwin":
        return (Path(home) / "Library" / "Application Support" / "Cua Driver"
                / "cursor-themes") if home else None
    data = os.environ.get("XDG_DATA_HOME")
    if data:
        return Path(data) / "cua-driver" / "cursor-themes"
    return Path(home) / ".local" / "share" / "cua-driver" / "cursor-themes" if home else None


def _bundled_artifact(lab: str) -> bytes:
    artifact = THEMES / (lab + ".cua-theme")
    if not artifact.is_file():
        raise RuntimeError(f"bundled cursor theme is missing: {lab}")
    data = artifact.read_bytes()
    if (len(data) > _MAX_ARTIFACT_BYTES or data[:8] != _ARTIFACT_MAGIC
            or int.from_bytes(data[8:10], "little") != _ARTIFACT_VERSION):
        raise RuntimeError(f"bundled cursor theme {lab} is not a Cua v{_ARTIFACT_VERSION} artifact")
    return data


def _store_install(root: Path) -> int:
    """Install every bundled artifact into the driver's theme store.

    This is exactly what ``cua-driver cursor-theme install`` does after its own
    validation: an atomic write of ``<id>.cua-theme`` into the store. The driver
    decodes and validates each file again whenever it loads one, so a copy it
    would reject is never shown. An identical file is left untouched, and an
    outdated one is replaced.
    """

    if root.is_symlink():
        raise RuntimeError("the Cua cursor-theme store must not be a symlink")
    root.mkdir(parents=True, exist_ok=True)
    installed = 0
    for lab, entry in PALETTE.items():
        data = _bundled_artifact(lab)
        target = root / (entry["id"] + ".cua-theme")
        if target.is_symlink():
            raise RuntimeError(f"installed cursor theme {entry['id']} must not be a symlink")
        try:
            current = target.read_bytes() if target.is_file() else None
        except OSError:
            current = None
        if current != data:
            staged = root / (".%s.%d.tmp" % (entry["id"], os.getpid()))
            try:
                staged.write_bytes(data)
                os.replace(staged, target)
            finally:
                if staged.exists():
                    staged.unlink()
        installed += 1
    return installed


def _store_ids(root: Path) -> set[str]:
    ids = {"cua.default"}
    try:
        entries = list(root.iterdir())
    except OSError:
        return ids
    for entry in entries:
        if entry.name.endswith(".cua-theme") and entry.is_file() and not entry.is_symlink():
            ids.add(entry.name[: -len(".cua-theme")])
    return ids


def installed_theme_ids(binary: Path) -> set[str]:
    if _sidecar(binary) is None:
        root = theme_store_root()
        if root is None:
            raise ValueError("the Cua cursor-theme store is not resolvable")
        return _store_ids(root)
    result = subprocess.run([str(binary), "cursor-theme", "list", "--json"],
                            capture_output=True, text=True, encoding="utf-8",
                            errors="replace", timeout=15, check=True)
    ids = json.loads(result.stdout)
    if not isinstance(ids, list) or not all(isinstance(value, str) for value in ids):
        raise ValueError("invalid Cua cursor-theme inventory")
    return set(ids)


def install_bundled_themes(binary: Path) -> int:
    """Install only reviewed, bundled artifacts.

    With Cua's compiler present, the driver CLI validates and installs each
    artifact. The pip-provisioned driver has no compiler, so its store receives
    the same atomic file install directly. No caller-supplied path, source, or
    color reaches either route. Existing same-ID installs are accepted.
    """

    if _sidecar(binary) is None:
        root = theme_store_root()
        if root is None:
            raise RuntimeError("the Cua cursor-theme store is not resolvable")
        return _store_install(root)
    installed = 0
    for lab in PALETTE:
        artifact = THEMES / (lab + ".cua-theme")
        if not artifact.is_file():
            raise RuntimeError(f"bundled cursor theme is missing: {lab}")
        command = subprocess.run(
            [str(binary), "cursor-theme", "install", str(artifact)],
            capture_output=True, text=True, encoding="utf-8",
            errors="replace", timeout=30, check=False,
        )
        if command.returncode != 0:
            raise RuntimeError(f"cursor theme {lab} installation failed: "
                               f"{(command.stderr or command.stdout).strip()[:300]}")
        installed += 1
    return installed
