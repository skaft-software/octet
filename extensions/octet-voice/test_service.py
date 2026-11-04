"""Provider-free duplex conversation and lifecycle fixtures; no real devices."""
import asyncio
import sys
import threading
import time
import unittest
from unittest.mock import patch

from service import VoiceService, plain, SPEECH_BYTES
from settings import DEFAULTS

OWNER = {"session_id": "owner", "extension_instance_id": "instance", "process_generation": 1}


def eventually(predicate):
    deadline = time.monotonic() + 2
    while time.monotonic() < deadline:
        if predicate():
            return
        time.sleep(0.002)
    raise AssertionError("voice fixture did not settle")


class FakeAudio:
    def __init__(self, **kwargs):
        self.capture = asyncio.Queue()
        self.on_capture_loss = kwargs.get("on_capture_loss")
        self.on_playback_failure = kwargs.get("on_playback_failure")
        self.played = []
        self.started = False
        self.stopped = False
        self.muted = False
        self.interrupts = 0

    def levels(self):
        return (0.0, 0.0)

    async def start(self):
        self.started = True

    async def read(self):
        return await self.capture.get()

    async def play(self, data):
        self.played.append(data)

    async def interrupt(self):
        self.interrupts += 1

    def set_muted(self, value):
        self.muted = value

    async def stop(self):
        self.stopped = True


class FakeClient:
    def __init__(self, config, credential):
        self.events = asyncio.Queue()
        self.sent = []
        self.results = []
        self.texts = []
        self.closed = False

    async def connect(self):
        pass

    async def receive(self):
        return await self.events.get()

    async def set_muted(self, value):
        self.muted = value

    async def send_audio(self, data):
        self.sent.append(data)

    async def send_result(self, identifier, text, final=True):
        self.results.append((identifier, text, final))

    async def send_text(self, text):
        self.texts.append(text)

    async def close(self):
        self.closed = True


