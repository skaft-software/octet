#!/usr/bin/env python3
"""Optional bidirectional voice for octet; importing never opens audio/network."""
from __future__ import annotations

import json
import os
from pathlib import Path
import subprocess
import sys

ROOT = Path(os.environ.get("OCTET_EXTENSION_DIR", Path(__file__).resolve().parent)).resolve()
sys.path.insert(0, str(ROOT))
sys.path.insert(0, str(ROOT / "vendor"))
from octet_extension import Extension, RpcError
from audio import AudioError, _child_environment
from service import VoiceService, audio_backend, plain
from voice_ui import VoiceStrip
import settings

VOICE_INPUT_PREFIX = "[Voice handoff to coding agent]\n"

HELP = ("/voice [start|stop|mute|status|devices|settings]\n"
        "/voice captions up|down · /voice backend codex|openai · /voice model NAME · /voice speaker NAME\n"
        "Ctrl+F6/F7 scroll captions; Ctrl+F8 toggles voice; Ctrl+F9 toggles microphone mute; "
        "Ctrl+F10 stops immediately, even during coding.\n"
        "Starting voice sends microphone audio, observed typed prompts, and completed agent answers to OpenAI. "
        "No recording is saved. Use headphones: local acoustic echo cancellation is not included.")


def probe():
    return audio_backend()[1].probe()


