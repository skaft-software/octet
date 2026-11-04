"""Provider-free transcript-to-strip integration with a host-cropped frame."""
import json
import unittest

from realtime import RealtimeClient
from service import VoiceService
from settings import DEFAULTS
from test_service import FakeAudio, OWNER, eventually
from test_voice_ui import FakeExtension, ANSI
from voice_ui import VoiceStrip, caption_page_count, caption_text


class LoopbackClient(RealtimeClient):
    async def connect(self):
        # Exercise the real decoder and service queue without a provider or device.
        self._ready.set()


class VoicePipelineTests(unittest.TestCase):
    def exercise_backend(self, backend):
        clients = []

        def client_factory(config, credential):
            client = LoopbackClient(config, credential)
            clients.append(client)
            return client

        service = VoiceService(lambda *args, **kwargs: None, lambda *args, **kwargs: None,
                               client_factory=client_factory, audio_factory=FakeAudio)
        ext = FakeExtension()
        strip = VoiceStrip(ext, service)
        try:
            settings = dict(DEFAULTS, backend=backend)
            ticket = service.call(service.reserve(OWNER, "host-session", 3))
            self.assertTrue(service.call(service.activate(ticket, settings, {"api_key": "fixture"})))
            eventually(lambda: service.snapshot()["state"] == "listening")
            strip.open(OWNER, 3)
            client = clients[0]

            def emit(event):
                service.call(client._handle(json.dumps(event)))

            words = [f"token{number:03d}" for number in range(55)]
            user = " ".join(words) + " 世界"
            reply = "Voice response remains visible too."
            if backend == "codex":
                emit({"type": "input_transcript.added", "item": {"text": "partial"}})
            else:
                emit({"type": "conversation.item.input_audio_transcription.delta",
                      "item_id": "user-1", "delta": "partial"})
            eventually(lambda: service.snapshot()["user_transcript"] == "partial")
            if backend == "codex":
                emit({"type": "turn.done", "turn": {"role": "user", "transcript": user}})
                emit({"type": "output_transcript.added", "item": {"text": "Voice"}})
                emit({"type": "turn.done", "turn": {"role": "assistant", "transcript": reply}})
            else:
                emit({"type": "conversation.item.input_audio_transcription.completed",
                      "item_id": "user-1", "transcript": user})
                emit({"type": "response.output_audio_transcript.delta",
                      "item_id": "voice-1", "delta": "Voice"})
                emit({"type": "response.output_audio_transcript.done",
                      "item_id": "voice-1", "transcript": reply})
            eventually(lambda: service.snapshot()["user_transcript"] == user
                       and service.snapshot()["voice_transcript"] == reply)
            expected_caption = caption_text(service.snapshot())
            pages = caption_page_count(expected_caption, 80, 24)
            self.assertGreater(pages, 1)
            eventually(lambda: strip._caption == expected_caption)
            strip.page(-pages)  # Start reconstruction at the first retained row.

            # The host may retain only the first caption row and preceding
            # vertical padding. Scrolling must keep every word reachable.
            visible = []
            for page in range(pages):
                marker = f"preview {page + 1}/{pages}"

                def current_frame():
                    with ext.lock:
                        return ext.frames[-1] if ext.frames else None

                eventually(lambda: current_frame() is not None
                           and marker in ANSI.sub("", current_frame()["lines"][1]))
                frame = current_frame()
                self.assertGreaterEqual(len(frame["lines"]), 6)
                self.assertEqual(frame["lines"][0], "")
                self.assertEqual(frame["lines"][2], "")
                self.assertEqual(frame["lines"][4], "")
                visible.append(ANSI.sub("", frame["lines"][5]))
                if page + 1 < pages:
                    self.assertTrue(strip.page(1))
            displayed = " ".join(visible)
            for word in words + ["世界", "Voice", "response", "visible", "too."]:
                self.assertIn(word, displayed)

            service.call(service.stop())
            eventually(lambda: strip._mount is None)
            self.assertEqual(service.snapshot()["user_transcript"], "")
            self.assertEqual(service.snapshot()["voice_transcript"], "")
        finally:
            strip.close()
            service.close()

    def test_codex_v3_pipeline(self):
        self.exercise_backend("codex")

    def test_openai_pipeline(self):
        self.exercise_backend("openai")


if __name__ == "__main__":
    unittest.main()
