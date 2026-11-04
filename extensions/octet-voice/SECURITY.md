# Security and privacy

This experimental extension is trusted executable code, not a sandbox. Review its
manifest before installing or enabling it. Network, processes, unrestricted local
filesystem access, and disclosure of one native provider credential are declared.

The microphone opens only after an explicit start and remote connection. Muting
keeps the device open but drains/discards captured audio; stop closes it. If audio
cleanup cannot be confirmed, voice reports that failure, fences restart, and asks
you to close octet for process-tree cleanup. Voice sends microphone audio, new
typed prompts and successful final agent answers to
the selected OpenAI/Codex realtime service. Do not enable it for conversations you
cannot share with that provider. Headphones are recommended; no local acoustic
echo cancellation is implemented.

Credentials are resolved through octet's existing provider flow, retained only in
memory, and sent only to fixed authenticated endpoints. Redirects and environment
proxies are disabled for those requests. Error bodies, tokens, SDP and raw media
are not logged or recorded. Device settings contain no credentials and stay in
the user's private configuration directory, outside this repository.

Playback/capture, UI observations and coding handoffs use bounded queues. Session
ownership and generation fence injection. On connection/audio/UI failure, voice
revokes pending handoffs and requests microphone cleanup, without automatically
replaying ambiguous work. If cleanup times out, it continues when possible and
reports that closure is unconfirmed; close octet rather than relying on it.
The ordinary coding agent still owns tool permissions and effect confirmation.
Final agent answers may contain untrusted project or web content. They are labeled
as shared data, not promoted to OpenAI developer messages, and the voice model is
instructed not to delegate them as new requests. These measures are not a sandbox:
a realtime model can still misinterpret or follow injected instructions. Local
capture-buffer overflow or a PortAudio input discontinuity stops voice and revokes
pending handoffs, but an already-started handoff may still be admitted. Some other
losses, including upstream PipeWire and network/media loss, cannot be detected by
this extension. Incomplete speech can change an instruction's meaning; use typed
input when omitted words or corrections would be consequential.

Dependency installation is an explicit setup operation using hash-locked packages
in a private virtual environment. Runtime discovery and package building do not
install or download dependencies. Hashes establish artifact integrity, not safety;
upstream dependency updates still require review. The vendored SDK is source only
with retained license/provenance. CI never needs provider credentials or audio hardware.

Do not attach credentials, recordings, private prompts, or unsanitized logs to bug
reports. For a sensitive issue, use GitHub's private vulnerability reporting when
available, or contact the Skaft maintainers privately before public disclosure.
