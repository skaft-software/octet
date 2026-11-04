"""Provider-free cached strip layout, caption preservation and ownership fences."""
import asyncio
import json
import os
import re
import threading
import time
import unittest
from unittest.mock import patch

from voice_ui import (BARS, MAX_CAPTION_BYTES, MAX_COLUMNS, VoiceStrip,
                      caption_page_count, caption_text, cell_width, clip, render,
                      wrap_caption)
from service import VoiceService
from settings import DEFAULTS
from test_service import FakeAudio, FakeClient, OWNER, eventually

ANSI = re.compile(r"\x1b\[[0-9;]*m")


def plain(lines):
    return [ANSI.sub("", line) for line in lines]


def snapshot(**changes):
    state = dict(state="listening", muted=False, elapsed_seconds=65,
                 microphone_level=0.5, speaker_level=0.0, caption="")
    state.update(changes)
    return state


class RenderTests(unittest.TestCase):
    def tearDown(self):
        wrap_caption.cache_clear()

    def test_all_widths_color_plain_spacing_and_height(self):
        state = snapshot(caption="A caption 界e\u0301 👩‍💻 🇺🇳 " * 30)
        for width in (*range(1, 85), 160, 4096, 65535):
            for rows in (1, 3, 12, 20, 21, 24, 36):
                with self.subTest(width=width, rows=rows):
                    lines = render(state, width, [0, 0.5, 1], [1, 0], False, rows=rows)
                    colored = render(state, width, [0, 0.5, 1], [1, 0], True, rows=rows)
                    self.assertEqual(plain(colored), lines)
                    self.assertTrue(all(cell_width(line) <= min(width, MAX_COLUMNS) for line in lines), lines)
                    self.assertTrue(all("\x1b" not in line for line in lines))
                    self.assertLessEqual(len(lines), min(13, rows))
                    if rows >= 5:
                        self.assertEqual(lines[0], "")
                    if rows >= 21:
                        self.assertEqual((lines[2], lines[4], lines[-2]), ("", "", ""))
        self.assertIn("01:05", render(state, 80, [], [], False)[1])
        self.assertIn("preview", render(state, 80, [], [], False)[1])

    def test_vertical_padding_without_horizontal_justification(self):
        state = snapshot(caption="first line\nsecond line")
        lines = render(state, 80, [1], [1], False, rows=36)
        self.assertEqual((lines[0], lines[2], lines[4], lines[7]), ("", "", "", ""))
        self.assertEqual(lines[1], "● voice · listening · preview 1/2 · ^F6/^F7 · 01:05")
        self.assertEqual(lines[3], "MIC " + BARS[0] * 23 + BARS[-1] + "   OUT " + BARS[0] * 23 + BARS[-1])
        self.assertEqual(lines[5:7], ["first line", "second line"])
        self.assertEqual(lines[8], "^F10 stop · ^F9 mute · ^F8 toggle · ^F6/^F7 captions")
        self.assertTrue(all(not line.endswith(" ") for line in lines))

    def test_speaker_transcripts_stay_adjacent_and_empty_state_has_no_extra_gap(self):
        state = snapshot(user_transcript="one two three", voice_transcript="answer")
        lines = render(state, 80, [], [], False, rows=24)
        self.assertEqual(lines[5:7], ["You: one two three", "Voice: answer"])
        self.assertEqual(lines[7], "")
        empty = render(snapshot(), 80, [], [], False, rows=24)
        self.assertEqual(len(empty), 6)
        self.assertEqual((empty[0], empty[2], empty[4]), ("", "", ""))

    def test_small_terminal_uses_compact_rows_to_preserve_caption(self):
        lines = render(snapshot(caption="readable"), 80, [], [], False, rows=12)
        self.assertEqual(len(lines), 5)
        self.assertEqual(lines[0], "")
        self.assertTrue(lines[1].startswith("● voice"))
        self.assertTrue(lines[2].startswith("MIC "))
        self.assertEqual(lines[3], "readable")
        self.assertTrue(lines[4].startswith("^F10"))
        text = "one two three four five six seven eight"
        expected = wrap_caption(text, 7)
        visible = [render(snapshot(caption=text), 7, [], [], False, rows=12,
                          caption_page=page)[:4][3] for page in range(len(expected))]
        self.assertEqual(tuple(visible), expected)

    def test_tiny_terminals_prioritize_content_over_the_top_spacer(self):
        for rows in range(1, 5):
            with self.subTest(rows=rows):
                lines = render(snapshot(caption="readable"), 80, [], [], False, rows=rows)
                self.assertTrue(lines[0].startswith("● voice"))
        lines = render(snapshot(caption="readable"), 80, [], [], False, rows=5)
        self.assertEqual(lines[0], "")
        self.assertEqual(lines[3], "readable")
        self.assertTrue(lines[4].startswith("^F10"))

    def test_vertical_padding_preserves_caption_blank_lines_and_indentation(self):
        lines = render(snapshot(caption="first\n\n  indented\nlast\n"), 80, [], [], False,
                       rows=36, caption_page=1)
        self.assertEqual(lines[5:9], ["", "  indented", "last", ""])
        self.assertEqual(lines[9], "")  # Gap before the controls.
        last = render(snapshot(caption="first\n\n  indented\nlast\n"), 80, [], [], False,
                      rows=36, caption_page=4)
        self.assertEqual(last[5], "")
        self.assertIn("preview 5/5", last[1])

    def test_mute_is_textual_and_meter_histories_are_bounded(self):
        lines = render(snapshot(muted=True), 80, [1] * 100, [1] * 100, False)
        self.assertIn("muted", lines[1])
        self.assertIn("MIC muted " + BARS[0] * 24, lines[3])
        self.assertIn("OUT " + BARS[-1] * 24, lines[3])
        self.assertIn("unmute", lines[-1])
        self.assertIn("MIC muted", render(snapshot(muted=True), 15, [1], [1], False)[3])
        lines = render(snapshot(), 80, [1] * 100 + [0] * 24, [], False)
        self.assertNotIn(BARS[-1], lines[3])
        for width in range(1, 85):
            self.assertTrue(all(cell_width(line) <= width for line in
                                render(snapshot(muted=True, caption="long caption " * 20), width, [1], [1], False)))
        self.assertEqual(clip("界e\u0301x", 3), "界e\u0301")
        self.assertNotIn("\x1b", clip("a\x1bb\x00c", 20))

    def test_every_long_caption_row_reaches_top_despite_host_cropping(self):
        text = "".join(f"{number:05d}" for number in range(5000))
        state = snapshot(caption=text)
        # Simulate the real host retaining heading + meter + ONE caption row
        # (with blank rows between). Every caption row must remain reachable.
        for width in (3, 17, 80, 137):
            expected = wrap_caption(text, width)
            self.assertEqual("".join(expected), text)
            self.assertEqual(caption_page_count(text, width), len(expected))
            recovered = []
            for position, row in enumerate(expected):
                frame = render(state, width, [], [], False, caption_page=position)[:6]
                recovered.append(frame[5])
            self.assertEqual(tuple(recovered), expected)
            self.assertEqual("".join(recovered), text)
        frame = render(state, 80, [], [], False)
        self.assertIn("^F6/^F7", frame[1])  # Hints survive a cropped controls row.
        self.assertIn("^F6/^F7", frame[-1])

    def test_newlines_blank_lines_indentation_and_tabs_are_preserved(self):
        self.assertEqual(wrap_caption("first\n\n  indented\nlast\n", 40),
                         ("first", "", "  indented", "last", ""))
        self.assertEqual(wrap_caption("a\tb\n界\tb", 40), ("a   b", "界  b"))
        self.assertEqual("".join(wrap_caption("one two  three four", 7)), "one two  three four")
        # Whitespace wrapping must make enough room for a wide following unit.
        self.assertTrue(all(cell_width(line) <= 4 for line in wrap_caption(" abc界z", 4)))

    def test_common_graphemes_have_known_cell_widths_and_never_split(self):
        cases = {"界": 2, "e\u0301": 1, "👩‍💻": 2, "👨‍👩‍👧‍👦": 2,
                 "🇺🇳": 2, "👍🏽": 2, "❤️": 2, "1️⃣": 2, "각": 2}
        for grapheme, width in cases.items():
            with self.subTest(grapheme=grapheme):
                self.assertEqual(cell_width(grapheme), width)
                self.assertEqual(wrap_caption(grapheme * 3, width), (grapheme,) * 3)
                self.assertEqual("".join(wrap_caption("x" + grapheme + "z", 3)), "x" + grapheme + "z")
                if width == 2:
                    one_cell = "".join(wrap_caption(grapheme, 1))
                    self.assertTrue(one_cell.isascii())
                    self.assertIn("\\", one_cell)
                    self.assertEqual(one_cell.encode().decode("unicode_escape"), grapheme)
        self.assertEqual(wrap_caption("\u0301abc", 8), ("◌\u0301abc",))

    def test_terminal_controls_bidi_and_surrogates_are_visible_not_executed(self):
        source = "a\x1b]52;c;SECRET\x07\x00\r\x85\u202e\u2066\ud800\u200b\u200d"
        output = "".join(wrap_caption(source, 30))
        for escaped in ("\\x1b", "\\x07", "\\x00", "\\x0d", "\\x85", "\\u202e",
                        "\\u2066", "\\ud800", "\\u200b", "\\u200d"):
            self.assertIn(escaped, output)
        self.assertTrue(all(ord(char) >= 32 and not 127 <= ord(char) < 160 for char in output))
        self.assertNotIn("\u202e", output)
        self.assertEqual("".join(wrap_caption("👩‍a", 20)).encode().decode("unicode_escape"), "👩‍a")

    def test_pathological_clusters_and_frames_stay_inside_protocol_bounds(self):
        source = "a" + "\u0301" * 4000 + "END"
        rows = wrap_caption(source, 4096)
        self.assertTrue("".join(rows).endswith("END"))
        self.assertIn("\\u0301", "".join(rows))
        self.assertEqual("".join(rows).encode().decode("unicode_escape"), source)
        for text in (source, ("a" + "\u0301" * 60) * 1000, "\x00" * 20000):
            frame = render(snapshot(caption=text), 65535, [], [], True)
            self.assertLessEqual(len(frame), 256)
            self.assertTrue(all(len(line.encode()) <= 16384 for line in frame))
            self.assertLessEqual(sum(len(line.encode()) for line in frame), 512 * 1024)
            self.assertLess(len(json.dumps(frame).encode()), 1024 * 1024)
        for text in ("x" * (MAX_CAPTION_BYTES + 1), "界" * (MAX_CAPTION_BYTES // 3 + 1)):
            with self.assertRaisesRegex(ValueError, "byte budget"):
                wrap_caption(text, 80)

    def test_both_retained_utterances_are_accessible_with_legacy_fallback(self):
        state = snapshot(caption="old preview", user_transcript="user " * 1200,
                         voice_transcript="answer " * 1200)
        text = caption_text(state)
        self.assertEqual(text, "You: " + state["user_transcript"] + "\nVoice: " + state["voice_transcript"])
        rows = wrap_caption(text, 80)
        self.assertIn("You:", rows[0])
        voice_position = next(i for i, row in enumerate(rows) if row.startswith("Voice:"))
        self.assertIn("Voice:", render(state, 80, [], [], False, caption_page=voice_position)[5])
        self.assertEqual(caption_text(snapshot(caption="legacy")), "legacy")
        self.assertEqual(caption_text(snapshot(user_transcript="", voice_transcript="")), "")

    def test_caption_is_not_dimmed_and_colors_use_terminal_palette(self):
        lines = render(snapshot(caption="readable caption"), 80, [1], [1])
        self.assertNotIn("\x1b", lines[5])
        self.assertIn("\x1b[36m", lines[3])
        self.assertIn("\x1b[35m", lines[3])
        self.assertNotIn("38;", "".join(lines))
        self.assertNotIn("48;", "".join(lines))


class FakeExtension:
    def __init__(self):
        self.requests, self.frames = [], []
        self.lock = threading.Lock()

    def request(self, method, params, **kwargs):
        with self.lock:
            self.requests.append((method, params, kwargs))
        return {"columns": 80, "rows": 24} if method == "ui/open" else {}

    def publish_ui_frame(self, frame):
        with self.lock:
            self.frames.append(frame)


class FakeService:
    def __init__(self):
        self.stops = 0
        self.state = snapshot()
        self._owner, self._parent, self._epoch = dict(OWNER), 7, 1

    def snapshot(self):
        return dict(self.state)

    async def stop(self):
        self.stops += 1
        self._epoch += 1
        self._owner = self._parent = None

    def call(self, coroutine, **kwargs):
        return asyncio.run(coroutine)


class StripTests(unittest.TestCase):
    def setUp(self):
        self.ext, self.service = FakeExtension(), FakeService()
        self.strip = VoiceStrip(self.ext, self.service)
        self.addCleanup(self.strip.close)

    def test_mount_has_no_focus_and_frames_resize(self):
        self.strip.open(OWNER, 7)
        method, params, kwargs = self.ext.requests[0]
        self.assertEqual(method, "ui/open")
        self.assertEqual(params, {"surface_id": "voice-1", "title": "Voice",
            "placement": "above_editor", "resource_owner": OWNER})
        self.assertEqual(kwargs["parent_request_id"], 7)
        eventually(lambda: bool(self.ext.frames))
        self.strip.event({"surface_id": "voice-1", "columns": 33, "rows": 12})
        eventually(lambda: self.ext.frames[-1]["columns"] == 33)
        self.assertEqual(self.ext.frames[-1]["rows"], 12)
        self.assertEqual(self.ext.frames[-1]["resource_owner"], OWNER)
        self.assertGreater(self.ext.frames[-1]["revision"], self.ext.frames[0]["revision"])
        self.assertTrue(all(method in ("ui/open", "ui/close") for method, _, _ in self.ext.requests))

    def test_scroll_retains_full_text_and_does_not_snap_back_on_live_updates(self):
        self.assertFalse(self.strip.page(1))
        self.service.state.update(user_transcript="abcde" * 1000, voice_transcript="answer" * 100)
        self.strip.open(OWNER, 7)
        eventually(lambda: bool(self.ext.frames))
        self.assertTrue(self.strip.page(-10**6))
        self.assertTrue(self.strip.page(4))
        eventually(lambda: "preview 5/" in plain(self.ext.frames[-1]["lines"])[1])
        self.service.state["voice_transcript"] += "new words " * 100
        expected_text = caption_text(self.service.state)
        eventually(lambda: self.strip._caption == expected_text)
        self.assertEqual(self.strip._caption_page, 4)
        self.strip.event({"surface_id": "voice-1", "columns": 19, "rows": 12})
        eventually(lambda: self.ext.frames[-1]["columns"] == 19)
        self.assertEqual(self.strip._caption, expected_text)
        self.assertEqual(self.strip._caption_page, 4)
        self.strip.page(10**6)
        eventually(lambda: self.strip._caption_page == caption_page_count(expected_text, 19, 12) - 1)
        self.strip.page(-10**6)
        self.assertEqual(self.strip._caption_page, 0)
        self.assertTrue(self.strip._browsing)  # The first row remains readable.
        self.strip.page(10**6)
        self.assertTrue(self.strip._browsing)  # The last row is readable too.
        self.strip.page(1)
        self.assertFalse(self.strip._browsing)  # Down again resumes follow mode.

    def test_long_captions_follow_latest_speaker_but_allow_review(self):
        user = "long user utterance " * 60
        self.service.state.update(user_transcript=user, voice_transcript="spoken reply",
                                  caption="Voice: spoken reply")
        self.strip.open(OWNER, 7)
        eventually(lambda: bool(self.ext.frames) and "Voice: spoken reply" in
                   plain(self.ext.frames[-1]["lines"])[5])
        self.assertGreater(self.strip._caption_page, 0)
        self.strip.page(-10**6)
        eventually(lambda: "You: long user" in plain(self.ext.frames[-1]["lines"])[5])
        self.service.state["voice_transcript"] += " continues"
        self.service.state["caption"] = "Voice: spoken reply continues"
        eventually(lambda: self.strip._caption.endswith("spoken reply continues"))
        self.assertEqual(self.strip._caption_page, 0)
        self.assertTrue(self.strip._browsing)
        self.strip.page(10**6)
        self.strip.page(1)
        eventually(lambda: "Voice: spoken reply continues" in
                   plain(self.ext.frames[-1]["lines"])[5])
        self.assertFalse(self.strip._browsing)

    def test_no_color_environment_disables_all_sgr(self):
        with patch.dict(os.environ, {"NO_COLOR": ""}):
            strip = VoiceStrip(self.ext, self.service)
        self.addCleanup(strip.close)
        self.service.state["caption"] = "Caption 界"
        strip.open(OWNER, 7)
        eventually(lambda: bool(self.ext.frames))
        self.assertTrue(all("\x1b" not in line for line in self.ext.frames[-1]["lines"]))

    def test_cache_is_cleared_on_stop_close_and_owner_retirement(self):
        for action in ("stopped", "close", "retired"):
            with self.subTest(action=action):
                ext, service = FakeExtension(), FakeService()
                service.state["caption"] = "private ephemeral caption"
                strip = VoiceStrip(ext, service)
                try:
                    strip.open(OWNER, 7)
                    eventually(lambda: bool(ext.frames))
                    self.assertEqual(wrap_caption.cache_info().currsize, 1)
                    if action == "stopped":
                        service.state["state"] = "stopped"
                        eventually(lambda: strip._mount is None)
                    elif action == "close":
                        strip.close()
                    else:
                        strip.event({"surface_id": "voice-1", "reason": "owner_retired"})
                    self.assertEqual(strip._caption, "")
                    self.assertEqual(wrap_caption.cache_info().currsize, 0)
                finally:
                    strip.close()

    def test_failed_frame_publication_stops_microphone_and_closes_mount(self):
        notices = []
        def failed(frame):
            raise RuntimeError("transport gone")
        self.ext.publish_ui_frame = failed
        self.ext.notify = lambda text, **kwargs: notices.append((text, kwargs))
        self.strip.open(OWNER, 7)
        eventually(lambda: self.service.stops == 1 and bool(notices))
        self.assertEqual(notices[0][1], {"level": "error"})
        self.assertEqual(self.ext.requests[-1][0], "ui/close")
        self.assertEqual(wrap_caption.cache_info().currsize, 0)
        time.sleep(0.15)
        self.assertEqual(self.service.stops, 1)
        self.assertEqual(len(notices), 1)

    def test_ambiguous_open_is_not_retried_or_closed_as_if_confirmed(self):
        request = self.ext.request

        def ambiguous(method, params, **kwargs):
            request(method, params, **kwargs)
            if method == "ui/open":
                raise TimeoutError("open may have reached the host")

        self.ext.request = ambiguous
        with self.assertRaises(TimeoutError):
            self.strip.open(OWNER, 7)
        self.assertIsNone(self.strip._mount)
        self.strip.close()
        self.assertEqual([method for method, _, _ in self.ext.requests], ["ui/open"])

    def test_ambiguous_close_is_not_retried(self):
        request = self.ext.request

        def ambiguous(method, params, **kwargs):
            response = request(method, params, **kwargs)
            if method == "ui/close":
                raise TimeoutError("close may have reached the host")
            return response

        self.ext.request = ambiguous
        self.strip.open(OWNER, 7)
        eventually(lambda: bool(self.ext.frames))
        self.service.state["state"] = "stopped"
        eventually(lambda: any(method == "ui/close" for method, _, _ in self.ext.requests))
        time.sleep(0.15)
        self.assertEqual([method for method, _, _ in self.ext.requests].count("ui/close"), 1)

    def test_oversized_caption_stops_visibly_instead_of_silently_clipping(self):
        notices = []
        self.ext.notify = lambda text, **kwargs: notices.append(text)
        self.service.state["caption"] = "x" * (MAX_CAPTION_BYTES + 1)
        self.strip.open(OWNER, 7)
        eventually(lambda: self.service.stops == 1 and bool(notices))
        self.assertFalse(self.ext.frames)
        self.assertIn("voice stopped", notices[0])

    def test_replaced_surface_ignores_stale_resize_and_close(self):
        self.strip.open(OWNER, 7)
        self.service._epoch += 1
        self.service._parent = 8
        self.strip.open(OWNER, 8)
        self.strip.event({"surface_id": "voice-1", "columns": 2, "rows": 2})
        self.strip.event({"surface_id": "voice-1", "reason": "owner_retired"})
        eventually(lambda: bool(self.ext.frames))
        self.assertEqual(self.ext.frames[-1]["surface_id"], "voice-2")
        self.assertEqual(self.ext.frames[-1]["columns"], 80)
        self.assertEqual(self.service.stops, 0)
        self.strip.event({"surface_id": "voice-2", "reason": "owner_retired"})
        self.assertEqual(self.service.stops, 1)
        count = len(self.ext.frames)
        time.sleep(0.15)
        self.assertEqual(len(self.ext.frames), count)
        self.strip.event({"surface_id": "voice-2", "reason": "owner_retired"})
        self.assertEqual(self.service.stops, 1)


class BlockedWriterExtension(FakeExtension):
    """Model the SDK's single lock around a blocked stdout frame write."""
    def __init__(self):
        super().__init__()
        self.writing = threading.Event()
        self.release = threading.Event()
        self.closing = threading.Event()

    def request(self, method, params, **kwargs):
        if method == "ui/close":
            self.closing.set()
        return super().request(method, params, **kwargs)

    def publish_ui_frame(self, frame):
        with self.lock:
            self.writing.set()
            self.release.wait()
            self.frames.append(frame)


class PendingOpenExtension(FakeExtension):
    """Hold the first open reply after the host has accepted its request."""
    def __init__(self):
        super().__init__()
        self.opening = threading.Event()
        self.release = threading.Event()

    def request(self, method, params, **kwargs):
        response = super().request(method, params, **kwargs)
        if method == "ui/open" and params["surface_id"] == "voice-1":
            self.opening.set()
            self.release.wait()
        return response


class StripConcurrencyTests(unittest.TestCase):
    def make_strip(self, ext):
        self.audios, self.notices = [], []

        def audio(**kwargs):
            value = FakeAudio(**kwargs)
            self.audios.append(value)
            return value

        self.service = VoiceService(lambda *args, **kwargs: None,
                                    lambda text, **kwargs: self.notices.append(text),
                                    client_factory=FakeClient, audio_factory=audio)
        self.strip = VoiceStrip(ext, self.service)
        self.addCleanup(self.service.close)
        self.addCleanup(self.strip.close)
        if hasattr(ext, "release"):
            self.addCleanup(ext.release.set)

    def start(self, parent):
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", parent))
        self.service.call(self.service.activate(ticket, DEFAULTS, {"api_key": "fixture"}))
        eventually(lambda: self.service.snapshot()["state"] == "listening")
        return self.audios[-1]

    def spawn(self, function):
        done, errors = threading.Event(), []

        def run():
            try:
                function()
            except BaseException as error:
                errors.append(error)
            finally:
                done.set()

        threading.Thread(target=run, daemon=True).start()
        return done, errors

    def test_resize_and_owner_retirement_stop_audio_while_frame_writer_is_blocked(self):
        ext = BlockedWriterExtension()
        self.make_strip(ext)
        audio = self.start(7)
        self.strip.open(OWNER, 7)
        self.assertTrue(ext.writing.wait(2))

        resized, errors = self.spawn(lambda: self.strip.event(
            {"surface_id": "voice-1", "columns": 31, "rows": 12}))
        self.assertTrue(resized.wait(1), "resize waited for the stdout writer")
        self.assertEqual(errors, [])
        self.assertEqual(self.strip._mount["columns"], 31)
        retired, errors = self.spawn(lambda: self.strip.event(
            {"surface_id": "voice-1", "reason": "owner_retired"}))
        self.assertTrue(retired.wait(1), "retirement waited for the stdout writer")
        self.assertEqual(errors, [])
        self.assertTrue(audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "stopped")
        self.assertIsNone(self.strip._mount)
        self.assertEqual(wrap_caption.cache_info().currsize, 0)
        self.assertFalse(ext.release.is_set())
        self.assertEqual(ext.frames, [])
        ext.release.set()
        eventually(lambda: len(ext.frames) == 1)  # This in-flight old frame is host-fenced.
        time.sleep(0.15)
        self.assertEqual(len(ext.frames), 1)

    def test_close_stops_audio_before_its_close_write_can_wait_on_frame(self):
        ext = BlockedWriterExtension()
        self.make_strip(ext)
        audio = self.start(7)
        self.strip.open(OWNER, 7)
        self.assertTrue(ext.writing.wait(2))
        closed, errors = self.spawn(self.strip.close)
        self.assertTrue(ext.closing.wait(1), "close did not reach its protocol write")
        self.assertTrue(audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "stopped")
        self.assertIsNone(self.strip._mount)
        self.assertFalse(ext.release.is_set())
        self.assertFalse(closed.is_set())  # The shared stdout writer is still blocked.
        ext.release.set()
        self.assertTrue(closed.wait(2))
        self.assertEqual(errors, [])
        self.assertEqual([method for method, _, _ in ext.requests].count("ui/close"), 1)

    def test_failed_frame_stops_audio_before_blocked_ui_close(self):
        class FailingFrame(FakeExtension):
            def __init__(self):
                super().__init__()
                self.release, self.closing = threading.Event(), threading.Event()
                self.attempts = 0
                self.notices = []

            def publish_ui_frame(self, frame):
                self.attempts += 1
                raise RuntimeError("ambiguous frame write")

            def request(self, method, params, **kwargs):
                if method == "ui/close":
                    self.closing.set()
                    self.release.wait()
                return super().request(method, params, **kwargs)

            def notify(self, text, **kwargs):
                self.notices.append(text)

        ext = FailingFrame()
        self.make_strip(ext)
        audio = self.start(7)
        self.strip.open(OWNER, 7)
        self.assertTrue(ext.closing.wait(2))
        self.assertTrue(audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "stopped")
        self.assertIsNone(self.strip._mount)
        self.assertFalse(ext.release.is_set())
        self.assertEqual(ext.attempts, 1)
        ext.release.set()
        eventually(lambda: bool(ext.notices))
        self.assertEqual([method for method, _, _ in ext.requests].count("ui/close"), 1)
        self.assertEqual(ext.attempts, 1)

    def test_old_owner_retirement_cannot_stop_a_new_reservation(self):
        ext = BlockedWriterExtension()
        self.make_strip(ext)
        first = self.start(7)
        self.strip.open(OWNER, 7)
        self.assertTrue(ext.writing.wait(2))
        self.service.call(self.service.stop())
        self.assertTrue(first.stopped)
        second = self.start(8)
        retired, errors = self.spawn(lambda: self.strip.event(
            {"surface_id": "voice-1", "reason": "owner_retired"}))
        self.assertTrue(retired.wait(1))
        self.assertEqual(errors, [])
        self.assertFalse(second.stopped)
        self.assertEqual(self.service.snapshot()["state"], "listening")
        self.assertFalse(ext.release.is_set())
        ext.release.set()
        self.strip.open(OWNER, 8)
        self.strip.event({"surface_id": "voice-2", "reason": "owner_retired"})
        self.assertTrue(second.stopped)

    def test_delayed_retirement_stop_is_checked_on_service_loop(self):
        ext = FakeExtension()
        self.make_strip(ext)
        first = self.start(7)
        self.strip.open(OWNER, 7)
        pending, release = threading.Event(), threading.Event()
        self.addCleanup(release.set)
        call = self.service.call

        def delayed_call(coroutine, **kwargs):
            if coroutine.cr_code.co_name == "_stop_mount":
                pending.set()
                release.wait()
            return call(coroutine, **kwargs)

        with patch.object(self.service, "call", side_effect=delayed_call):
            retired, errors = self.spawn(lambda: self.strip.event(
                {"surface_id": "voice-1", "reason": "owner_retired"}))
            self.assertTrue(pending.wait(1))
            self.assertIsNone(self.strip._mount)
            call(self.service.stop())
            self.assertTrue(first.stopped)
            # Same owner, but a distinct reservation epoch and start parent.
            second = self.start(8)
            release.set()
            self.assertTrue(retired.wait(1))
            self.assertEqual(errors, [])
            self.assertFalse(second.stopped)
            self.assertEqual(self.service.snapshot()["state"], "listening")

    def test_old_ui_close_reply_does_not_block_new_owner_retirement(self):
        class PendingClose(FakeExtension):
            def __init__(self):
                super().__init__()
                self.closing, self.release = threading.Event(), threading.Event()

            def request(self, method, params, **kwargs):
                response = super().request(method, params, **kwargs)
                if method == "ui/close" and params["surface_id"] == "voice-1":
                    self.closing.set()
                    self.release.wait()
                return response

        ext = PendingClose()
        self.make_strip(ext)
        first = self.start(7)
        self.strip.open(OWNER, 7)
        self.service.call(self.service.stop())
        self.assertTrue(first.stopped)
        self.assertTrue(ext.closing.wait(2))
        second = self.start(8)
        self.strip.open(OWNER, 8)
        self.strip.event({"surface_id": "voice-1", "reason": "owner_retired"})
        self.assertFalse(second.stopped)
        retired, errors = self.spawn(lambda: self.strip.event(
            {"surface_id": "voice-2", "reason": "owner_retired"}))
        self.assertTrue(retired.wait(1), "old ui/close held the retirement lock")
        self.assertEqual(errors, [])
        self.assertTrue(second.stopped)
        self.assertFalse(ext.release.is_set())
        ext.release.set()

    def test_late_failed_frame_does_not_close_or_stop_replacement(self):
        class DelayedFailure(FakeExtension):
            def __init__(self):
                super().__init__()
                self.writing, self.release = threading.Event(), threading.Event()
                self.notices = []
                self.attempts = 0

            def publish_ui_frame(self, frame):
                if frame["surface_id"] == "voice-1":
                    self.attempts += 1
                    self.writing.set()
                    self.release.wait()
                    raise RuntimeError("ambiguous old frame")
                super().publish_ui_frame(frame)

            def notify(self, text, **kwargs):
                self.notices.append(text)

        ext = DelayedFailure()
        self.make_strip(ext)
        first = self.start(7)
        self.strip.open(OWNER, 7)
        self.assertTrue(ext.writing.wait(2))
        self.service.call(self.service.stop())
        self.assertTrue(first.stopped)
        second = self.start(8)
        self.strip.open(OWNER, 8)
        ext.release.set()
        eventually(lambda: bool(ext.frames))
        self.assertEqual(ext.frames[-1]["surface_id"], "voice-2")
        self.assertEqual(self.strip._mount["surface_id"], "voice-2")
        self.assertFalse(second.stopped)
        self.assertEqual(ext.notices, [])
        self.assertEqual(ext.attempts, 1)
        self.assertEqual([params["surface_id"] for method, params, _ in ext.requests
                          if method == "ui/close"], ["voice-1"])
        self.strip.event({"surface_id": "voice-2", "reason": "owner_retired"})
        self.assertTrue(second.stopped)

    def test_retirement_during_open_stops_audio_without_waiting_for_reply(self):
        ext = PendingOpenExtension()
        self.make_strip(ext)
        audio = self.start(7)
        opened, errors = self.spawn(lambda: self.strip.open(OWNER, 7))
        self.assertTrue(ext.opening.wait(2))
        retired, retirement_errors = self.spawn(lambda: self.strip.event(
            {"surface_id": "voice-1", "reason": "owner_retired"}))
        self.assertTrue(retired.wait(1))
        self.assertEqual(retirement_errors, [])
        self.assertTrue(audio.stopped)
        self.assertIsNone(self.strip._mount)
        self.assertFalse(opened.is_set())
        ext.release.set()
        self.assertTrue(opened.wait(2))
        self.assertEqual(len(errors), 1)
        self.assertIsInstance(errors[0], ValueError)
        self.assertEqual([method for method, _, _ in ext.requests].count("ui/close"), 0)

    def test_late_open_closes_only_old_surface_after_replacement(self):
        ext = PendingOpenExtension()
        self.make_strip(ext)
        first = self.start(7)
        opened, errors = self.spawn(lambda: self.strip.open(OWNER, 7))
        self.assertTrue(ext.opening.wait(2))
        self.service.call(self.service.stop())
        self.assertTrue(first.stopped)
        second = self.start(8)
        self.strip.open(OWNER, 8)
        ext.release.set()
        self.assertTrue(opened.wait(2))
        self.assertEqual(len(errors), 1)
        self.assertIsInstance(errors[0], ValueError)
        self.assertEqual([params["surface_id"] for method, params, _ in ext.requests
                          if method == "ui/close"], ["voice-1"])
        self.assertEqual(self.strip._mount["surface_id"], "voice-2")
        self.assertFalse(second.stopped)
        self.strip.event({"surface_id": "voice-1", "reason": "owner_retired"})
        self.assertFalse(second.stopped)
        self.strip.event({"surface_id": "voice-2", "reason": "owner_retired"})
        self.assertTrue(second.stopped)

    def test_resize_during_open_is_not_overwritten_by_open_geometry(self):
        ext = PendingOpenExtension()
        self.make_strip(ext)
        self.start(7)
        opened, errors = self.spawn(lambda: self.strip.open(OWNER, 7))
        self.assertTrue(ext.opening.wait(2))
        self.strip.event({"surface_id": "voice-1", "columns": 29, "rows": 12})
        ext.release.set()
        self.assertTrue(opened.wait(2))
        self.assertEqual(errors, [])
        eventually(lambda: bool(ext.frames))
        self.assertEqual((ext.frames[0]["columns"], ext.frames[0]["rows"]), (29, 12))


if __name__ == "__main__":
    unittest.main()
