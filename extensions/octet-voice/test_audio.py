"""Provider-free audio tests. No test opens a microphone or speaker."""

import asyncio
from array import array
import math
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest import mock

import audio


async def eventually(predicate):
    for _ in range(200):
        if predicate():
            return
        await asyncio.sleep(0.001)
    raise AssertionError("condition did not become true")


class FakeTransport:
    def __init__(self):
        self.aborted = False
        self.limits = None

    def abort(self):
        self.aborted = True

    def set_write_buffer_limits(self, **kwargs):
        self.limits = kwargs


class FakeWriter:
    def __init__(self):
        self.transport = FakeTransport()
        self.writes = []
        self.blocked = None
        self.error = None

    def write(self, data):
        if self.error:
            raise self.error
        self.writes.append(data)

    async def drain(self):
        if self.blocked:
            await self.blocked.wait()


class FakeProcess:
    def __init__(self, capture):
        self.stdin = None if capture else FakeWriter()
        self.stdout = asyncio.StreamReader() if capture else None
        self.returncode = None
        self.exited = asyncio.Event()
        self.terminated = False
        self.killed = False
        self.ignore_terminate = False
        self.waited = False

    def exit(self, code=0):
        self.returncode = code
        if self.stdout and not self.stdout.at_eof():
            self.stdout.feed_eof()
        self.exited.set()

    def terminate(self):
        self.terminated = True
        if not self.ignore_terminate:
            self.exit(-15)

    def kill(self):
        self.killed = True
        self.exit(-9)

    async def wait(self):
        await self.exited.wait()
        self.waited = True
        return self.returncode


class LevelTests(unittest.TestCase):
    def test_pcm_rms_dbfs_scale(self):
        for samples, expected in [([], 0.0), ([0] * 480, 0.0), ([-32768] * 480, 1.0),
                                  ([32767] * 480, 1.0), ([1, -1] * 240, 0.0),
                                  ([16, -16] * 240, 0.0),
                                  ([3277, -3277] * 240, (60 + 20 * math.log10(3277 / 32768)) / 60)]:
            with self.subTest(samples=samples[:2]):
                self.assertAlmostEqual(audio.pcm_level(array("h", samples).tobytes()), expected, places=5)


@unittest.skipUnless(sys.platform == "linux", "PipeWire runtime/ownership prerequisites are Linux-only")
class ProbeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        os.chmod(self.temp.name, 0o700)

    def test_probe_never_starts_process(self):
        with mock.patch.object(audio.asyncio, "create_subprocess_exec") as spawn, \
                mock.patch.object(audio.shutil, "which", side_effect=lambda name, **_: "/bin/" + name):
            result = audio.probe(self.temp.name)
        spawn.assert_not_called()
        self.assertTrue(result["available"])
        self.assertEqual(result["runtime_dir"], self.temp.name)

    def test_environment_preserved_and_explicit_runtime_wins(self):
        with mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": "/bad", "VOICE_TEST": "preserved"}):
            env = audio._child_environment(self.temp.name)
        self.assertEqual(env["VOICE_TEST"], "preserved")
        self.assertEqual(env["XDG_RUNTIME_DIR"], self.temp.name)

    def test_inherited_runtime_used(self):
        with mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": self.temp.name}):
            self.assertEqual(audio._child_environment()["XDG_RUNTIME_DIR"], self.temp.name)

    def test_secure_fallback_only_when_runtime_absent(self):
        info = os.stat(self.temp.name)
        with mock.patch.dict(os.environ, {}, clear=True), \
                mock.patch.object(audio.Path, "lstat", return_value=info) as lstat:
            self.assertEqual(audio._child_environment()["XDG_RUNTIME_DIR"], f"/run/user/{os.getuid()}")
            lstat.assert_called_once_with()
        with mock.patch.dict(os.environ, {"XDG_RUNTIME_DIR": "/missing"}), \
                mock.patch.object(audio.Path, "lstat", side_effect=FileNotFoundError):
            self.assertFalse(audio.probe()["available"])

    def test_reject_insecure_runtime_directory(self):
        os.chmod(self.temp.name, 0o755)
        with self.assertRaises(audio.AudioError):
            audio._child_environment(self.temp.name)

    def test_reject_wrong_owner_and_symlinks(self):
        info = os.stat(self.temp.name)
        wrong_owner = mock.Mock(st_mode=info.st_mode, st_uid=os.getuid() + 1)
        with mock.patch.object(audio.Path, "lstat", return_value=wrong_owner):
            with self.assertRaises(audio.AudioError):
                audio._child_environment(self.temp.name)
        link = Path(self.temp.name) / "link"
        link.symlink_to(self.temp.name)
        with self.assertRaises(audio.AudioError):
            audio._child_environment(link)

    def test_missing_binaries(self):
        with mock.patch.object(audio.shutil, "which", return_value=None):
            self.assertIn("executables", audio.probe(self.temp.name)["error"])

    def test_unsupported_platform_or_endianness(self):
        for attr, value in (("platform", "darwin"), ("byteorder", "big")):
            with self.subTest(attr=attr), mock.patch.object(audio.sys, attr, value):
                self.assertFalse(audio.probe(self.temp.name)["supported"])
                with self.assertRaises(audio.UnsupportedAudioError):
                    audio._child_environment(self.temp.name)

    def test_invalid_targets(self):
        for value in ("", 2, "bad\0target"):
            with self.assertRaises(ValueError):
                audio.PipeWireAudio(input_target=value)