def build_extension():
    ext = Extension(api_version="0.4", max_concurrent_requests=4,
                    supported_features=["request_cancellation", "content_parts", "lifecycle_events",
                                        "message_injection", "provider_credentials", "remote_ui"])

    def inject(text, owner, parent, *, cancellation_event):
        # User-originated delegation enters the ordinary host execution/policy
        # path. Never run tools/shell directly from the voice model.
        ext.request("session/send_user_message", {
            "text": VOICE_INPUT_PREFIX + text, "deliver_as": "steer", "resource_owner": owner,
        }, parent_request_id=parent, timeout=10, cancellation_event=cancellation_event)

    service = VoiceService(inject, ext.notify)
    strip = VoiceStrip(ext, service)

    @ext.on_ui_event("ui/resize")
    @ext.on_ui_event("ui/closed")
    def ui_event(event):
        strip.event(event)

    def stop_quietly():
        result = service.call(service.stop(), timeout=6)
        return "" if service.snapshot()["state"] == "stopped" else result

    def start(context):
        required = {"message_injection", "provider_credentials", "remote_ui"}
        if not required <= ext.negotiated_features:
            return "This host did not offer voice's required input/auth services. Use the candidate host."
        owner = context.get("resource_owner")
        host = context.get("host", {})
        if host.get("has_ui") is False:
            return "Voice requires an interactive foreground session."
        ticket = service.call(service.reserve(owner, host.get("session_id"), ext.request_id))
        try:
            strip.open(owner, ext.request_id)
            config = settings.load()
            available = probe()
            if not available["available"]:
                raise settings.SettingsError(available["error"])
            provider, model = settings.credential_route(config, context)
            response = ext.request("provider/credentials", {"provider": provider, "model": model},
                                   timeout=10)
            credential = settings.credential_value(config, response)
            ext.cancellation.raise_if_cancelled()
            service.call(service.activate(ticket, config, credential))
            return ""
        except BaseException:
            service.call(service.failed_start(ticket))
            raise

    def control(arguments, context):
        arguments = list(arguments)
        action = arguments[0] if arguments else "toggle"
        if action in ("toggle", "start") and len(arguments) <= 1:
            if action == "toggle" and service.snapshot()["state"] not in ("stopped", "failed"):
                return stop_quietly()
            return start(context)
        if action == "stop" and len(arguments) == 1:
            return stop_quietly()
        if action == "mute" and len(arguments) == 1:
            result = service.call(service.mute(context.get("resource_owner")))
            return "" if result in ("Microphone muted", "Microphone live", "Voice is stopped") else result
        if action == "captions" and len(arguments) == 2 and arguments[1] in ("up", "down"):
            if service.owns(context) and strip.page(-1 if arguments[1] == "up" else 1):
                return ""
            return "No live voice captions to scroll in this session."
        if action == "status" and len(arguments) == 1:
            # Command results enter native history; ephemeral speech belongs only
            # on the live strip, never in a status report or diagnostic transcript.
            snapshot = service.snapshot()
            status = {key: snapshot[key] for key in (
                "state", "muted", "microphone_level", "speaker_level",
                "backend", "error", "elapsed_seconds")}
            return json.dumps({"voice": status, "audio": probe()}, indent=2)
        if action == "devices" and len(arguments) == 1:
            if sys.platform in ("darwin", "win32"):
                try:
                    return plain(json.dumps(audio_backend()[1].probe_devices(), ensure_ascii=False, indent=2))
                except AudioError as error:
                    return plain(str(error))
            try:
                result = subprocess.run(["wpctl", "status", "--name"],
                    env=_child_environment(), capture_output=True, timeout=3, check=False)
            except (OSError, subprocess.TimeoutExpired):
                return "Cannot list devices. Run wpctl status in your desktop session."
            if result.returncode:
                return "PipeWire is unavailable. Run wpctl status in your desktop session."
            return plain(result.stdout[:16384].decode("utf-8", errors="replace"))
        if action == "settings" and len(arguments) == 1:
            return f"{settings.settings_path()}\n{json.dumps(settings.load(), indent=2)}\n{HELP}"
        if action in ("backend", "model", "speaker") and len(arguments) == 2:
            if service.snapshot()["state"] not in ("stopped", "failed"):
                return "Stop voice before changing settings; changes apply to the next conversation."
            config = settings.load()
            value = arguments[1]
            if action == "backend":
                if value not in settings.PRESETS:
                    return "Backend must be codex or openai"
                config["backend"] = value
                config["model"], config["voice"] = settings.PRESETS[value]
                config["credential_model"] = None
            else:
                config["model" if action == "model" else "voice"] = value
            settings.save(config)
            return "Voice settings saved; apply at the next /voice start."
        return HELP

    def safe_control(arguments, context):
        try:
            return control(arguments, context)
        except settings.SettingsError as error:
            return str(error)
        except ValueError:
            return "Voice is already active, or this command has no foreground session owner."
        except (TimeoutError, RpcError):
            return "Voice input/auth request failed or timed out. No request is automatically retried. Check /voice status."

    @ext.command(name="voice", description="Two-way realtime voice with the active coding conversation",
                 usage="/voice [start|stop|mute|status|devices|settings]")
    def voice(arguments, context):
        return {"text": safe_control(arguments, context)}

    @ext.shortcut(name="voice-caption-up")
    def caption_up(context):
        if service.owns(context):
            strip.page(-1)
        return {"text": ""}

    @ext.shortcut(name="voice-caption-down")
    def caption_down(context):
        if service.owns(context):
            strip.page(1)
        return {"text": ""}

    @ext.shortcut(name="voice-toggle")
    def toggle(context):
        return {"text": safe_control([], context)}

    @ext.shortcut(name="voice-mute")
    def mute(context):
        return {"text": safe_control(["mute"], context)}

    @ext.shortcut(name="voice-stop")
    def stop(context):
        return {"text": safe_control(["stop"], context)}

    @ext.hook("before_prompt")
    def before_prompt(payload, context):
        text = payload.get("prompt", "")
        # Idle handoffs pass before_prompt; live steering bypasses it. A source
        # marker prevents reflecting our own input back as a second request.
        if service.owns(context) and not text.startswith(VOICE_INPUT_PREFIX):
            service.call(service.update("prompt", text, context["resource_owner"]))
        return None

    @ext.hook("after_response")
    def after_response(payload, context):
        if service.owns(context):
            service.call(service.update("answer", payload.get("response", ""), context["resource_owner"]))
        return None

    @ext.on_lifecycle("session_settled")
    def session_settled(event):
        service.call(service.session_settled(event.get("session_id")), timeout=6)

    @ext.on_lifecycle("turn_settled")
    def turn_settled(event):
        if (event.get("session_id") == service._host_session
                and event.get("outcome") not in (None, "completed")):
            # Status only, not a fabricated final answer or raw failure text.
            service.call(service.update("answer", "The coding-agent run did not complete successfully. "
                "Check octet for its status; do not claim success.", service._owner))

    @ext.menu
    def menu(params):
        snapshot = service.snapshot()
        active = snapshot["state"] not in ("stopped", "failed")
        return {"title": "Voice", "status": {
            "state": "active" if active else "stopped", "label": snapshot["state"]},
            "detail": "Two-way voice. Ctrl+F8 toggles; Ctrl+F9 mutes. Headphones recommended.",
            "items": [{"id": action, "label": label, "command": "voice", "arguments": [action]}
                      for action, label in [("stop" if active else "start", "Stop voice" if active else "Start voice"),
                                            ("mute", "Toggle microphone mute"), ("devices", "Audio devices"),
                                            ("settings", "Settings"), ("status", "Status")]]}

    @ext.on_shutdown
    def shutdown(params):
        service.close()
        strip.close()

    return ext, service


if __name__ == "__main__":
    extension, service = build_extension()
    try:
        extension.run()
    finally:
        service.close()
