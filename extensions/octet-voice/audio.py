"""Bounded, dependency-free asyncio audio via Linux PipeWire's pw-record/pw-play.

The wire format is 24 kHz, mono, signed little-endian PCM16 (no WAV header).
Only little-endian Linux is supported: pw-cat's supported ``s16`` format is
native-endian. Commands use the installed tools' --raw/--rate/--channels/
--format/--latency/--target options, with ``-`` for stdio. No shell, session,
or process group is created; children stay in the extension's supervision tree.

``start()`` explicitly opens the microphone and speaker; importing, constructing,
``probe()`` and ``status()`` never do. Capture is continuously drained in 20 ms
chunks, buffering at most 200 ms; overflow stops the session rather than silently
removing speech. Muted audio is discarded, not saved.
Playback admission (including the in-flight chunk) is capped at 1.5 seconds;
``play`` raises AudioBackpressure rather than accumulating stale audio or waiting
indefinitely. Calls must contain whole samples and fit the remaining budget.
Playback assembles full frames, briefly prebuffers network jitter, and continuously
feeds local silence when idle. An absolute sample clock with a small output lead
prevents scheduler oversleep from accumulating playback delay or starving PipeWire.
``interrupt()`` drops queued audio and restarts only playback to flush downstream
buffers. Playback failures remain fatal until stop/start. Already audible samples
cannot be retracted. Call ``stop()`` in finally.

Probe checks local prerequisites only, not server/device connectivity. Default
PipeWire devices are used unless target node names/serials are supplied. Children
inherit the environment; absent XDG_RUNTIME_DIR is resolved only to a private,
current-user-owned /run/user/<uid>. No device is selected or changed globally.
"""

from __future__ import annotations

import asyncio
from array import array
from collections import deque
import math
import time
import os
from pathlib import Path
import shutil
import stat
import sys

SAMPLE_RATE = 24_000
CHANNELS = 1
SAMPLE_WIDTH = 2
CHUNK_SECONDS = 0.02
CHUNK_BYTES = int(SAMPLE_RATE * CHANNELS * SAMPLE_WIDTH * CHUNK_SECONDS)
CAPTURE_CHUNKS = 10
PLAYBACK_MAX_BYTES = 72_000
PLAYBACK_PREBUFFER_SECONDS = 0.06
PLAYBACK_PREBUFFER_BYTES = int(SAMPLE_RATE * SAMPLE_WIDTH * PLAYBACK_PREBUFFER_SECONDS)
PLAYBACK_LEAD_SECONDS = 0.08
SILENCE = bytes(CHUNK_BYTES)
PROCESS_TIMEOUT = 0.5
START_TIMEOUT = 3.0


def pcm_level(pcm):
    """Measured RMS on a bounded -60..0 dBFS scale; no samples are retained."""
    samples = array("h")
    samples.frombytes(pcm)
    if not samples:
        return 0.0
    rms = math.sqrt(sum(value * value for value in samples) / len(samples))
    return max(0.0, min(1.0, (60 + 20 * math.log10(max(rms / 32768, 1e-6))) / 60))


class AudioError(RuntimeError):
    """Audio is unavailable, stopped, or a child process failed."""


class UnsupportedAudioError(AudioError):
    """This platform is not supported by this backend."""


class AudioBackpressure(AudioError):
    """Playback's bounded budget is full; interrupt or slow the producer."""


def _child_environment(runtime_dir=None):
    if sys.platform != "linux" or sys.byteorder != "little":
        raise UnsupportedAudioError("PipeWire audio requires little-endian Linux")
    env = os.environ.copy()
    runtime = runtime_dir if runtime_dir is not None else env.get("XDG_RUNTIME_DIR")
    if runtime is None:
        runtime = f"/run/user/{os.getuid()}"
    path = Path(runtime)
    try:
        info = path.lstat()
    except OSError as exc:
        raise AudioError(f"PipeWire runtime directory unavailable: {path}") from exc
    if (not path.is_absolute() or not stat.S_ISDIR(info.st_mode)
            or info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700):
        raise AudioError("PipeWire runtime directory must be absolute, owned by the current user, and mode 0700")
    env["XDG_RUNTIME_DIR"] = str(path)
    return env


def probe(runtime_dir=None):
    """Return prerequisite status without starting processes or opening devices."""
    result = {"supported": sys.platform == "linux" and sys.byteorder == "little",
              "available": False, "runtime_dir": None,
              "pw_record": None, "pw_play": None, "error": None}
    try:
        env = _child_environment(runtime_dir)
        result["runtime_dir"] = env["XDG_RUNTIME_DIR"]
        result["pw_record"] = shutil.which("pw-record", path=env.get("PATH"))
        result["pw_play"] = shutil.which("pw-play", path=env.get("PATH"))
        if not result["pw_record"] or not result["pw_play"]:
            raise AudioError("Install PipeWire's pw-record and pw-play executables")
        result["available"] = True
    except AudioError as exc:
        result["error"] = str(exc)
    return result


