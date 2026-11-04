# octet-voice

Experimental first-party realtime voice extension for [octet](https://github.com/skaft-software/octet).
This standalone repository is its source of truth. Experimental status is not a
production or cross-platform audio qualification.

Optional **bidirectional realtime voice**, not dictation followed by text-to-speech:
live microphone audio, conversational spoken responses, and delegation into the
active coding conversation. The voice model never executes tools itself. The
ordinary octet agent still owns coding, tools, effects and cancellation policy.
Targets octet v0.10.0 exactly. The needed Python SDK snapshot is vendored under
`vendor/` with its license, provenance and regression tests. Voice is optional:
installation, dependency setup and enablement are separate explicit actions; it
never starts recording automatically. Inclusion in the official v0.10.0 catalog
and Packages page is prepared for the release owner, not a claim of publication.

## Controls

- **Ctrl+F6 / Ctrl+F7** or `/voice captions up|down`: scroll live captions one
  wrapped row at a time without moving composer focus.
- **Ctrl+F8** or `/voice`: start/stop. Nothing opens the microphone on startup.
- **Ctrl+F9** or `/voice mute`: mute/unmute the microphone for the owning session.
  Capture is drained and discarded while muted; queued outgoing WebRTC samples are
  cleared. On OpenAI, mute also clears uncommitted server input and waits for its
  acknowledgement before allowing unmute; if confirmation fails, voice stops.
  Input already committed by the server cannot be retracted. The capture device
  remains open. **Stop** to close it.
- **Ctrl+F10** or `/voice stop`: stop, including while coding via the shortcut.
  Stop is intentionally available across sessions as an emergency privacy control.
  If your terminal intercepts these modified function keys, use slash commands while idle. Stopping
  voice **does not cancel already admitted coding work**; use native Escape/Ctrl+C.
- `/voice status`, `/voice devices`, `/voice settings`; options in `/extensions`.
- `/voice backend codex|openai`, `/voice model NAME`, `/voice speaker NAME`.
  Preferences change only while stopped and apply to the next conversation.

### Live voice strip

A persistent strip above the composer shows connection state, elapsed time,
microphone mute, and separate microphone/output waveforms measured from actual
PCM (RMS, -60..0 dBFS), with bounded 2.4-second histories. Muting clears the
microphone meter; stopped/stalled streams decay to silence. The output meter
measures PCM written to playback, not a hardware loopback. Partial speech captions
update live; the latest user and voice utterances are retained separately in the
strip instead of replacing one another or appending transcript messages. They are
ephemeral and are omitted from `/voice status`. Each utterance is bounded to
16,384 UTF-8 bytes; exceeding the limit stops voice with an explicit error rather
than silently clipping a caption. Older utterances are not a caption history.

Captions wrap to terminal width and the heading marks them as a **preview**.
`preview 1/3` means the visible window starts at wrapped caption row 1 of 3,
not that there are three recordings or conversations. The count covers the
retained user and voice utterances together; a window may show multiple rows,
so these positions overlap rather than being separate pages. The label remains
because the host may crop the strip even when the extension renders all the rows.
Long captions follow the most recent speaker unless you scroll to review them. Use
Ctrl+F6/F7 (or the slash commands above) to scroll by one wrapped row; scrolling
persists across live updates and reflow. Press down again at the last row to resume
following. The host does not tell the extension how
many above-composer rows it actually allocates, and may crop the strip to protect
other UI. If there is room for the top padding, heading, meters, their intervening
padding and one caption row, scrolling can bring each retained row into view even if
additional rows are hidden. In very
small or crowded terminals the host may hide even the first caption row: enlarge
the terminal or reduce other panels. The strip cannot guarantee all captions are
simultaneously visible on the current host.

The strip uses a blank row above its heading and blank rows between its heading,
meters, caption block and controls rather than spreading content across the
terminal. Caption lines remain adjacent; blank lines actually present in a transcript
are preserved. It switches to compact rows on short terminals and omits the top
padding below five terminal rows; the host can still crop the strip because its
actual widget budget is not exposed. The strip resizes
without taking focus or replacing the normal composer, and uses terminal palette
colors (`NO_COLOR` disables styling). It disappears on
stop, failure, session replacement or shutdown. Routine start/mute/stop and
caption shortcut actions are silent; actionable failures still appear in octet.
Explicit `/voice status` and settings/device queries retain their requested reports.

Use **headphones**. There is no local acoustic echo cancellation, so open speakers
can feed replies back into the microphone. Server VAD handles interruption; local
playback queues are flushed on speech-start/interrupt. Codex's current frameless
sideband has no response-linked RTP timestamps or reliable media-restart markers:
newly decoded RTP after interruption cannot safely be classified as old versus
new. Server cancellation is responsible for stopping that media. The OpenAI
backend clears local playback on interruption but does not send an item truncation
based on what was actually heard, so its conversation context may retain unheard
output. Neither path is a claim of fully qualified Codex-equivalent interruption.

## Authentication and backends

Default: `codex`, model `gpt-live-1-codex`, voice `cove`. Uses the native host's
**one reviewed provider/model credential**, from the selected `codex` coding
model. The same native Codex OAuth login is used on Linux, macOS and Windows;
there is no platform-specific voice login or credential-file workaround. Native octet owns sign-in/refresh; this extension never reads credentials
files, refresh tokens, arbitrary headers or ambient API keys. A Codex subscription
and realtime entitlement are required. Backend availability is not guaranteed by
ordinary coding-model access. No silent fallback to another backend or dictation.

Codex source inspected at `cb1bc0ae4ed996c67a6e118bf79a7435f0d3f0d2`:

- `codex-rs/codex-api/src/endpoint/realtime_call.rs`: OAuth WebRTC call creation
  at `https://chatgpt.com/backend-api/codex/realtime/calls?intent=quicksilver&architecture=avas`.
- `codex-rs/core/src/client.rs`, `realtime_conversation.rs`: authenticated control
  sideband `wss://api.openai.com/v1/live/{call_id}`, `openai-alpha: quicksilver=v2`.
- `codex-rs/codex-api/src/endpoint/realtime_websocket/`: frameless V3
  `delegation.created`, `delegation.context.append`, `session.context.append`.

The separate `openai` backend uses public GA Realtime WebSocket
`wss://api.openai.com/v1/realtime`, default `gpt-realtime-1.5` / `marin`, audio
PCM16 mono 24 kHz, server VAD and function calls for coding handoff. It requires
an API key configured **in octet's native OpenAI provider** and incurs ordinary
Realtime API charges. A Codex OAuth token is never sent to that public path.
Select a matching native coding provider, or set `credential_model` to the exact
registered native coding-model ID whose credential should be resolved. It is
not the realtime model name. Sign in using native octet controls first.

Credential destinations are fixed, redirects and environment proxies disabled;
Location is parsed only as a bounded opaque call ID, not followed. WebRTC media
uses the authenticated SDP's negotiated remote media addresses (no custom
STUN/TURN servers). Network/provider error bodies, tokens and SDP are not logged.
Connections do not reconnect or replay ambiguous handoffs automatically.

