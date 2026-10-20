# Images, audio and PDFs

[Documentation](README.md) · [Providers](providers.md) · [Security](../SECURITY.md)

## Attach explicitly

To ask about a sound and a screenshot:

1. Pick a model whose route supports the media.
2. Paste or drop each file path through the terminal, or select an `@` path
   completion.
3. Check for `[Audio #N]`, `[Image #N]` and `[PDF #N]` chips before you send.

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
| TUI attachment or built-in `read` | PNG/JPEG/GIF/WebP images, **5 MiB each**; native audio, **20 MiB each**, with a compatible model: WAV/MP3 on OpenAI Chat Completions; WAV/MP3/AAC/FLAC/Ogg Opus on Google `generateContent`. |
| Native `octet-host` `media` | Same per-file limits; at most **8 images / 20 MiB total**, **4 audio clips / 40 MiB total**, and **12 items per request**. [Run request contract](sdk.md#run-requests). |

Attachments stay in order with the text. Each submission admits at most **8
images / 20 MiB of inline image bytes** before decoding. That doesn't limit
separate `read` tool results. Unsupported formats, unreadable files and
oversized files fail with a message. Video paths are never native media: an
explicit video attachment is refused with a diagnostic and stays visible as
text. Recognizing a file isn't the same as the provider accepting it. Native
audio needs both a capable model and a supported format on its selected route.
Gemini, Vertex AI and Google-compatible OpenCode routes send inline audio bytes
as `inlineData`; OpenRouter keeps its Chat WAV/MP3 format contract. Responses
and Anthropic aren't native-audio routes, and an arbitrary OpenAI-compatible
endpoint does not establish audio support. Raw PCM16 is not supported on Google
because the canonical format lacks required sample-rate metadata. Google audio
output and provider file references are not implemented. There's no automatic
transcription or transcoding, and a provider may still reject a file octet
accepted. The raw-byte safety cap is not a guarantee that a base64-encoded
request fits the provider's total request limit.

### PDFs

A dropped or `@`-selected `.pdf` becomes a `[PDF #N]` chip. On a model that
reads PDFs the file's bytes go with the prompt as a native document, next to a
`[attached PDF: <path>]` line that names it:

| Route | Wire form |
| --- | --- |
| Anthropic Messages | `document` block (base64) |
| OpenAI Responses | `input_file` with a `data:` URL |
| OpenAI Chat Completions | `file` part with a `data:` URL |
| Gemini API, Vertex AI | `inlineData` (`application/pdf`) |
| Bedrock Converse | `document` (`format: pdf`) |

A model reads PDFs when its pinned models.dev record lists `pdf` input (Claude,
Grok, many newer GPT models, and many OpenCode and OpenRouter models), or, without a
record, when it is a `gemini-*` model on the Gemini API or Vertex AI, or Claude
on Bedrock. The model picker shows a `pdf` badge. The cap is **10 MiB of raw
PDF bytes**, which base64 grows to about 13.3 MiB, and the file must start like a
PDF (a `%PDF-` header). Providers bill each page as text plus a page image;
octet budgets about 3,000 tokens a page.

On any other model, including Mistral and Pi-messages routes, the chip stands
for the file's path instead, so the model is told which file you attached and
can open it with its own tools. The built-in `read` tool does not decode PDFs: it
says so and suggests attaching the file on a PDF-capable model or extracting the
text with a shell tool such as `pdftotext`. Switching to a model that cannot
read PDFs replaces earlier PDFs in the conversation with a placeholder naming
the file. Tools cannot return documents.

### Oversized audio

The **20 MiB audio cap counts raw file bytes** and is an octet safety bound, not
Gemini's provider allowance. Gemini documents **20 MB for the entire inline
request**, including base64 expansion and the rest of the prompt. A file below
octet's cap may therefore still exceed the provider's limit.

High sample-rate/bit-depth WAVs can be much larger than compressed audio. A
73,608,392-byte WAV and a 22,542,626-byte WAV both exceed the same 20 MiB audio
cap; lowering the sample rate or bit depth alone does not guarantee a fit.
Export a compressed copy or split the recording into smaller clips. For example,
with a separately installed FFmpeg (this keeps the original and refuses to
overwrite an existing output):

```sh
ffmpeg -n -i input.wav -vn -c:a libmp3lame -b:a 192k input-inline.mp3
```

Check the output size and use a model/route that supports its format. Leave
room for base64 and the rest of the request. octet does not automatically
transcode or upload oversized files; large-file API support is tracked in
[#593](https://github.com/skaft-software/octet/issues/593).

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
