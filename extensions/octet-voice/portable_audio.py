"""Optional callback-driven PortAudio backend; importing/probing never opens devices.

Only start() opens native streams. PCM is mono, 24 kHz, little-endian int16.
Capture buffers at most 200 ms and fails on detected loss; playback admits at
most 1.5 seconds atomically.
Output remains open and emits silence when idle. New bursts prebuffer 60 ms;
short final tails start within 60 ms even below that threshold. interrupt() fences future
callbacks, not samples already submitted to hardware (approximately 60 ms).
"""

from __future__ import annotations

import asyncio
from collections import deque
import importlib
import math
import sys
import threading
import time

from audio import AudioBackpressure, AudioError, UnsupportedAudioError, pcm_level

SAMPLE_RATE = 24_000
SAMPLE_WIDTH = 2
CHANNELS = 1
CHUNK_BYTES = 960
CAPTURE_MAX_BYTES = 9_600
PLAYBACK_MAX_BYTES = 72_000
PREBUFFER_SECONDS = 0.06
PREBUFFER_BYTES = int(SAMPLE_RATE * SAMPLE_WIDTH * PREBUFFER_SECONDS)
START_TIMEOUT = 3.0
CLOSE_TIMEOUT = 0.75


def _sounddevice():
    try:
        return importlib.import_module("sounddevice")
    except (ImportError, OSError) as exc:
        raise AudioError("Install sounddevice and its PortAudio runtime for native audio") from exc


def probe(runtime_dir=None):
    """Check the optional library only; do not construct streams or open devices."""
    supported = sys.platform in ("darwin", "win32", "linux") and sys.byteorder == "little"
    result = {"supported": supported, "available": False, "backend": "portaudio", "error": None}
    if not supported:
        result["error"] = "PortAudio voice requires little-endian macOS, Windows or Linux"
        return result
    try:
        _sounddevice()
        result["available"] = True
    except AudioError as exc:
        result["error"] = str(exc)
    return result


def probe_devices():
    """Read-only PortAudio metadata; targets are device-name strings, not opens."""
    try:
        devices = _sounddevice().query_devices()
        return [{"index": index, "name": device["name"], "target": device["name"],
                 "input_channels": device["max_input_channels"],
                 "output_channels": device["max_output_channels"],
                 "default_sample_rate": device["default_samplerate"]}
                for index, device in enumerate(devices)]
    except AudioError:
        raise
    except Exception as exc:
        raise AudioError("PortAudio device listing failed") from exc


class _StreamCleanupError(AudioError):
    """A native stream could not be confirmed closed."""


def _close_streams(streams):
    """Run exclusively off-loop; abort discards playback rather than draining it."""
    error = None
    for stream in reversed(streams):
        try:
            stream.abort()
        except Exception as exc:
            error = exc
        try:
            stream.close()
        except Exception as exc:
            error = exc
    if error is not None:
        raise _StreamCleanupError("PortAudio stream cleanup failed") from error


class _Opening:
    """Atomic ownership handoff: a late native open must close itself on timeout.

    Python cannot kill a blocked native call. The worker keeps responsibility for
    partial streams until publishing under this lock; cancellation either takes
    published streams or marks the still-opening worker for self-cleanup.
    """

    def __init__(self):
        self.lock = threading.Lock()
        self.abandoned = False
        self.streams = ()

    def abandon(self):
        with self.lock:
            self.abandoned = True
            streams, self.streams = self.streams, ()
            return streams


