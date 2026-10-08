#!/usr/bin/env python3
"""Deterministic observer tests, not a Doom or terminal performance qualification."""
import importlib.util
from pathlib import Path
from types import SimpleNamespace
import unittest

SPEC = importlib.util.spec_from_file_location("pi_pty", Path(__file__).with_name("test-pi-compat-pty.py"))
h = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(h)


def pixels(columns=2, value=10):
    return (f"\x1b[38;2;{value};20;30m\x1b[48;2;40;50;60m" + "▀"*columns).encode()


def frame(value=10):
    return b"\x1b[?2026h\x1b[H" + b"\r\n".join([pixels(value=value)]*3) + h.SYNC_END


class PerformanceObserverTests(unittest.TestCase):
    def test_delimiter_all_fragment_boundaries(self):
        for split in range(len(h.SYNC_END)+1):
            clock = h.FrameClock()
            clock.feed(b"prefix"+h.SYNC_END[:split], 10)
            clock.feed(h.SYNC_END[split:]+b"suffix", 20)
            self.assertEqual(len(clock.frames), 1)
            self.assertEqual(clock.frames[0]["end"], 6+len(h.SYNC_END))
            self.assertEqual(clock.frames[0]["ns"], 10 if split == len(h.SYNC_END) else 20)

    def test_fragmented_marker_not_recounted(self):
        clock = h.FrameClock()
        for byte in b"abc"+h.SYNC_END+b"xyz"+h.SYNC_END:
            clock.feed(bytes([byte]), clock.bytes)
        self.assertEqual(len(clock.frames), 2)
        self.assertEqual(clock.frames[-1]["end"], clock.bytes)

    def test_coalesced_reads_are_not_invented_intervals(self):
        clock = h.FrameClock()
        clock.feed(h.SYNC_END*2, 1_000_000_000)
        clock.feed(h.SYNC_END, 1_030_000_000)
        sample = h.frame_window(clock, 1_000_000_000, 2_000_000_000)
        self.assertEqual(sample["interval_samples_ms"], [0, 30])
        self.assertEqual(sample["output_fps"], 3)
        self.assertEqual(sample["coalesced_read_intervals"], 1)

    def test_half_open_window_and_empty_distribution(self):
        clock = h.FrameClock()
        clock.feed(h.SYNC_END, 10)
        clock.feed(h.SYNC_END, 20)
        sample = h.frame_window(clock, 10, 20)
        self.assertEqual(sample["synchronized_output_frames"], 1)
        self.assertEqual(sample["interval_ms"], {"count": 0})
        self.assertEqual(h.distribution(range(1, 101))["p95"], 95)

    def test_metadata_is_bounded(self):
        clock = h.FrameClock()
        clock.reads = [None]*100000
        with self.assertRaisesRegex(AssertionError, "metadata"):
            clock.feed(b"x", 1)

    def test_rgb_signature_style_reuse_and_fragmented_utf8(self):
        screen = h.Screen(2, 3, track_colors=True)
        for byte in frame():
            screen.feed(bytes([byte]))
        expected = h.hashlib.sha256(bytes([10, 20, 30, 40, 50, 60])*6).hexdigest()
        self.assertEqual(screen.pixel_signature(), expected)
        self.assertEqual(screen.frames, 1)
        screen.feed(b"\x1b[2J")
        self.assertIsNone(screen.pixel_signature())

    def test_color_only_diff_changes_signature(self):
        screen = h.Screen(2, 3, track_colors=True)
        screen.feed(frame())
        before = screen.pixel_signature()
        screen.feed(b"\x1b[H"+pixels(value=11))
        self.assertNotEqual(screen.pixel_signature(), before)
        screen.resize(1, 3)
        self.assertEqual(len(screen.colors[0]), 1)
        self.assertIsNotNone(screen.pixel_signature())

    def test_scroll_keeps_color_rows_aligned(self):
        screen = h.Screen(2, 3, track_colors=True)
        screen.feed(frame())
        screen.feed(b"\r\n"+pixels(value=12))
        self.assertEqual(screen.colors[-1][0][0], (12, 20, 30))
        self.assertEqual(screen.colors[0][0][0], (10, 20, 30))

    def test_correlation_excludes_repeated_source_signatures(self):
        # Correlation starts at the harness's 96x32 geometry; resize at offset 0.
        raw = frame()
        screen = h.Screen(2, 3, track_colors=True)
        screen.feed(raw)
        signature = screen.pixel_signature()
        terminal = SimpleNamespace(capture=raw+raw, clock=SimpleNamespace(frames=[
            {"ns": 2_000_000, "end": len(raw)}, {"ns": 3_000_000, "end": 2*len(raw)}]))
        source = {"ns": 1_000_000, "revision": 1, "hash": signature}
        resize = [{"offset": 0, "columns": 2, "rows": 3}]
        observed, matches = h.correlate_frames(terminal, [source], resize)
        self.assertEqual(len(matches), 1)
        self.assertEqual(matches[0]["lag_ms"], 1)
        self.assertFalse(observed[1]["first_observation"])
        _, ambiguous = h.correlate_frames(terminal, [source, source], resize)
        self.assertEqual(ambiguous, [])

    def test_correlation_does_not_match_future_source(self):
        raw = frame()
        screen = h.Screen(2, 3, track_colors=True)
        screen.feed(raw)
        terminal = SimpleNamespace(capture=raw, clock=SimpleNamespace(frames=[{"ns": 1, "end": len(raw)}]))
        _, matches = h.correlate_frames(terminal, [{"ns": 2, "revision": 0, "hash": screen.pixel_signature()}],
                                       [{"offset": 0, "columns": 2, "rows": 3}])
        self.assertEqual(matches, [])


if __name__ == "__main__":
    unittest.main()
