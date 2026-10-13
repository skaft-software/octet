# Images and audio

[Documentation](README.md) · [Providers](providers.md) · [Security](../SECURITY.md)

## Attach explicitly

To ask about a sound and a screenshot:

1. Pick a model whose route supports the media.
2. Paste or drop each file path through the terminal, or select an `@` path
   completion.
3. Check for `[Audio #N]` and `[Image #N]` chips before you send.

```text
Describe the sound in the audio. Compare the screenshot with this repository's
player UI, then propose a change. Do not edit yet.
```

A typed path is just text, even a raw-key terminal drop. A failed attachment
leaves the path visible and reports why, with no chip. An enabled `read` tool
can still read a file you name, under its policy. Use `--no-tools` for attached
input only.

To attach several files from one paste or drop, separate the paths with
whitespace, and quote or backslash-escape names that contain spaces. octet
admits the whole list in order and gives each attachable item its own chip
(paths that aren't media stay text):

```text
/Users/me/first.png '/Users/me/voice memo.wav' /Users/me/last.jpg
```

It's all or nothing: a malformed, missing or non-local token leaves the whole
payload as visible text, with no partial chips. Consecutive explicit drops get
separate chips. Editing or deleting a chip revokes only that attachment, and the
rest keep their order and payload.

## Formats and limits

| Where | What it accepts |
| --- | --- |
| TUI attachment or built-in `read` | PNG/JPEG/GIF/WebP images, **5 MiB each**; native WAV/MP3 audio, **20 MiB each**, only with a compatible model on OpenAI Chat Completions. |
| Native `octet-host` `media` | Same per-file limits; at most **8 images / 20 MiB total**, **4 audio clips / 40 MiB total**, and **12 items per request**. [Run request contract](sdk.md#run-requests). |

Attachments stay in order with the text. Each submission admits at most **8
images / 20 MiB of inline image bytes** before decoding. That doesn't limit
separate `read` tool results. Unsupported formats, unreadable files and
oversized files fail with a message. Video paths are never native media: an
explicit video attachment is refused with a diagnostic and stays visible as
text. Recognizing a file isn't the same as the provider accepting it: FLAC,
Opus, AAC and PCM16 aren't native inputs just because octet knows the extension.
Native audio needs both a capable model and the OpenAI Chat WAV or MP3 route.
Responses, Anthropic, Gemini and arbitrary OpenAI-compatible endpoints aren't
native-audio routes. There's no automatic transcription or transcoding, and a
provider may still reject a file octet accepted.

<details>
<summary>Image size limits and resizing</summary>

A model can declare `preset.image_input_limits` (`max_width`, `max_height`,
`max_bytes`), which apply to inline user images. Without that, octet uses a
safety fallback of **4000×4000 px** (at most **16 million decoded pixels**) and
**5 MiB encoded**. The fallback doesn't claim the provider will accept the
image.

An image over the limit is resized within bounded decode, encode and output
limits. The resized bytes (with a matching PNG media type) are kept in history,
so later turns replay the same image and prompt-cache prefixes survive. Images
within bounds keep their original bytes. A malformed image, a decompression
bomb, or an image that still can't fit is rejected before the turn is committed.
There's no provider-only resize and no silent drop. The **5 MiB input cap**
still applies before model preparation. A resized animated image may be
flattened to its first frame.

</details>

Inline tool-result images are visual-only previews in the TUI, not extra model
input. On Kitty-compatible terminals each takes at most 16 rows. If the terminal
doesn't report cell size in pixels, octet assumes a 1:2 cell aspect rather than
shrinking a screenshot to one cell, so fonts with unusual cell proportions may
look slightly off. Other terminals show text.

## Privacy and remote reads

Only send media your provider may receive. The original bytes become model and
session input. Transcript summaries and NDJSON events carry no payload, but that
doesn't make stored sessions or exports safe to publish. Review and redact
media, paths, sessions, recordings and captures yourself. [Export
redaction](sessions.md#portable-export-and-redaction) isn't proof that arbitrary
content has no secrets.

Remote HTTPS image and audio reads by `read` are off by default. Turn them on
with `--allow-remote-read`, `allow_remote_read = true` or
`OCTET_ALLOW_REMOTE_READ=true`. `--offline` turns off remote reads and optional
discovery, **not inference network access**. Use OS network limits when you need
real isolation.