class PortableAudio:
    """Same async lifecycle/read/play interface as PipeWireAudio.

    Native callbacks share only bounded buffers behind a short threading lock.
    A waiting reader gets at most one pending thread-safe loop notification, even
    if the loop stalls. Native stream creation/start/abort/close run off-loop.
    """

    def __init__(self, input_target=None, output_target=None, runtime_dir=None,
                 on_capture_loss=None, on_playback_failure=None):
        for target in (input_target, output_target):
            if target is not None and (not isinstance(target, str) or not target or "\0" in target):
                raise ValueError("Audio targets must be nonempty device-name strings")
        self.input_target = input_target
        self.output_target = output_target
        self.runtime_dir = runtime_dir  # Interface compatibility; PortAudio does not use it.
        self._on_capture_loss = on_capture_loss
        self._on_playback_failure = on_playback_failure
        self._lock = threading.Lock()
        self._lifecycle = asyncio.Lock()
        self._ready = asyncio.Event()
        self._loop = None
        self._epoch = 0
        self._waiting = False
        self._wake_pending = False
        self._opening = False
        self._running = False
        self._muted = False
        self._capture = bytearray()
        self._playback = deque()
        self._playback_offset = 0
        self._playback_bytes = 0
        self._burst_active = False
        self._burst_at = None
        self._capture_error = self._playback_error = None
        self._mic_level = self._speaker_level = (0.0, 0.0)
        self._streams = ()
        self._native_tasks = set()
        self._native_cleanup_error = None

    @property
    def muted(self):
        with self._lock:
            return self._muted

    @muted.setter
    def muted(self, value):
        self.set_muted(value)

    def set_muted(self, value):
        if not isinstance(value, bool):
            raise TypeError("muted must be a boolean")
        with self._lock:
            if self._muted != value:
                self._muted = value
                self._capture.clear()
                self._mic_level = (0.0, 0.0)

    def levels(self):
        now = time.monotonic()
        with self._lock:
            mic, mic_at = self._mic_level
            speaker, speaker_at = self._speaker_level
            return (mic if self._running and not self._muted and now - mic_at < 0.15 else 0.0,
                    speaker if self._running and now - speaker_at < 0.15 else 0.0)

    def status(self):
        result = probe(self.runtime_dir)
        with self._lock:
            result.update(running=self._running, muted=self._muted,
                          playback_queued_bytes=self._playback_bytes,
                          capture_queued_chunks=math.ceil(len(self._capture) / CHUNK_BYTES),
                          capture_error=self._capture_error, playback_error=self._playback_error)
        return result

    def _notify_locked(self):
        if self._waiting and not self._wake_pending:
            self._wake_pending = True
            self._waiting = False
            try:
                self._loop.call_soon_threadsafe(self._notify, self._epoch)
            except RuntimeError:  # Loop already closed during process teardown.
                self._wake_pending = False

    def _notify(self, epoch):
        with self._lock:
            if epoch == self._epoch:
                self._wake_pending = False
                self._ready.set()

    def _input_callback(self, epoch, data, frames, timing, status):
        with self._lock:
            if (epoch != self._epoch or not (self._opening or self._running)
                    or self._capture_error or self._playback_error
                    or (self._running and self._muted)):
                return
            try:
                view = memoryview(data).cast("B")
                if not self._running:
                    return  # Validate during opening, but do not retain early capture.
                # Check the original block, before copying, so even a single
                # oversized callback cannot silently discard its first samples.
                lost = status and (status.input_overflow or status.input_underflow)
                if lost or len(self._capture) + len(view) > CAPTURE_MAX_BYTES:
                    self._capture_error = "PortAudio capture discontinuity; audio was lost"
                    self._capture.clear()
                    self._mic_level = (0.0, 0.0)
                    # This callback runs on the native thread: the service hook
                    # only revokes its thread-safe token and wakes the event loop.
                    if self._on_capture_loss is not None:
                        self._on_capture_loss()
                else:
                    pcm = bytes(view)
                    self._capture.extend(pcm)
                    self._mic_level = (pcm_level(pcm), time.monotonic())
            except Exception:
                self._capture_error = "PortAudio capture callback failed"
                self._capture.clear()
                if self._running and self._on_capture_loss is not None:
                    self._on_capture_loss()
            self._notify_locked()

    def _output_callback(self, epoch, data, frames, timing, status):
        with self._lock:
            view = None
            try:
                view = memoryview(data).cast("B")
                view[:] = b"\0" * len(view)
                if epoch != self._epoch or not self._running or self._playback_error:
                    return
                now = time.monotonic()
                if not self._burst_active and self._playback_bytes:
                    self._burst_active = (self._playback_bytes >= PREBUFFER_BYTES
                                          or now - self._burst_at >= PREBUFFER_SECONDS)
                if not self._burst_active:
                    self._speaker_level = (0.0, now)
                    return
                position = 0
                while self._playback and position < len(view):
                    chunk = self._playback[0]
                    count = min(len(view) - position, len(chunk) - self._playback_offset)
                    view[position:position + count] = chunk[self._playback_offset:self._playback_offset + count]
                    position += count
                    self._playback_offset += count
                    self._playback_bytes -= count
                    if self._playback_offset == len(chunk):
                        self._playback.popleft()
                        self._playback_offset = 0
                if position < len(view):
                    self._burst_active = False
                if not self._playback_bytes:
                    self._burst_at = None
                # Include inserted silence in the measured output, not queued future PCM.
                self._speaker_level = (pcm_level(bytes(view)), time.monotonic())
            except Exception:
                try:
                    if view is not None:
                        view[:] = b"\0" * len(view)
                except Exception:
                    pass  # An invalid native output buffer cannot be silenced here.
                if (epoch == self._epoch and (self._opening or self._running)
                        and not self._playback_error):
                    self._playback_error = "PortAudio playback callback failed"
                    self._capture.clear()
                    self._clear_playback_locked()
                    if self._running and self._on_playback_failure is not None:
                        self._on_playback_failure()
                    self._notify_locked()

    def _finished(self, epoch, capture):
        with self._lock:
            if epoch == self._epoch and (self._opening or self._running):
                if capture:
                    if not self._capture_error:
                        self._capture_error = "PortAudio capture stopped unexpectedly"
                        if self._running and self._on_capture_loss is not None:
                            self._on_capture_loss()
                    self._capture.clear()
                else:
                    if not self._playback_error:
                        self._playback_error = "PortAudio playback stopped unexpectedly"
                        if self._running and self._on_playback_failure is not None:
                            self._on_playback_failure()
                    self._capture.clear()
                    self._clear_playback_locked()
                self._notify_locked()

    def _track_native(self, function, *args):
        async def run():
            try:
                return await asyncio.to_thread(function, *args)
            except _StreamCleanupError as exc:
                # Record failure before the task becomes done and can be forgotten.
                self._native_cleanup_error = exc
                raise

        task = asyncio.create_task(run())
        self._native_tasks.add(task)

        def done(completed):
            self._native_tasks.discard(completed)
            if not completed.cancelled():
                completed.exception()  # Timed-out operations still have an owner.

        task.add_done_callback(done)
        return task

    def _check_opening_error(self):
        with self._lock:
            error = self._capture_error or self._playback_error
        if error:
            raise AudioError(error)

    def _open(self, sd, opening, epoch):
        streams = []
        try:
            for capture, target in ((True, self.input_target), (False, self.output_target)):
                with opening.lock:
                    if opening.abandoned:
                        raise AudioError("PortAudio start cancelled")
                factory = sd.RawInputStream if capture else sd.RawOutputStream
                callback = self._input_callback if capture else self._output_callback
                stream = factory(samplerate=SAMPLE_RATE, channels=CHANNELS, dtype="int16",
                                 device=target, blocksize=0, latency=0.06,
                                 callback=lambda data, frames, timing, status, callback=callback:
                                     callback(epoch, data, frames, timing, status),
                                 finished_callback=lambda capture=capture: self._finished(epoch, capture))
                streams.append(stream)
                with opening.lock:
                    if opening.abandoned:
                        raise AudioError("PortAudio start cancelled")
                self._check_opening_error()
                stream.start()
                self._check_opening_error()
            with opening.lock:
                if opening.abandoned:
                    raise AudioError("PortAudio start cancelled")
                opening.streams = tuple(streams)
            return tuple(streams)
        except BaseException:
            with self._lock:
                if epoch == self._epoch:
                    self._opening = False
            _close_streams(streams)
            raise

    async def _bounded_close(self, streams):
        if streams:
            task = self._track_native(_close_streams, streams)
            done, _ = await asyncio.wait((task,), timeout=CLOSE_TIMEOUT)
            if not done:
                raise AudioError("PortAudio cleanup timed out; native cleanup is still pending")
            task.result()

    async def start(self):
        async with self._lifecycle:
            if self._running:
                return
            if self._native_cleanup_error:
                raise AudioError(
                    "PortAudio native cleanup failed; streams may still be open"
                ) from self._native_cleanup_error
            if any(not task.done() for task in self._native_tasks):
                raise AudioError("PortAudio native cleanup is still pending")
            prerequisites = probe(self.runtime_dir)
            if not prerequisites["supported"]:
                raise UnsupportedAudioError(prerequisites["error"])
            if not prerequisites["available"]:
                raise AudioError(prerequisites["error"])
            self._loop = asyncio.get_running_loop()
            with self._lock:
                self._epoch += 1
                epoch = self._epoch
                self._waiting = self._wake_pending = False
                self._capture_error = self._playback_error = None
                self._opening = True
                self._ready.clear()
            opening = _Opening()
            try:
                task = self._track_native(self._open, _sounddevice(), opening, epoch)
                done, _ = await asyncio.wait((task,), timeout=START_TIMEOUT)
                if not done:
                    raise AudioError("PortAudio stream start timed out; late opens will be closed")
                streams = task.result()
                with self._lock:
                    error = self._capture_error or self._playback_error
                    if error:
                        raise AudioError(error)
                    self._streams = streams
                    self._opening = False
                    self._running = True
            except BaseException as exc:
                with self._lock:
                    self._opening = False
                # A blocked worker owns and closes any abandoned late streams.
                streams = opening.abandon()
                try:
                    await self._bounded_close(streams)
                except AudioError:
                    pass
                if isinstance(exc, (asyncio.CancelledError, AudioError)):
                    raise
                raise AudioError("PortAudio stream start failed") from exc

    async def read(self):
        """Return live capture or fail promptly if either audio stream fails."""
        while True:
            with self._lock:
                if self._capture_error:
                    raise AudioError(self._capture_error)
                if self._playback_error:
                    raise AudioError(self._playback_error)
                if not self._running:
                    raise AudioError("Audio is stopped")
                if self._capture:
                    pcm = bytes(self._capture[:CHUNK_BYTES])
                    del self._capture[:len(pcm)]
                    return pcm
                self._ready.clear()
                self._waiting = True
            await self._ready.wait()

    async def play(self, pcm):
        if not isinstance(pcm, bytes):
            raise TypeError("PCM must be bytes")
        if len(pcm) % SAMPLE_WIDTH:
            raise ValueError("PCM must contain whole 16-bit samples")
        with self._lock:
            if not self._running:
                raise AudioError("Playback is stopped")
            if self._playback_error:
                raise AudioError(self._playback_error)
            if self._playback_bytes + len(pcm) > PLAYBACK_MAX_BYTES:
                raise AudioBackpressure("Playback queue exceeds 1.5 seconds; interrupt or slow down")
            if pcm:
                if not self._playback_bytes:
                    self._burst_at = time.monotonic()
                self._playback.append(pcm)
                self._playback_bytes += len(pcm)

    def _clear_playback_locked(self):
        self._playback.clear()
        self._playback_offset = self._playback_bytes = 0
        self._burst_active = False
        self._burst_at = None
        self._speaker_level = (0.0, 0.0)

    async def interrupt(self):
        async with self._lifecycle:
            with self._lock:
                # Waiting for this lock also fences any callback already filling a buffer.
                self._clear_playback_locked()

    async def stop(self):
        async with self._lifecycle:
            with self._lock:
                self._opening = self._running = False
                self._capture.clear()
                self._clear_playback_locked()
                self._mic_level = (0.0, 0.0)
                self._ready.set()
            streams, self._streams = self._streams, ()
            # Shield bounded cleanup from cancellation; the worker owns streams regardless.
            cleanup = asyncio.create_task(self._bounded_close(streams))
            cancelled = False
            while not cleanup.done():
                try:
                    await asyncio.wait((cleanup,))
                except asyncio.CancelledError:
                    cancelled = True
            cleanup.result()
            if self._native_cleanup_error:
                raise AudioError(
                    "PortAudio native cleanup failed; streams may still be open"
                ) from self._native_cleanup_error
            # A timed-out/cancelled start or an earlier stop may still own native
            # streams in a worker, even when this call had no streams to close.
            if any(not task.done() for task in self._native_tasks):
                raise AudioError("PortAudio native cleanup is still pending")
            if cancelled:
                raise asyncio.CancelledError
