# Recorded-audio qualification checklist

Scope: recorded audio input (#626), not realtime (#471/#472), audio output,
transcription services or general model quality. [Media contract](../media.md).
All provider checks below are **NOT RUN** in the offline implementation pass.
This checklist is not authorization to access credentials, upload recordings or
spend money. Obtain separate approval for each provider/account/model and budget.

## Evidence levels

Keep separate results for each exact provider, endpoint, model, format and input
path. A picker badge proves metadata admission, not a working provider route.

1. **Offline construction:** local fixtures preserve bytes, MIME/format and order
   from attachment and built-in `read` through continuation and session resume.
   Fresh/cached inventories and models absent from the pinned snapshot use the
   same effective capability projection. Unsupported routes name provider/model.
2. **Transport acceptance:** an authorized provider completes the request with
   the audio part present. Record status, sanitized request ID, codec, input byte
   count/hash, format and whether inline or fileData was used. A successful POST,
   upload, `ACTIVE` file, or text-only answer does not prove recognition.
3. **Recognition:** an answer identifies information available only in the sound
   against a pre-recorded answer key. Record the scoring rule, observations and
   uncertainty separately from transport. Correct filename guesses do not count.

Report **PASS**, **FAIL** or **UNTESTED** independently at levels 2 and 3. Missing
credentials, account access, regional access or an unavailable model mean
**UNTESTED**, not success and not evidence that every route on the codec fails.
Offline dummy codec bytes are not live recognition fixtures.

## Route worksheet

| Route | Recorded input to check | Transport | Recognition |
| --- | --- | --- | --- |
| OpenAI Chat, explicitly audio-capable model | WAV, MP3 inline | UNTESTED | UNTESTED |
| OpenRouter Chat, exact upstream model | WAV, MP3 inline | UNTESTED | UNTESTED |
| Custom Chat endpoint with explicit audio assertion | WAV, MP3 inline | UNTESTED | UNTESTED |
| Gemini Developer API | WAV, MP3, AAC, FLAC, Ogg Opus inline | UNTESTED | UNTESTED |
| Gemini Developer API Files | Same formats, resumable upload + fileData | UNTESTED | UNTESTED |
| Vertex AI, authorized project/location/model | Same formats inline; explicit GCS fileData | UNTESTED | UNTESTED |
| OpenCode Go `gemini-4-argon` (0.10.5) | Recorded audio attachment | PASS | PASS |
| OpenCode Google-compatible route, exact model | Same formats inline | UNTESTED | UNTESTED |
| Responses/Codex, Anthropic, Bedrock, Mistral Conversations, Pi Messages, extension stream | Native audio is not implemented; verify local route-named continuation/refusal only | UNTESTED (no native audio route) | UNTESTED |

Each accessible model gets its own row; never copy one model's result to its
provider, a subscription route, another account, or a compatible gateway.

## Separately authorized live procedure

- [ ] Record revision, account permission scope (no identifiers/secrets), exact
  provider/model/codec, region when relevant, inventory/cache state and budget.
- [ ] Prepare owner-approved, non-sensitive recordings with independent ground
  truth: a spoken random phrase and a non-speech sound with an observable sequence
  or rhythm. Use neutral filenames, no transcript/answer in metadata or prompt,
  and verified container/codec/MIME. Check playback locally before upload.
- [ ] Run attachment-only (`--no-tools`) and then a separate built-in `read`
  request for each supported format. Preserve two distinct clips in order with
  text between them. Confirm the provider received both original byte hashes and
  the correct MIME/format without publishing payloads or secrets.
- [ ] Score content-only questions against the hidden answer key. Use a second,
  clearly different approved recording and a no-audio control to distinguish
  actual recognition from boilerplate or filename/prompt inference. Mark an
  ambiguous answer inconclusive; do not promote acceptance to recognition.
- [ ] Ask a follow-up without reattaching. Replace/remove the original local
  files, reopen the session and ask another content-only question. Confirm the
  same durable bytes are used; terminal preview size must not affect input.
- [ ] Switch temporarily to a route without native audio, verify the named
  placeholder/transcript keeps the session usable, then switch back and verify
  original audio returns. Do not claim native recognition on the fallback route.
- [ ] With explicit upload permission, exercise Gemini's resumable Files path
  using admitted clips whose aggregate encoded size triggers file preparation.
  Check finalize/state polling, order, credential isolation, reuse and resume
  from local bytes. Record remote retention and authorized cleanup separately.
- [ ] For Vertex GCS, use a separately authorized immutable object/bucket and
  document its access policy. Do not treat Gemini Files as Vertex upload support.
- [ ] Under a separately bounded request budget, check oversize/error handling:
  local 20 MiB raw cap, whole-request limits, file preparation failure and HTTP
  413. Confirm provider/model and recovery advice, retained history, no silent
  media loss, no automatic transcription/transcoding/provider switch.
- [ ] Publish only redacted result metadata and scores. Keep raw sessions,
  recordings, request traces and credentials private. Record every inaccessible
  route as UNTESTED and every check not executed as NOT RUN.

Provider contracts to recheck before live authorization:
[Gemini audio](https://ai.google.dev/gemini-api/docs/audio),
[Gemini Files](https://ai.google.dev/gemini-api/docs/files),
[Vertex audio](https://cloud.google.com/vertex-ai/generative-ai/docs/multimodal/audio-understanding),
[OpenRouter audio](https://openrouter.ai/docs/guides/overview/multimodal/audio).
