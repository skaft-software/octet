import json
import os
from pathlib import Path
import tempfile
import unittest

import settings


class SettingsTests(unittest.TestCase):
    def test_defaults_and_private_atomic_roundtrip(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "voice.json"
            self.assertEqual(settings.load(path), settings.DEFAULTS)
            preferences = dict(settings.DEFAULTS, input_target="stable-node", credential_model="gpt-6.1-sol")
            settings.save(preferences, path)
            self.assertEqual(settings.load(path), preferences)
            if os.name == "posix":  # Windows uses the user's inherited profile ACL.
                self.assertEqual(path.stat().st_mode & 0o777, 0o600)
            self.assertEqual(list(path.parent.iterdir()), [path])

    def test_invalid_boundary(self):
        invalid = [[], None, {"backend": []}, {"backend": "other"}, {"url": "https://bad"},
                   {"model": "gpt/name"}, {"voice": "a" * 129}, {"voice": None},
                   {"input_target": "\0"}, {"output_target": 5}, {"credential_model": "\n"}]
        for value in invalid:
            with self.subTest(value=value), self.assertRaises(settings.SettingsError):
                settings.validate(value)

    def test_native_device_names_preserve_existing_validation(self):
        for target in (None, "alsa_input.usb-Microphone", "Built-in Microphone",
                       "Microphone (USB Audio Device)", "Casque stéréo"):
            with self.subTest(target=target):
                config = settings.validate({"input_target": target, "output_target": target})
                self.assertEqual(config["input_target"], target)
                self.assertEqual(config["output_target"], target)
        for target in ("", " ", 3, "bad\0name", "bad\nname", "bad\x7fname", "é" * 129):
            for key in ("input_target", "output_target"):
                with self.subTest(key=key, target=target), self.assertRaises(settings.SettingsError):
                    settings.validate({key: target})

    def test_bad_json_and_oversized_file(self):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "voice.json"
            for content in ("not json", "[]", "{" + " " * 17000):
                path.write_text(content)
                with self.assertRaises(settings.SettingsError):
                    settings.load(path)

    def test_exact_native_provider_model_not_realtime_model(self):
        for host in ({"model_view": {"provider": "codex", "id": "coding-model"}},
                     {"model": "codex/coding-model"}):
            self.assertEqual(settings.credential_route(settings.DEFAULTS, {"host": host}),
                             ("codex", "coding-model"))
        for host in ({}, {"model": "openai/coding-model"}):
            with self.assertRaises(settings.SettingsError):
                settings.credential_route(settings.DEFAULTS, {"host": host})
        self.assertEqual(settings.credential_route(dict(settings.DEFAULTS, credential_model="coding-model"), {}),
                         ("codex", "coding-model"))

    def test_only_allowlisted_auth_material_forwarded(self):
        response = {"ok": True, "apiKey": "fixture-token", "headers": {
            "chatgpt-account-id": "fixture-account", "Private-Other-Header": "not-forwarded"}}
        self.assertEqual(settings.credential_value(settings.DEFAULTS, response),
                         {"access_token": "fixture-token", "account_id": "fixture-account"})
        self.assertEqual(settings.credential_value(dict(settings.DEFAULTS, backend="openai"), response),
                         {"api_key": "fixture-token"})
        for value in (None, {"ok": False}, {"ok": True}, {"ok": True, "apiKey": "bad\n"},
                      {"ok": True, "apiKey": "fixture-token", "headers": {}}):
            with self.subTest(value=value), self.assertRaises(settings.SettingsError):
                settings.credential_value(settings.DEFAULTS, value)


if __name__ == "__main__":
    unittest.main()