class ServiceTests(unittest.TestCase):
    def setUp(self):
        self.clients = []
        self.audios = []
        self.injections = []
        self.notices = []
        self.inject_error = False

        def client(*args):
            value = FakeClient(*args)
            self.clients.append(value)
            return value

        def audio(**kwargs):
            value = FakeAudio(**kwargs)
            self.audios.append(value)
            return value

        def inject(*args, **kwargs):
            self.injections.append(args)
            if self.inject_error:
                raise TimeoutError("ambiguous receipt")

        self.service = VoiceService(inject, lambda text, **kw: self.notices.append((text, kw)),
                                    client_factory=client, audio_factory=audio)
        self.addCleanup(self.service.close)

    def start(self):
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
        self.assertTrue(self.service.call(self.service.activate(ticket, DEFAULTS, {"api_key": "fixture"})))
        eventually(lambda: self.service.snapshot()["state"] == "listening")
        return self.clients[-1], self.audios[-1]

    def event(self, value):
        self.service._loop.call_soon_threadsafe(self.clients[-1].events.put_nowait, value)

    def test_explicit_start_only_and_cancelled_credential_callback(self):
        self.assertEqual(self.audios, [])
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
        self.assertEqual(self.audios, [])
        self.service.call(self.service.stop())
        self.assertFalse(self.service.call(self.service.activate(ticket, DEFAULTS, {})))
        self.assertEqual(self.clients, [])
        self.assertEqual(self.audios, [])

    def test_audio_cleanup_failure_fences_restart_and_does_not_claim_closed(self):
        from audio import AudioError

        class PendingAudio(FakeAudio):
            async def start(self):
                raise AudioError("native output open timed out")

            async def stop(self):
                raise AudioError("native open/cleanup still pending")

        self.service.audio_factory = PendingAudio
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
        self.service.call(self.service.activate(ticket, DEFAULTS, {}))
        eventually(lambda: self.service._cleanup_failed and self.service._task.done())
        self.assertIn("cleanup", self.service.call(self.service.stop()).lower())
        self.assertEqual(self.service.snapshot()["state"], "failed")
        self.assertIsNone(self.service._task)
        self.assertIsNone(self.service._audio)
        self.assertIsNone(self.service._client)
        with self.assertRaises(ValueError):
            self.service.call(self.service.reserve(OWNER, "host-session", 4))
        eventually(lambda: any("cleanup failed" in text for text, _ in self.notices))

    @unittest.skipUnless(sys.platform == "linux", "PipeWire subprocess backend is Linux-only")
    def test_late_pipewire_capture_cleanup_failure_fences_session_restart(self):
        import audio
        from test_audio import FakeProcess

        class Unreapable(FakeProcess):
            def terminate(self):
                self.terminated = True

            def kill(self):
                self.killed = True

        real_wait_for = asyncio.wait_for
        for cancelled in (False, True):
            with self.subTest(cancelled=cancelled):
                entered, release, interrupted = threading.Event(), threading.Event(), threading.Event()
                children, instances = [], []

                async def spawn(*args, **kwargs):
                    child = Unreapable(capture=True)
                    children.append(child)
                    entered.set()
                    while not release.is_set():
                        await asyncio.sleep(0.001)
                    return child

                async def wait_for(awaitable, timeout):
                    try:
                        return await real_wait_for(awaitable, timeout)
                    except (asyncio.TimeoutError, asyncio.CancelledError):
                        if timeout == 0.05:
                            interrupted.set()
                        raise

                def factory(**kwargs):
                    instance = audio.PipeWireAudio(**kwargs)
                    instances.append(instance)
                    return instance

                service = VoiceService(lambda *args, **kwargs: None, lambda *args, **kwargs: None,
                                       client_factory=FakeClient, audio_factory=factory)
                try:
                    prerequisites = {"supported": True, "available": True,
                                     "pw_record": "fake-record", "pw_play": "fake-play"}
                    with patch.object(audio, "probe", return_value=prerequisites), \
                            patch.object(audio, "_child_environment", return_value={}), \
                            patch.object(audio.asyncio, "create_subprocess_exec", side_effect=spawn), \
                            patch.object(audio.asyncio, "wait_for", side_effect=wait_for), \
                            patch.object(audio, "START_TIMEOUT", 0.05), \
                            patch.object(audio, "PROCESS_TIMEOUT", 0.01):
                        ticket = service.call(service.reserve(OWNER, "host-session", 3))
                        service.call(service.activate(ticket, DEFAULTS, {}))
                        self.assertTrue(entered.wait(1))
                        child = children[0]
                        if cancelled:
                            stopping = asyncio.run_coroutine_threadsafe(service.stop(), service._loop)
                        self.assertTrue(interrupted.wait(1))
                        release.set()
                        if cancelled:
                            result = stopping.result(2)
                        else:
                            eventually(lambda: service._task.done())
                            result = service.call(service.stop())
                        self.assertTrue(child.terminated and child.killed)
                        self.assertIsNone(child.returncode)
                        self.assertIn("cleanup", result.lower())
                        self.assertNotIn("Microphone closed", result)
                        self.assertIs(instances[0]._capture, child)
                        self.assertTrue(service._cleanup_failed)
                        self.assertEqual(service.snapshot()["state"], "failed")
                        with self.assertRaises(ValueError):
                            service.call(service.reserve(OWNER, "host-session", 4))
                finally:
                    release.set()
                    if children:
                        service._loop.call_soon_threadsafe(children[0].exit)
                    service.close()

    def test_cleanup_failure_on_session_expiry_is_not_reported_stopped(self):
        from audio import AudioError

        class PendingAudio(FakeAudio):
            async def stop(self):
                raise AudioError("native cleanup still pending")

        self.service.audio_factory = PendingAudio
        with patch("service.MAX_SESSION_SECONDS", 0.02):
            ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
            self.service.call(self.service.activate(ticket, DEFAULTS, {}))
            eventually(lambda: self.service._task.done())
        self.assertEqual(self.service.snapshot()["state"], "failed")
        self.assertIn("cleanup failed", self.service.snapshot()["error"])
        with self.assertRaises(ValueError):
            self.service.call(self.service.reserve(OWNER, "host-session", 4))
        self.assertIn("cleanup", self.service.call(self.service.stop()).lower())

    def test_explicit_stop_reports_terminal_unconfirmed_cleanup_as_failed(self):
        from audio import AudioError

        class FailedClose(FakeAudio):
            async def stop(self):
                raise AudioError("native close failed")

        self.service.audio_factory = FailedClose
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
        self.service.call(self.service.activate(ticket, DEFAULTS, {"api_key": "fixture"}))
        eventually(lambda: self.service.snapshot()["state"] == "listening")
        self.assertIn("could not be confirmed", self.service.call(self.service.stop()))
        self.assertEqual(self.service.snapshot()["state"], "failed")
        self.assertIn("cleanup failed", self.service.snapshot()["error"])
        self.assertIsNone(self.service._task)
        self.assertIsNone(self.service._audio)
        self.assertIsNone(self.service._client)
        with self.assertRaises(ValueError):
            self.service.call(self.service.reserve(OWNER, "host-session", 4))

    def test_explicit_stop_joins_spontaneous_cleanup_instead_of_cancelling_it(self):
        for trigger in ("disconnect", "failure", "expiry"):
            with self.subTest(trigger=trigger):
                release = asyncio.Event()
                entered = threading.Event()

                class GatedAudio(FakeAudio):
                    async def stop(self):
                        entered.set()
                        await release.wait()
                        await super().stop()

                def audio(**kwargs):
                    value = GatedAudio(**kwargs)
                    self.audios.append(value)
                    return value

                self.service.audio_factory = audio
                with patch("service.MAX_SESSION_SECONDS", 0.03 if trigger == "expiry" else 3600):
                    client, device = self.start()
                    if trigger == "disconnect":
                        self.event({"type": "closed"})
                    elif trigger == "failure":
                        self.event({"type": "error", "code": "fixture"})
                    self.assertTrue(entered.wait(2))
                result, done = [], threading.Event()

                def stop():
                    try:
                        result.append(self.service.call(self.service.stop(), timeout=6))
                    finally:
                        done.set()

                worker = threading.Thread(target=stop, daemon=True)
                worker.start()
                try:
                    eventually(lambda: self.service.snapshot()["state"] == "stopping")
                    self.assertFalse(done.is_set())
                    with self.assertRaises(ValueError):
                        self.service.call(self.service.reserve(OWNER, "host-session", 4))
                finally:
                    self.service._loop.call_soon_threadsafe(release.set)
                self.assertTrue(done.wait(3))
                self.assertIn("Microphone closed", result[0])
                self.assertTrue(device.stopped)
                self.assertTrue(client.closed)
                self.assertEqual(self.service.snapshot()["state"], "stopped")

    def test_concurrent_stops_cannot_clear_a_new_session_task(self):
        release = asyncio.Event()
        entered = threading.Event()

        class GatedAudio(FakeAudio):
            async def stop(self):
                entered.set()
                await release.wait()
                await super().stop()

        def audio(**kwargs):
            value = GatedAudio(**kwargs)
            self.audios.append(value)
            return value

        self.service.audio_factory = audio
        _, first_audio = self.start()

        async def race():
            async def stop_then_restart():
                await self.service.stop()
                try:
                    ticket = await self.service.reserve(OWNER, "host-session", 4)
                except ValueError:
                    return False
                await self.service.activate(ticket, DEFAULTS, {"api_key": "fixture"})
                return True

            first = asyncio.create_task(stop_then_restart())
            while not entered.is_set():
                await asyncio.sleep(0)
            second = asyncio.create_task(self.service.stop())
            await asyncio.sleep(0)  # Both stops now await the same cleanup.
            release.set()
            restarted, _ = await asyncio.gather(first, second)
            return restarted

        self.assertFalse(self.service.call(race()))
        self.assertTrue(first_audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "stopped")
        # A start after both stops complete must retain a cancellable task.
        _, new_audio = self.start()
        self.assertIn("Microphone closed", self.service.call(self.service.stop()))
        self.assertTrue(new_audio.stopped)

    def test_default_audio_factory_selects_native_platform_only_on_activation(self):
        import audio
        import portable_audio

        self.service.audio_factory = None
        config = dict(DEFAULTS, input_target="Built-in Microphone", output_target="Headphones")
        for platform, backend, class_name in (("linux", audio, "PipeWireAudio"),
                ("darwin", portable_audio, "PortableAudio"),
                ("win32", portable_audio, "PortableAudio")):
            with self.subTest(platform=platform), patch("sys.platform", platform), \
                    patch.object(backend, class_name, side_effect=FakeAudio) as factory, \
                    patch.object(portable_audio, "_sounddevice", side_effect=AssertionError("native import")):
                ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
                factory.assert_not_called()
                self.service.call(self.service.activate(ticket, config, {}))
                eventually(lambda: self.service.snapshot()["state"] == "listening")
                from unittest.mock import ANY
                factory.assert_called_once_with(input_target="Built-in Microphone", output_target="Headphones",
                                                on_capture_loss=ANY, on_playback_failure=ANY)
                selected = self.service._audio
                self.assertTrue(selected.started)
                self.service.call(self.service.stop())
                self.assertTrue(selected.stopped)

    def test_capture_loss_closes_audio_even_when_sender_is_blocked(self):
        entered = threading.Event()

        class BlockedClient(FakeClient):
            async def send_audio(self, data):
                entered.set()
                await asyncio.Event().wait()

        def factory(*args):
            client = BlockedClient(*args)
            self.clients.append(client)
            return client

        self.service.client_factory = factory
        client, device = self.start()
        self.service._loop.call_soon_threadsafe(device.capture.put_nowait, b"\0\0" * 480)
        self.assertTrue(entered.wait(2))
        # Model a native callback thread, independently of the blocked read/send.
        device.on_capture_loss()
        self.event({"type": "handoff", "handoff_id": "lost", "text": "do not run"})
        eventually(lambda: self.service._task.done())
        self.assertTrue(device.stopped)
        self.assertTrue(client.closed)
        self.assertFalse(self.injections)
        self.assertEqual(self.service.snapshot()["state"], "failed")
        self.assertIn("microphone audio was lost", self.service.snapshot()["error"])
        self.assertIn("may already be queued", self.service.snapshot()["error"])
        self.assertIn("No automatic retry", self.service.snapshot()["error"])

    @unittest.skipUnless(sys.platform == "linux", "PipeWire subprocess backend is Linux-only")
    def test_pipewire_playback_failure_closes_audio_with_blocked_sender(self):
        import audio
        from test_audio import FakeProcess

        for mode in ("exit", "write"):
            with self.subTest(mode=mode):
                processes, entered = [], threading.Event()

                class BlockedClient(FakeClient):
                    async def send_audio(self, data):
                        entered.set()
                        await asyncio.Event().wait()

                def client_factory(*args):
                    client = BlockedClient(*args)
                    self.clients.append(client)
                    return client

                def audio_factory(**kwargs):
                    device = audio.PipeWireAudio(**kwargs)
                    self.audios.append(device)
                    return device

                async def spawn(*args, **kwargs):
                    child = FakeProcess(kwargs["stdout"] == asyncio.subprocess.PIPE)
                    processes.append(child)
                    return child

                self.service.client_factory = client_factory
                self.service.audio_factory = audio_factory
                prerequisites = {"supported": True, "available": True,
                                 "pw_record": "pw-record", "pw_play": "pw-play"}
                with patch.object(audio, "probe", return_value=prerequisites), \
                        patch.object(audio, "_child_environment", return_value={}), \
                        patch.object(asyncio, "create_subprocess_exec", side_effect=spawn):
                    client, device = self.start()
                    capture, playback = processes
                    self.service._loop.call_soon_threadsafe(capture.stdout.feed_data, b"\0" * audio.CHUNK_BYTES)
                    self.assertTrue(entered.wait(2))
                    if mode == "exit":
                        self.service._loop.call_soon_threadsafe(playback.exit, 1)
                    else:
                        self.service._loop.call_soon_threadsafe(setattr, playback.stdin, "error", BrokenPipeError())
                    eventually(self.service._playback_failure.is_set)
                    self.event({"type": "handoff", "handoff_id": "late", "text": "do not run"})
                    eventually(lambda: self.service._task.done())
                    self.assertEqual(self.service.snapshot()["state"], "failed")
                    self.assertIn("audio playback failed", self.service.snapshot()["error"])
                    self.assertFalse(self.injections)
                    self.assertTrue(capture.waited and playback.waited and client.closed)
                    self.assertFalse(device._running)

    def test_portaudio_playback_failure_closes_audio_with_blocked_sender(self):
        import portable_audio
        from test_portable_audio import FakeSoundDevice

        entered = threading.Event()

        class BlockedClient(FakeClient):
            async def send_audio(self, data):
                entered.set()
                await asyncio.Event().wait()

        def client_factory(*args):
            client = BlockedClient(*args)
            self.clients.append(client)
            return client

        def audio_factory(**kwargs):
            device = portable_audio.PortableAudio(**kwargs)
            self.audios.append(device)
            return device

        self.service.client_factory = client_factory
        self.service.audio_factory = audio_factory
        native = FakeSoundDevice()
        with patch.object(portable_audio, "_sounddevice", return_value=native):
            client, device = self.start()
            capture, playback = native.streams
            capture.capture_pcm(b"\0\0" * 480)
            self.assertTrue(entered.wait(2))
            playback.kwargs["finished_callback"]()
            self.event({"type": "handoff", "handoff_id": "late", "text": "do not run"})
            eventually(lambda: self.service._task.done())
            self.assertEqual(self.service.snapshot()["state"], "failed")
            self.assertIn("audio playback failed", self.service.snapshot()["error"])
            self.assertIn("may already be queued", self.service.snapshot()["error"])
            self.assertFalse(self.injections)
            self.assertTrue(capture.closed and playback.closed and client.closed)
            self.assertFalse(device.status()["running"])

    def test_capture_loss_revokes_inflight_and_queued_handoffs(self):
        entered, revoked = threading.Event(), threading.Event()
        calls = []

        def inject(text, owner, parent, *, cancellation_event):
            calls.append(text)
            entered.set()
            if cancellation_event.wait(2):
                revoked.set()

        self.service.inject = inject
        _, device = self.start()
        self.event({"type": "handoff", "handoff_id": "first", "text": "first"})
        self.assertTrue(entered.wait(2))
        self.event({"type": "handoff", "handoff_id": "second", "text": "second"})
        eventually(lambda: self.service._handoffs.qsize() == 1)
        device.on_capture_loss()
        self.assertTrue(revoked.wait(2))
        eventually(lambda: self.service._task.done())
        self.assertEqual(calls, ["first"])
        self.assertTrue(device.stopped)
        self.assertIn("may already be queued", self.service.snapshot()["error"])

    def test_capture_loss_racing_explicit_stop_retains_diagnostic(self):
        _, device = self.start()

        async def loss_then_stop():
            device.on_capture_loss()
            return await self.service.stop()

        result = self.service.call(loss_then_stop(), timeout=6)
        self.assertTrue(device.stopped)
        self.assertIn("microphone audio was lost", result)
        self.assertIn("may already be queued", result)
        self.assertEqual(self.service.snapshot()["state"], "failed")

    def test_late_old_capture_loss_cannot_revoke_replacement_session(self):
        _, old = self.start()
        self.service.call(self.service.stop())
        _, new = self.start()
        old.on_capture_loss()
        old.on_playback_failure()
        self.service.call(asyncio.sleep(0.02))
        self.assertFalse(self.service._injection_cancel.is_set())
        self.assertFalse(self.service._capture_loss.is_set())
        self.assertEqual(self.service.snapshot()["state"], "listening")
        self.assertFalse(new.stopped)

    def test_stubborn_sender_cannot_strand_microphone_and_fences_restart(self):
        entered, finished = threading.Event(), threading.Event()
        release = asyncio.Event()

        class StubbornClient(FakeClient):
            async def send_audio(self, data):
                entered.set()
                while not release.is_set():
                    try:
                        await release.wait()
                    except asyncio.CancelledError:
                        pass
                finished.set()

        def factory(*args):
            client = StubbornClient(*args)
            self.clients.append(client)
            return client

        self.service.client_factory = factory
        try:
            with patch("service.TASK_DRAIN_TIMEOUT", 0.05):
                client, device = self.start()
                self.service._loop.call_soon_threadsafe(device.capture.put_nowait, b"\0\0" * 480)
                self.assertTrue(entered.wait(2))
                device.on_capture_loss()
                eventually(lambda: device.stopped)
                eventually(lambda: self.service._task.done())
                self.assertTrue(client.closed)
                self.assertTrue(self.service._cleanup_failed)
                self.assertIn("microphone audio was lost", self.service.snapshot()["error"])
                with self.assertRaises(ValueError):
                    self.service.call(self.service.reserve(OWNER, "host-session", 4))
        finally:
            self.service._loop.call_soon_threadsafe(release.set)
        self.assertTrue(finished.wait(2))

    @unittest.skipUnless(sys.platform == "linux", "PipeWire subprocess backend is Linux-only")
    def test_pipewire_stop_timeout_completes_after_interruption_unlocks(self):
        import audio
        from test_audio import FakeProcess

        processes, entered, release = [], threading.Event(), asyncio.Event()

        async def spawn(*args, **kwargs):
            if len(processes) == 2:
                entered.set()
                await release.wait()
            child = FakeProcess(kwargs["stdout"] == asyncio.subprocess.PIPE)
            processes.append(child)
            return child

        def audio_factory(**kwargs):
            device = audio.PipeWireAudio(**kwargs)
            self.audios.append(device)
            return device

        self.service.audio_factory = audio_factory
        original_close = self.service._bounded_close

        async def short_close(awaitable, timeout):
            return await original_close(awaitable, 0.05)

        self.service._bounded_close = short_close
        prerequisites = {"supported": True, "available": True,
                         "pw_record": "pw-record", "pw_play": "pw-play"}
        with patch.object(audio, "probe", return_value=prerequisites), \
                patch.object(audio, "_child_environment", return_value={}), \
                patch.object(asyncio, "create_subprocess_exec", side_effect=spawn), \
                patch("service.TASK_DRAIN_TIMEOUT", 0.05):
            try:
                _, device = self.start()
                self.event({"type": "interrupted"})
                self.assertTrue(entered.wait(2))
                result = self.service.call(self.service.stop(), timeout=3)
                self.assertIn("could not be confirmed", result)
                self.assertTrue(device._running)
                self.assertFalse(processes[0].terminated)
                self.assertTrue(self.service._pending_closes)
                with self.assertRaises(ValueError):
                    self.service.call(self.service.reserve(OWNER, "host-session", 4))
            finally:
                self.service._loop.call_soon_threadsafe(release.set)
            eventually(lambda: len(processes) == 3 and all(child.waited for child in processes))
            eventually(lambda: not self.service._pending_closes)
            self.assertFalse(device._running)
            self.assertEqual(self.service.snapshot()["state"], "failed")

    def test_timed_out_audio_stop_stays_owned_until_interrupt_unlocks(self):
        interrupt_entered, release, stopped = threading.Event(), asyncio.Event(), threading.Event()

        class SlowInterruptAudio(FakeAudio):
            def __init__(self, **kwargs):
                super().__init__(**kwargs)
                self.lifecycle = asyncio.Lock()

            async def interrupt(self):
                async with self.lifecycle:
                    interrupt_entered.set()
                    while not release.is_set():
                        try:
                            await release.wait()
                        except asyncio.CancelledError:
                            pass

            async def stop(self):
                async with self.lifecycle:
                    await super().stop()
                    stopped.set()

        def audio_factory(**kwargs):
            device = SlowInterruptAudio(**kwargs)
            self.audios.append(device)
            return device

        self.service.audio_factory = audio_factory
        original_close = self.service._bounded_close

        async def short_close(awaitable, timeout):
            return await original_close(awaitable, 0.05)

        self.service._bounded_close = short_close
        try:
            with patch("service.TASK_DRAIN_TIMEOUT", 0.05):
                _, device = self.start()
                self.event({"type": "interrupted"})
                self.assertTrue(interrupt_entered.wait(2))
                result = self.service.call(self.service.stop(), timeout=3)
                self.assertIn("could not be confirmed", result)
                self.assertFalse(device.stopped)
                self.assertTrue(self.service._pending_closes)
                self.assertTrue(self.service._cleanup_failed)
                with self.assertRaises(ValueError):
                    self.service.call(self.service.reserve(OWNER, "host-session", 4))
        finally:
            self.service._loop.call_soon_threadsafe(release.set)
        self.assertTrue(stopped.wait(2))
        eventually(lambda: not self.service._pending_closes)
        self.assertTrue(device.stopped)
        self.assertEqual(self.service.snapshot()["state"], "failed")

    def test_bidirectional_audio_and_live_mute(self):
        client, audio = self.start()
        self.service._loop.call_soon_threadsafe(audio.capture.put_nowait, b"\0\0" * 480)
        eventually(lambda: bool(client.sent))
        self.event({"type": "audio", "audio": b"\1\0" * 480})
        eventually(lambda: bool(audio.played))
        self.assertEqual(len(client.sent[0]), 960)
        self.assertEqual(len(audio.played[0]), 960)
        self.assertEqual(self.service.call(self.service.mute(OWNER)), "Microphone muted")
        self.assertTrue(audio.muted)
        self.assertEqual(self.service.call(self.service.mute(OWNER)), "Microphone live")
        self.assertFalse(audio.muted)
        self.event({"type": "interrupted"})
        eventually(lambda: audio.interrupts == 1)
        self.service.call(self.service.stop())
        self.assertTrue(audio.stopped)
        self.assertTrue(client.closed)

    def test_blocked_notification_cannot_hold_open_failed_audio(self):
        entered, release = threading.Event(), threading.Event()

        def blocked_notify(*args, **kwargs):
            entered.set()
            release.wait(3)

        self.service.notify = blocked_notify
        _, audio = self.start()
        try:
            self.event({"type": "error", "code": "provider_error"})
            self.assertTrue(entered.wait(2))
            eventually(lambda: audio.stopped)
            self.assertEqual(self.service.snapshot()["state"], "failed")
        finally:
            release.set()

    def test_blocked_warning_cannot_prevent_stop(self):
        entered, release = threading.Event(), threading.Event()

        def blocked_notify(*args, **kwargs):
            entered.set()
            release.wait(3)

        self.service.notify = blocked_notify
        _, audio = self.start()
        try:
            self.service.call(self.service.update("prompt", "x" * 16_385, OWNER))
            self.assertTrue(entered.wait(2))
            self.assertIn("Microphone closed", self.service.call(self.service.stop(), timeout=2))
            self.assertTrue(audio.stopped)
        finally:
            release.set()

    def test_notification_backlog_is_bounded_and_reports_loss_after_recovery(self):
        entered, release = threading.Event(), threading.Event()
        delivered = []

        def notify(message, **kwargs):
            if not entered.is_set():
                entered.set()
                release.wait(3)
            delivered.append((message, kwargs))

        self.service.notify = notify
        try:
            self.service._notify("first", level="warning")
            self.assertTrue(entered.wait(2))
            for index in range(20):
                self.service._notify(f"notice {index}", level="warning")
            self.assertEqual(self.service._notices.qsize(), 8)
            self.assertTrue(self.service._notice_lost.is_set())
            release.set()
            eventually(lambda: any("notices were lost" in message for message, _ in delivered))
        finally:
            release.set()

    def test_late_and_queued_mute_cannot_mutate_a_replacement_session(self):
        entered, release = threading.Event(), asyncio.Event()
        clients = self.clients

        class GatedClient(FakeClient):
            async def set_muted(self, value):
                if value and self is clients[0]:
                    entered.set()
                    await release.wait()
                await super().set_muted(value)

        def client(*args):
            value = GatedClient(*args)
            clients.append(value)
            return value

        self.service.client_factory = client
        old, old_audio = self.start()
        first = asyncio.run_coroutine_threadsafe(self.service.mute(OWNER), self.service._loop)
        self.assertTrue(entered.wait(2))
        second = asyncio.run_coroutine_threadsafe(self.service.mute(OWNER), self.service._loop)
        self.service.call(asyncio.sleep(0))  # Let the queued toggle reach the lock.
        try:
            self.assertTrue(old_audio.muted)
            self.assertIn("Microphone closed", self.service.call(self.service.stop(), timeout=6))
            new, new_audio = self.start()
        finally:
            self.service._loop.call_soon_threadsafe(release.set)
        self.assertIn("session changed", first.result(2).lower())
        self.assertIn("session changed", second.result(2).lower())
        self.assertTrue(old.closed)
        self.assertFalse(new_audio.muted)
        self.assertFalse(new.muted)
        self.assertEqual(self.service.snapshot()["state"], "listening")
        self.assertEqual(self.service._owner, OWNER)

    def test_unmute_keeps_local_capture_muted_until_client_completes(self):
        entered, release = threading.Event(), asyncio.Event()

        class GatedClient(FakeClient):
            gate_unmute = False

            async def set_muted(self, value):
                if not value and self.gate_unmute:
                    entered.set()
                    await release.wait()
                await super().set_muted(value)

        def client(*args):
            value = GatedClient(*args)
            self.clients.append(value)
            return value

        self.service.client_factory = client
        client, audio = self.start()
        self.assertEqual(self.service.call(self.service.mute(OWNER)), "Microphone muted")
        client.gate_unmute = True
        result = asyncio.run_coroutine_threadsafe(self.service.mute(OWNER), self.service._loop)
        self.assertTrue(entered.wait(2))
        try:
            self.assertTrue(audio.muted)
            self.assertTrue(self.service.snapshot()["muted"])
        finally:
            self.service._loop.call_soon_threadsafe(release.set)
        self.assertEqual(result.result(2), "Microphone live")
        self.assertFalse(audio.muted)
        self.assertFalse(self.service.snapshot()["muted"])

    def test_pending_unmute_does_not_reopen_capture_after_audio_failure(self):
        entered, release = threading.Event(), asyncio.Event()

        class GatedClient(FakeClient):
            gate_unmute = False

            async def set_muted(self, value):
                if not value and self.gate_unmute:
                    entered.set()
                    await release.wait()
                await super().set_muted(value)

        def client_factory(*args):
            client = GatedClient(*args)
            self.clients.append(client)
            return client

        self.service.client_factory = client_factory
        client, device = self.start()
        self.assertEqual(self.service.call(self.service.mute(OWNER)), "Microphone muted")
        client.gate_unmute = True
        pending = asyncio.run_coroutine_threadsafe(self.service.mute(OWNER), self.service._loop)
        self.assertTrue(entered.wait(2))

        def fail_and_release():
            device.on_capture_loss()
            release.set()

        self.service._loop.call_soon_threadsafe(fail_and_release)
        self.assertIn("not updated", pending.result(2))
        eventually(lambda: self.service._task.done())
        self.assertTrue(device.muted)
        self.assertTrue(device.stopped)
        self.assertEqual(self.service.snapshot()["state"], "failed")

    def test_mute_checks_owner_at_mutation_not_command_dispatch(self):
        client, audio = self.start()
        self.assertEqual(self.service.call(self.service.mute(OWNER)), "Microphone muted")
        for other in (dict(OWNER, session_id="other"), dict(OWNER, process_generation=2),
                      dict(OWNER, extension_instance_id="other"), None):
            with self.subTest(owner=other):
                self.assertIn("another session", self.service.call(self.service.mute(other)))
                self.assertTrue(audio.muted)
                self.assertTrue(client.muted)
        self.assertEqual(self.service.call(self.service.mute(OWNER)), "Microphone live")
        self.assertFalse(audio.muted)

    def test_handoff_admission_dedup_and_shared_response_not_false_completion(self):
        client, _ = self.start()
        handoff = {"type": "handoff", "handoff_id": "request-1", "text": "Inspect tests"}
        self.event(handoff)
        eventually(lambda: bool(client.results))
        self.event(handoff)
        self.service.call(self.service.update("answer", "Tests passed.", OWNER))
        eventually(lambda: len(client.results) == 2)
        self.assertEqual(len(self.injections), 1)
        self.assertEqual(self.injections[0], ("Inspect tests", OWNER, 3))
        self.assertEqual(client.results[0][0], "request-1")
        self.assertFalse(client.results[0][2])
        self.assertIn("does not establish execution status or success", client.results[0][1])
        self.assertEqual(client.results[1], (None, "Tests passed.", True))

    def test_delayed_admission_ack_does_not_retract_an_earlier_agent_update(self):
        entered, release = threading.Event(), threading.Event()

        def hold_admission(*args, **kwargs):
            entered.set()
            release.wait(2)

        self.service.inject = hold_admission
        client, _ = self.start()
        try:
            self.event({"type": "handoff", "handoff_id": "request-1", "text": "Inspect tests"})
            self.assertTrue(entered.wait(2))
            self.service.call(self.service.update("answer", "Tests passed.", OWNER))
            eventually(lambda: bool(client.results))
            self.assertEqual(client.results[0], (None, "Tests passed.", True))
            release.set()
            eventually(lambda: len(client.results) == 2)
            identifier, acknowledgement, final = client.results[1]
            self.assertEqual(identifier, "request-1")
            self.assertFalse(final)
            self.assertIn("does not establish execution status or success", acknowledgement)
            self.assertNotIn("not complete", acknowledgement)
            self.assertNotIn("Wait for", acknowledgement)
        finally:
            release.set()

    def test_ambiguous_handoff_stops_and_never_replays(self):
        client, audio = self.start()
        self.inject_error = True
        self.event({"type": "handoff", "handoff_id": "request-1", "text": "Run tests"})
        eventually(lambda: audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "failed")
        self.assertEqual(len(self.injections), 1)
        self.assertEqual(client.results, [])
        self.assertTrue(client.closed)
        self.assertIn("may already be queued", self.service.snapshot()["error"])
        self.assertIn("Check octet before resending", self.service.snapshot()["error"])
        self.assertNotIn("ambiguous receipt", str(self.notices))

    def test_typed_prompt_interrupts_and_owner_fences_updates(self):
        client, audio = self.start()
        self.service.call(self.service.update("answer", "foreign secret", dict(OWNER, session_id="other")))
        self.service.call(self.service.update("prompt", "Explain instead", OWNER))
        eventually(lambda: bool(client.texts))
        self.assertEqual(client.results, [])
        self.assertNotIn("foreign secret", str(client.texts))
        self.assertEqual(audio.interrupts, 1)
        from realtime import INSTRUCTIONS, SHARED_USER_PREFIX
        self.assertEqual(client.texts[0], SHARED_USER_PREFIX + "Explain instead")
        self.assertIn(SHARED_USER_PREFIX, INSTRUCTIONS)
        self.assertTrue(self.service.owns({"resource_owner": OWNER}))
        self.assertFalse(self.service.owns({"resource_owner": dict(OWNER, process_generation=2)}))

    def test_budget_keeps_full_answer_in_native_octet(self):
        client, _ = self.start()
        for number, text in enumerate(("é" * SPEECH_BYTES, "x" * 16_385, "x" * 100_000), 1):
            self.service.call(self.service.update("answer", text, OWNER))
            eventually(lambda: len(client.results) == number)
            self.assertIn("too long", client.results[-1][1])
            self.assertLess(len(client.results[-1][1]), SPEECH_BYTES)
        self.assertEqual(self.notices, [])

    def test_live_caption_snapshots_are_sanitized_without_transcript_notices(self):
        self.start()
        self.event({"type": "input_transcript", "text": "partial", "final": False})
        eventually(lambda: self.service.snapshot()["caption"] == "You: partial")
        self.assertEqual(self.service.snapshot()["user_transcript"], "partial")
        self.assertFalse(self.service.snapshot()["user_transcript_final"])
        self.event({"type": "input_transcript", "text": "hello\x1b[31m\x00", "final": True})
        eventually(lambda: self.service.snapshot()["caption"].startswith("You: hello"))
        self.assertNotIn("\x1b", self.service.snapshot()["caption"])
        self.assertNotIn("\x00", self.service.snapshot()["caption"])
        self.assertEqual(self.notices, [])
        self.event({"type": "output_transcript", "text": "A spoken answer", "final": True})
        eventually(lambda: self.service.snapshot()["caption"] == "Voice: A spoken answer")
        self.assertEqual(self.service.snapshot()["user_transcript"], "hello[31m")
        self.assertTrue(self.service.snapshot()["user_transcript_final"])
        self.assertEqual(self.service.snapshot()["voice_transcript"], "A spoken answer")
        self.assertTrue(self.service.snapshot()["voice_transcript_final"])
        self.assertEqual(self.notices, [])
        self.service.call(self.service.stop())
        self.start()
        self.assertEqual(self.service.snapshot()["caption"], "")

    def test_late_older_transcripts_do_not_erase_newer_caption_or_user_text(self):
        self.start()
        for kind, text, sequence, final in [
            ("input_transcript", "old user", 1, False),
            ("output_transcript", "old voice", 2, False),
            ("input_transcript", "new user", 3, False),
            ("output_transcript", "old voice corrected", 2, True),
            ("input_transcript", "old user corrected", 1, True),
        ]:
            self.event({"type": kind, "text": text, "final": final, "sequence": sequence})
        eventually(lambda: self.service.snapshot()["voice_transcript"] == "old voice corrected")
        self.assertEqual(self.service.snapshot()["caption"], "You: new user")
        self.assertEqual(self.service.snapshot()["user_transcript"], "new user")
        self.assertEqual(self.service.snapshot()["voice_transcript"], "old voice corrected")

    def test_full_user_transcript_survives_voice_reply_and_stop_clears_it(self):
        self.start()
        text = "word " * 3000
        self.event({"type": "input_transcript", "text": text, "final": True})
        eventually(lambda: self.service.snapshot()["user_transcript"] == text)
        self.event({"type": "output_transcript", "text": "Reply", "final": False})
        eventually(lambda: self.service.snapshot()["caption"] == "Voice: Reply")
        self.assertEqual(self.service.snapshot()["user_transcript"], text)
        self.service.call(self.service.stop())
        self.assertEqual(self.service.snapshot()["user_transcript"], "")
        self.assertEqual(self.service.snapshot()["voice_transcript"], "")

    def test_transcript_overflow_is_actionable_and_fails_closed(self):
        _, audio = self.start()
        self.event({"type": "input_transcript", "text": "é" * 8193, "final": False})
        eventually(lambda: audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "failed")
        self.assertIn("transcript exceeded", self.service.snapshot()["error"])
        self.assertEqual(self.service.snapshot()["user_transcript"], "")
        self.assertEqual(self.injections, [])

    def test_connecting_turns_are_shared_in_order_and_owner_fenced(self):
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
        self.service.call(self.service.update("prompt", "During login", OWNER))
        self.service.call(self.service.update("answer", "Native answer", OWNER))
        self.service.call(self.service.update("prompt", "Foreign", dict(OWNER, session_id="other")))
        self.service.call(self.service.activate(ticket, DEFAULTS, {}))
        eventually(lambda: bool(self.clients) and bool(self.clients[-1].results))
        self.assertEqual(len(self.clients[-1].texts), 1)
        self.assertIn("During login", self.clients[-1].texts[0])
        self.assertEqual(self.clients[-1].results, [(None, "Native answer", True)])

    def test_connecting_update_overflow_revokes_activation(self):
        ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
        for index in range(17):
            self.service.call(self.service.update("prompt", str(index), OWNER))
        self.assertEqual(self.service.snapshot()["state"], "stopped")
        self.assertFalse(self.service.call(self.service.activate(ticket, DEFAULTS, {})))
        self.assertEqual(self.clients, [])
        self.assertEqual(self.audios, [])
        eventually(lambda: bool(self.notices))
        self.assertIn("queue filled", self.notices[0][0])

    def test_snapshot_uses_observed_audio_levels_and_quiet_connect(self):
        _, audio = self.start()
        audio.levels = lambda: (0.25, 0.75)
        snapshot = self.service.snapshot()
        self.assertEqual(snapshot["microphone_level"], 0.25)
        self.assertEqual(snapshot["speaker_level"], 0.75)
        self.assertEqual(self.notices, [])
        self.service.call(self.service.stop())
        self.assertEqual(self.service.snapshot()["microphone_level"], 0.0)
        self.assertEqual(self.service.snapshot()["speaker_level"], 0.0)
        self.assertEqual(self.notices, [])

    def test_session_limit_closes_media_without_routine_notice(self):
        with patch("service.MAX_SESSION_SECONDS", 0.02):
            client, audio = self.start()
            eventually(lambda: audio.stopped)
            eventually(lambda: self.service.snapshot()["state"] == "stopped")
        self.assertTrue(client.closed)
        self.assertEqual(self.notices, [])

    def test_session_settlement_closes_only_own_session(self):
        _, audio = self.start()
        self.service.call(self.service.session_settled("foreign"))
        self.assertFalse(audio.stopped)
        self.service.call(self.service.session_settled("host-session"))
        self.assertTrue(audio.stopped)
        self.assertEqual(self.service.snapshot()["state"], "stopped")

    def test_slow_admission_does_not_block_audio_or_barge_in(self):
        client, audio = self.start()
        entered, release = threading.Event(), threading.Event()
        self.addCleanup(release.set)

        def slow(*args, **kwargs):
            entered.set()
            release.wait(2)

        self.service.inject = slow
        self.event({"type": "handoff", "handoff_id": "slow", "text": "Inspect"})
        self.assertTrue(entered.wait(1))
        self.event({"type": "audio", "audio": b"\0\0" * 480})
        self.event({"type": "interrupted"})
        self.event({"type": "input_transcript", "text": "Another thought", "final": False})
        eventually(lambda: self.service.snapshot()["user_transcript"] == "Another thought")
        self.assertEqual(audio.interrupts, 1)
        self.assertTrue(audio.played)
        self.assertEqual(client.results, [])
        self.service.call(self.service.stop())
        release.set()
        self.assertEqual(client.results, [])

    def test_invalid_handoff_fails_without_injection(self):
        _, audio = self.start()
        self.event({"type": "handoff", "handoff_id": "id", "text": "x" * 20000})
        eventually(lambda: audio.stopped)
        self.assertEqual(self.injections, [])

    def test_failure_does_not_open_audio_before_connect(self):
        async def fail(_):
            raise RuntimeError("fixture-secret")
        original = FakeClient.connect
        FakeClient.connect = fail
        try:
            ticket = self.service.call(self.service.reserve(OWNER, "host-session", 3))
            self.service.call(self.service.activate(ticket, DEFAULTS, {}))
            eventually(lambda: self.service.snapshot()["state"] == "failed")
            self.assertEqual(self.audios, [])
            self.assertNotIn("fixture-secret", str(self.notices))
        finally:
            FakeClient.connect = original

    def test_plain_budget_and_control_removal(self):
        self.assertEqual(plain("ok\x00\x1b\x85\n"), "ok\n")
        with self.assertRaises(ValueError):
            plain("é" * 100, 100)


if __name__ == "__main__":
    unittest.main()