@unittest.skipUnless(sys.platform == "linux", "PipeWire subprocess backend is Linux-only")
class AudioTests(unittest.IsolatedAsyncioTestCase):
    async def asyncSetUp(self):
        self.temp = tempfile.TemporaryDirectory()
        os.chmod(self.temp.name, 0o700)
        self.addCleanup(self.temp.cleanup)
        self.processes = []
        self.calls = []

        async def spawn(*args, **kwargs):
            self.calls.append((args, kwargs))
            process = FakeProcess(kwargs["stdout"] == asyncio.subprocess.PIPE)
            self.processes.append(process)
            return process

        self.spawn = mock.patch.object(audio.asyncio, "create_subprocess_exec", side_effect=spawn).start()
        self.addCleanup(mock.patch.stopall)
        mock.patch.object(audio.shutil, "which", side_effect=lambda name, **_: "/bin/" + name).start()
        self.audio = audio.PipeWireAudio(input_target="Volt 1 input", output_target="Volt 1 output",
                                        runtime_dir=self.temp.name)
        self.addAsyncCleanup(self.audio.stop)

    async def test_measured_levels_decay_mute_interrupt_and_stop(self):
        self.assertEqual(self.audio.levels(), (0.0, 0.0))
        await self.audio.start()
        pcm = array("h", [16384] * 480).tobytes()
        self.processes[0].stdout.feed_data(pcm)
        await eventually(lambda: self.audio.levels()[0] > 0)
        await self.audio.play(pcm)
        await eventually(lambda: self.audio.levels()[1] > 0)
        expected = audio.pcm_level(pcm)
        self.assertAlmostEqual(self.audio.levels()[0], expected)
        self.assertAlmostEqual(self.audio.levels()[1], expected)
        # Advance only the observation clock; no wall-clock sleeps or hardware.
        now = audio.time.monotonic()
        with mock.patch.object(audio.time, "monotonic", return_value=now + 0.2):
            self.assertEqual(self.audio.levels(), (0.0, 0.0))
        self.audio.set_muted(True)
        self.assertEqual(self.audio.levels()[0], 0.0)
        await self.audio.interrupt()
        self.assertEqual(self.audio.levels()[1], 0.0)
        await self.audio.stop()
        self.assertEqual(self.audio.levels(), (0.0, 0.0))

    async def test_start_arguments_and_idempotence(self):
        await self.audio.start()
        await self.audio.start()
        self.assertEqual(len(self.calls), 2)
        for index, (args, kwargs) in enumerate(self.calls):
            self.assertEqual(args[0], "/bin/pw-play" if index else "/bin/pw-record")
            self.assertEqual(args[1:], ("--raw", "--rate", "24000", "--channels", "1",
                                      "--format", "s16", "--latency", "80ms" if index else "20ms", "--target",
                                      "Volt 1 output" if index else "Volt 1 input", "-"))
            self.assertEqual(kwargs["env"]["XDG_RUNTIME_DIR"], self.temp.name)
            self.assertEqual(kwargs["stderr"], asyncio.subprocess.DEVNULL)
            self.assertNotIn("start_new_session", kwargs)
            self.assertNotIn("process_group", kwargs)
            self.assertNotIn("preexec_fn", kwargs)
        self.assertEqual(self.processes[1].stdin.transport.limits, {"high": 960, "low": 0})

    async def test_default_devices_not_forced(self):
        self.audio.input_target = self.audio.output_target = None
        await self.audio.start()
        self.assertNotIn("--target", self.calls[0][0])
        self.assertNotIn("--target", self.calls[1][0])

    async def test_capture_exact_chunks_and_partial_reads(self):
        await self.audio.start()
        capture = self.processes[0].stdout
        capture.feed_data(b"a" * 301)
        read = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        self.assertFalse(read.done())
        capture.feed_data(b"a" * 659)
        self.assertEqual(await asyncio.wait_for(read, 0.2), b"a" * 960)

    async def test_capture_overflow_fails_and_signals_without_a_reader(self):
        losses = []
        self.audio._on_capture_loss = lambda: losses.append(True)
        await self.audio.start()
        chunks = [bytes([i]) * 960 for i in range(audio.CAPTURE_CHUNKS + 1)]
        self.processes[0].stdout.feed_data(b"".join(chunks[:-1]))
        await eventually(lambda: len(self.audio._capture_chunks) == audio.CAPTURE_CHUNKS)
        self.assertIsNone(self.audio.status()["capture_error"])
        self.processes[0].stdout.feed_data(chunks[-1])
        await eventually(lambda: bool(losses))
        self.assertEqual(losses, [True])
        self.assertEqual(self.audio.status()["capture_queued_chunks"], 0)
        self.audio.set_muted(True)
        self.audio.set_muted(False)
        with self.assertRaisesRegex(audio.AudioError, "capture queue overflow"):
            await self.audio.read()

    async def test_mute_transition_while_capture_waits_on_condition_discards_old_chunk(self):
        await self.audio.start()
        capture = self.processes[0].stdout
        async with self.audio._capture_ready:
            capture.feed_data(b"a" * audio.CHUNK_BYTES)
            await eventually(lambda: not capture._buffer)
            # The pump consumed the old chunk but cannot publish it until this
            # lock is released. Cross both transitions before it can do so.
            self.audio.set_muted(True)
            self.audio.set_muted(False)
        await asyncio.sleep(0.01)
        self.assertFalse(self.audio._capture_chunks)
        capture.feed_data(b"b" * audio.CHUNK_BYTES)
        self.assertEqual(await asyncio.wait_for(self.audio.read(), 0.2), b"b" * audio.CHUNK_BYTES)

    async def test_mute_clears_queue_and_continues_discarding(self):
        await self.audio.start()
        self.processes[0].stdout.feed_data(b"a" * 960)
        await eventually(lambda: len(self.audio._capture_chunks) == 1)
        self.audio.set_muted(True)
        self.assertEqual(len(self.audio._capture_chunks), 0)
        self.processes[0].stdout.feed_data(b"b" * 960 * 30)
        await asyncio.sleep(0.01)
        self.assertEqual(len(self.audio._capture_chunks), 0)
        self.audio.muted = False
        # The read begun before the mute transition is deliberately discarded.
        self.processes[0].stdout.feed_data(b"c" * 960 + b"d" * 960)
        self.assertEqual(await asyncio.wait_for(self.audio.read(), 0.2), b"d" * 960)
        with self.assertRaises(TypeError):
            self.audio.muted = "yes"

    async def test_playback_budget_is_atomic_and_includes_inflight(self):
        await self.audio.start()
        writer = self.processes[1].stdin
        writer.blocked = asyncio.Event()
        await self.audio.play(b"x" * audio.PLAYBACK_MAX_BYTES)
        await eventually(lambda: bool(writer.writes))
        self.assertEqual(self.audio.status()["playback_queued_bytes"], audio.PLAYBACK_MAX_BYTES)
        with self.assertRaises(audio.AudioBackpressure):
            await self.audio.play(b"xx")
        self.assertEqual(self.audio.status()["playback_queued_bytes"], audio.PLAYBACK_MAX_BYTES)

    async def test_playback_prebuffers_jitter_and_flushes_a_short_tail(self):
        await self.audio.start()
        writer = self.processes[1].stdin
        await self.audio.play(b"a" * 1920 + b"bb")
        self.assertEqual(self.audio._output_frame(audio.time.monotonic()), (audio.SILENCE, 0))
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 1922)
        await eventually(lambda: self.audio.status()["playback_queued_bytes"] == 0)
        voiced = [frame for frame in writer.writes if frame.strip(b"\0")]
        self.assertEqual(voiced, [b"a" * 960, b"a" * 960, b"bb" + bytes(958)])

    async def test_playback_keeps_idle_device_fed_with_full_silent_frames(self):
        await self.audio.start()
        writer = self.processes[1].stdin
        await eventually(lambda: len(writer.writes) >= 4)
        self.assertTrue(all(frame == bytes(audio.CHUNK_BYTES) for frame in writer.writes))
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)
        self.assertEqual(self.audio.levels()[1], 0.0)

    async def test_packet_boundaries_do_not_insert_padding_into_pcm(self):
        await self.audio.start()
        writer = self.processes[1].stdin
        # Resamplers need not produce exactly 480 samples per packet. Inserting
        # silence after each short packet would put clicks into continuous PCM.
        pcm = bytes([41, 42]) * (480 * 4)
        for begin, end in ((0, 928), (928, 960), (960, 1538), (1538, len(pcm))):
            await self.audio.play(pcm[begin:end])
        await eventually(lambda: self.audio.status()["playback_queued_bytes"] == 0)
        voiced = [frame for frame in writer.writes if frame.strip(b"\0")]
        self.assertEqual(b"".join(voiced), pcm)
        self.assertTrue(all(len(frame) == audio.CHUNK_BYTES for frame in writer.writes))

    async def test_scheduler_oversleep_does_not_accumulate_playback_backlog(self):
        await self.audio.start()
        self.audio._playback_task.cancel()
        await asyncio.gather(self.audio._playback_task, return_exceptions=True)
        writer = self.processes[1].stdin
        writer.writes.clear()
        await self.audio.play(b"ab" * (480 * 50))
        now = 1000.0
        timestamps = []
        original_write = writer.write
        def write(frame):
            timestamps.append(now)
            original_write(frame)
            if len(timestamps) == 50:
                raise asyncio.CancelledError
        async def oversleep(delay):
            nonlocal now
            now += delay + 0.003  # Scheduler overhead, even for sleep(0).
        writer.write = write
        with mock.patch.object(audio, "time", mock.Mock(monotonic=lambda: now)), \
                mock.patch.object(audio.asyncio, "sleep", side_effect=oversleep):
            with self.assertRaises(asyncio.CancelledError):
                await self.audio._playback_loop()
        # 3ms of scheduling delay per frame must not turn 980ms of PCM into
        # ~1120ms of wall time (and eventually overflow a bounded live queue).
        self.assertLessEqual(timestamps[-1] - timestamps[0], 0.99)

    async def test_invalid_pcm_and_stopped_calls(self):
        with self.assertRaises(audio.AudioError):
            await self.audio.read()
        with self.assertRaises(audio.AudioError):
            await self.audio.play(b"aa")
        await self.audio.start()
        with self.assertRaises(ValueError):
            await self.audio.play(b"a")
        with self.assertRaises(TypeError):
            await self.audio.play(bytearray(b"aa"))
        with self.assertRaises(audio.AudioBackpressure):
            await self.audio.play(bytes(audio.PLAYBACK_MAX_BYTES + 2))

    async def test_interrupt_flushes_and_restarts_only_playback(self):
        await self.audio.start()
        old_play = self.processes[1]
        old_play.stdin.blocked = asyncio.Event()
        await self.audio.play(b"a" * 9600)
        await eventually(lambda: bool(old_play.stdin.writes))
        await self.audio.interrupt()
        self.assertTrue(old_play.terminated)
        self.assertTrue(old_play.waited)
        self.assertTrue(old_play.stdin.transport.aborted)
        self.assertFalse(self.processes[0].terminated)
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)
        await self.audio.play(b"b" * 960)
        await eventually(lambda: self.audio.status()["playback_queued_bytes"] == 0)
        voiced = [frame for frame in self.processes[2].stdin.writes if frame.strip(b"\0")]
        self.assertEqual(voiced, [b"b" * 960])

    async def test_capture_continues_during_intentional_playback_restart(self):
        await self.audio.start()
        original = self.spawn.side_effect
        entered = asyncio.Event()
        release = asyncio.Event()

        async def delayed(*args, **kwargs):
            entered.set()
            await release.wait()
            return await original(*args, **kwargs)

        self.spawn.side_effect = delayed
        interrupt = asyncio.create_task(self.audio.interrupt())
        try:
            await asyncio.wait_for(entered.wait(), 0.2)
            self.assertTrue(self.processes[1].terminated)
            self.processes[0].stdout.feed_data(b"a" * 960)
            self.assertEqual(await asyncio.wait_for(self.audio.read(), 0.2), b"a" * 960)
        finally:
            release.set()
            await interrupt

    async def test_capture_eof_wakes_readers_and_rejects_partial_chunk(self):
        losses = []
        self.audio._on_capture_loss = lambda: losses.append(True)
        await self.audio.start()
        read = asyncio.create_task(self.audio.read())
        self.processes[0].stdout.feed_data(b"a" * 11)
        self.processes[0].exit(1)
        with self.assertRaisesRegex(audio.AudioError, "capture failed"):
            await asyncio.wait_for(read, 0.2)
        self.assertEqual(losses, [True])

    async def test_playback_failure_is_reported_until_stop_and_restart(self):
        failures = []
        self.audio._on_playback_failure = lambda: failures.append(True)
        await self.audio.start()
        self.processes[1].stdin.error = BrokenPipeError()
        await self.audio.play(b"a" * 960)
        await eventually(lambda: self.audio._playback_error is not None)
        self.assertEqual(failures, [True])
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await self.audio.play(b"bb")
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await self.audio.interrupt()
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await self.audio.read()
        await self.audio.stop()
        await self.audio.start()
        await self.audio.play(b"cc")

    async def test_silent_playback_failure_wakes_read_even_if_interrupted_first(self):
        await self.audio.start()
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        try:
            # Hold the notification lock so interruption deterministically races
            # the output error before the waiting reader can observe it.
            async with self.audio._capture_ready:
                self.processes[1].stdin.error = BrokenPipeError()
                await eventually(lambda: self.audio._playback_error is not None)
                with self.assertRaisesRegex(audio.AudioError, "playback failed"):
                    await self.audio.interrupt()
                self.assertEqual(len(self.processes), 2)
            with self.assertRaisesRegex(audio.AudioError, "playback failed"):
                await asyncio.wait_for(reader, 0.2)
        finally:
            reader.cancel()
            await asyncio.gather(reader, return_exceptions=True)

    async def test_playback_failure_discards_queued_and_future_capture(self):
        await self.audio.start()
        self.processes[0].stdout.feed_data(b"a" * 960)
        await eventually(lambda: len(self.audio._capture_chunks) == 1)
        self.processes[1].stdin.error = BrokenPipeError()
        await eventually(lambda: self.audio._playback_error is not None)
        self.processes[0].stdout.feed_data(b"b" * 960)
        await asyncio.sleep(0.01)
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await self.audio.read()
        self.assertEqual(len(self.audio._capture_chunks), 0)
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await self.audio.play(b"cc")

    async def test_playback_exit_before_worker_writes(self):
        await self.audio.start()
        self.processes[1].exit(1)
        with self.assertRaisesRegex(audio.AudioError, "exited"):
            await self.audio.play(b"aa")

    async def test_playback_exit_during_silence_wakes_read(self):
        await self.audio.start()
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        self.processes[1].exit(1)
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await asyncio.wait_for(reader, 0.2)
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await self.audio.read()

    async def test_child_exit_fences_queued_capture_before_the_worker_observes_it(self):
        await self.audio.start()
        self.processes[0].stdout.feed_data(b"a" * 960)
        await eventually(lambda: len(self.audio._capture_chunks) == 1)
        self.processes[1].exit(1)
        with self.assertRaisesRegex(audio.AudioError, "playback exited"):
            await self.audio.read()
        await eventually(lambda: self.audio._playback_error is not None)
        self.assertFalse(self.audio._capture_chunks)

    async def test_interrupt_cannot_hide_a_child_exit_before_the_worker_observes_it(self):
        await self.audio.start()
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        self.processes[1].exit(1)
        with self.assertRaisesRegex(audio.AudioError, "playback exited"):
            await self.audio.interrupt()
        self.assertEqual(len(self.processes), 2)
        with self.assertRaisesRegex(audio.AudioError, "playback failed"):
            await asyncio.wait_for(reader, 0.2)

    async def test_drain_timeout_fails_without_unbounded_backlog(self):
        await self.audio.start()
        self.processes[1].stdin.blocked = asyncio.Event()
        with mock.patch.object(audio, "PROCESS_TIMEOUT", 0.01):
            await self.audio.play(b"a" * 960)
            await eventually(lambda: self.audio._playback_error is not None)
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)

    async def test_stop_wakes_readers_reaps_children_and_is_restartable(self):
        await self.audio.start()
        reader = asyncio.create_task(self.audio.read())
        await self.audio.stop()
        with self.assertRaises(audio.AudioError):
            await reader
        self.assertTrue(all(p.waited for p in self.processes))
        self.assertFalse(self.audio.status()["running"])
        await self.audio.stop()
        await self.audio.start()
        self.assertEqual(len(self.processes), 4)

    async def test_stop_escalates_to_kill_and_survives_cancellation(self):
        await self.audio.start()
        for process in self.processes:
            process.ignore_terminate = True
        with mock.patch.object(audio, "PROCESS_TIMEOUT", 0.02):
            stop = asyncio.create_task(self.audio.stop())
            await eventually(lambda: self.processes[0].terminated)
            stop.cancel()
            await asyncio.sleep(0.002)
            stop.cancel()
            with self.assertRaises(asyncio.CancelledError):
                await stop
        self.assertTrue(all(p.killed and p.waited for p in self.processes))

    async def test_partial_start_failure_reaps_capture(self):
        original = self.spawn.side_effect

        async def failing(*args, **kwargs):
            if self.processes:
                raise OSError("playback cannot start")
            return await original(*args, **kwargs)

        self.spawn.side_effect = failing
        with self.assertRaises(OSError):
            await self.audio.start()
        self.assertTrue(self.processes[0].waited)
        self.assertFalse(self.audio.status()["running"])

    async def test_cancelled_start_reaps_concurrently_created_child(self):
        original = self.spawn.side_effect
        entered = asyncio.Event()
        release = asyncio.Event()

        async def delayed(*args, **kwargs):
            if self.processes:
                entered.set()
                await release.wait()
            return await original(*args, **kwargs)

        self.spawn.side_effect = delayed
        start = asyncio.create_task(self.audio.start())
        await entered.wait()
        start.cancel()
        release.set()
        with self.assertRaises(asyncio.CancelledError):
            await start
        self.assertTrue(all(p.waited for p in self.processes))
        self.assertFalse(self.audio.status()["running"])

    async def test_late_child_cleanup_failure_retains_ownership_and_fences_restart(self):
        # The process can arrive after either cancellation or the first spawn
        # timeout. If closing it fails, start() must retain its handle even
        # though _spawn() never returned normally.
        real_wait_for = asyncio.wait_for
        real_close = audio._close_process
        original_spawn = self.spawn.side_effect
        for playback in (False, True):
            for cancelled in (False, True):
                with self.subTest(playback=playback, cancelled=cancelled):
                    entered, release, interrupted = asyncio.Event(), asyncio.Event(), asyncio.Event()
                    child = FakeProcess(capture=not playback)
                    initial_capture = FakeProcess(capture=True)

                    async def spawn(*args, **kwargs):
                        if playback and kwargs["stdout"] == asyncio.subprocess.PIPE:
                            return initial_capture
                        entered.set()
                        await release.wait()
                        return child

                    async def wait_for(awaitable, timeout):
                        try:
                            return await real_wait_for(awaitable, timeout)
                        except (asyncio.TimeoutError, asyncio.CancelledError):
                            if timeout == 0.05:
                                interrupted.set()
                            raise

                    async def close(process):
                        if process is child:
                            raise audio.AudioError("late child cleanup failed")
                        await real_close(process)

                    self.spawn.side_effect = spawn
                    try:
                        with mock.patch.object(audio, "START_TIMEOUT", 0.05), \
                                mock.patch.object(audio.asyncio, "wait_for", side_effect=wait_for), \
                                mock.patch.object(audio, "_close_process", side_effect=close):
                            start = asyncio.create_task(self.audio.start())
                            await real_wait_for(entered.wait(), 1)
                            if cancelled:
                                start.cancel()
                            await real_wait_for(interrupted.wait(), 1)
                            release.set()
                            with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
                                await real_wait_for(start, 1)
                            retained = self.audio._playback if playback else self.audio._capture
                            self.assertIs(retained, child)
                            with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
                                await self.audio.stop()
                            with self.assertRaisesRegex(audio.AudioError, "cleanup"):
                                await self.audio.start()
                    finally:
                        release.set()
                        await self.audio.stop()
                    self.assertTrue(child.waited)
                    self.spawn.side_effect = original_spawn
                    await self.audio.start()
                    await self.audio.stop()

    async def test_second_cancel_at_spawn_completion_cannot_lose_the_child(self):
        real_close = audio._close_process
        for playback in (False, True):
            for cleanup_failure in (False, True):
                with self.subTest(playback=playback, cleanup_failure=cleanup_failure):
                    entered, release = asyncio.Event(), asyncio.Event()
                    child = FakeProcess(capture=not playback)
                    initial_capture = FakeProcess(capture=True)

                    async def spawn(*args, **kwargs):
                        if playback and kwargs["stdout"] == asyncio.subprocess.PIPE:
                            return initial_capture
                        entered.set()
                        await release.wait()
                        start.cancel()  # Cancellation races the completed subprocess result.
                        return child

                    async def close(process):
                        if process is child and cleanup_failure:
                            raise audio.AudioError("late child cleanup failed")
                        await real_close(process)

                    self.spawn.side_effect = spawn
                    try:
                        with mock.patch.object(audio, "_close_process", side_effect=close):
                            start = asyncio.create_task(self.audio.start())
                            await asyncio.wait_for(entered.wait(), 1)
                            start.cancel()
                            await asyncio.sleep(0)
                            await asyncio.sleep(0)
                            release.set()
                            if cleanup_failure:
                                with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
                                    await asyncio.wait_for(start, 1)
                                retained = self.audio._playback if playback else self.audio._capture
                                self.assertIs(retained, child)
                                with self.assertRaisesRegex(audio.AudioError, "cleanup"):
                                    await self.audio.start()
                            else:
                                with self.assertRaises(asyncio.CancelledError):
                                    await asyncio.wait_for(start, 1)
                                self.assertTrue(child.terminated and child.waited)
                    finally:
                        release.set()
                        await self.audio.stop()
                    self.assertTrue(child.waited)

    async def test_late_playback_restart_cleanup_failure_retains_child(self):
        await self.audio.start()
        capture = self.processes[0]
        child = FakeProcess(capture=False)
        entered, release, timed_out = asyncio.Event(), asyncio.Event(), asyncio.Event()
        real_wait_for, real_close = asyncio.wait_for, audio._close_process

        async def spawn(*args, **kwargs):
            entered.set()
            await release.wait()
            return child

        async def wait_for(awaitable, timeout):
            try:
                return await real_wait_for(awaitable, timeout)
            except asyncio.TimeoutError:
                if timeout == 0.05:
                    timed_out.set()
                raise

        async def close(process):
            if process is child:
                raise audio.AudioError("late playback cleanup failed")
            await real_close(process)

        self.spawn.side_effect = spawn
        try:
            with mock.patch.object(audio, "START_TIMEOUT", 0.05), \
                    mock.patch.object(audio.asyncio, "wait_for", side_effect=wait_for), \
                    mock.patch.object(audio, "_close_process", side_effect=close):
                interrupt = asyncio.create_task(self.audio.interrupt())
                await real_wait_for(entered.wait(), 1)
                await real_wait_for(timed_out.wait(), 1)
                release.set()
                with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
                    await real_wait_for(interrupt, 1)
                self.assertIs(self.audio._playback, child)
                with self.assertRaisesRegex(audio.AudioError, "playback restart failed"):
                    await self.audio.read()
                with self.assertRaisesRegex(audio.AudioError, "cleanup failed"):
                    await self.audio.stop()
                self.assertTrue(capture.waited)
                with self.assertRaisesRegex(audio.AudioError, "cleanup"):
                    await self.audio.start()
        finally:
            release.set()
            await self.audio.stop()
        self.assertTrue(child.waited)

    async def test_play_during_interrupt_fails_instead_of_admitting_stale_audio(self):
        await self.audio.start()
        self.processes[1].ignore_terminate = True
        with mock.patch.object(audio, "PROCESS_TIMEOUT", 0.02):
            interrupt = asyncio.create_task(self.audio.interrupt())
            await eventually(lambda: self.audio._interrupting)
            with self.assertRaises(audio.AudioError):
                await self.audio.play(b"aa")
            await interrupt
        self.assertEqual(self.audio.status()["playback_queued_bytes"], 0)

    async def test_failed_interrupt_is_recoverable_by_stop(self):
        await self.audio.start()
        reader = asyncio.create_task(self.audio.read())
        await asyncio.sleep(0)
        self.spawn.side_effect = OSError("cannot restart")
        with self.assertRaises(OSError):
            await self.audio.interrupt()
        with self.assertRaisesRegex(audio.AudioError, "playback restart failed"):
            await asyncio.wait_for(reader, 0.2)
        with self.assertRaises(audio.AudioError):
            await self.audio.play(b"aa")
        await self.audio.stop()
        self.assertTrue(self.processes[0].waited)

    async def test_unsupported_start_and_missing_tools_never_spawn(self):
        with mock.patch.object(audio.sys, "platform", "win32"):
            with self.assertRaises(audio.UnsupportedAudioError):
                await self.audio.start()
        with mock.patch.object(audio.shutil, "which", return_value=None):
            with self.assertRaises(audio.AudioError):
                await self.audio.start()
        self.assertEqual(self.calls, [])


