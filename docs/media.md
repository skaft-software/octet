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

This is an input recipe, not a recorded/live-provider demonstration. The
[current candidate record](qualification/core-journeys-df5a7e80.md) separately
reports deterministic process-to-provider media transport checks and their limits.
Ordinary
typed paths—including raw-key terminal drops—remain text, not upload consent.
A failed attachment leaves the path visible and reports a diagnostic rather
than creating a chip. An enabled `read` tool can still read a named file under
its policy; use `--no-tools` for explicitly attached input only.

To attach several files from one explicit paste/drop, separate the paths with
whitespace; quote or backslash-escape names containing spaces. The complete
path list is admitted in order, and each attachable item receives its own chip
(non-media source paths remain text):

```text
/Users/me/first.png '/Users/me/voice memo.wav' /Users/me/last.jpg
```

Path-list admission is all-or-nothing: a malformed, missing, or non-local token
leaves the complete payload as visible text rather than creating partial chips.
Consecutive explicit drops also receive distinct chips. Editing or deleting a
chip revokes only that attachment; the remaining chips retain their original
payload identity and order.

## Formats and limits

| Surface | Supported input |
| --- | --- |
| TUI attachment or built-in `read` | PNG/JPEG/GIF/WebP images, **5 MiB each**; native WAV/MP3 audio, **20 MiB each**, only with a compatible model on OpenAI Chat Completions. |
| Native `octet-host` `media` | Same per-file limits; at most **8 images / 20 MiB total**, **4 audio clips / 40 MiB total**, and **12 items per request**. [Run request contract](sdk.md#run-requests). |
| Serve web composer | PNG/JPEG/GIF/WebP and bounded document context. Audio attachments are **not implemented**. [Serve](experimental/octet-serve/README.md). |

Attachments remain ordered with text. Each user submission admits at most
**8 images / 20 MiB of inline image bytes** before decoding; this does not
limit independent `read` tool results. Explicit per-model
`preset.image_input_limits`
(`max_width`, `max_height`, `max_bytes`) are applied to inline user images;
models without that declaration use a host safety fallback of **4000×4000 px**
(maximum **16 million decoded pixels**) and **5 MiB encoded**. The fallback is
not a claim of provider acceptance. An image over the applicable bound is
resized within bounded decode, encode and output limits; the resized bytes
(with a matching PNG media type) are retained in history so later turns replay
the same image and preserve prompt-cache prefixes. Images already within bounds
retain their original bytes. A malformed image, decompression bomb, or image
that still cannot fit is rejected before the user turn is committed; there is
no provider-only resize or silent image drop. The existing **5 MiB input cap**
still applies before model preparation. A resized animated image may be
flattened to its first frame. Unsupported modalities/formats, unreadable files,
and oversized files fail diagnostically. Video paths are never native
media: an explicit video attachment is refused with a diagnostic and remains
visible as text. File recognition does not establish provider support:
FLAC/Opus/AAC and host-recognized PCM16 are not native inputs merely because
their extensions are recognized. Native audio requires both model capability and
the OpenAI Chat WAV/MP3 codec. Responses, Anthropic, Gemini, and arbitrary
OpenAI-compatible endpoints must not be advertised as native-audio routes on
recognition alone. There is no automatic transcription or transcoding fallback;
even admitted file contents may be rejected by the provider.

Inline tool-result images are visual-only TUI previews, not additional model
input. On Kitty-compatible terminals they reserve at most 16 rows per image.
When the terminal supplies no cell-pixel measurement, the preview uses an
approximate 1:2 cell aspect instead of shrinking a screenshot to one cell;
fonts with unusual cell proportions may display a slightly different aspect.
Other terminals retain a text fallback.

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
