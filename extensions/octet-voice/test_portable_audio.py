"""Portable audio fixtures: no providers, native audio libraries or device opens."""

import asyncio
from array import array
import importlib
import threading
import time
import unittest
from unittest import mock

import portable_audio as audio


class FakeStream:
    def __init__(self, owner, capture, kwargs):
        self.owner = owner
        self.capture = capture
        self.kwargs = kwargs
        self.started = self.aborted = self.closed = False
        self.thread_ids = [threading.get_ident()]

    def start(self):
        self.thread_ids.append(threading.get_ident())
        if self.owner.start_failure == self.capture:
            raise RuntimeError("fixture start failure")
        if (self.owner.start_gate is not None
                and self.owner.start_gate_capture in (None, self.capture)):
            self.owner.start_entered.set()
            self.owner.start_gate.wait(2)
        self.started = True
        if self.owner.finished_on_start == self.capture:
            self.kwargs["finished_callback"]()
        if self.owner.invalid_callback_on_start == self.capture:
            self.kwargs["callback"](object(), 480, None, None)
        if self.capture and self.owner.capture_on_start:
            self.capture_pcm(self.owner.capture_on_start)

    def abort(self):
        self.thread_ids.append(threading.get_ident())
        self.aborted = True
        if self.owner.abort_gate is not None:
            self.owner.abort_entered.set()
            self.owner.abort_gate.wait(2)

    def close(self):
        self.thread_ids.append(threading.get_ident())
        if self.owner.close_failure == self.capture:
            raise RuntimeError("fixture close failure")
        self.closed = True
        self.kwargs["finished_callback"]()
        if self.owner.invalid_callback_on_close:
            self.kwargs["callback"](object(), 480, None, None)

    def capture_pcm(self, pcm, status=None):
        self.kwargs["callback"](memoryview(pcm), len(pcm) // 2, None, status)

    def render(self, frames):
        result = bytearray(b"\xff" * (frames * 2))
        self.kwargs["callback"](memoryview(result), frames, None, None)
        return bytes(result)


class FakeSoundDevice:
    def __init__(self):
        self.streams = []
        self.start_failure = None
        self.finished_on_start = None
        self.invalid_callback_on_start = None
        self.finished_on_construct = None
        self.invalid_callback_on_construct = None
        self.invalid_callback_on_close = False
        self.capture_on_start = None
        self.close_failure = None
        self.factory_gate = None
        self.factory_gate_capture = None
        self.factory_entered = threading.Event()
        self.start_gate = None
        self.start_gate_capture = None
        self.start_entered = threading.Event()
        self.abort_gate = None
        self.abort_entered = threading.Event()

    def factory(self, capture, **kwargs):
        if self.factory_gate is not None and self.factory_gate_capture in (None, capture):
            self.factory_entered.set()
            self.factory_gate.wait(2)
        stream = FakeStream(self, capture, kwargs)
        self.streams.append(stream)
        if self.finished_on_construct == capture:
            stream.kwargs["finished_callback"]()
        if self.invalid_callback_on_construct == capture:
            stream.kwargs["callback"](object(), 480, None, None)
        return stream

    def RawInputStream(self, **kwargs):
        return self.factory(True, **kwargs)

    def RawOutputStream(self, **kwargs):
        return self.factory(False, **kwargs)

    def query_devices(self):
        return [{"name": "fixture", "max_input_channels": 1,
                 "max_output_channels": 2, "default_samplerate": 48000.0}]


async def eventually(predicate):
    for _ in range(200):
        if predicate():
            return
        await asyncio.sleep(0.002)
    raise AssertionError("condition did not become true")


class ProbeTests(unittest.TestCase):
    def test_import_is_lazy(self):
        with mock.patch.object(importlib, "import_module", side_effect=AssertionError("eager import")):
            importlib.reload(audio)
            instance = audio.PortableAudio()
            self.assertEqual(instance.levels(), (0.0, 0.0))

    def test_probe_and_device_listing_do_not_construct_streams(self):
        sd = FakeSoundDevice()
        with mock.patch.object(audio, "_sounddevice", return_value=sd):
            self.assertTrue(audio.probe()["available"])
            devices = audio.probe_devices()
        self.assertEqual(devices[0]["target"], "fixture")
        self.assertEqual(devices[0]["output_channels"], 2)
        self.assertFalse(sd.streams)

    def test_optional_dependency_missing(self):
        with mock.patch.object(importlib, "import_module", side_effect=ImportError):
            result = audio.probe()
        self.assertFalse(result["available"])
        self.assertIn("sounddevice", result["error"])

    def test_unsupported_endianness(self):
        with mock.patch.object(audio.sys, "byteorder", "big"):
            self.assertFalse(audio.probe()["supported"])

    def test_invalid_targets(self):
        for target in ("", 4, "bad\0name"):
            with self.assertRaises(ValueError):
                audio.PortableAudio(input_target=target)


class PortableAudioTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.sd = FakeSoundDevice()
        self.patch = mock.patch.object(audio, "_sounddevice", return_value=self.sd)
        self.patch.start()
        self.addCleanup(self.patch.stop)
        self.audio = audio.PortableAudio("fixture input", "fixture output")
        await self.audio.start()
        self.addAsyncCleanup(self.audio.stop)
        self.capture, self.output = self.sd.streams

    async def test_native_lifecycle_off_loop_and_native_blocksize(self):
        for stream, target in ((self.capture, "fixture input"), (self.output, "fixture output")):
            self.assertEqual(stream.kwargs["device"], target)
            self.assertEqual(stream.kwargs["samplerate"], 24000)
            self.assertEqual(stream.kwargs["channels"], 1)
            self.assertEqual(stream.kwargs["dtype"], "int16")
            self.assertEqual(stream.kwargs["blocksize"], 0)
            self.assertEqual(stream.kwargs["latency"], 0.06)
        await self.audio.stop()
        for stream in self.sd.streams:
            self.assertTrue(stream.aborted and stream.closed)
            self.assertNotIn(threading.get_ident(), stream.thread_ids)

    async def test_arbitrary_frames_partial_chunks_without_seams(self):
        pcm = array("h", range(1600)).tobytes()
        for start, end in ((0, 2), (2, 100), (100, 2074), (2074, len(pcm))):
            await self.audio.play(pcm[start:end])
        rendered = b"".join(self.output.render(frames) for frames in (1, 17, 483, 41, 1000, 58))
        self.assertEqual(rendered, pcm)
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)

    async def test_tiny_pcm_empty_input_and_continuous_idle_silence(self):
        await self.audio.play(b"")
        for frames in (1, 13, 480, 1024):
            self.assertEqual(self.output.render(frames), b"\0" * frames * 2)
        await self.audio.play(b"\x10\x20")
        self.assertEqual(self.output.render(3), b"\0" * 6)
        with mock.patch.object(audio.time, "monotonic", return_value=time.monotonic() + 0.061):
            self.assertEqual(self.output.render(3), b"\x10\x20\0\0\0\0")
        self.assertFalse(self.output.aborted)
        self.assertEqual(self.output.render(1), b"\0\0")

    async def test_capture_exact_budget_preserves_partial_frames(self):
        pcm = array("h", range(audio.CAPTURE_MAX_BYTES // 2)).tobytes()
        for offset in range(0, len(pcm), 14):
            self.capture.capture_pcm(pcm[offset:offset + 14])
        self.assertEqual(len(self.audio._capture), audio.CAPTURE_MAX_BYTES)
        self.assertIsNone(self.audio.status()["capture_error"])
        result = b"".join([await self.audio.read() for _ in range(10)])
        self.assertEqual(result, pcm)
        self.capture.capture_pcm(b"\x01\x02")
        self.assertEqual(await self.audio.read(), b"\x01\x02")

    async def test_capture_overflow_signals_without_a_waiting_reader(self):
        losses = []
        self.audio._on_capture_loss = lambda: losses.append(True)
        self.capture.capture_pcm(b"\x01\x02" * (audio.CAPTURE_MAX_BYTES // 2))
        self.capture.capture_pcm(b"\x03\x04")
        self.capture.capture_pcm(b"\x05\x06")
        self.assertEqual(losses, [True])
        self.assertFalse(self.audio._capture)
        self.audio.set_muted(True)
        self.audio.set_muted(False)
        with self.assertRaisesRegex(audio.AudioError, "capture discontinuity"):
            await self.audio.read()

    async def test_single_large_capture_callback_fails_without_copying(self):
        losses = []
        self.audio._on_capture_loss = lambda: losses.append(True)
        self.capture.capture_pcm(b"\x01\x02" * 10000)
        self.assertEqual(losses, [True])
        self.assertFalse(self.audio._capture)
        with self.assertRaisesRegex(audio.AudioError, "capture discontinuity"):
            await self.audio.read()

    async def test_native_input_discontinuity_fails_but_output_status_does_not(self):
        from types import SimpleNamespace
        output_only = SimpleNamespace(input_overflow=False, input_underflow=False)
        self.capture.capture_pcm(b"\x01\x02", output_only)
        self.assertEqual(await self.audio.read(), b"\x01\x02")
        for flag in ("input_overflow", "input_underflow"):
            with self.subTest(flag=flag):
                await self.audio.stop()
                await self.audio.start()
                self.capture = self.sd.streams[-2]
                losses = []
                self.audio._on_capture_loss = lambda: losses.append(True)
                status = SimpleNamespace(input_overflow=False, input_underflow=False)
                setattr(status, flag, True)
                self.capture.capture_pcm(b"\x03\x04", status)
                self.assertEqual(losses, [True])
                with self.assertRaisesRegex(audio.AudioError, "capture discontinuity"):
                    await self.audio.read()

    async def test_muted_input_overflow_is_intentionally_discarded(self):
        from types import SimpleNamespace
        self.audio.set_muted(True)
        self.capture.capture_pcm(b"\x01\x02", SimpleNamespace(input_overflow=True, input_underflow=False))
        self.assertIsNone(self.audio.status()["capture_error"])
        self.audio.set_muted(False)
        self.capture.capture_pcm(b"\x03\x04")
        self.assertEqual(await self.audio.read(), b"\x03\x04")

    async def test_stalled_loop_gets_one_coalesced_notification(self):
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        loop = asyncio.get_running_loop()
        original = loop.call_soon_threadsafe
        notifications = []

        def schedule(callback, *args, **kwargs):
            if callback == self.audio._notify:
                notifications.append(callback)
            return original(callback, *args, **kwargs)

        def produce():
            for _ in range(2000):
                self.capture.capture_pcm(b"\x01\x02" * 240)

        with mock.patch.object(loop, "call_soon_threadsafe", side_effect=schedule):
            thread = threading.Thread(target=produce)
            thread.start()
            thread.join(2)  # Deliberately stall this event loop, not the callback thread.
            self.assertFalse(thread.is_alive())
            self.assertEqual(len(notifications), 1)
            self.assertLessEqual(len(self.audio._capture), audio.CAPTURE_MAX_BYTES)
            with self.assertRaisesRegex(audio.AudioError, "capture discontinuity"):
                await asyncio.wait_for(reader, 1)

    async def test_full_budget_admission_is_atomic(self):
        await self.audio.play(b"\x11\x22" * (audio.PLAYBACK_MAX_BYTES // 2))
        with self.assertRaises(audio.AudioBackpressure):
            await self.audio.play(b"\x33\x44")
        self.assertEqual(self.audio.status()["playback_queued_bytes"], audio.PLAYBACK_MAX_BYTES)
        self.assertEqual(self.output.render(audio.PLAYBACK_MAX_BYTES // 2), b"\x11\x22" * 36000)
        with self.assertRaises(audio.AudioBackpressure):
            await self.audio.play(b"\0" * (audio.PLAYBACK_MAX_BYTES + 2))
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)

    async def test_concurrent_admission_never_exceeds_budget(self):
        results = await asyncio.gather(*(self.audio.play(b"\x11\x22" * 18000) for _ in range(3)),
                                       return_exceptions=True)
        self.assertEqual(sum(isinstance(result, audio.AudioBackpressure) for result in results), 1)
        self.assertEqual(self.audio.status()["playback_queued_bytes"], audio.PLAYBACK_MAX_BYTES)

    async def test_mute_clears_captured_audio_and_discards_until_unmuted(self):
        self.capture.capture_pcm(b"\xff\x7f" * 480)
        self.assertGreater(self.audio.levels()[0], 0.9)
        self.audio.set_muted(True)
        self.capture.capture_pcm(b"\xff\x7f" * 480)
        self.assertFalse(self.audio._capture)
        self.assertEqual(self.audio.levels()[0], 0.0)
        self.audio.muted = False
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        self.assertFalse(reader.done())
        self.capture.capture_pcm(b"\x01\x02")
        self.assertEqual(await asyncio.wait_for(reader, 1), b"\x01\x02")
        with self.assertRaises(TypeError):
            self.audio.set_muted(1)

    async def test_interrupt_fences_future_callbacks_without_reopening(self):
        await self.audio.play(b"\xff\x7f" * 500)
        self.output.render(3)
        await self.audio.interrupt()
        self.assertEqual(self.output.render(100), b"\0\0" * 100)
        self.assertEqual(len(self.sd.streams), 2)
        self.assertFalse(self.output.closed)
        await self.audio.play(b"\x34\x12")
        with mock.patch.object(audio.time, "monotonic", return_value=time.monotonic() + 0.061):
            self.assertEqual(self.output.render(2), b"\x34\x12\0\0")

    async def test_levels_measure_rendered_pcm_and_decay(self):
        await self.audio.play(b"\xff\x7f" * 480)
        self.assertEqual(self.audio.levels()[1], 0)
        with mock.patch.object(audio.time, "monotonic", return_value=time.monotonic() + 0.061):
            self.output.render(480)
        self.assertGreater(self.audio.levels()[1], 0.9)
        self.capture.capture_pcm(b"\xff\x7f" * 480)
        with mock.patch.object(audio.time, "monotonic", return_value=time.monotonic() + 0.3):
            self.assertEqual(self.audio.levels(), (0, 0))
        self.output.render(480)
        self.assertEqual(self.audio.levels()[1], 0)

    async def test_stop_wakes_readers_and_is_idempotent(self):
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        await self.audio.stop()
        with self.assertRaises(audio.AudioError):
            await reader
        with self.assertRaises(audio.AudioError):
            await self.audio.play(b"\0\0")
        await self.audio.stop()
        self.assertEqual(self.audio.levels(), (0, 0))

    async def test_unexpected_native_finish_is_reported(self):
        losses = []
        self.audio._on_capture_loss = lambda: losses.append(True)
        self.capture.kwargs["finished_callback"]()
        self.assertEqual(losses, [True])
        with self.assertRaisesRegex(audio.AudioError, "unexpectedly"):
            await self.audio.read()
        self.output.kwargs["finished_callback"]()
        with self.assertRaisesRegex(audio.AudioError, "unexpectedly"):
            await self.audio.play(b"\0\0")

    async def test_capture_callback_failure_signals_without_a_reader(self):
        losses = []
        self.audio._on_capture_loss = lambda: losses.append(True)
        self.capture.kwargs["callback"](object(), 480, None, None)
        self.assertEqual(losses, [True])
        with self.assertRaisesRegex(audio.AudioError, "capture callback failed"):
            await self.audio.read()

    async def test_playback_finish_wakes_read_and_fences_capture_and_output(self):
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        # Queue captured and playback PCM before failing output, without yielding
        # to the waiting reader; neither may be delivered after the failure.
        self.capture.capture_pcm(b"\x01\x02" * 480)
        await self.audio.play(b"\x03\x04" * 1440)
        self.output.kwargs["finished_callback"]()
        self.capture.capture_pcm(b"\x05\x06" * 480)
        with self.assertRaisesRegex(audio.AudioError, "playback stopped unexpectedly"):
            await asyncio.wait_for(reader, 0.2)
        self.assertFalse(self.audio._capture)
        self.assertEqual(self.output.render(480), bytes(960))
        await self.audio.interrupt()
        with self.assertRaisesRegex(audio.AudioError, "playback stopped unexpectedly"):
            await self.audio.read()
        with self.assertRaisesRegex(audio.AudioError, "playback stopped unexpectedly"):
            await self.audio.play(b"\0\0")

    async def test_playback_finish_during_silence_wakes_read(self):
        failures = []
        self.audio._on_playback_failure = lambda: failures.append(True)
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        await asyncio.to_thread(self.output.kwargs["finished_callback"])
        self.assertEqual(failures, [True])
        with self.assertRaisesRegex(audio.AudioError, "playback stopped unexpectedly"):
            await asyncio.wait_for(reader, 0.2)

    async def test_playback_callback_failure_during_silence_wakes_read(self):
        failures = []
        self.audio._on_playback_failure = lambda: failures.append(True)
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        clock = mock.Mock()
        clock.monotonic.side_effect = RuntimeError("fixture callback failure")
        with mock.patch.object(audio, "time", clock):
            self.assertEqual(await asyncio.to_thread(self.output.render, 480), bytes(960))
        self.assertEqual(failures, [True])
        with self.assertRaisesRegex(audio.AudioError, "playback callback failed"):
            await asyncio.wait_for(reader, 0.2)
        self.capture.capture_pcm(b"\x01\x02" * 480)
        with self.assertRaisesRegex(audio.AudioError, "playback callback failed"):
            await self.audio.read()
        self.assertFalse(self.audio._capture)

    async def test_invalid_native_output_buffer_wakes_read(self):
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        await asyncio.to_thread(self.output.kwargs["callback"], object(), 480, None, None)
        with self.assertRaisesRegex(audio.AudioError, "playback callback failed"):
            await asyncio.wait_for(reader, 0.2)

    async def test_readonly_native_output_buffer_wakes_read(self):
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        await asyncio.to_thread(self.output.kwargs["callback"], bytes(960), 480, None, None)
        with self.assertRaisesRegex(audio.AudioError, "playback callback failed"):
            await asyncio.wait_for(reader, 0.2)

    async def test_playback_callback_failure_silences_partially_rendered_pcm(self):
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        await self.audio.play(b"\x01\x02" * 1440)
        with mock.patch.object(audio, "pcm_level", side_effect=RuntimeError("fixture level failure")):
            self.assertEqual(self.output.render(480), bytes(960))
        with self.assertRaisesRegex(audio.AudioError, "playback callback failed"):
            await asyncio.wait_for(reader, 0.2)
        self.assertEqual(self.output.render(480), bytes(960))

    async def test_jittered_rtp_segmentation_and_native_frames_have_no_silence_seams(self):
        # A virtual callback clock avoids sleeps and tests a long idle/restart too.
        clock = [10.0]
        with mock.patch.object(audio.time, "monotonic", side_effect=lambda: clock[0]):
            for burst in range(2):
                samples = array("h", ((index % 30000) + 1 for index in range(24000)))
                pcm = samples.tobytes()
                events = []
                base = clock[0]
                for packet, offset in enumerate(range(0, len(pcm), 960)):
                    jitter = (0.0, 0.004, -0.003, 0.007, -0.005)[packet % 5]
                    events.append((base + packet * 0.02 + jitter, "packet", pcm[offset:offset + 960]))
                callback_at = base
                callback_index = 0
                while callback_at < base + 1.2:
                    frames = (127, 513, 256, 331)[callback_index % 4]
                    events.append((callback_at, "callback", frames))
                    callback_at += frames / 24000
                    callback_index += 1
                rendered = bytearray()
                for at, kind, value in sorted(events, key=lambda event: event[0]):
                    clock[0] = at
                    if kind == "packet":
                        # Split a single RTP frame into tiny and unaligned packet sizes.
                        for start, end in ((0, 2), (2, 40), (40, len(value))):
                            await self.audio.play(value[start:end])
                    else:
                        rendered.extend(self.output.render(value))
                actual = array("h")
                actual.frombytes(rendered)
                while actual and actual[0] == 0:
                    del actual[0]
                while actual and actual[-1] == 0:
                    actual.pop()
                self.assertEqual(actual, samples, f"burst {burst} contains gaps or discarded PCM")
                self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)
                clock[0] += 2.0
                self.assertEqual(self.output.render(1024), b"\0" * 2048)

    async def test_short_tail_flushes_on_deadline_without_more_packets(self):
        clock = [100.0]
        with mock.patch.object(audio.time, "monotonic", side_effect=lambda: clock[0]):
            await self.audio.play(b"\x11\x22" * 5)
            clock[0] += 0.059
            self.assertEqual(self.output.render(7), b"\0" * 14)
            self.assertEqual(self.audio.status()["playback_queued_bytes"], 10)
            clock[0] += 0.002
            self.assertEqual(self.output.render(7), b"\x11\x22" * 5 + b"\0" * 4)
            self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)

    async def test_pcm_validation(self):
        for pcm, error in ((bytearray(2), TypeError), (b"x", ValueError)):
            with self.assertRaises(error):
                await self.audio.play(pcm)


class LifecycleTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.sd = FakeSoundDevice()
        patch = mock.patch.object(audio, "_sounddevice", return_value=self.sd)
        patch.start()
        self.addCleanup(patch.stop)
        self.audio = audio.PortableAudio()
        self.addAsyncCleanup(self.audio.stop)

    async def test_partial_start_failure_closes_all_created_streams(self):
        self.sd.start_failure = False
        self.sd.invalid_callback_on_close = True
        with self.assertRaises(audio.AudioError):
            await self.audio.start()
        self.assertEqual(len(self.sd.streams), 2)
        self.assertTrue(all(stream.closed and stream.aborted for stream in self.sd.streams))
        self.assertFalse(self.audio.status()["running"])
        self.assertIsNone(self.audio.status()["capture_error"])
        self.assertIsNone(self.audio.status()["playback_error"])

    async def test_stream_finish_during_start_fails_closed(self):
        for capture in (True, False):
            with self.subTest(capture=capture):
                instance = audio.PortableAudio()
                first = len(self.sd.streams)
                self.sd.finished_on_start = capture
                try:
                    kind = "capture" if capture else "playback"
                    with self.assertRaisesRegex(audio.AudioError, f"{kind} stopped unexpectedly"):
                        await instance.start()
                    self.assertFalse(instance.status()["running"])
                    self.assertEqual(instance.status()[f"{kind}_error"],
                                     f"PortAudio {kind} stopped unexpectedly")
                    other = "playback" if capture else "capture"
                    self.assertIsNone(instance.status()[f"{other}_error"])
                    created = self.sd.streams[first:]
                    self.assertEqual(len(created), 1 if capture else 2)
                    self.assertTrue(all(stream.aborted and stream.closed for stream in created))
                finally:
                    self.sd.finished_on_start = None
                    await instance.stop()
                await instance.start()
                self.assertTrue(instance.status()["running"])
                self.assertIsNone(instance.status()["capture_error"])
                self.assertIsNone(instance.status()["playback_error"])
                await instance.stop()

    async def test_callback_failure_during_construction_fails_without_starting_stream(self):
        for capture in (True, False):
            for finished in (True, False):
                with self.subTest(capture=capture, finished=finished):
                    instance = audio.PortableAudio()
                    first = len(self.sd.streams)
                    attr = "finished_on_construct" if finished else "invalid_callback_on_construct"
                    setattr(self.sd, attr, capture)
                    try:
                        kind = "capture" if capture else "playback"
                        failure = "stopped unexpectedly" if finished else "callback failed"
                        with self.assertRaisesRegex(audio.AudioError, f"{kind} {failure}"):
                            await instance.start()
                        created = self.sd.streams[first:]
                        self.assertEqual(len(created), 1 if capture else 2)
                        self.assertFalse(created[-1].started)
                        self.assertTrue(all(stream.aborted and stream.closed for stream in created))
                        self.assertFalse(instance.status()["running"])
                        other = "playback" if capture else "capture"
                        self.assertIsNone(instance.status()[f"{other}_error"])
                    finally:
                        setattr(self.sd, attr, None)
                        await instance.stop()

    async def test_invalid_callback_during_start_fails_closed_even_when_muted(self):
        for capture in (True, False):
            with self.subTest(capture=capture):
                instance = audio.PortableAudio()
                instance.set_muted(True)
                first = len(self.sd.streams)
                self.sd.invalid_callback_on_start = capture
                try:
                    kind = "capture" if capture else "playback"
                    with self.assertRaisesRegex(audio.AudioError, f"{kind} callback failed"):
                        await instance.start()
                    self.assertFalse(instance.status()["running"])
                    self.assertEqual(instance.status()[f"{kind}_error"],
                                     f"PortAudio {kind} callback failed")
                    other = "playback" if capture else "capture"
                    self.assertIsNone(instance.status()[f"{other}_error"])
                    created = self.sd.streams[first:]
                    self.assertTrue(created)
                    self.assertTrue(all(stream.aborted and stream.closed for stream in created))
                finally:
                    self.sd.invalid_callback_on_start = None
                    await instance.stop()

    async def test_stream_finish_after_worker_returns_before_handoff_fails_start(self):
        for capture in (True, False):
            with self.subTest(capture=capture):
                instance = audio.PortableAudio()
                first = len(self.sd.streams)
                track_native = instance._track_native

                def track(function, *args):
                    task = track_native(function, *args)
                    if function.__name__ == "_open":
                        def finish_before_handoff(completed):
                            stream = completed.result()[0 if capture else 1]
                            stream.kwargs["finished_callback"]()
                        task.add_done_callback(finish_before_handoff)
                    return task

                try:
                    kind = "capture" if capture else "playback"
                    with mock.patch.object(instance, "_track_native", side_effect=track):
                        with self.assertRaisesRegex(audio.AudioError, f"{kind} stopped unexpectedly"):
                            await instance.start()
                    self.assertFalse(instance.status()["running"])
                    self.assertIsNone(instance.status()[f"{'playback' if capture else 'capture'}_error"])
                    created = self.sd.streams[first:]
                    self.assertEqual(len(created), 2)
                    self.assertTrue(all(stream.aborted and stream.closed for stream in created))
                finally:
                    await instance.stop()

    async def test_capture_from_opening_is_not_forwarded(self):
        self.sd.capture_on_start = b"\x01\x02" * 480
        await self.audio.start()
        self.assertFalse(self.audio._capture)
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        self.assertFalse(reader.done())
        self.sd.streams[0].capture_pcm(b"\x03\x04" * 480)
        self.assertEqual(await asyncio.wait_for(reader, 0.2), b"\x03\x04" * 480)

    async def test_intentional_close_callbacks_do_not_report_stream_failure(self):
        await self.audio.start()
        self.sd.invalid_callback_on_close = True
        await self.audio.stop()
        self.assertIsNone(self.audio.status()["capture_error"])
        self.assertIsNone(self.audio.status()["playback_error"])
        self.sd.invalid_callback_on_close = False
        await self.audio.start()
        self.assertTrue(self.audio.status()["running"])

    async def test_stale_callbacks_from_stopped_streams_do_not_affect_restart(self):
        await self.audio.start()
        old_input, old_output = self.sd.streams
        await self.audio.stop()
        self.assertIsNone(self.audio.status()["capture_error"])
        self.assertIsNone(self.audio.status()["playback_error"])
        await self.audio.start()
        new_input, new_output = self.sd.streams[2:]
        await self.audio.play(b"\x05\x06" * 1440)
        old_input.kwargs["finished_callback"]()
        old_output.kwargs["finished_callback"]()
        old_input.capture_pcm(b"\x01\x02" * 480)
        old_input.kwargs["callback"](object(), 480, None, None)
        old_output.kwargs["callback"](object(), 480, None, None)
        self.assertEqual(old_output.render(480), bytes(960))
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 2880)
        self.assertIsNone(self.audio.status()["capture_error"])
        self.assertIsNone(self.audio.status()["playback_error"])
        new_input.capture_pcm(b"\x03\x04" * 480)
        self.assertEqual(await asyncio.wait_for(self.audio.read(), 0.2), b"\x03\x04" * 480)
        self.assertEqual(new_output.render(480), b"\x05\x06" * 480)

    async def test_stale_callbacks_are_ignored_during_next_open(self):
        await self.audio.start()
        old_input, old_output = self.sd.streams
        await self.audio.stop()
        self.sd.factory_gate = threading.Event()
        self.sd.factory_gate_capture = False
        start = asyncio.create_task(self.audio.start())
        try:
            await eventually(self.sd.factory_entered.is_set)
            old_input.kwargs["finished_callback"]()
            old_output.kwargs["finished_callback"]()
            old_input.kwargs["callback"](object(), 480, None, None)
            old_output.kwargs["callback"](object(), 480, None, None)
            self.assertIsNone(self.audio.status()["capture_error"])
            self.assertIsNone(self.audio.status()["playback_error"])
        finally:
            self.sd.factory_gate.set()
            await asyncio.wait_for(start, 0.5)
        self.assertTrue(self.audio.status()["running"])

    async def test_cancelled_open_is_owned_until_late_native_completion(self):
        self.sd.factory_gate = threading.Event()
        task = asyncio.create_task(self.audio.start())
        await eventually(self.sd.factory_entered.is_set)
        task.cancel()
        with self.assertRaises(asyncio.CancelledError):
            await asyncio.wait_for(task, 0.2)
        with self.assertRaisesRegex(audio.AudioError, "pending"):
            await self.audio.start()
        self.sd.factory_gate.set()
        await eventually(lambda: self.sd.streams and all(stream.closed for stream in self.sd.streams))
        self.assertFalse(self.audio.status()["running"])
        self.assertEqual(len(self.sd.streams), 1)

    async def test_start_timeout_cleans_late_open(self):
        self.sd.factory_gate = threading.Event()
        with mock.patch.object(audio, "START_TIMEOUT", 0.02):
            with self.assertRaises(audio.AudioError):
                await self.audio.start()
        self.sd.factory_gate.set()
        await eventually(lambda: self.sd.streams and all(stream.closed for stream in self.sd.streams))

    async def test_output_factory_timeout_keeps_input_owned_until_late_cleanup(self):
        self.sd.factory_gate = threading.Event()
        self.sd.factory_gate_capture = False
        self.sd.invalid_callback_on_close = True
        try:
            with mock.patch.object(audio, "START_TIMEOUT", 0.02):
                with self.assertRaisesRegex(audio.AudioError, "timed out"):
                    await self.audio.start()
            self.assertTrue(self.sd.factory_entered.is_set())
            self.assertEqual(len(self.sd.streams), 1)
            self.assertTrue(self.sd.streams[0].started)
            self.assertFalse(self.sd.streams[0].closed)
            for operation in (self.audio.stop, self.audio.stop, self.audio.start):
                with self.assertRaisesRegex(audio.AudioError, "pending"):
                    await operation()
            self.assertEqual(len(self.sd.streams), 1)
        finally:
            self.sd.factory_gate.set()
            await eventually(lambda: not self.audio._native_tasks)
        self.assertEqual(len(self.sd.streams), 2)
        self.assertTrue(all(stream.aborted and stream.closed for stream in self.sd.streams))
        self.assertFalse(self.sd.streams[1].started)
        self.assertIsNone(self.audio.status()["capture_error"])
        self.assertIsNone(self.audio.status()["playback_error"])
        await self.audio.stop()
        self.sd.factory_gate = None
        await self.audio.start()
        self.assertEqual(len(self.sd.streams), 4)
        await self.audio.stop()
        self.assertTrue(all(stream.closed for stream in self.sd.streams))

    async def test_cancelled_output_start_fences_stop_until_late_cleanup(self):
        self.sd.start_gate = threading.Event()
        self.sd.start_gate_capture = False
        try:
            task = asyncio.create_task(self.audio.start())
            await eventually(self.sd.start_entered.is_set)
            self.assertEqual(len(self.sd.streams), 2)
            self.assertTrue(self.sd.streams[0].started)
            task.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await asyncio.wait_for(task, 0.2)
            with self.assertRaisesRegex(audio.AudioError, "pending"):
                await self.audio.stop()
            with self.assertRaisesRegex(audio.AudioError, "pending"):
                await self.audio.start()
            self.assertFalse(self.sd.streams[0].closed)
        finally:
            self.sd.start_gate.set()
            await eventually(lambda: not self.audio._native_tasks)
        self.assertTrue(all(stream.aborted and stream.closed for stream in self.sd.streams))
        await self.audio.stop()

    async def test_late_open_cleanup_failure_remains_fenced(self):
        instance = audio.PortableAudio()
        self.sd.factory_gate = threading.Event()
        self.sd.factory_gate_capture = False
        self.sd.close_failure = True
        try:
            with mock.patch.object(audio, "START_TIMEOUT", 0.02):
                with self.assertRaisesRegex(audio.AudioError, "timed out"):
                    await instance.start()
        finally:
            self.sd.factory_gate.set()
            await eventually(lambda: not instance._native_tasks)
        self.assertFalse(self.sd.streams[0].closed)
        with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
            await instance.stop()
        with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
            await instance.start()
        self.assertEqual(len(self.sd.streams), 2)
        # No real devices are involved; explicitly release the fake stream.
        self.sd.close_failure = None
        self.sd.streams[0].close()

    async def test_close_failure_remains_fenced_after_failed_stop(self):
        instance = audio.PortableAudio()
        await instance.start()
        self.sd.close_failure = True
        with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
            await instance.stop()
        self.assertFalse(self.sd.streams[0].closed)
        with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
            await instance.stop()
        with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
            await instance.start()
        self.assertEqual(len(self.sd.streams), 2)
        self.sd.close_failure = None
        self.sd.streams[0].close()

    async def test_cancelled_stop_reports_pending_native_cleanup(self):
        await self.audio.start()
        self.sd.abort_gate = threading.Event()
        try:
            with mock.patch.object(audio, "CLOSE_TIMEOUT", 0.02):
                task = asyncio.create_task(self.audio.stop())
                await eventually(self.sd.abort_entered.is_set)
                task.cancel()
                with self.assertRaisesRegex(audio.AudioError, "timed out"):
                    await task
                with self.assertRaisesRegex(audio.AudioError, "pending"):
                    await self.audio.stop()
                with self.assertRaisesRegex(audio.AudioError, "pending"):
                    await self.audio.start()
        finally:
            self.sd.abort_gate.set()
            await eventually(lambda: not self.audio._native_tasks)
        self.assertTrue(all(stream.closed for stream in self.sd.streams))
        await self.audio.stop()

    async def test_cancellation_while_stopping_finishes_cleanup(self):
        await self.audio.start()
        self.sd.abort_gate = threading.Event()
        task = asyncio.create_task(self.audio.stop())
        await eventually(self.sd.abort_entered.is_set)
        task.cancel()
        self.sd.abort_gate.set()
        with self.assertRaises(asyncio.CancelledError):
            await task
        self.assertTrue(all(stream.closed for stream in self.sd.streams))

    async def test_close_timeout_is_bounded_and_prevents_new_open(self):
        await self.audio.start()
        self.sd.abort_gate = threading.Event()
        with mock.patch.object(audio, "CLOSE_TIMEOUT", 0.02):
            with self.assertRaisesRegex(audio.AudioError, "timed out"):
                await self.audio.stop()
        with self.assertRaisesRegex(audio.AudioError, "pending"):
            await self.audio.start()
        self.sd.abort_gate.set()
        await eventually(lambda: all(stream.closed for stream in self.sd.streams))


if __name__ == "__main__":
    unittest.main()
