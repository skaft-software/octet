# Images, audio, video and PDFs

[Documentation](README.md) · [Providers](providers.md) · [Security](../SECURITY.md)

## Attach explicitly

To ask about a sound and a screenshot:

1. Pick a model whose route supports the media.
2. Paste or drop each file path through the terminal, or select an `@` path
   completion.
3. Check for `[Audio #N]`, `[Image #N]`, `[Video … #N]` and `[PDF #N]` chips before you send.

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
oversized files fail with a message. Local video attachments and `read` use the
[video contract](#video) below. Recognizing a file isn't the same as the provider accepting it. Native
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

Admitted image, audio, video and PDF bytes are stored in the session JSONL, not as paths
that must be re-read. Replacing or deleting the original file does not change a
later turn, branch or resumed session. Terminal summaries/previews never trim
model-visible audio. Model switching derives a compatible request without
rewriting that durable input.

Clones share immutable-byte base64 preparation; each audio clip/image/PDF is encoded
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

### Video

Drop/paste or `@`-select a local MP4/M4V, MOV, WebM/MKV, AVI, MPEG/MPG,
WMV, FLV, 3GP or OGV file, or ask the built-in `read` tool to open it.
The cap is **10 MiB of original bytes per video**. TUI/Rust-agent submissions
accept at most **two videos**, sharing the eight-image budget (each video reserves
four images) and **20 MiB** aggregate source-byte budget with images. The
native host's typed `media` paths and RPC `images` upload remain image/audio and
image-only interfaces respectively; name a video for `read` instead.
Remote video reads are not enabled.

On an **image-capable route**, including **openai-codex / Responses**, octet
samples **four frames** at the midpoints of four equal duration intervals
(12.5%, 37.5%, 62.5%, 87.5%). Each JPEG fits within **768×768 pixels**;
together they occupy at most **4 MiB**. A requested timestamp after the last video frame reuses that frame. Text immediately before each image gives
its requested timestamp in seconds from the start. The composer chip and read
result show **“video sent as 4 frames”**. These are sparse visual samples, not
continuous video understanding; **audio is not sent** on this fallback.

The [Responses API](https://platform.openai.com/docs/api-reference/responses/create)
accepts `input_text`, `input_image` and `input_file`, not a native video input
part. Image capability permits frame analysis, not sending an MP4 as an image
or inventing `input_video`. Catalog/provider image assertions remain the admission
contract; a models.dev `video` listing alone does not enable a wire codec.
`ModelSpec::supports_video_input()` reports native video or frame-capable image
input, while `effective_input_modalities()` includes `Video` only for native
Google video. Google `generateContent` routes with an explicit video capability,
or image-capable `gemini-*` models, keep the **original container** as native
`inlineData` with its video MIME type, including any audio. They do not run a
local decoder. Google's whole-request **20 MB** ceiling still applies; native
video Files uploads are not implemented.

Frame sampling needs **FFmpeg and FFprobe on PATH**. The existing `image` decoder
handles still images only; octet uses fixed, non-shell subprocess arguments rather
than adding a video codec dependency. Install a maintained FFmpeg distribution
from your OS package manager. Only whitelisted local containers/protocols are
opened; source frames are capped at **16 million pixels**, duration at **24 hours**,
decoder allocation size at **64 MiB**, and the complete preparation at **30 seconds**.
Decoder scratch is owner-only from creation (mode `0700` on Unix, protected
user-only ACL on Windows). It is removed after preparation; a process crash can
leave private scratch behind.
Preparation runs off the interactive driver, with at most two decoder workers per
process; queueing and decoding share the 30-second deadline. Input and interrupts
remain available while a cold video read is prepared. Cancellation and timeout
kill and reap the decoder. The decoder is not an OS sandbox.

The **original video bytes** stay in session JSONL, including tool reads; deleting
or replacing the source path does not affect later turns or resume. Sampled frames
are shared in memory across turns and rebuilt from those durable bytes after a
restart. Model-specific frame validation and resizing also run in cancellable async
preparation. Each video retains one derived variant, keyed by sampled-frame identity
and all image limits, with at most four images of 5 MiB each. Changed limits replace
that variant; warm projection and replay consume cached images without decoding.
Keep FFmpeg available for resumed frame analysis; changing decoder versions may
change JPEG bytes. Missing decoders or invalid clips produce an
actionable preparation error before a new sampled user attachment is committed. A saved
video whose frames cannot be prepared uses an explanatory placeholder so the
session can continue; a read result reports the problem and retains its source.
Completed foreground and background tool reads commit their original bytes
before optional frame preparation begins. Interrupting preparation or closing the
run without draining it preserves that completed result, including after restart.
A cold read records deferred frame delivery, not a preparation failure; the
provider-input projection reports the current frame outcome. Preparation failures
are cached for that source in the current process, so encoding and later turns do
not retry the decoder; read or attach the clip again to retry. Switching to a
text-only model also derives a placeholder without erasing video, including
in native Responses replay and compaction. New attachments still require a
compatible route.

Rust embedders call `video::prepare_message_frames(&model, &messages).await` before
synchronous lowering or encoding so sampled frames also fit the selected model's
image limits. `VideoMedia::prepare_frames().await` prepares the source samples only;
`sampled_frames()` and `cached_frames()` consume those local source samples,
including while a decoder is running. `AiClient` prepares cold request videos
asynchronously; the agent also prepares resumed session bytes from the active
canonical context or route-affine Responses replay window before projection. Videos on abandoned branches or before a local/native compaction
boundary stay cold. Incremental preparation inspects only the changed active
suffix (including coalesced tool results), and resets its cursor when checkout,
compaction, model selection or image-limit changes affect that window. An explicit
Responses input already owns the wire content, so the client neither decodes
nor counts the canonical media it replaces against the media byte limit. The
selected input still has to fit the final request byte ceiling.

For Pi compatibility, the reference defines only `ImageContent` for binary visual
input (`packages/ai/src/types.ts:438–442`), sniffs still images
(`packages/coding-agent/src/utils/mime.ts:7–24`) and returns image blocks from
`read` (`packages/coding-agent/src/core/tools/read.ts:128–149`); it has no video
attachment/frame decoder. Its Responses image mapping is
`packages/ai/src/api/openai-responses-shared.ts:93–109, 307–320`. octet's native
media layer owns durable video and projects ordinary image parts for that wire
contract; the optional Pi process is not required.

Offline tests use a tiny synthetic video to check frame timestamps, bounded
images, protocol bytes, tool-result placement and durable resume. This proves
local request construction, not live model recognition or provider acceptance.

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
`generateContent`. File preparation validates request settings, tool schemas and the
upload URL's proxy configuration before resolving credentials or making any Files
request. Unsupported or malformed proxies fail closed with an audio preparation
stage error; they never select a direct provider connection.
Audio uploads finalize, wait for `ACTIVE` within a bounded
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
local wire construction, not provider acceptance or recognition. See the
[separately authorized qualification checklist](maintainers/recorded-audio-qualification.md)
for per-route transport and recognition evidence; inaccessible routes stay untested.

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