async def _close_process(process):
    """Abort buffered playback, terminate/kill and reap, with bounded waits."""
    if process is None:
        return
    if process.stdin is not None:
        process.stdin.transport.abort()

    # A full capture pipe can keep asyncio Process.wait pending even after exit.
    async def discard():
        if process.stdout is not None:
            while await process.stdout.read(CHUNK_BYTES):
                pass

    drain = asyncio.create_task(discard())
    try:
        if process.returncode is None:
            try:
                process.terminate()
            except ProcessLookupError:
                pass
        try:
            await asyncio.wait_for(process.wait(), PROCESS_TIMEOUT)
        except asyncio.TimeoutError:
            try:
                process.kill()
            except ProcessLookupError:
                pass
            try:
                await asyncio.wait_for(process.wait(), PROCESS_TIMEOUT)
            except asyncio.TimeoutError as exc:
                raise AudioError("PipeWire child did not exit after kill") from exc
    finally:
        drain.cancel()
        await asyncio.gather(drain, return_exceptions=True)


async def _settle(awaitable):
    """Finish bounded child cleanup even when its caller is cancelled."""
    task = asyncio.create_task(awaitable)
    cancelled = False
    while not task.done():
        try:
            await asyncio.shield(task)
        except asyncio.CancelledError:
            cancelled = True
    result = task.result()
    if cancelled:
        raise asyncio.CancelledError
    return result


