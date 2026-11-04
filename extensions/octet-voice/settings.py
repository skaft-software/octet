"""Local voice preferences; credentials stay in octet's native auth resolver."""
from __future__ import annotations

import json
import os
import re
from pathlib import Path
import tempfile

DEFAULTS = {
    "backend": "codex", "model": "gpt-live-1-codex", "voice": "cove",
    "input_target": None, "output_target": None, "credential_model": None,
}
PRESETS = {"codex": ("gpt-live-1-codex", "cove"),
           "openai": ("gpt-realtime-1.5", "marin")}


class SettingsError(ValueError):
    pass


def settings_path():
    return Path.home() / ".config" / "octet" / "voice.json"


def validate(settings):
    if not isinstance(settings, dict) or set(settings) - set(DEFAULTS):
        raise SettingsError("Unknown voice settings; see the extension README")
    merged = dict(DEFAULTS, **settings)
    if not isinstance(merged["backend"], str) or merged["backend"] not in PRESETS:
        raise SettingsError("Voice backend must be codex or openai")
    for key, value in merged.items():
        if value is None and key in ("input_target", "output_target", "credential_model"):
            continue
        if (not isinstance(value, str) or not value.strip() or
                len(value.encode("utf-8")) > 256 or any(ord(c) < 32 or ord(c) == 127 for c in value)):
            raise SettingsError(f"Invalid voice setting: {key}")
        if key in ("model", "voice") and not re.fullmatch(r"[a-zA-Z0-9_.-]{1,128}", value):
            raise SettingsError(f"Invalid voice setting: {key}")
    return merged


def load(path=None):
    path = path or settings_path()
    try:
        with path.open("rb") as stream:
            data = stream.read(16385)
        if len(data) > 16384:
            raise SettingsError("Voice settings exceed 16 KiB")
        settings = json.loads(data)
    except FileNotFoundError:
        return dict(DEFAULTS)
    except (OSError, ValueError) as exc:
        raise SettingsError("Cannot read voice settings; check ~/.config/octet/voice.json") from exc
    return validate(settings)


def save(settings, path=None):
    settings = validate(settings)
    path = path or settings_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".voice-", dir=path.parent)
    try:
        with os.fdopen(fd, "w") as stream:
            json.dump(settings, stream, indent=2)
            stream.write("\n")
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
    finally:
        if os.path.exists(temporary):
            os.unlink(temporary)


def credential_route(settings, context):
    """Select exactly one reviewed native provider/model, never another provider."""
    provider = settings["backend"]
    model = settings["credential_model"]
    view = context.get("host", {}).get("model_view") or {}
    if not model and view.get("provider") == provider:
        model = view.get("id")
    if not model:
        canonical = context.get("host", {}).get("model", "") or ""
        if canonical.startswith(provider + "/"):
            model = canonical.split("/", 1)[1]
    if not model:
        raise SettingsError(f"Select a {provider} coding model first, or set credential_model in voice.json")
    return provider, model


def credential_value(settings, response):
    """Do not copy arbitrary native headers to a different provider endpoint."""
    if not isinstance(response, dict) or response.get("ok") is not True:
        raise SettingsError("Voice credentials unavailable; sign in to the selected native provider")
    token = response.get("apiKey")
    if not isinstance(token, str) or not token or any(ord(c) < 32 for c in token):
        raise SettingsError("The selected native provider has no usable bearer credential")
    if settings["backend"] == "openai":
        return {"api_key": token}
    headers = response.get("headers") or {}
    account = next((value for key, value in headers.items()
                    if key.lower() == "chatgpt-account-id"), None)
    if not isinstance(account, str) or not account or any(ord(c) < 32 for c in account):
        raise SettingsError("Codex voice requires a subscription account; run octet --login codex")
    return {"access_token": token, "account_id": account}