## Conversation and handoff semantics

Voice speaks for the **shared active conversation**, including normal typed prompts
and successful final agent answers, not a separate hidden executor. Only turns
observed after voice starts are shared; existing history is not uploaded. A typed
message used to steer an already-running agent bypasses the host's prompt hook and
is not forwarded to voice. Reasoning and raw tool output are not forwarded. Spoken
answers over 3960 UTF-8 bytes get a short notice directing you to the native answer.
Shared agent updates are labeled as untrusted context rather than new user requests,
and the OpenAI
backend does not promote their text to developer instructions. This reduces, but
cannot eliminate, prompt-injection or mistaken-delegation risk.

Only an explicit provider delegation becomes a handoff: Codex's client delegation
or OpenAI's `background_agent` function call. Speech transcripts alone update
captions; they are never submitted as coding instructions or used as a fallback
when no delegation arrives. The voice model is instructed to formulate a clear,
self-contained request that preserves the user's intent, relevant context and
corrections, rather than sending a partial recognition fragment. This is a model
instruction, not a guarantee of wording or delegation; live behavior still needs
qualification. If the bounded 200 ms capture queue fills or PortAudio reports an
input discontinuity, voice revokes pending handoffs and stops with a warning. A
handoff whose write has already begun may nevertheless be admitted: check octet
before repeating the request. Muted audio is intentionally discarded. Other
losses, including upstream PipeWire or network/media loss, may not be detected;
use typed input when missing words or corrections would be consequential.

A handoff enters `session/send_user_message` with `deliver_as: steer`, the
retained native resource owner and the voice-start request's parent ID. Idle
input starts a normal run; active input waits at the next steering boundary.
A visible `[Voice handoff to coding agent]` source marker prevents reflecting an
idle handoff back into realtime as a new typed request. Typed context is marked
already submitted and must not be delegated again.

The current host acknowledges **input admission only**, not execution completion,
and does not issue an input-to-run correlation receipt. Live steering bypasses
`before_prompt`. Therefore this extension does **not** fabricate per-handoff
completion: admission gets an explicitly nonfinal acknowledgement; successful
native final answers become standalone shared conversation updates, never a
made-up completion for a specific handoff. Multiple voiced/typed turns may be
coalesced by the native coding run. Final-answer/lifecycle events are not an
independent proof that any particular request succeeded. This is a deliberate
parity boundary until the host exposes durable correlated input receipts.