class PipeWireAudio:
    """Single-event-loop audio owner; start/interrupt/stop are serialized.

    ``read`` waits for the next live chunk, or raises AudioError on stop or failure
    of either stream.
    ``play`` accepts bytes immediately or raises AudioBackpressure. ``muted``
    can be assigned, or use synchronous ``set_muted(bool)``. Muting does not
    close the capture device: the pump keeps draining and discarding it.
    """

    def __init__(self, input_target=None, output_target=None, runtime_dir=None,
                 on_capture_loss=None, on_playback_failure=None):
        for target in (input_target, output_target):
            if target is not None and (not isinstance(target, str) or not target or "\0" in target):
                raise ValueError("Audio targets must be nonempty node names or serial strings")
        self.input_target = input_target
        self.output_target = output_target
        self.runtime_dir = runtime_dir
        self._on_capture_loss = on_capture_loss
        self._on_playback_failure = on_playback_failure
        self._muted = False
        self._mute_generation = 0
        self._running = False
        self._interrupting = False
        self._capture = None
        self._playback = None
        self._capture_task = None
        self._playback_task = None
        self._capture_error = None
        self._playback_error = None
        self._capture_chunks = deque(maxlen=CAPTURE_CHUNKS)
        self._playback_chunks = deque()
        self._playback_bytes = 0
        self._capture_ready = asyncio.Condition()
        self._playback_buffering = True
        self._playback_first_at = None
        self._lifecycle = asyncio.Lock()
        self._env = None
        self._commands = None
        self._mic_level = (0.0, 0.0)
        self._speaker_level = (0.0, 0.0)

    def levels(self):
        """Thread-safe immutable observations, decayed on stalled/stopped streams."""
        now = time.monotonic()
        mic, mic_at = self._mic_level
        speaker, speaker_at = self._speaker_level
        return (mic if self._running and not self.muted and now - mic_at < 0.15 else 0.0,
                speaker if self._running and now - speaker_at < 0.15 else 0.0)

    @property
    def muted(self):
        return self._muted

    @muted.setter
    def muted(self, value):
        self.set_muted(value)

    def set_muted(self, value):
        if not isinstance(value, bool):
            raise TypeError("muted must be a boolean")
        if self._muted != value:
            self._muted = value
            self._mute_generation += 1
            self._capture_chunks.clear()
            self._mic_level = (0.0, 0.0)

    def status(self):
        result = probe(self.runtime_dir)
        result.update(running=self._running, muted=self.muted,
                      playback_queued_bytes=self._playback_bytes,
                      capture_queued_chunks=len(self._capture_chunks),
                      capture_error=self._capture_error,
                      playback_error=self._playback_error)
        return result

    def _playback_exited(self):
        # The old playback process is intentionally closed during interruption.
        return (not self._interrupting and self._playback is not None
                and self._playback.returncode is not None)

    async def _spawn(self, playback):
        command = self._commands[1 if playback else 0]
        target = self.output_target if playback else self.input_target
        args = [command, "--raw", "--rate", str(SAMPLE_RATE), "--channels", "1",
                "--format", "s16", "--latency", "80ms" if playback else "20ms"]
        if target is not None:
            args.extend(["--target", target])
        args.append("-")
        async def open_owned():
            process = await asyncio.create_subprocess_exec(
                *args, stdin=asyncio.subprocess.PIPE if playback else asyncio.subprocess.DEVNULL,
                stdout=asyncio.subprocess.DEVNULL if playback else asyncio.subprocess.PIPE,
                stderr=asyncio.subprocess.DEVNULL, env=self._env, limit=CHUNK_BYTES * 4)
            # Publish ownership inside the spawn task, before its result can race
            # a second cancellation of the waiter.
            if playback:
                self._playback = process
            else:
                self._capture = process
            return process

        spawn = asyncio.create_task(open_owned())
        try:
            return await asyncio.wait_for(asyncio.shield(spawn), START_TIMEOUT)
        except (asyncio.CancelledError, asyncio.TimeoutError):
            # Do not abandon a child created concurrently with cancellation.
            try:
                process = await asyncio.wait_for(asyncio.shield(spawn), START_TIMEOUT)
            except BaseException:
                spawn.cancel()
                await asyncio.gather(spawn, return_exceptions=True)
                raise
            # _stop() retains the registered child if cleanup is interrupted
            # or cannot confirm exit.
            await _settle(_close_process(process))
            if playback:
                self._playback = None
            else:
                self._capture = None
            raise

    async def start(self):
        async with self._lifecycle:
            if self._running:
                return
            if self._capture is not None or self._playback is not None:
                raise AudioError("PipeWire child cleanup is unconfirmed; stop audio before restarting")
            prerequisites = probe(self.runtime_dir)
            if not prerequisites["supported"]:
                raise UnsupportedAudioError(prerequisites["error"])
            if not prerequisites["available"]:
                raise AudioError(prerequisites["error"])
            self._env = _child_environment(self.runtime_dir)
            self._commands = (prerequisites["pw_record"], prerequisites["pw_play"])
            self._capture_error = self._playback_error = None
            try:
                self._capture = await self._spawn(False)
                self._playback = await self._spawn(True)
                self._playback.stdin.transport.set_write_buffer_limits(high=CHUNK_BYTES, low=0)
                self._running = True
                self._capture_task = asyncio.create_task(self._capture_loop())
                self._playback_task = asyncio.create_task(self._playback_loop())
            except BaseException:
                await _settle(self._stop())
                raise

    async def read(self):
        async with self._capture_ready:
            await self._capture_ready.wait_for(
                lambda: (self._capture_chunks or self._capture_error or self._playback_error
                         or not self._running or self._playback_exited()))
            if self._capture_error:
                raise AudioError(self._capture_error)
            if self._playback_error:
                raise AudioError(self._playback_error)
            if not self._running:
                raise AudioError("Audio is stopped")
            if self._playback_exited():
                raise AudioError("PipeWire playback exited")
            return self._capture_chunks.popleft()

    async def play(self, pcm):
        if not isinstance(pcm, bytes):
            raise TypeError("PCM must be bytes")
        if len(pcm) % SAMPLE_WIDTH:
            raise ValueError("PCM must contain whole 16-bit samples")
        if not self._running or self._interrupting:
            raise AudioError("Playback is stopped or being interrupted")
        if self._playback_error or self._playback_exited():
            raise AudioError(self._playback_error or "PipeWire playback exited")
        if self._playback_bytes + len(pcm) > PLAYBACK_MAX_BYTES:
            raise AudioBackpressure("Playback queue exceeds 1.5 seconds; interrupt or slow down")
        if pcm and self._playback_first_at is None:
            self._playback_first_at = time.monotonic()
        for offset in range(0, len(pcm), CHUNK_BYTES):
            self._playback_chunks.append(pcm[offset:offset + CHUNK_BYTES])
        self._playback_bytes += len(pcm)

    async def _capture_loop(self):
        try:
            while True:
                generation = self._mute_generation
                chunk = await self._capture.stdout.readexactly(CHUNK_BYTES)
                if (not self.muted and generation == self._mute_generation
                        and not self._playback_error and not self._playback_exited()):
                    async with self._capture_ready:
                        # Muting may have cleared the queue while this task was
                        # waiting for the condition lock. Never reinsert an old
                        # chunk after a mute or an intervening mute/unmute cycle.
                        if (not self.muted and generation == self._mute_generation
                                and not self._playback_error and not self._playback_exited()):
                            if len(self._capture_chunks) == CAPTURE_CHUNKS:
                                self._capture_error = "PipeWire capture queue overflow; audio was lost"
                                self._capture_chunks.clear()
                                self._mic_level = (0.0, 0.0)
                                # Revoke handoffs immediately, even if the sender is
                                # blocked and no read() is waiting for this error.
                                if self._on_capture_loss is not None:
                                    self._on_capture_loss()
                                self._capture_ready.notify_all()
                                return
                            self._mic_level = (pcm_level(chunk), time.monotonic())
                            self._capture_chunks.append(chunk)
                            self._capture_ready.notify_all()
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            async with self._capture_ready:
                self._capture_error = f"PipeWire capture failed ({type(exc).__name__})"
                self._capture_chunks.clear()
                if self._on_capture_loss is not None:
                    self._on_capture_loss()
                self._capture_ready.notify_all()

    def _output_frame(self, now):
        if self._playback_buffering:
            ready = self._playback_bytes >= PLAYBACK_PREBUFFER_BYTES
            tail = (self._playback_first_at is not None
                    and now - self._playback_first_at >= PLAYBACK_PREBUFFER_SECONDS)
            if not ready and not tail:
                return SILENCE, 0
            self._playback_buffering = False
        remaining = CHUNK_BYTES
        pieces = []
        while remaining and self._playback_chunks:
            chunk = self._playback_chunks.popleft()
            take = min(remaining, len(chunk))
            pieces.append(chunk[:take])
            if take < len(chunk):
                self._playback_chunks.appendleft(chunk[take:])
            remaining -= take
        if not self._playback_chunks:
            self._playback_first_at = None
        if remaining:
            self._playback_buffering = True
        return b"".join(pieces).ljust(CHUNK_BYTES, b"\0"), CHUNK_BYTES - remaining

    async def _playback_loop(self):
        # Write a bounded lead, then follow one cumulative sample clock. A fresh
        # relative sleep per packet runs systematically slower than real audio
        # whenever the scheduler oversleeps, eventually filling even an idle
        # realtime stream with stale PCM. Reset only after a long scheduler stall.
        deadline = time.monotonic() - PLAYBACK_LEAD_SECONDS + CHUNK_SECONDS
        try:
            while True:
                deadline = max(deadline, time.monotonic() - PLAYBACK_LEAD_SECONDS + CHUNK_SECONDS)
                await asyncio.sleep(max(0, deadline - time.monotonic()))
                if self._playback.returncode is not None:
                    raise AudioError("PipeWire playback exited")
                chunk, admitted = self._output_frame(time.monotonic())
                self._playback.stdin.write(chunk)
                await asyncio.wait_for(self._playback.stdin.drain(), PROCESS_TIMEOUT)
                self._playback_bytes -= admitted
                self._speaker_level = (pcm_level(chunk), time.monotonic())
                deadline += CHUNK_SECONDS
        except asyncio.CancelledError:
            raise
        except Exception as exc:
            await self._fail_playback(f"PipeWire playback failed ({type(exc).__name__})")

    async def _fail_playback(self, message):
        self._playback_error = message
        self._playback_chunks.clear()
        self._playback_bytes = 0
        self._capture_chunks.clear()
        self._mic_level = self._speaker_level = (0.0, 0.0)
        if self._on_playback_failure is not None:
            self._on_playback_failure()
        async with self._capture_ready:
            self._capture_ready.notify_all()

    async def interrupt(self):
        async with self._lifecycle:
            if not self._running:
                return
            if self._playback_error or self._playback_exited():
                raise AudioError(self._playback_error or "PipeWire playback exited")
            await _settle(self._interrupt())

    async def _interrupt(self):
        # The playback worker or child may have failed while _settle scheduled us.
        if self._playback_error or self._playback_exited():
            raise AudioError(self._playback_error or "PipeWire playback exited")
        self._interrupting = True
        try:
            await self._stop_playback()
            self._playback = await self._spawn(True)
            self._playback.stdin.transport.set_write_buffer_limits(high=CHUNK_BYTES, low=0)
            self._playback_task = asyncio.create_task(self._playback_loop())
        except BaseException:
            if not self._playback_error:
                await self._fail_playback("PipeWire playback restart failed; stop audio before retrying")
            raise
        finally:
            self._interrupting = False

    async def _stop_playback(self):
        self._speaker_level = (0.0, 0.0)
        if self._playback_task:
            self._playback_task.cancel()
            await asyncio.gather(self._playback_task, return_exceptions=True)
            self._playback_task = None
        self._playback_chunks.clear()
        self._playback_bytes = 0
        self._playback_buffering = True
        self._playback_first_at = None
        await _close_process(self._playback)
        self._playback = None

    async def stop(self):
        async with self._lifecycle:
            await _settle(self._stop())

    async def _stop(self):
        self._running = False
        async with self._capture_ready:
            self._capture_chunks.clear()
            self._capture_ready.notify_all()
        if self._capture_task:
            self._capture_task.cancel()
            await asyncio.gather(self._capture_task, return_exceptions=True)
            self._capture_task = None
        results = await asyncio.gather(self._stop_playback(), _close_process(self._capture),
                                       return_exceptions=True)
        if not isinstance(results[1], BaseException):
            self._capture = None
        for result in results:
            if isinstance(result, BaseException):
                raise result
