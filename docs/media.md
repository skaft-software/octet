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
output is not implemented. Gemini Files and Vertex GCS audio references use
`fileData`; other Google-compatible routes retain their inline contract. There's no automatic
transcription or transcoding, and a provider may still reject a file octet
accepted. The raw-byte safety cap is not a guarantee that a base64-encoded
request fits the provider's total request limit.

### Input and capability paths

- TUI paste, drop and `@` completion attach recognized local audio as typed media;
  typing a path alone does not attach it. `read` can return structured audio from
  local files or explicitly enabled HTTPS reads. A tool result remains media, not
  a transcript, and is replayed with the conversation. If the selected route
  cannot accept the clip, admission and codec diagnostics name the provider and
  model. Switching to an incompatible route keeps the saved media and uses a
  route-named placeholder (or transcript) in that request; switch back to replay
  the original clip.
- The native host accepts ordered typed `media` paths; its legacy `image_paths`
  field is image-only. The separate line-oriented RPC prompt `images` field is
  also image-only; it is not a native audio upload interface. A prompt can name a
  file for `read` instead.
- Static provider presets, discovered model inventories, explicit custom-model
  declarations and pinned models.dev records can supply modality information.
  Explicit audio assertions take precedence over provider-scoped models.dev
  metadata; only silence allows a catalog fallback. Catalog entries retain the
  decision's source: declaration, catalog, absent evidence or unsupported protocol.
  Custom Chat models can set `preset.audio_input: true` (or `false` to deny);
  normalized caches preserve the assertion. Resolved `preset.audio_input_source`
  records capability provenance even when no request overrides are configured;
  it adds no headers, sampling defaults or reasoning controls to provider requests.
  The selected protocol further limits the audio formats actually available. A
  model listing audio by itself does not make an unsupported wire format available.
- Google `generateContent` encodes supported audio and PDF input as base64
  `inlineData` with its MIME type. OpenAI Chat encodes WAV and MP3 as base64
  `input_audio`. OpenAI Responses does not implement native audio input here;
  it does support PDF `input_file`. These are octet's implemented wire paths,
  not a guarantee that every model or compatible endpoint accepts them.

### Durable recorded media

Admitted image, audio and PDF bytes are stored in the session JSONL, not as paths
that must be re-read. Replacing or deleting the original file does not change a
later turn, branch or resumed session. Terminal summaries/previews never trim
model-visible audio. Model switching derives a compatible request without
rewriting that durable input.

Clones share immutable-byte base64 preparation; each clip/image/PDF is encoded
once per process unless its bytes change. Resume reconstructs preparation from
the stored bytes once. Constructing the final HTTP body still copies the bytes
that must go over the wire. Preflight bounds encoded media at **64 MiB**, and a
bounded JSON writer includes escaped text and schemas in the **64 MiB** request
ceiling (**20 MB** for Google). Gemini Files preparation admits at most **256 MiB**
of estimated encoded source media before replacing audio with references. A
limit error names the provider/model and suggests smaller batches; nothing is
silently truncated. Payload hooks are also subject to the final body ceiling.

Gemini upload reuse is process-local, tied to the actual credential and endpoint,
and refreshed after 15 minutes. A resumed session uploads from its durable bytes
again when necessary. Files have Google's documented 48-hour remote retention;
octet does not silently delete provider files or store credential material in the
session. Upload and file-state failures preserve local history and report the
preparation stage. Review [Gemini Files](https://ai.google.dev/gemini-api/docs/files)
and [Vertex audio fileData](https://cloud.google.com/vertex-ai/generative-ai/docs/multimodal/audio-understanding)
before authorizing provider checks.

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

Earlier audio reads remain in history and are re-sent: an album can exceed the
request-byte limit even when every track fits individually and token context
remains. On the built-in Gemini Developer API route, an estimated 16 MB of
encoded media triggers the documented resumable Files API before constructing
`generateContent`. Audio uploads finalize, wait for `ACTIVE` within a bounded
60-second preparation deadline, and become `fileData` parts in their original
positions. Vertex can use explicitly supplied `gs://bucket/object` audio
references; octet does not provision GCS buckets or upload to Vertex/OpenCode on
the authority of a Gemini API credential. OpenRouter remains inline-only. On HTTP **413**, octet reports the provider/model, status and available
request ID, with advice to start a fresh session with fewer or smaller
attachments. A larger token context window may not raise the request-byte limit.
A generic `request_too_large` rejection does **not** trigger token compaction or
an automatic retry; explicit token/context-overflow errors retain their existing
compaction behavior. The failed request leaves native media in session history,
including after reopening. octet does not silently remove tracks, replace them
with summaries, switch providers or transcode them to fit.

[OpenRouter audio](https://openrouter.ai/docs/guides/overview/multimodal/audio)
requires inline base64, not audio URLs; its workspace Files API is not a Gemini
audio-upload transport. Use fresh-session batches with explicit per-track notes
or deliberately compressed copies. The provider's actual encoded-request limit
is route-dependent; octet's per-file cap does not establish that limit.

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
transcode or upload files above the raw 20 MiB safety cap. Gemini Files avoids
the smaller whole-request inline ceiling for admitted clips, not this raw cap.

Context capacity checks are estimates, not provider tokenization. Inline media
you attached counts as a fixed allowance per image, audio clip or document page,
not as base64 text. This holds when an extension context hook is active and the
request replays a Responses window: only payloads identical to media in your
history get the allowance, and opaque provider content counts at full size.

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

## Recorded-audio qualification

Offline fixtures exercise every supported audio format through composer
attachment, built-in `read`, tool continuation, later turns and session resume,
checking exact bytes, MIME/format and ordering on both audio codecs. The same
fixtures take OpenRouter, OpenCode, Gemini and Vertex metadata through catalog
registration, the picker's effective capability projection, attachment admission
and actual loopback HTTP requests. Fresh and raw-cache replay inventories include
new IDs absent from the pinned models.dev snapshot; explicit text-only, false or
malformed assertions stay authoritative over catalog fallback. Custom normalized
cache tests retain configured positive and negative audio assertions. This proves
local wire construction, not provider acceptance or recognition.

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