@unittest.skipUnless(sys.platform == "linux", "Linux subprocess cleanup test")
class RealProcessCleanupTests(unittest.IsolatedAsyncioTestCase):
    async def test_sigterm_ignoring_child_is_killed_and_reaped(self):
        process = await asyncio.create_subprocess_exec(
            sys.executable, "-c",
            "import os, signal, time; signal.signal(signal.SIGTERM, signal.SIG_IGN); "
            "os.write(1, b'x'); time.sleep(30)",
            stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL)
        try:
            await asyncio.wait_for(process.stdout.readexactly(1), 2)
            with mock.patch.object(audio, "PROCESS_TIMEOUT", 0.02):
                await asyncio.wait_for(audio._close_process(process), 2)
            self.assertEqual(process.returncode, -9)
        finally:
            if process.returncode is None:
                process.kill()
                await process.communicate()

    async def test_full_pipe_child_is_reaped_without_recording(self):
        # Python emits synthetic bytes only: no audio command or device access.
        process = await asyncio.create_subprocess_exec(
            sys.executable, "-c", "import os\nwhile True: os.write(1, b'x' * 4096)",
            stdin=asyncio.subprocess.DEVNULL, stdout=asyncio.subprocess.PIPE,
            stderr=asyncio.subprocess.DEVNULL, limit=960)
        try:
            await asyncio.sleep(0.03)
            await asyncio.wait_for(audio._close_process(process), 2)
            self.assertIsNotNone(process.returncode)
        finally:
            if process.returncode is None:
                process.kill()
                await process.communicate()


if __name__ == "__main__":
    unittest.main()
