"""Bundled Cua cursor themes matching Octet's stable model-prompt palette.

Theme installation is a trusted local setup operation, never an agent-facing
Cua tool. Custom Cua themes are static; selecting a different installed theme
on the next Octet tool boundary follows a model switch without recompilation.
"""

from __future__ import annotations

import json
import platform
import subprocess
from pathlib import Path
from typing import Any, Mapping

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


def themes_supported() -> bool:
    """Whether this platform's driver can install and show cursor themes.

    Linux runs the driver direct, where there is no agent-cursor overlay, and
    the Linux wheel ships no ``cua-cursor-theme`` compiler to install with.
    """

    return platform.system().lower() != "linux"


def installed_theme_ids(binary: Path) -> set[str]:
    result = subprocess.run([str(binary), "cursor-theme", "list", "--json"],
                            capture_output=True, text=True, timeout=15, check=True)
    ids = json.loads(result.stdout)
    if not isinstance(ids, list) or not all(isinstance(value, str) for value in ids):
        raise ValueError("invalid Cua cursor-theme inventory")
    return set(ids)


def install_bundled_themes(binary: Path) -> int:
    """Install only reviewed, bundled artifacts from an explicit local setup.

    The driver CLI validates each artifact again. No caller-supplied path, source,
    or color reaches the installer. Existing same-ID installs are accepted.
    """

    installed = 0
    for lab in PALETTE:
        artifact = THEMES / (lab + ".cua-theme")
        if not artifact.is_file():
            raise RuntimeError(f"bundled cursor theme is missing: {lab}")
        command = subprocess.run(
            [str(binary), "cursor-theme", "install", str(artifact)],
            capture_output=True, text=True, timeout=30, check=False,
        )
        if command.returncode != 0:
            raise RuntimeError(f"cursor theme {lab} installation failed: "
                               f"{(command.stderr or command.stdout).strip()[:300]}")
        installed += 1
    return installed
