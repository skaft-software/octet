# Images and audio

[Documentation](README.md) · [Providers](providers.md) · [Security](../SECURITY.md)

## Attach explicitly

To ask about a sound and a screenshot:

1. Select a model whose route supports the required media.
2. Paste/drop each file path through the terminal's paste mechanism, or select
   an `@` path completion.
3. Confirm `[Audio #N]` and `[Image #N]` chips before submitting:

```text
Describe the sound in the audio. Compare the screenshot with this repository's
player UI, then propose a change. Do not edit yet.
```

This is an input recipe, not a recorded/live-provider demonstration. Ordinary
typed paths—including raw-key terminal drops—remain text, not upload consent.
A failed attachment leaves the path visible and reports a diagnostic rather
than creating a chip. An enabled `read` tool can still read a named file under
its policy; use `--no-tools` for explicitly attached input only.

## Formats and limits

| Surface | Supported input |
| --- | --- |
| TUI attachment or built-in `read` | PNG/JPEG/GIF/WebP images, **5 MiB each**; native WAV/MP3 audio, **20 MiB each**, only with a compatible model on OpenAI Chat Completions. |
| Native `octet-host` `media` | Same per-file limits; at most **8 images / 20 MiB total**, **4 audio clips / 40 MiB total**, and **12 items per request**. [Run request contract](sdk.md#run-requests). |
| Serve web composer | PNG/JPEG/GIF/WebP and bounded document context. Audio attachments are **not implemented**. [Serve](experimental/octet-serve/README.md). |

Attachments remain ordered with text. Unsupported modalities/formats, unreadable
files, and oversized files fail diagnostically. File recognition does not establish
provider support: FLAC/Opus/AAC and host-recognized PCM16 are not native inputs
merely because their extensions are recognized. Native audio requires both model
capability and the OpenAI Chat WAV/MP3 codec. Responses, Anthropic, Gemini, and
arbitrary OpenAI-compatible endpoints must not be advertised as native-audio
routes on recognition alone. There is no automatic transcription or transcoding
fallback; even admitted file contents may be rejected by the provider.

## Privacy and remote reads

Only submit media the selected provider may receive. Original bytes become typed
model/session input. Payload-free transcript summaries and NDJSON events do not
make stored sessions or exports safe to publish. Review/redact media, paths,
sessions, recordings, and captures separately; [export redaction](sessions.md#portable-export-and-redaction)
is not a proof that arbitrary content is secret-free.

Remote HTTPS image/audio `read` is default-off. Explicitly enable it with
`--allow-remote-read`, `allow_remote_read = true`, or
`OCTET_ALLOW_REMOTE_READ=true`. `--offline` disables remote reads and optional
discovery, **not inference network access**. Use OS-level network restrictions
when actual isolation is required.