A handoff with the same provider ID is never executed twice in a session; separate
IDs may represent repeated requests and are not deduplicated by transcript. Pending
delegation and media have independent bounded queues so host admission cannot stall
playback or barge-in. Rejection or an ambiguous ACK stops voice with no retry; an
unconfirmed request may already be queued, so check octet before resending.
Capture, caption, playback and handoff/update-queue overflow, audio failure or
disconnect also stop voice without retries; capture has the limits described above.
Owner/generation changes fence further injection; session settlement, shutdown and
extension process-tree supervision close media. A conversation is limited to one
hour; there is no reliable realtime spend meter in this version.

## Install and setup

After a verified octet v0.10.0 release includes the voice bundle, install it with
`octet extension install octet-voice`. This command does not enable voice or
provision dependencies. Use the installed bundle's `requirements.lock` with the
explicit setup below, then enable `octet-voice` only when wanted. Until that
release is published, use the reviewed local-archive route below instead. See
[RELEASE.md](https://github.com/skaft-software/octet-voice/blob/main/RELEASE.md)
for release qualification and publication gates.

Audio backend selection is native to the machine running octet:

- **Linux:** little-endian PCM and PipeWire (`pw-record`, `pw-play`, `wpctl`). No
  PortAudio library or sounddevice package is required. Capture continuously sends
  fixed-duration PCM frames (including silence), with clock-corrected pacing,
  a 60 ms playback jitter prebuffer and an 80 ms output lead. Packet fragments
  are assembled without padding each packet; interruption clears queued media.
- **macOS:** sounddevice/PortAudio callbacks using Core Audio. Grant microphone
  permission to the terminal/application hosting octet when the OS requests it.
- **Windows:** sounddevice/PortAudio callbacks using the native default audio
  devices. Allow microphone access for desktop applications in Windows privacy
  settings if needed. No PipeWire/POSIX shell is used.

The macOS and Windows paths are experimental implementations, **not live-device
qualification claims**. Headphones remain required for reliable duplex use;
there is no local acoustic echo cancellation or remote-client microphone tunnel.

Install `uv` and make an existing Python 3.11+ bootstrap interpreter available on
PATH (or explicitly set up octet's private Python runtime). The manifest points
at `launcher.py`: the existing host already supports native Python-script launch
on all three platforms; no host change, shell or argument expansion is needed.
The stdlib launcher selects only the preprovisioned Python 3.14 voice venv below,
never falls back to global dependencies, and never downloads or installs anything.
On Windows it queries the native profile directory because the host sanitizes
`USERPROFILE`. Keep `uv` and dependency acquisition in explicit setup only.

From this repository (explicit setup; dependency acquisition may use the network).
For a catalog installation, skip the package build and `--path` installation and
use `~/.octet/extensions/octet-voice/requirements.lock` in the `uv pip sync` step:

```sh
uv venv --python 3.14 ~/.local/share/octet-voice/venv
uv pip sync --python ~/.local/share/octet-voice/venv/bin/python \
  --require-hashes requirements.lock
python3 build-package.py /tmp/octet-voice-stage \
  --archive /tmp/octet-voice-release/octet-voice-0.10.0.tar.gz
octet extension install --path /tmp/octet-voice-release/octet-voice-0.10.0.tar.gz
octet --enable-extension octet-voice
```

On Windows, the same private environment is under
`%USERPROFILE%\.local\share\octet-voice\venv`; provision it explicitly in PowerShell.
For a catalog installation, skip the package build and `--path` installation and
use `$HOME/.octet/extensions/octet-voice/requirements.lock` for `uv pip sync`:

```powershell
uv venv --python 3.14 "$HOME/.local/share/octet-voice/venv"
uv pip sync --python "$HOME/.local/share/octet-voice/venv/Scripts/python.exe" --require-hashes requirements.lock
python build-package.py "$env:TEMP/octet-voice-stage" --archive "$env:TEMP/octet-voice-0.10.0.tar.gz"
octet extension install --path "$env:TEMP/octet-voice-0.10.0.tar.gz"
octet --enable-extension octet-voice
```

Use the POSIX setup commands above on macOS as well as Linux. Setup may download
Python/packages; ordinary discovery, installation and extension initialization do
not. The stdlib package builder works without Bash on all three platforms;
review its source-only archive before installing it on the target.
The explicit staging allowlist includes both audio backends, launcher and vendored
SDK, never a venv, cached bytecode or local credentials.

For an existing managed installation, use `octet extension update --path ARCHIVE`
instead of `install`. First end all potentially affected octet sessions: their
extension-directory watchers can reload other extensions as well, and a running
voice process may otherwise mix old and new Python modules. Do not modify the
shared voice environment while another session is using it.

The staging directory must be new. Review capability declarations first:
network (OpenAI/Codex), process (private Python and Linux PipeWire), unrestricted filesystem (local
preferences and private interpreter) and provider-credential disclosure.
These declarations describe authority, **not a sandbox**. Installation and
persistent enablement remain separate; the latter is an explicit user decision.
The strip uses negotiated `remote_ui`; SDK changes are vendored into the staged
extension, not globally installed. PR #639's candidate head `f51fe29` already
includes the generic SDK and quiet-shortcut host changes. Older candidates may
still emit shortcut-dispatch notices; `patches/quiet-shortcuts.patch` is an
optional host patch for the matching older source, not something to reapply to
that head. See [HOST-INTEGRATION.md](HOST-INTEGRATION.md); installation never
applies a host patch or modifies the release catalog.

Preferences are atomically written at `~/.config/octet/voice.json` (no secrets).
Files use mode 0600 on POSIX; Windows uses the inherited user-profile ACL
(no automatic ACL changes):

```json
{
  "backend": "codex",
  "model": "gpt-live-1-codex",
  "voice": "cove",
  "input_target": null,
  "output_target": null,
  "credential_model": null
}
```

Null device targets use native defaults. `/voice devices` lists devices; Linux
accepts stable PipeWire `node.name` strings rather than changing numeric IDs.
macOS/Windows accept the native device names reported by that command. The extension
subprocess environment is sanitized by the host. If `XDG_RUNTIME_DIR` is absent,
only an owned, mode-0700 nonsymlink `/run/user/<uid>` is used. No desktop/session
or system audio settings are modified. Device choice and gain remain user-owned.
Live macOS/Windows hardware, remote-terminal client microphones, Bluetooth
latency, acoustic speaker feedback and native Codex feature gates are not qualified.
No desktop/session settings, device routing, gain or system audio configuration
are changed automatically.

### Dependency review

`requirements.in` pins aiortc 1.14.0, aiohttp 3.13.3 and sounddevice 0.5.6;
sounddevice is marked only for Darwin/Windows. The universal Python 3.14 lock
contains SHA-256 hashes for the complete resolved dependency set. Development-only
lock refresh: `uv pip compile requirements.in --generate-hashes
--universal --python-version 3.14 --output-file requirements.lock`.
No package installation is performed by that command.

sounddevice 0.5.6 PyPI metadata declares MIT, Python >=3.7, and CFFI (already in
the WebRTC closure); NumPy is an unselected extra. The reviewed macOS universal2
and Windows x64/ARM64 wheels contain PortAudio and the MIT license; downloaded
wheel bytes were checked against PyPI SHA-256 metadata. PortAudio is MIT-licensed;
retain upstream license notices with separately provisioned binary dependencies.
The source bundle vendors no sounddevice/PortAudio binary; only the octet SDK is
vendored, with its MIT `SDK-LICENSE` and source provenance. Metadata/hash review
is not a security audit or platform/hardware qualification.

To disable, use `/extensions` or remove `octet-voice` from `enabled_extensions`.
`octet extension remove octet-voice` removes the bundle, not the private venv or
preferences. There is no background recorder outside the extension process tree.

## Verification

Provider-free tests never open devices, call providers or spend money:

```sh
PYTHONPATH=vendor ~/.local/share/octet-voice/venv/bin/python \
  -Werror::ResourceWarning -m unittest discover -s . -q
PYTHONPATH=vendor:tests/sdk ~/.local/share/octet-voice/venv/bin/python \
  -Werror::ResourceWarning -m unittest discover -s tests/sdk -q
```

On Windows PowerShell, set `$env:PYTHONPATH = "vendor;tests/sdk"` and use
`& "$HOME/.local/share/octet-voice/venv/Scripts/python.exe"` for the interpreter.
Run the same two `-m unittest discover` commands (without the POSIX assignments).

This source repository also contains `scripts/host-smoke.py` for testing an
explicit Linux octet binary with a preprovisioned locked voice venv. It uses an
isolated home and a PATH without PipeWire tools to stop voice before credentials,
audio or provider calls. The workflow uses it against a source-bound host; this
test script is not included in the source-only extension archive. The workflow
and final-release gates are described in `RELEASE.md`. Neither fixture tests nor
the Linux host smoke test qualify macOS/Windows audio.

Fixtures cover duplex media, V2/V3 wire frames, OAuth signaling/sideband header
containment, malformed/oversized events, mute queue invalidation, silence pacing,
response serialization, detected capture loss and bounded audio backpressure,
blocked sender/audio-failure cleanup, deferred close under a slow interruption,
slow admission/barge-in,
ambiguous ACKs without replay, handoff deduplication, source reflection prevention,
shared-answer attribution, owner fences, credential/start cancellation races,
shortcuts, lifecycle teardown, measured metering, partial-caption reconciliation,
scrolling with simulated host cropping, quiet UI frames and private preferences.
Live backend and hardware qualification are separate from fixture success.
