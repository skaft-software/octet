"""Build a Pi 1.4.0 fixture mirroring the maintainer's real ~/.pi/agent/settings.json.

Same package set, versions and autoload flags; same Pi settings. deviceId and the
openai-codex provider are replaced: the provider by the eval's mock (configured per
leg by fixture.write_provider_config).
"""
import json, sys
from pathlib import Path
sys.path.insert(0, str(Path(__file__).resolve().parent))
import fixture as fx

PACKAGES = [  # (name, version, autoload)
    ("pi-agent-extensions", "0.5.4", True), ("pi-mcp-adapter", "2.31.0", True), ("pi-web-access", "0.27.0", True),
    ("pi-subagents", "0.59.0", True), ("pi-background-tasks", "2.4.2", True), ("pi-lens", "4.1.3", True),
    ("@gotgenes/pi-permission-system", "27.1.2", True), ("pi-hermes-memory", "0.9.7", True),
    ("@juicesharp/rpiv-ask-user-question", "2.8.0", True), ("@juicesharp/rpiv-todo", "2.8.0", False),
    ("pi-hashline-edit-pro", "2.8.3", True), ("bigpowers", "2.87.5", True), ("awesome-pi-themes", "1.1.19", True),
    ("context-mode", "1.0.169", False), ("@plannotator/pi-extension", "0.27.9", False), ("pi-fabric", "0.70.0", False),
    ("@narumitw/pi-plan-mode", "0.56.0", False), ("@narumitw/pi-lsp", "0.49.6", False),
    ("pi-agent-browser-native", "0.5.0", False), ("@llblab/pi-telegram", "0.39.5", False), ("pi-langfuse", "1.5.15", False),
    ("pi-interactive-shell", "0.15.1", False), ("pi-claude-bridge", "0.7.0", False), ("pi-memory", "0.4.2", False),
    ("@narumitw/pi-goal", "0.54.3", False), ("@tintinweb/pi-subagents", "0.19.0", False),
    ("@dietrichgebert/ponytail", "4.9.0", False), ("pi-zentui", "0.21.0", False), ("@termdraw/pi", "0.4.1", True),
]
LOCK = {
    "schema": "octet.pi-eval-lock/v1",
    "pi": {"package": "@earendil-works/pi-coding-agent", "version": "1.4.0"},
    "note": "Maintainer's real Pi setup (2026-11-03); @termdraw/pi pinned to its latest (unpinned in the original).",
    "packages": [{"name": n, "version": v, "autoload": a} for n, v, a in PACKAGES],
    "settings": {
        "theme": "ghostty-dark", "defaultThinkingLevel": "max", "defaultTools": ["+codemode"], "codemode": {"mode": "on"},
        "compaction": {"enabled": True}, "steeringMode": "all", "terminal": {"showTerminalProgress": True},
        "markdown": {"mermaid": "streaming"}, "tuiMode": "fullscreen",
    },
}

def settings_document(lock):
    packages = [f"npm:{p['name']}@{p['version']}" if p["autoload"] else {"source": f"npm:{p['name']}@{p['version']}", "autoload": False}
                for p in lock["packages"]]
    return {"packages": packages, "extensions": [fx.SENTINEL_SETTING], "defaultProvider": fx.PROVIDER, "defaultModel": fx.MODEL,
            "enableInstallTelemetry": False, "lastChangelogVersion": lock["pi"]["version"], **lock["settings"]}

fx.settings_document = settings_document
cache = Path(sys.argv[1]) if len(sys.argv) > 1 else fx.default_cache()
f = fx.build_fixture(cache, LOCK)
print("FIXTURE", f.root)
