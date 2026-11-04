"""Session-fenced realtime conversation, independent of the native agent loop.

Voice is a spoken surface of the active conversation (including typed turns),
not a second executor. Injection ACKs mean admission, never task completion.
Only the host's successful final-response hook is shared as an agent answer;
reasoning and raw tool output are never forwarded.
"""
from __future__ import annotations

import asyncio
import concurrent.futures
import queue
import sys
import threading
import time

from audio import AudioBackpressure
from realtime import SHARED_USER_PREFIX

MAX_TEXT_BYTES = 16_384
SPEECH_BYTES = 3960  # Codex's ~990-token spoken-final budget, conservatively UTF-8.
MAX_HANDOFFS = 4096
MAX_SESSION_SECONDS = 3600
TASK_DRAIN_TIMEOUT = 1
CAPTURE_LOSS_MESSAGE = (
    "Voice stopped: microphone audio was lost. A handoff may already be queued; "
    "check octet before repeating the request. No automatic retry was attempted."
)
PLAYBACK_FAILURE_MESSAGE = (
    "Voice stopped: audio playback failed. A handoff may already be queued; "
    "check octet before repeating the request. No automatic retry was attempted."
)


class TranscriptLimit(ValueError):
    """Captions are bounded, never silently clipped."""


class HandoffAdmissionError(RuntimeError):
    """The host did not confirm admission; it may nevertheless have queued the input."""


class AudioFailure(RuntimeError):
    """A capture or playback failure was reported independently of read()."""


def _audio_failure_message(capture_loss, playback_failure):
    if capture_loss.is_set():
        return CAPTURE_LOSS_MESSAGE
    if playback_failure.is_set():
        return PLAYBACK_FAILURE_MESSAGE
    return None


def _consume_task(task):
    if not task.cancelled():
        task.exception()


def audio_backend():
    """Select native audio lazily; selection never imports sounddevice or opens devices."""
    if sys.platform in ("darwin", "win32"):
        import portable_audio
        return portable_audio.PortableAudio, portable_audio
    import audio
    return audio.PipeWireAudio, audio


def plain(text, limit=MAX_TEXT_BYTES):
    if not isinstance(text, str) or len(text.encode("utf-8")) > limit:
        raise ValueError("Voice text exceeds its byte budget")
    # Provider transcripts and responses cannot inject terminal escapes/controls.
    return "".join(c for c in text if c in "\n\t" or (ord(c) >= 32 and not 127 <= ord(c) < 160))


class VoiceService:
    def __init__(self, inject, notify, client_factory=None, audio_factory=None):
        self.inject = inject
        self.notify = notify
        # Notification writes may stall behind the host's stdout transport. Keep
        # them off the media loop so errors cannot prevent microphone cleanup.
        self._notices = queue.Queue(maxsize=8)
        self._notice_stop = threading.Event()
        self._notice_lost = threading.Event()
        self._notice_thread = threading.Thread(target=self._send_notices, name="octet-voice-notices", daemon=True)
        self._notice_thread.start()
        self.client_factory = client_factory
        self.audio_factory = audio_factory
        self._loop = asyncio.new_event_loop()
        self._thread = threading.Thread(target=self._loop.run_forever,
                                        name="octet-voice", daemon=True)
        self._thread.start()
        self._epoch = 0
        self._owner = None
        self._host_session = None
        self._parent = None
        self._task = None
        self._client = None
        self._audio = None
        self._outbox = None
        self._handoffs = None
        self._seen = set()
        self._state = "stopped"
        self._error = None
        self._muted = False
        self._started = None
        self._backend = None
        self._closed = False
        self._cleanup_failed = False
        self._tearing_down = False
        self._mute_lock = asyncio.Lock()
        self._caption = ""
        self._caption_sequence = 0
        self._transcript_views = {kind: ("", True, 0)
                                  for kind in ("input_transcript", "output_transcript")}
        self._injection_cancel = threading.Event()
        self._capture_loss = threading.Event()
        self._playback_failure = threading.Event()
        self._pending_closes = set()

    async def _bounded_close(self, awaitable, timeout):
        """Report a timeout without abandoning a queued native-resource close."""
        task = asyncio.create_task(awaitable)
        done, _ = await asyncio.wait((task,), timeout=timeout)
        if not done:
            # In particular, audio.stop() may be waiting for the lifecycle lock
            # during a cancellation-resistant interruption. Cancelling that wait
            # would permanently abandon the microphone. Retain the task until it
            # can finish; restart remains fenced even after its eventual success.
            self._pending_closes.add(task)

            def completed(finished):
                self._pending_closes.discard(finished)
                _consume_task(finished)

            task.add_done_callback(completed)
            raise TimeoutError("Voice cleanup timed out")
        task.result()

    def _notify(self, message, *, level):
        try:
            self._notices.put_nowait((message, level))
        except queue.Full:
            self._notice_lost.set()

    def _send_notices(self):
        while not self._notice_stop.is_set():
            try:
                message, level = self._notices.get(timeout=0.1)
            except queue.Empty:
                continue
            try:
                self.notify(message, level=level)
                if self._notice_lost.is_set():
                    self._notice_lost.clear()
                    self.notify("Some voice notices were lost; check /voice status.", level="warning")
            except Exception:
                # A broken host channel must never block or revive media.
                pass

    def call(self, coroutine, timeout=3):
        if self._closed:
            coroutine.close()
            raise RuntimeError("Voice service is closed")
        future = asyncio.run_coroutine_threadsafe(coroutine, self._loop)
        try:
            return future.result(timeout)
        except concurrent.futures.TimeoutError:
            future.cancel()
            raise TimeoutError("Voice control timed out") from None

    def snapshot(self):
        audio = self._audio
        mic, speaker = audio.levels() if audio is not None else (0.0, 0.0)
        user, user_final, _ = self._transcript_views["input_transcript"]
        voice, voice_final, _ = self._transcript_views["output_transcript"]
        return {"state": self._state, "muted": self._muted,
                "microphone_level": mic, "speaker_level": speaker, "caption": self._caption,
                "user_transcript": user, "user_transcript_final": user_final,
                "voice_transcript": voice, "voice_transcript_final": voice_final,
                "backend": self._backend, "error": self._error,
                "elapsed_seconds": int(time.monotonic() - self._started) if self._started else 0}

    def owns(self, context):
        return self._owner is not None and context.get("resource_owner") == self._owner

    async def reserve(self, owner, host_session, parent):
        if not isinstance(owner, dict) or not host_session or not isinstance(parent, int):
            raise ValueError("Voice requires a foreground session-owned command")
        if (self._cleanup_failed or self._state not in ("stopped", "failed")
                or (self._task is not None and not self._task.done())):
            raise ValueError("Voice is already active; stop it before restarting")
        self._epoch += 1
        self._injection_cancel = threading.Event()
        self._capture_loss = threading.Event()
        self._playback_failure = threading.Event()
        self._tearing_down = False
        self._owner = dict(owner)
        self._host_session = host_session
        self._parent = parent
        self._state, self._error = "connecting", None
        self._caption = ""
        self._caption_sequence = 0
        self._transcript_views = {kind: ("", True, 0) for kind in self._transcript_views}
        # Hooks can arrive during credential resolution and connection setup.
        # Retain those observed turns rather than silently losing their context.
        self._outbox = asyncio.Queue(maxsize=16)
        return self._epoch

    async def activate(self, ticket, settings, credential):
        if ticket != self._epoch or self._state != "connecting":
            return False
        self._backend = settings["backend"]
        self._task = asyncio.create_task(self._session(ticket, settings, credential))
        return True

    async def failed_start(self, ticket):
        if ticket == self._epoch:
            await self.stop()
            self._state, self._error = "failed", "Credential resolution failed; check native provider login"

    async def _session(self, epoch, settings, credential):
        client = audio = None
        tasks = []
        cancellation = self._injection_cancel
        capture_loss = self._capture_loss
        playback_failure = self._playback_failure
        failure_ready = asyncio.Event()
        loop = asyncio.get_running_loop()

        def audio_failed(failure):
            # PortAudio invokes this on its native callback thread. Revoke the
            # original session's requests before scheduling event-loop cleanup.
            failure.set()
            cancellation.set()
            try:
                loop.call_soon_threadsafe(failure_ready.set)
            except RuntimeError:
                pass  # A late native callback after service shutdown.

        def on_capture_loss():
            audio_failed(capture_loss)

        def on_playback_failure():
            audio_failed(playback_failure)

        async def watch_audio():
            await failure_ready.wait()
            raise AudioFailure()

        try:
            if self.client_factory is None:
                from realtime import RealtimeClient
                factory = RealtimeClient
            else:
                factory = self.client_factory
            client = factory(settings, credential)
            self._client = client
            await asyncio.wait_for(client.connect(), 35)
            if epoch != self._epoch:
                return
            audio_factory = self.audio_factory or audio_backend()[0]
            audio = audio_factory(input_target=settings["input_target"],
                                  output_target=settings["output_target"],
                                  on_capture_loss=on_capture_loss,
                                  on_playback_failure=on_playback_failure)
            self._audio = audio
            audio.set_muted(self._muted)
            await client.set_muted(self._muted)
            await asyncio.wait_for(audio.start(), 8)
            if capture_loss.is_set() or playback_failure.is_set():
                raise AudioFailure()
            if epoch != self._epoch:
                return
            self._handoffs = asyncio.Queue(maxsize=8)
            self._seen.clear()
            self._started = time.monotonic()
            self._state = "muted" if self._muted else "listening"
            tasks = [asyncio.create_task(self._capture(audio, client, cancellation)),
                     asyncio.create_task(self._receive(audio, client, epoch, cancellation)),
                     asyncio.create_task(self._send_updates(client, epoch, cancellation)),
                     asyncio.create_task(self._delegate(client, epoch, cancellation)),
                     asyncio.create_task(watch_audio()),
                     asyncio.create_task(asyncio.sleep(MAX_SESSION_SECONDS))]
            done, _ = await asyncio.wait(tasks, return_when=asyncio.FIRST_COMPLETED)
            for task in done:
                task.result()
        except asyncio.CancelledError:
            pass
        except Exception as error:
            if epoch == self._epoch:
                # Do not expose network exception bodies/headers/SDP or credentials.
                failure_message = _audio_failure_message(capture_loss, playback_failure)
                if failure_message:
                    self._error = failure_message
                elif isinstance(error, TranscriptLimit):
                    self._error = "Voice stopped: transcript exceeded its bounded capacity; no text was silently clipped. Restart voice for a new conversation."
                elif isinstance(error, AudioBackpressure):
                    self._error = "Voice stopped (AudioBackpressure): playback fell behind. Restart voice; no audio was replayed."
                elif isinstance(error, HandoffAdmissionError):
                    self._error = ("Voice stopped: handoff admission was not confirmed and the request may already "
                                   "be queued. Check octet before resending; no automatic retry was attempted.")
                else:
                    self._error = f"Voice failed ({type(error).__name__}); check connection, provider access, and audio devices"
                self._state = "failed"
                self._notify(self._error, level="error")
        finally:
            # Mark teardown before its first await. A later explicit stop must
            # join this cleanup, not cancel it and falsely declare devices closed.
            self._tearing_down = True
            cancellation.set()
            for task in tasks:
                task.cancel()
            # A network sender can suppress cancellation. Do not join it before
            # closing the microphone, and do not allow cleanup to wait forever.
            if audio:
                try:
                    await self._bounded_close(audio.stop(), 2)
                except Exception:
                    self._cleanup_failed = True
                    message = "Voice audio cleanup failed; close octet to force child cleanup."
                    self._error = (self._error + " " if self._error else "") + message
                    self._notify(message, level="error")
            if client:
                try:
                    await self._bounded_close(client.close(), 2)
                except Exception:
                    self._cleanup_failed = True
                    message = "Voice connection cleanup failed; close octet before restarting voice."
                    self._error = (self._error + " " if self._error else "") + message
                    self._notify(message, level="error")
            if tasks:
                done, pending = await asyncio.wait(tasks, timeout=TASK_DRAIN_TIMEOUT)
                for task in done:
                    _consume_task(task)
                if pending:
                    self._cleanup_failed = True
                    for task in pending:
                        task.cancel()
                        task.add_done_callback(_consume_task)
                    message = "Voice tasks did not stop; close octet before restarting voice."
                    self._error = (self._error + " " if self._error else "") + message
                    self._notify(message, level="error")
            failure_message = _audio_failure_message(capture_loss, playback_failure)
            if failure_message and epoch == self._epoch and failure_message not in (self._error or ""):
                self._error = failure_message + (" " + self._error if self._error else "")
                self._state = "failed"
                self._notify(failure_message, level="error")
            if epoch == self._epoch:
                self._client = self._audio = self._outbox = self._handoffs = None
                self._owner = self._host_session = self._parent = None
                self._started = None
                self._caption = ""
                self._transcript_views = {kind: ("", True, 0) for kind in self._transcript_views}
                if self._state != "failed":
                    self._state = "failed" if self._cleanup_failed else "stopped"

    async def _capture(self, audio, client, cancellation):
        while not cancellation.is_set():
            chunk = await audio.read()
            if cancellation.is_set():
                return
            await client.send_audio(chunk)

    async def _receive(self, audio, client, epoch, cancellation):
        while epoch == self._epoch and not cancellation.is_set():
            event = await client.receive()
            if epoch != self._epoch or cancellation.is_set():
                return
            kind = event.get("type")
            if kind == "audio":
                self._state = "muted" if self._muted else "speaking"
                await audio.play(event["audio"])
            elif kind == "interrupted":
                await audio.interrupt()
                if epoch != self._epoch or cancellation.is_set():
                    return
                self._state = "muted" if self._muted else "listening"
            elif kind in ("input_transcript", "output_transcript"):
                # Transport emits complete partial snapshots, not raw deltas.
                # Keep each speaker separately: a reply must not erase the user's
                # full transcription, and late older items must not replace it.
                try:
                    text = plain(event.get("text", ""))
                except ValueError:
                    raise TranscriptLimit() from None
                sequence = event.get("sequence", self._caption_sequence + 1)
                final = event.get("final", False)
                if sequence < self._transcript_views[kind][2]:
                    continue
                self._transcript_views[kind] = (text, final, sequence)
                if sequence >= self._caption_sequence:
                    label = "You" if kind == "input_transcript" else "Voice"
                    self._caption = f"{label}: {text}" if text.strip() else ""
                    self._caption_sequence = sequence
                if kind == "input_transcript":
                    self._state = "muted" if self._muted else "listening"
            elif kind == "handoff":
                identifier = event.get("handoff_id")
                text = plain(event.get("text", ""))
                if not isinstance(identifier, str) or not identifier or len(identifier) > 256 or not text.strip():
                    raise ValueError("Invalid voice handoff")
                if identifier in self._seen:
                    continue  # Never execute a duplicate delegation.
                if len(self._seen) >= MAX_HANDOFFS:
                    raise ValueError("Voice handoff budget exhausted; restart voice")
                self._seen.add(identifier)
                # Admission can take seconds; it must not block playback, VAD or
                # barge-in. Bound pending input separately from the media queue.
                self._handoffs.put_nowait((identifier, text))
            elif kind == "error":
                if event.get("code") == "transcript_limit":
                    raise TranscriptLimit()
                raise RuntimeError("Realtime provider reported an error")
            elif kind == "closed":
                return

    async def _delegate(self, client, epoch, cancellation):
        while epoch == self._epoch and not cancellation.is_set():
            identifier, text = await self._handoffs.get()
            if epoch != self._epoch or cancellation.is_set():
                return
            # A lost ACK is ambiguous: never replay potentially admitted input.
            try:
                await asyncio.to_thread(self.inject, text, dict(self._owner), self._parent,
                                        cancellation_event=cancellation)
            except Exception:
                # A rejection or lost reply must not be mistaken for completion or
                # automatically replayed. Do not expose host/provider error bodies.
                raise HandoffAdmissionError() from None
            if epoch != self._epoch or cancellation.is_set():
                return
            await client.send_result(identifier,
                "Request admitted to the coding-agent input. This acknowledgement confirms admission only; "
                "it does not establish execution status or success.", final=False)

    async def _send_updates(self, client, epoch, cancellation):
        while epoch == self._epoch and not cancellation.is_set():
            action, text = await self._outbox.get()
            if epoch != self._epoch or cancellation.is_set():
                return
            if action == "prompt":
                if self._audio:
                    await self._audio.interrupt()
                if epoch != self._epoch or cancellation.is_set():
                    return
                await client.send_text(SHARED_USER_PREFIX + text)
            else:
                await client.send_result(None, text, final=True)

    async def update(self, action, text, owner):
        if owner != self._owner or self._outbox is None:
            return
        if (action == "answer" and isinstance(text, str)
                and (len(text) > SPEECH_BYTES or len(text.encode("utf-8")) > SPEECH_BYTES)):
            # Decide before the larger context bound: every oversized final gets
            # the spoken notice without forwarding or clipping its contents.
            text = "The agent finished. Its answer is too long to read aloud; see the full answer in octet."
        else:
            try:
                text = plain(text)
            except ValueError:
                self._notify("Voice update exceeds its budget; the full text remains in octet.", level="warning")
                return
        try:
            self._outbox.put_nowait((action, text))
        except asyncio.QueueFull:
            # Stale speech is worse than ending the explicitly started session.
            await self.stop()
            self._notify("Voice stopped: its bounded update queue filled.", level="warning")

    async def mute(self, owner, toggle=True):
        epoch = self._epoch
        # A network clear can suspend a transition. Serialize toggles, but never
        # let an old continuation or queued toggle mutate a replacement session.
        async with self._mute_lock:
            if epoch != self._epoch:
                return "Voice session changed; microphone state was not updated."
            if self._state in ("stopped", "failed"):
                return "Voice is stopped"
            if self._tearing_down or self._injection_cancel.is_set():
                return "Voice is stopping; microphone state was not updated."
            # Check on the service loop, immediately before changing capture state:
            # an owner can retire between command dispatch and this coroutine.
            if not isinstance(owner, dict) or owner != self._owner:
                return "Voice belongs to another session; its microphone was not changed."
            muted = not self._muted if toggle else True
            if muted:
                # Silence locally before waiting for the network. For unmute do
                # the reverse: keep capture muted until the client is ready.
                self._muted = True
                if self._audio:
                    self._audio.set_muted(True)
            if self._client:
                await self._client.set_muted(muted)
            if epoch != self._epoch or owner != self._owner:
                return "Voice session changed; microphone state was not updated."
            if self._tearing_down or self._injection_cancel.is_set():
                return "Voice is stopping; microphone state was not updated."
            if not muted:
                self._muted = False
                if self._audio:
                    self._audio.set_muted(False)
            self._state = ("muted" if muted else "listening") if self._audio else "connecting"
            return "Microphone muted" if muted else "Microphone live"

    async def session_settled(self, session_id):
        if session_id == self._host_session:
            await self.stop()

    async def stop(self):
        # Cancelling to_thread does not stop its running reverse RPC. Revoke it
        # explicitly as well; cancellation cannot roll back already admitted input.
        self._injection_cancel.set()
        self._epoch += 1  # Revoke pending credential callbacks and future handoffs first.
        epoch = self._epoch
        task = self._task
        already_stopping = self._state == "stopping"
        self._owner = self._host_session = self._parent = None
        self._outbox = self._handoffs = None
        self._caption = ""
        self._transcript_views = {kind: ("", True, 0) for kind in self._transcript_views}
        self._state = "stopping"
        self._started = None
        self._muted = False
        if task:
            if not already_stopping and not self._tearing_down:
                task.cancel()
            try:
                await asyncio.wait_for(asyncio.shield(task), 5)
            except (asyncio.TimeoutError, asyncio.CancelledError):
                pass  # Host process-tree supervision is the final cleanup fence.
        # Another stop may have superseded this one while both awaited cleanup.
        # It alone owns finalization; an old waiter must never clear a new task.
        if epoch != self._epoch:
            return "Voice stop requested; cleanup is still in progress."
        if task and not task.done():
            self._error = "Voice cleanup is still pending; close octet to force cleanup"
            return "Voice stop requested; cleanup is still pending. Close octet to force process-tree cleanup."
        self._task = None
        self._client = self._audio = None
        failure_message = _audio_failure_message(self._capture_loss, self._playback_failure)
        if failure_message:
            if failure_message not in (self._error or ""):
                self._error = failure_message + (" " + self._error if self._error else "")
                self._notify(failure_message, level="error")
            self._state = "failed"
            if self._cleanup_failed:
                return failure_message + " Voice cleanup could not be confirmed; close octet."
            return failure_message
        if self._cleanup_failed:
            self._state = "failed"
            return "Voice cleanup could not be confirmed. Close octet to force process-tree cleanup."
        self._state = "stopped"
        return "Voice stopped. Microphone closed; already admitted coding work is not cancelled."

    def close(self):
        if self._closed:
            return
        try:
            self.call(self.stop(), timeout=6)
        finally:
            self._closed = True
            self._loop.call_soon_threadsafe(self._loop.stop)
            self._thread.join(timeout=1)
            if not self._thread.is_alive():
                self._loop.close()
            self._notice_stop.set()
            self._notice_thread.join(timeout=0.2)
