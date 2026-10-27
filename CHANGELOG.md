# Changelog

## [Unreleased]

## [0.10.0]

**The first Pi-compatible octet release.** Bring your Pi extensions, skills,
prompts, themes and custom UI to a native, open-source coding agent.
See the [release notes](docs/releases/v0.10.0.md) for setup and known boundaries.

### Added

- The release catalog includes `octet-pi-compat` among six optional bundles,
  all at 0.10.0 with exact `=0.10.0` host pins and API `0.4`. Installation and
  discovery remain inert; enabling the stock bridge imports/runs enabled Pi
  factories with OS permissions, without granting native credential disclosure.
- Native command catalogs and resource provenance stay bound to the current
  owner across reloads; metadata never grants execution authority.

### Security

- Update zstd from 0.13.3 to 0.14.3 (zstd-safe 8.2.0, zstd-sys 2.1.2) for
  RUSTSEC-2026-0391, RUSTSEC-2026-0398, RUSTSEC-2026-0403 and
  RUSTSEC-2026-0410. octet only compresses Codex request bodies with one-shot
  zstd calls and does not use the affected reused-stream, seekable,
  legacy-frame decompression or WebAssembly paths.

### Fixed

- Pi assistant messages from `ctx.sessionManager` carry their recorded token
  usage, cost and stop reason, so Pi footers and status lines such as the
  `custom-footer` example work once a session has a model reply. Unpriced and
  unrecorded turns keep those facts absent rather than invented.
- On Windows, OpenAI, Copilot and other sign-in flows, and transcript link
  clicks, open the default browser instead of File Explorer. URLs go to the
  registered protocol handler as one argument, so OAuth query strings survive.
- Pasting multi-line text in Windows terminals inserts it as one paste instead
  of submitting at the first line break. The Windows console input API never
  reports bracketed paste, so a queued run of text keys containing a line break
  is recombined; all other input still arrives as the original keys.
- Captured transcript links retain their click targets on terminals without
  OSC 8 support, including Apple Terminal, without emitting unsupported controls
  or changing the HTTP(S)-only activation and stale-geometry fences.
- Successful live `/settings tui` switches acknowledge their exact preference
  writes, avoiding redundant reloads that reset the transcript viewport. Active
  runs retain ordered receipts for idle ownership; failed switches and external
  edits still participate in normal reload.
- HTTP 413 includes recovery advice for accumulated native media. Generic
  `request_too_large` errors no longer trigger token compaction; failed requests
  retain native audio in history without silent removal or provider switching.
- Google streaming preserves signed assistant text/reasoning boundaries across tool
  calls and chunk-position changes, including signatures on empty final text
  chunks. Repeated and prefix-related text/thought deltas are no longer mistaken
  for cumulative snapshots, preserving the original signed text on tool-result
  replay through Gemini, Vertex and native Google-compatible gateways.
- OpenRouter `reasoning_details` are kept with each reply and returned unchanged
  on the next request to the same model, so Gemini 3-class and Anthropic models
  keep their signed reasoning across tool calls. As in OpenRouter's SDK, unsigned
  Anthropic/Gemini text entries are not returned, and replayed reasoning goes in
  `reasoning` instead of the visible assistant content.
- A provider stream may stay silent for 15 minutes after its first byte instead
  of 5. Gemini models never stream tool-call arguments, so a large file write was
  cut off while the model was still generating it. The 15-minute first-byte wait
  and one-hour response deadline are unchanged.
- Pi startup publishes real model/scoped-model facts before callbacks run.
- CamelCase extension tools such as `mcpScript` retain their exact spelling.
  Tool allowlists and exclusions are consistently case-sensitive, including
  builtin names (`read`, not `Read`).
- Theme preference commits acknowledge their exact applied write, avoiding the
  redundant resource reload that retired composed Pi editor/footer mounts;
  independent resource changes still reload and native TOML appearance remains.
- Pi startup callbacks can await late-tool catalog acknowledgements without a
  lifecycle-lock cycle; generation, reload and failed-ACK fences stay intact.
- Live admitted commands read the actual native system prompt instead of a
  missing cached snapshot. Retained/headless prompt access remains unqualified.
- App-deferred headless startup receives credential-filtered model/scoped facts
  and refreshed mirrored defaults; immediate pre-App/initialize-time starts
  remain outside this repair, and unavailable facts still refuse explicitly.
- Legacy raw `0x1f` matches Pi Ctrl+- undo; raw bytes are retained and explicit
  Kitty Ctrl+7 stays distinct. Legacy bindings relying on that ambiguous byte
  as Ctrl+7 change meaning. Original-editor guest replay remains open.

### Fixed

- Managed extension updates (`octet extension update`) work on
  Windows instead of failing with "atomic directory exchange is unavailable on
  this platform". Validated candidates use locked, recoverable replacement;
  failed publication restores the previous bundle, or retains its backup with
  recovery instructions if Windows blocks restoration. Listing and global managed
  discovery recover interrupted updates. Linux/macOS keep atomic exchange.

### Changed

- Startup reuses the static CLI parser, and endpoint matching parses the selected
  URL once before comparing builtin provider declarations.
- Full session listings load cached catalog summaries in one query rather than
  one per session. Latest-only lookup and transcript fingerprint checks remain.
- Settling a disposable partial-assistant journal no longer syncs the file just
  before reading and deleting it. Authoritative session appends retain their
  existing durability; Linux executable ASLR is unchanged.

### Known limitations

- Individual API gaps are recorded in the [API ledger](docs/pi-extension-api.md).
  Compatibility targets Pi 1.0.2's public extension API, not its private internals
  or complete CLI. Windows remains preview; Bun is unqualified.
- Live provider/audio and model-quality acceptance are not part of this launch.
  The versioned release page records actual distribution-channel availability.
- Product/installer and Python/TypeScript source identities target 0.10.0;
  independent Rust SDK 0.8.2, Pi 1.0.2, private adapter npm 0.1.0, API/schema and
  example fixture versions stay unchanged.

## [0.9.6] - Unreleased

The [release notes](docs/releases/v0.9.6.md) record shipped scope, qualification
boundaries and known limitations. Benchmark-driven work from the Buffon harness
lets workloads that could not run against octet do so, and speeds up the
slowest measured paths.

### Added

- PDF attachments reach models that read PDFs as documents. A dropped or
  `@`-selected PDF is sent as an Anthropic `document`, OpenAI Responses
  `input_file`, Chat Completions `file`, Gemini `inlineData` or Bedrock
  `document` on models whose pinned record lists PDF input (Claude, Grok, newer
  GPT models, many OpenCode and OpenRouter models) and on Gemini and Bedrock
  Claude. Up to 10 MiB per PDF; the picker shows a `pdf` badge. Other models
  keep receiving the file's path, `read` explains that it does not decode PDFs
  instead of returning compressed bytes as text, and switching to a model
  without PDF input replaces earlier PDFs with a placeholder naming the file.
- Custom registry providers can set `"api": "anthropic-messages"` or
  `"api": "openai-responses"` to reach an Anthropic- or Responses-compatible
  gateway (or a local benchmark mock) at their `base_url`; Chat Completions
  remains the default. Anthropic keys are sent as `x-api-key`. These providers
  use their declared models; `GET /models` discovery stays Chat-only.
- `--session FILE.jsonl` addresses a session by its file: a missing file is
  created there and later runs resume it. A Pi v3 session file is imported as a
  new session in this workspace on each launch and that copy is resumed; the Pi
  file is never modified. Only missing parent directories are created.
- RPC `tool_execution_end` results and their `toolResult` messages carry Pi's
  `details` (a Pi-compatible tool's own details, otherwise an extension tool's
  metadata), `structuredContent`, and media parts, instead of text only.

### Fixed

- `octet-web-search` starts on Windows. Its manifest forwarded no environment,
  and Windows Python finds the home directory through `USERPROFILE`, so the
  extension failed before initializing; saving its settings or Brave key also
  failed there (no `os.fchmod`, POSIX permission bits). It now declares
  `USERPROFILE`, `APPDATA` and `LOCALAPPDATA` and skips POSIX-only checks on
  Windows.
- `/extensions status` and other one-shot summaries (`/pi status`, `/prompt`,
  `/update`, extension output) no longer flash and vanish: the release of the
  Enter that opened them, and terminal focus changes, closed them.
- Windows workspaces are shown as `C:\project`, not `\\?\C:\project`, in the
  footer, `/status`, session details, `octet doctor`, prompt templates and the
  workspace line of the system prompt.

- A dropped PDF reached the model as the bare label `[PDF #1]`. Prompt
  composition rebuilt the model message from the transcript text, which keeps
  the chip, and discarded the text that carried the PDF's path, so the model was
  never told which file was attached. `/answer` likewise sent its `/answer`
  display text instead of its answer-now directive.
- Alacritty, WezTerm, Tern and other terminals hosting octet on Windows got the
  plain-console profile: ASCII bullets, rules and tree lines, 256 colours and
  no italics or links. A Windows console now gets the rich profile when its
  terminal announces itself with `COLORTERM` or `TERM_PROGRAM`, as Windows
  Terminal does with `WT_SESSION`; a bare conhost window keeps the conservative
  profile.
- Imported Pi sessions, extension-created sessions and sessions with
  cache-warming records were listed as "(unreadable session)" in the resume
  picker and `octet sessions list`, without a title, and were re-read on every
  listing because unreadable summaries are never cached. Their first listing
  now reads them once; later listings come from the catalog.
- Finished subagents no longer block `/reload`. A worker that completed, reached
  its turn limit, failed, timed out or was interrupted only waits for a
  follow-up, so a reload (including a host binary reload) parks it with its
  result and session kept, and `subagent_continue` resumes it afterwards. A
  turn-limited or detached worker used to block every reload until the session
  was exited and resumed.
- `/reload --force` now forces. A worker that is still running or has queued
  work blocks plain and automatic reloads; `--force` detaches it through its
  cancellation path, keeps its durable session and queued work, resumes
  undelivered tasks on the next turn and never replays delivered work. It used
  to refuse exactly like plain `/reload`. If the forced reload does not replace
  the application, the parked workers are reattached immediately.
- OpenCode Go and OpenCode Zen models without a hand-written preset offer their
  thinking levels instead of only `off`. OpenCode's `/models` list carries IDs
  only, so thinking now comes from each model's pinned models.dev record on its
  own route (Gemini thinking levels, Claude adaptive thinking, Qwen budgets,
  Chat and Responses effort). The 0.9.4 fix covered Gemini Argon alone.
- OpenCode presets keep every input the pinned record documents: image input,
  and audio where the route can send it, are no longer dropped by a stale
  hand-written flag.
- Native Tern rendering works on Windows. Tern answers there with OSC 877
  framing, which octet did not recognize: the hello timed out, rendering fell
  back to the terminal, and the reply appeared in the composer as
  `]877;tsp;r;{"r":"hello",…}\`.
- Gemini routes (Gemini API, Vertex AI, OpenCode Go and OpenCode Zen, including
  Gemini 4 Argon Contributor) no longer fail a request with `Media in tool
  results is unsupported` when the conversation holds an image or audio clip
  inside a tool result, for example a `read` made earlier on a Claude or GPT
  model. The media is sent as inline data after the turn's function responses.
- Transcript images no longer spill over the composer and status bar in
  Kitty-protocol terminals (Ghostty, Kitty, WezTerm). An image cut by the bottom
  of the transcript view, for example while scrolled back, was still placed at
  full height, and the terminal draws placements over text. A cut image is now
  placed only for its visible rows, and one whose top has scrolled above the
  view keeps showing its visible part instead of disappearing.
- Native Tern no longer drops back to the ANSI renderer when the terminal
  reports one error, and waits longer for a slow one. A terminal `error` event
  reopens the surface and replays the retained tree; only a terminal that
  rejects more than three replays within a minute still yields to ANSI, with the
  existing notice. A visible surface left unacknowledged for five seconds (two
  before) pauses and reconnects on a fresh hello, as a detached window does.

### Changed

- The native Tern composer is Tern's editor card, the container omp uses. The
  editor, model and effort controls, working directory and Send share one inset,
  and the context meter runs along the card's top edge; they sat on four
  different distances from the pane edge, with uneven gaps between them. The
  card is present from the first startup frame and shows a pending tone while a
  turn runs. Mirrored Pi themes keep Pi's flat composer.
- The welcome row groups its items (identity, release, access, tips) with a wider
  gap between groups, wraps whole groups on narrow panes and no longer repeats
  the `/ commands · @ files · ! shell` hints that the composer placeholder
  shows. Panes under 56 columns use a short placeholder and show the hints in
  the welcome row instead, so the placeholder never wraps and resizes the
  composer.
- Model picker rows show prices as `$in·out`; the price column truncated
  `$0.35 · $0.75` to `$0.35 · $0…`. The preview pane keeps the readable form.
- Fullscreen TUI streaming re-aligned the copy/selection rows of every visible
  block from its first row on every frame, so a long answer cost more per token
  as it grew. Alignments are now kept between frames and only rows whose painted
  text or copy text changed are redone. Extension theme sync rebuilt the whole
  theme palette on every UI refresh; it now does so only when the theme changes.
  Together these cut renderer instructions while streaming a long answer by
  about a third, and CPU per streamed token by about a fifth.
- RPC `get_state` counts history messages without projecting the whole history
  to JSON, and the per-turn context breakdown serializes the history once instead
  of twice. A resumed 1,000-turn session is ready over RPC about a third sooner
  and answers its first turn about a quarter faster.
- `--no-session` print and RPC runs no longer make a workspace marker or session
  catalog durable inside their throwaway store, removing several fsyncs from
  startup and exit.
- Pi session import decodes records straight from the parsed JSON and validates
  the converted transcript once, cutting a 1,000-turn import by about a third.
- Rust minor and patch dependency updates, including Tokio 1.53.3, whose
  deprecated `AsyncFd::with_interest` is replaced by `register_with_interest`;
  pinned GitHub Actions updates.

### Known limitations

- Pasting on Windows still arrives key by key: each newline in a paste submits
  the prompt and a dropped file stays typed text. Paste one line at a time, or
  attach files with an `@` mention.
- `octet-codemode` has no Windows build; on Windows its POSIX launcher is now
  refused with "this extension has no Windows build" rather than "not a valid
  Win32 application".
- Pi compatibility remains experimental and unstable; the `octet-pi-compat`
  adapter does not ship (#587).
- Windows remains a preview platform (#572, #588).
- Closing and reopening a real shared Tern window, and transcript image
  cropping in a real Ghostty or Kitty window, are attended RC checks.
- The 0.9.5 and earlier known limitations below still apply.

## [0.9.5] - 2026-10-18

### Changed

- The native Tern composer is Tern's editor card, the container omp uses. The
  editor, model and effort controls, working directory and Send share one inset,
  and the context meter runs along the card's top edge; they sat on four
  different distances from the pane edge, with uneven gaps between them. The
  card is present from the first startup frame and shows a pending tone while a
  turn runs. Mirrored Pi themes keep Pi's flat composer.
- The welcome row groups its items (identity, release, access, tips) with a wider
  gap between groups, wraps whole groups on narrow panes and no longer repeats
  the `/ commands · @ files · ! shell` hints that the composer placeholder
  shows. Panes under 56 columns use a short placeholder and show the hints in
  the welcome row instead, so the placeholder never wraps and resizes the
  composer.
- Model picker rows show prices as `$in·out`; the price column truncated
  `$0.35 · $0.75` to `$0.35 · $0…`. The preview pane keeps the readable form.

### Fixed

- Native Tern no longer drops back to the ANSI renderer for the rest of the
  session when the terminal is merely slow or reports one error. A visible
  surface left unacknowledged for five seconds has the missing acknowledgements
  forgiven and keeps presenting, and a terminal `error` event reopens the surface
  and replays the retained tree. Both ended native rendering after two seconds
  or on the first event. Only a terminal that rejects more than three replays
  within a minute still yields to ANSI, with the existing notice.

## [0.9.5] - 2026-10-16

The [release notes](docs/releases/v0.9.5.md) record shipped scope, qualification
boundaries and known limitations.

### Fixed

- Parallel Google function calls delivered in separate stream chunks stay
  distinct, including Gemini 4 Argon on OpenCode Zen and Argon Contributor on
  OpenCode Go. Calls keep their own arguments, thought signatures and paired
  results; cumulative updates are matched by call ID, not chunk-local position.
- Audio-capable Gemini, Vertex AI and Google-compatible OpenCode models retain
  audio in discovery, attachments and `read` results, and send inline
  WAV/MP3/AAC/FLAC/Ogg Opus bytes through native Google `generateContent`.
  Google audio output, raw PCM16 and provider file references remain unsupported;
  inline media safety limits are unchanged.
- Oversized WAV/audio reads report the 20 MiB raw-audio cap and suggest
  compression or splitting, instead of a misleading 32 MiB text-file limit.
  Inline safety limits and original media bytes are unchanged.
- `octet update` on Windows recognizes an `install-octet.ps1` installation and
  updates it by re-running the version-pinned PowerShell installer. It reported
  that it could not detect the install method and suggested the Unix `curl | sh`
  installer. An update still succeeds while an older session runs from
  `octet.exe.old`.
- Same-mode idle thinking changes preserve the active session and executable
  extensions instead of rebuilding them. Verified, already-applied thinking
  preference writes no longer trigger a second resource reload; external edits
  and extension configuration notifications retain their normal handling.
- Enter during a thinking-change wait retains one complete prompt for exactly-once
  idle submission under its intended session and thinking selection. Cancellation
  or a changed selection explicitly restores the draft, including attachments.
- Native Tern keeps its default touchpad scrolling and selection: octet no longer
  captures the mouse for the native surface. ANSI fallback keeps the requested
  capture policy.
- Native Tern reconnects after its UI stops acknowledging frames, including
  last-window detach: presentation pauses until a fresh hello, then replays
  the current transcript and draft with native input focus.
- Captured-mouse transcript web links open on a plain left-click release, without
  Shift or Ctrl. Dragging still selects/copies; movement, intervening input and
  changed layout revoke link activation. Only HTTP(S) targets reach the browser
  helper; native Tern and uncaptured terminal link gestures are unchanged.
- `/context` during a run shows the same usage grid and categories as when idle,
  instead of a flat list.
- Tool input previews wrap to six visible rows instead of clipping a generic
  tool's selected argument at 72 characters. Bash and PowerShell commands use
  the same row budget; Ctrl+O and semantic copy retain the complete sanitized
  input, including newlines, independently of output previews.
- Windows `powershell` tool calls render like `bash`: command header, output
  tail, failure reason and Ctrl+O disclosure.
- Surfaced provider errors after a mid-stream failure no longer include internal
  stream progress counters (`stream_progress=frames=… events=…`).
- Controlled shell policies require an exact one-shot approval for each shell
  command. `edit` and `write` preserve a file's newline convention and hash the
  raw bytes. Non-Unix temporary bulk-lock roots use a stable private lock file.

- Anthropic caching keeps the previous conversation breakpoint reachable across
  large parallel tool batches without removing or reordering context.
- Supported Claude Bedrock routes emit native, retention-aware prompt cache
  points; unsupported models and opaque application profiles are unchanged.
- Compatible Chat usage recognizes Kimi's top-level `cached_tokens` alias without
  double-counting other cache-read aliases.
- Responses prefix diagnostics report native cache breakpoints without treating
  annotation-only movement as rewritten history; provider payloads are unchanged.

### Added

- Opt-in provider request traces correlate bounded, salted prefix diagnostics at
  canonical, encoded and final-wire boundaries with terminal provider usage,
  without writing prompt, tool, reasoning or credential content. Credential-free
  large-session regressions and `CACHE_AUDIT.md` distinguish byte estimates and
  native-cache simulation from live-provider cache measurements. Shared Responses
  regressions cover all builtin models, including GPT-6 Luna, plus declared Flex
  tier fixtures, opaque replay and persisted-session reopen; these are not live
  cache-hit or tier-availability qualification.

### Known limitations

- Pi compatibility remains experimental and unstable; the `octet-pi-compat`
  adapter does not ship (#587).
- Windows remains a preview platform (#572, #588).
- The 0.9.4, 0.9.3, 0.9.2, 0.9.1 and 0.9.0 known limitations below still apply.

## [0.9.4] - 2026-10-15

The [release notes](docs/releases/v0.9.4.md) record shipped scope, qualification
boundaries and known limitations.

### Fixed

- Gemini 4 Argon Contributor on OpenCode Go and Gemini 4 Argon on OpenCode Zen
  offer low, medium and high thinking. OpenCode's model list carries no
  reasoning information, so `/thinking` offered only `off`.
- Gemini 3.1 Pro offers low, medium and high thinking on the Gemini API, Vertex
  AI and OpenCode Zen; it offered only low and high.
- Gemini 3 Pro, 3.5 Flash Lite, 3.6 Flash, 3.7 Flash and 3.8 Flash on OpenCode
  Zen offer their thinking levels instead of only `off`.

### Added

- Gemini 3.5 Flash Lite, 3.6 Flash, 3.7 Flash, 3.8 Flash and 4 Argon on the
  Gemini API and Vertex AI, with their thinking levels and prices.

### Known limitations

- Pi compatibility remains experimental and unstable; the `octet-pi-compat`
  adapter does not ship (#587).
- Windows remains a preview platform (#572, #588).
- The 0.9.3, 0.9.2, 0.9.1 and 0.9.0 known limitations below still apply.

## [0.9.3] - 2026-10-15

The [release notes](docs/releases/v0.9.3.md) record shipped scope, qualification
boundaries and known limitations.

### Fixed

- OpenCode Go `gemini-*` models, including Gemini 4 Argon Contributor, are sent
  to `https://opencode.ai/inference/go/v1/`, the only OpenCode Go base that
  serves them. 0.9.2 used the `zen/go` base and every request failed with
  `404 (route or model not found)`.

### Known limitations

- Pi compatibility remains experimental and unstable; the `octet-pi-compat`
  adapter does not ship (#587).
- Windows remains a preview platform (#572, #588).
- The 0.9.2, 0.9.1 and 0.9.0 known limitations below still apply.

## [0.9.2] - 2026-10-14

The [release notes](docs/releases/v0.9.2.md) record shipped scope, qualification
boundaries and known limitations.

### Added

- `/extensions install` downloads and installs every missing first-party
  extension bundle concurrently, with a live progress row per bundle, and
  `octet extension install --all` does the same from the shell. Installed
  bundles are left untouched, and installing never enables or trusts a bundle.

### Fixed

- OpenCode Go `gemini-*` models, including Gemini Argon, use OpenCode Go's
  Google endpoint and protocol with their declared thinking levels instead of
  the OpenAI-compatible chat endpoint (#592).

### Known limitations

- Pi compatibility remains experimental and unstable; the `octet-pi-compat`
  adapter does not ship (#587).
- Windows remains a preview platform (#572, #588).
- The 0.9.1 and 0.9.0 known limitations below still apply.

## [0.9.1] - 2026-10-14

The [release notes](docs/releases/v0.9.1.md) record shipped scope, qualification
boundaries and known limitations. 0.9.1 is the first 0.9 release with published
native binaries; 0.9.0 published only its source archive and extension bundles.

### Security

- Update rustls from 0.23.45 to 0.23.46 for RUSTSEC-2026-0346 (`Debug` output
  for client session values could expose TLS secrets), RUSTSEC-2026-0349 (an
  unauthenticated ServerHello could downgrade later connections to classical key
  exchange), RUSTSEC-2026-0348 (Brotli certificate decompression could allocate
  up to 1 GiB before authentication) and RUSTSEC-2026-0347 (buffered data could
  reach the outer server after an ECH rejection).

### Fixed

- `install-octet.sh` accepts the signed `OCTET_SHA256SUMS` that also lists the
  Windows archive and `install-octet.ps1`. The 0.9.0 installer rejected it as an
  unexpected asset set on Linux and macOS.
- The release workflow skips one Windows file-publication test
  (`windows_publication_stays_bound_to_original_parent`) in the MSVC release
  qualification while it is investigated (#588); Windows CI still runs it.
- The CLI reference lists the five published extension bundles instead of six.

### Known limitations

- Pi compatibility remains experimental and unstable; the `octet-pi-compat`
  adapter does not ship (#587).
- Windows remains a preview platform (#572, #588).
- The 0.9.0 known limitations below still apply.

## [0.9.0] - 2026-10-13

The [release notes](docs/releases/v0.9.0.md) record shipped scope, qualification
boundaries and known limitations. The version-pinned release page records actual
artifact and channel publication.

**Pi compatibility is experimental and unstable.** Full Pi backwards
compatibility is not yet qualified, and the Pi adapter does not ship in this
release. Entries below about `octet-pi-compat`, imported Pi
factories and Pi editors describe source-only experimental development, not
shipped features. Many extensions already work: the release catalog ships five
first-party bundles (codemode, computer use, MCP, subagents and web search), and
in the source preview unchanged Pi extensions such as Doom, drawing, the
powerline footer and the rainbow editor run through the adapter.

**Windows support is a preview.** The native Windows x86-64 (MSVC) build is
published and release-tested, but it has not had attended desktop acceptance and
has open issues such as [#572](https://github.com/skaft-software/octet/issues/572).

### Added

- `/setup` can remove what octet saved: pick **Remove a provider or sign-in** to
  delete a ChatGPT/Codex, Copilot or other subscription sign-in, a built-in API
  key, or a custom provider, after a confirmation that defaults to Cancel. The
  provider behind the current model is refused, and environment variables are
  never changed.
- Themes choose how verbose the transcript is by default. `thinking_expanded`
  shows the thinking trace as it streams (and on resume) instead of a collapsed
  `Thinking` line: the built-in `pi` theme and Pi palette projections set it, and
  the default theme still collapses it. `quiet_tool_summaries`, which `Still` uses
  to group read-only calls into `Explored` rows, is now documented with it in
  [themes](docs/themes.md#tool-call-and-thinking-verbosity).
- Pickers such as Settings keep a usable window of options when a long
  transcript leaves little room, instead of showing one row.
- Quieter startup and reloads: only what you must act on is shown. Best-effort
  background work (model inventory and metadata caches, model discovery, provider
  registration, optional local probes) no longer prints warnings; a provider that
  is down reports its real error when you use it. A reload now ends in one note:
  a green dot with `reload succeeded` (explicit `/reload` only) or a red dot with
  `reload failed: reason (+N more)`. An extension notice that repeats identically
  is shown once per 30 seconds instead of on every event.
- An explicit `--extension-dir` that does not exist is now a startup error naming
  the path, instead of silently falling back to the installed extensions (which
  hid an unfinished Pi mirror setup).
- With the stock `octet-pi-compat` bundle enabled and no Pi setup mirrored, every
  prompt no longer prints `Pi compatibility error: not_foreground_owner lifecycle
  notification` lines.
- Default fullscreen launches now capture the wheel and trackpad (`mouse = "auto"`
  follows the surface), so the transcript scrolls without a flag. Inline sessions
  keep native selection and scrollback; `--mouse terminal` and `--mouse off`
  never capture.
- Custom providers pointed at a first-party endpoint octet already declares
  (for example `https://api.cerebras.ai/v1`) now get the reviewed catalog's
  context window and output limit for models whose `/v1/models` entry carries
  none, instead of losing those models as "unknown effective context". A
  context you configured or the endpoint asserted is never replaced.
- Startup no longer prints a warning when a custom provider's models have no
  known context or when its `/v1/models` discovery fails; a failing endpoint is
  reported when you use it.
- Pressing Ctrl+D while extension resource discovery is still running now exits
  cleanly instead of printing `Error: resource discovery closed`.
- `OCTET_PROVIDER_REQUEST_TRACE=<file|stderr>` appends a redacted trace of each
  provider request's shape (item types and counts, image counts and bytes,
  reasoning `encrypted_content` presence, transport, `previous_response_id`
  presence) and of any rejection's status, without prompts, tool output, headers
  or credentials. See [Provider request trace](docs/providers.md#provider-request-trace).
- Only the newest four tool-result images (typically computer-use screenshots)
  are sent to the model on each request; older ones become a text placeholder
  while the session journal keeps them. See
  [Model-visible tool-result images](docs/providers.md#model-visible-tool-result-images).
- Pi compatibility notifications now identify their originating extension via the negotiated `notification_source_v1` contract; info notices omit redundant severity while warnings and errors retain it.
- `OCTET_TRACE_REMOTE_UI_LIFECYCLE=1` opts into timestamped remote UI mount, retirement,
  and stale-operation traces carrying the session, process instance, generation, and surface.
- Pi 1.0.2 custom UI keybinding facades now implement `getEffectiveConfig()` from
  Octet's live native keybinding snapshot, including user overrides and reloads.
  `getDefinition`, `getConflicts` and `getUserBindings` remain unavailable because
  the host does not provide their source metadata; mutating operations remain
  unsupported because keybindings are host-owned.
- **Bring your Pi setup with you.** Enabling the optional
  [octet-pi-compat](extensions/octet-pi-compat/README.md) adapter imports a
  reviewed Pi 1.0.2 setup — extensions, installed packages, skills, prompt
  templates, themes, keybindings, the global context file and the selected
  model — and disabling it returns exactly to your octet-only setup. `--mirror`
  re-reads the Pi setup read-only at every startup instead of freezing one
  import. Pi is Mario Zechner's coding agent; the adapter is an independent
  implementation of Pi 1.0.2's public extension API.
- **More of Pi's extension API.** Tool call and tool result hooks can rewrite
  arguments and results, custom messages support `steer`, `followUp` and
  `nextTurn` delivery, `newSession`/`fork`/`switchSession`/`reload` replace the
  session safely, reviewed commands contribute argument completions to the
  composer, imported Pi palettes become selectable native themes, reviewed
  builtin tool replacements are admitted per grant, and `onTerminalInput`
  handlers see raw terminal input after native reserved keys and the slash
  popup, but before a composer slot editor.
- **Installed-Pi fallback (experimental).** `--pi-runtime installed` resolves
  imports the adapter's shims lack from your managed Pi 1.0.2 install. The
  [27-row ledger](docs/pi-extension-api.md) marks which rows that covers; they
  stay partial.
- **Credential-scoped Pi model registry.** The optional Pi 1.0.2 bridge withholds
  `getApiKeyAndHeaders` unless reviewed configuration explicitly grants
  `provider_credentials`; each request resolves only its exact native provider/model.
  Credential values are omitted from status and diagnostics, and unrepresentable
  request-signing credentials remain refused.
- **Cache warming.** `off`, `streaming` (default) and `idle` policies refresh
  billable provider prompt caches, with `/cache-warming`, and opt-in cache-miss
  notices. Live savings are not claimed.
- **Inference measurements.** `/status` and telemetry separate server-reported
  generation timing from billing; client-side estimates are labelled and never
  used for accounting. See [inference measurements](docs/inference-metrics.md).
- **Tern native rendering.** In a Tern pane, octet draws its transcript,
  composer and context meter with Tern's native components; `--tern auto|on|off`
  controls negotiation.
- **A real-binary end-to-end suite.** `scripts/e2e/run.sh` drives a built octet
  through disposable PTY sessions against a loopback mock provider, prints
  PASS/FAIL per check, and covers resume, long-session scroll, mouse
  selection/copy, themes, busy commands, steering, follow-up and Pi compat.
- **Mistral Large 4.** Mistral's `mistral-large-4` route (public preview, 524,288
  context, 262,144 output tokens, text and image input) is selectable on Mistral
  direct and on OpenCode Zen; OpenRouter discovers its
  `mistralai/mistral-large-4-0` listing with the route's own rates. Each route
  starts at `high` reasoning — the only enabled level Mistral documents besides
  `none` — and still accepts an explicit `--reasoning` choice.

### Changed

- Permanently removed the native Search tool, its schemas, registration and SDK
  exports. The core coding surface is `read`, `edit`, `write` and `bash`;
  repository content search uses `rg` through `bash`. Independent shell calls
  can still overlap reads under full access. Still retains its quiet **Explored**
  activity grouping and Ctrl+O disclosure; PowerShell stays explicit-only.
- The default coding prompt asks for the requested change plus one relevant
  check, then stops: no extra harnesses, no `git diff`, and no further
  verification unless the user asks. The diff-review and honest-reporting
  bullets stay.
- With no reasoning preference, octet uses the endpoint's declared default level
  (`effort.default_level` for DeepSeek) instead of choosing a level itself;
  `max` stays selectable whenever the endpoint declares it. That default is only
  the fallback: this run's `--model`/`--reasoning` flag, then the model and level
  saved in the session or in `~/.octet/config.toml`, still win across a restart.
- A launch that already names its model no longer waits for a cold provider
  inventory: the cached inventory and the pinned model record carry the first
  turn while `GET /v1/models` refreshes in the background. Model pickers and
  first-run setup still wait for the endpoint.
- The Harbor evaluation adapter no longer forces `--reasoning max` for every
  run; pass `--agent-kwarg reasoning=<level>` to pin an effort level. See
  [the adapter notes](evaluation/harbor/README.md).
- **The live models.dev refresh no longer materializes the whole catalog.** The
  published `api.json` is about 5 MiB across more than two hundred providers, and
  building one value tree for all of it peaked near 60 MiB on a first run. Only
  the providers whose records octet reads are built now, and the extracted
  records are unchanged.
- Model and thinking changes submitted while a run is active are queued in the
  UI and applied at the next safe request boundary instead of being silently
  refused.
- The interactive TUI starts fullscreen by default (`mouse = "app"`, the
  alternate-screen viewport that captures the mouse). `mouse = "auto"` or
  `--mouse auto` keeps inline native scrollback. Capture keeps every decoded
  wheel/touchpad event, and app-owned selection copy follows the painted
  transcript across scroll, wrap and chrome.
- A custom Pi editor occupies one composer slot with native chrome and a single
  native slash popup. Editor submissions use native idle/busy admission, a
  refusal preserves the component's paste and undo state, and `Ctrl+G` keeps the
  editor mounted instead of unmounting it.
- Pi imports resolve through Pi 1.0.2's module table with the versions Pi 1.0.2
  ships (jiti 2.7.0, TypeBox 1.3.27, pi-tui 1.0.2), and `octet migrate pi`
  describes Pi 1.0.2.
- `/hotkeys` prints grouped key/description tables with readable key names.
- Removed the packaged Serve application, its web client and the macOS/iOS
  Apple companions, the `serve` cargo feature and its embedded host, the `web`
  CI lane, and the deprecated `octet-browse` bundle. The extension release
  workflow (still named `release-serve.yml`) stays for first-party extension
  bundles. Five first-party bundles ship; Pi compatibility remains source-only. Computer use
  drives your own browser.

### Fixed

- Starting octet from your home directory no longer reports your global
  `~/.octet` prompts, skills and themes as untrusted project resources, which made
  every reload end in `reload failed`. Reload notes also show prompt diagnostics
  as `path: message` instead of a raw `PromptDiagnostic { … }` dump.
- Removing your last custom provider in `/setup` now deletes the registry only if
  it is unchanged since it was read, and cleans up only that provider's model
  cache, so another octet process's newer providers and caches are kept.
- Experimental Pi compatibility: stalled Pi factory setups now share one
  absolute load deadline (22 s) instead of each getting a minimum wait after the
  20 s budget runs out, so many stalled factories can no longer push the load past
  the host's 30 s limit.
- Starting with `octet-pi-compat` enabled no longer keeps the screen blank for
  seconds. The Pi mirror loaded every factory up to three times before the first
  frame (about 7-11 s for a 20-extension setup) on every start. It now reuses its
  captured catalog while your Pi setup, its factories, the adapter, Node and the
  installed Pi are unchanged, and otherwise captures them in one warm process:
  the same setup draws its first frame in about half a second.
- The Pi mirror applies its skills, prompts and theme again when the interactive
  shell attaches it after the first frame. Its `session_start` arrived before the
  session it belongs to was shared with it, so an extension that read
  `ctx.sessionManager` there (for example `pi-background-tasks`) failed it, and
  startup ended with `reload failed: … requires successful settled session_start`
  while the Pi footer still drew over octet's theme. A Pi default model served by
  one of your extensions' providers is also selected now.
- Prompts no longer fail with `extension input hook timed out` or `before_prompt
  hook exceeded 5s` when a Pi extension supplies the editor. Its editor calls are
  answered by the terminal, which did not serve them while waiting for those
  hooks, so both sides waited out the deadline; Enter pressed meanwhile showed
  `operation in progress`.
- The model picker and `--list-models` show the current OpenCode and OpenCode Go
  model lists instead of only the built-in ones; the live list used to arrive
  only on a later launch, and a failed fetch is now reported.
- `[Extension issues]` names the error code an extension stopped with (for
  example `pi_anthropic_attribution_host_adapter_missing`) instead of only
  saying its details were redacted; the message itself is still never shown.
- A stalled Pi extension no longer takes the mirror down with it. Every Pi hook
  runs on one ordered queue, so one factory whose handler never settled (a network
  call, a child process, an MCP connection) held it forever: startup failed with
  `resources_discover failed … requires successful settled session_start`,
  `/reload` failed, and the next prompt failed with `before_prompt hook exceeded
  5s` and stayed in the composer. A handler now has a time budget (5 s for
  lifecycle events; 2 s each and 4 s in total for the prompt hooks). One that
  outlives it keeps running, is named once in `[Extension issues]`, and is skipped
  while it stays busy; a tool guard that cannot answer in time blocks the call
  rather than letting it through. A prompt sent while an extension is still
  starting waits up to 12 s for it, then continues without it. With
  `OCTET_STARTUP_TRACE=1`, each handler prints one `pi-compat.handler` line with
  its factory and duration, which names the slow one.
- Enabling `octet-pi-compat` is the whole Pi setup: it mirrors this machine's Pi
  setup at every start with nothing to configure and no folder to be in. A
  factory whose own setup never finishes is cut off after 10 s, left out of the
  mirror and named in `[Extension issues]` (it used to stall the whole start),
  and an extension left out of the mirror is now named at start instead of only
  in quiet diagnostics.
- A tool name that clashes with a first-party tool no longer turns the whole
  `octet-pi-compat` bridge off: only the clashing tools are withheld, and one note
  names them. Other extensions keep the old behavior.
- `configure.mjs --mirror` leaves an unsupported factory out and lists it instead
  of refusing the entire mirror; `--strict` keeps the all-or-nothing review. A
  registration collision still needs your decision.
- Pi footers show their context and cost segments. `ctx.getContextUsage()`
  returns the model's capacity with unknown usage (and a measured zero for an
  empty conversation) instead of nothing, and `ctx.modelRegistry.isUsingOAuth`
  now has an answer from the host (Codex, subscription and Copilot logins are
  OAuth; API keys and Vertex are not) instead of always throwing. Footers such as
  `powerline-footer` read both inside one `try`, so the throw used to drop their
  context and cost segments.
- An extension on API 0.3 now starts in a session that already has a measured
  reply, for example after `/reload`, a resume, or enabling it mid-session. The
  host state in its initialize request carried a fractional context percentage,
  which 0.3's integer-only canonical JSON refused, so the startup handshake
  failed. `context_usage` is not part of the 0.3 contract and is no longer sent.
- The completed-turn line no longer ends with `· N responses`.
- A prompt card keeps its painted top and bottom padding after a completion line,
  and the working indicator is separated from a card by a neutral row. The layout
  treated a card's painted padding as blank spacing and merged it into its
  neighbour.
- Every `/settings` choice takes effect the moment you make it, mid-run or idle:
  steering and follow-up mode (which also reaches the running turn), default
  model, default reasoning and the screen mode. Theme and image display change
  at once and their saved preference follows at the idle boundary. Commands that
  change the running agent itself (`/model`, `/thinking`, `/fast`,
  `/auto-compact`, `/scoped-models`, sessions) still wait for the idle boundary.
- Pasted-text and attachment chips are ordinary text: Backspace edits them one
  character at a time like the text around them, in the terminal composer and the
  native editor. A chip that is still intact when you submit resolves to its
  payload; an edited one is plain text. Undo restores it.
- OpenCode Go shows its current catalog. It now uses live `/models` discovery
  with Messages (Claude, Qwen) and Responses (GPT) routes, instead of ten
  hand-kept models, two of which the service no longer serves.
- Fullscreen: scrolling with PgUp/PgDn no longer leaves stale or duplicated
  inline images (Kitty-protocol terminals such as Ghostty). The fullscreen painter
  erased and rewrote rows but never deleted the image placements anchored to them,
  so every scroll stacked another copy and left old ones over the text. A placement
  whose rows change or scroll is now deleted and placed again at its new position,
  and images are removed when you leave the fullscreen view. A status tick that does
  not touch an image's rows leaves it alone.
- Pi images work on the real host, including nested `ctx.executeTool` results,
  and Pi's `agent_settled` event fires once after `agent_end`.
- Pi footers and widgets can ask whether the active model uses OAuth; the host
  answers from its actual authentication resolver without reading or
  refreshing credentials, and unclassified resolvers refuse the query instead
  of guessing from an endpoint name.
- Ordered Pi hook callbacks settle before their response, so a second ordered
  callback cannot overlap a cancelled one.
- Pi session replacement commits parent linkage and writable setup against the
  replacement session, including requests made during a busy turn.
- A Pi completion query that arrives immediately after initialize is served
  instead of refused.
- `/theme <advertised built-in name>` resolves through the same resolver as
  startup and the picker.
- Pointer selection copies the painted row across scroll, wrapped Markdown and
  chrome instead of an adjacent row.
- Presentation-only renderer requests no longer read session history, so the
  real-adapter renderer keeps its latency margin under load.
- After an explicit Python runtime setup, `/extensions setup` starts the
  extension that failed cold start without a restart.
- Extension-process crashes found by the real-package matrix (`extension stdout
  closed`, lens and permission-system startup refusals) are fixed, and
  recoverable callback failures are reported once as bounded `[Extension
  issues]` diagnostics.
- Inherited failures in hook/lifecycle delivery, the MCP host-access replay
  check, activity/steering PTY contracts, headless before-prompt tool snapshots
  and the API 0.1 conformance wire are repaired, and the inherited clippy lint
  debt is cleared.
- The native linux packager no longer requires tracked private workstation
  records in the public documentation inventory.
- A cancelled extension request — an expired Pi `input` hook, an interrupted
  turn, or an extension-reported `request cancelled` — now retires only the
  remote UI surfaces that request created. A mounted Pi custom editor or footer
  survives it instead of disappearing until `/reload`, and the extension keeps
  its session resource owner for later mounts.
- Pi notices sourced from a specific Pi extension drop the bridge's own digest
  heading, so a sourced notice renders as `[Termdraw] message` or
  `[Termdraw Error] message` instead of `[Termdraw] [Extension issues]: …`.

### Known limitations

- **Native-backed Pi editor is phase 2.** Pi's editor suites are preserved as an
  upstream test corpus, and custom editors already render in the composer slot,
  but a native-backed editor facade with synchronous native editing operations
  and full upstream editor parity are not implemented.
- **Pi compatibility is a bounded sample, not parity.** The adapter targets
  Pi 1.0.2's public extension API; arbitrary third-party packages, private Pi
  internals and the Pi CLI/SDK are not targets. The recorded 2026-10-06 matrix
  of 34 real packages has 7 passing, 12 failing with Octet-side errors, 11
  needing prerequisites the test machine did not have, and 4 that are not
  extension factories. Later adapter repairs have not been re-measured against
  that matrix.
- **Qualification is artifact-specific.** Previous candidate runs do not qualify
  another source SHA. The version-pinned release page records exact-source CI,
  signing and public-install results; unrun or incomplete acceptance remains
  explicitly unqualified. npm, Homebrew and SDK registries are separate channels.
- **Live services are not exercised.** Provider, subscription and audio
  acceptance use a loopback mock provider; no paid model, live credential or
  physical display path is part of this candidate's evidence.
- **No performance comparison is published.** Historical pre-rename measurements
  in [benchmark evidence](docs/assets/evidence/README.md) are explicitly not
  octet results.

## [0.8.2] - 2026-09-30

See [release notes](docs/releases/v0.8.2.md) for candidate scope, availability,
and qualification limits. The date identifies this candidate, not a publication
receipt.

- Sign in to paid plans without a long-lived API key, the way Codex sign-in
  already works. `--login grok`, `--login kimi`, `--login meta`, and
  `--login openrouter` each run the provider's own OAuth grant and store one
  owner-private credential under `~/.octet/credentials/`;
  `--logout <provider>` removes only that credential. Grok, Kimi, and Meta use a
  device code and honor `--headless`, so they work over SSH; OpenRouter's browser
  login asks you to paste the redirect URL your browser lands on. Each is a
  separate provider from the same vendor's API-key preset
  (`xai-subscription/…` alongside `xai/…`), so signing in never replaces a key you
  configured, and a provider's models appear only while you are signed in.
  Meta's grant is two steps — the device flow yields an identity token, which is
  exchanged for a short-lived API key — and OpenRouter mints a durable key that
  octet never renews. Refresh-token rotation is serialized within and across
  octet processes and re-checks the credential after taking the cross-process
  lock, so two concurrent launches cannot spend the same single-use token.
  Anthropic (Claude Pro/Max) is deliberately not included: it needs a request-path
  change in the Anthropic Messages codec before a privately resolved OAuth
  credential would receive the beta headers a subscription token requires.
  `ANTHROPIC_AUTH_TOKEN` and `ANTHROPIC_OAUTH_TOKEN` are unchanged.

- Restore complete canonical native history after dimension changes and
  canonical mutations above the old viewport, using synchronized clear/replay
  instead of saved snapshots that could omit accepted rows. Settle resize bursts,
  reuse height-only wrapping, and promote threaded reading anchors before width
  reflow. Resize epochs cover away-and-back changes and final/suspend flushes.
  Ordinary appends and visible-row diffs are unchanged. Structural replay scales
  with history and can reset the native reading position; PageUp/`--mouse app`
  remain a separate bounded semantic viewport.

- Keep the startup welcome static, budget it for the actual pane height, and
  reserve fresh-start composer geometry before model admission. Draft edits and
  update hints no longer drive decorative historical replay. Position the native
  hardware cursor inside the synchronized frame; active-run animations remain.

- Keep edit and write diffs out of model-visible tool results. An edit or write
  now returns one concise line (`ok`, path, verb, and content hash) and carries
  its unified diff in the durable result metadata that the model never sees and
  that session replay does not turn into context. Tool cards render the same
  diffs from that channel, so the UI is unchanged, and sessions recorded before
  this change still show their diffs. Stale-file detection, exact-match
  reporting, and diff resource evidence are unchanged. Bound the serialized
  metadata, including JSON escaping, and validate the result before committing
  the file so a successful mutation cannot be reported as a metadata failure.

- Retain Meta's initial device identity across API-key minting and renewal;
  hold subscription refresh locks across credential replacement on Windows as
  well as Unix. Omit unrecognized OAuth error codes from diagnostics rather
  than reflecting credential-shaped provider text.
- Protect live cache-read, cache-write and effective reasoning prices against
  zeroing or greater-than-tenfold drops, just like input and output prices.
- Re-anchor active status rows after history reflow so `Working` shimmer and its
  elapsed clock continue after a narrow-to-wide resize. Use revision-fenced
  painted viewport/active-heading geometry so visible clocks keep self-waking
  after tall tool progress contracts, without requiring resize or more output.
- Complete default macOS computer-use setup with a checksum- and Developer
  ID-verified signed host in octet-owned state when needed. Target cursor moves
  with the current driver's exact-window contract, and reopen setup with a fresh
  transport-owned session identity.
- Refresh and review all four models.dev snapshots together: 921 pricing routes,
  402 names and 943 capability routes. Direct DeepSeek schedule pricing remains
  excluded; public metadata does not establish live inference acceptance.

- Add GPT-6.1 Sol on OpenAI API keys and ChatGPT (Codex) sign-in. On Codex it
  leads the fallback model list, starts at low reasoning, and offers Ultra only
  when the account advertises it; its window is 272K by default and up to 872K
  where the plan allows. On the API it offers low through max reasoning, a
  1.05M-token context, 128K output tokens, and long-context pricing above 272K
  input tokens.
- Send Codex discovery compatibility client version `0.159.2`. The live
  backend withheld GPT-6.1 Sol from `0.156.1`, which 0.8.0 introduced along
  with cache schema 8 without a changelog entry. Cached Codex inventories
  refresh once (cache schema 10), so GPT-6.1 Sol and its contracts appear
  without waiting for the cache to expire. An inventory entry with unusable
  reasoning metadata is now left out and named in a startup warning instead
  of discarding the whole inventory, and the offline fallback lists GPT-6 Sol
  and GPT-6 Luna alongside GPT-6.1 Sol and GPT-6 Astra.
- Sign in to ChatGPT (Codex) through the browser. `octet --login codex` and the
  in-app ChatGPT sign-in open OpenAI's authorization page (PKCE) and receive the
  callback only on `127.0.0.1:1455`, or on the registered fallback port `1457`
  when 1455 is busy, then show a self-contained octet signed-in page.
  `octet --login codex` asks which method to use; the device code remains the
  default over SSH, with `--headless`, without a browser opener, or when both
  ports are busy. If OpenAI issues a limited localhost-only credential through
  the browser, octet stores nothing and finishes with a device code.
- Replace executable-extension trust with On/off plus Host authority. Full
  access (the default) still runs enabled extensions without a grant. Safe mode
  and the other controlled policies now start an enabled extension whose exact
  source has host authority: a `trusted_extensions` entry (unchanged format),
  `--trust-extension`, or an explicit `--extension-dir`. Before, they never
  started executable extensions. Granted code runs with your OS permissions
  outside the tool-effect broker. `/extensions` shows each row's host authority,
  offers Grant and Revoke with a confirmation, and asks for a grant when you
  enable an extension in safe mode. The native-host protocol still never starts
  extension processes.

- One ambiguous provider failure no longer disables hard cost ceilings for the
  rest of the session; the attempt is charged its admitted worst case. Missing
  bounds or prices still fail closed for the affected ceiling.
- Keep models.dev names, prices and capability records current between
  releases. An interactive session refreshes them in the background at most
  every six hours, revalidating with the ETag, and caches them at
  `~/.octet/cache/models-dev/metadata.json`; `--offline` skips the refresh.
  Records go through the snapshot refresh's extraction and are checked against
  the built-in snapshot: a price may not become zero or fall more than tenfold,
  and a route new to octet needs non-zero input and output prices. A record
  that fails keeps the built-in data, which stays the fallback. A cache from
  another octet version is ignored, and nothing waits on the network at
  startup.

- Set up and configure every extension from `/extensions`.
  Choosing an extension opens its options menu (a disabled one is enabled
  first), which shows its state and offers only what applies. Actions show
  their steps live and can be cancelled with Esc; confirmations, choices, and
  hidden key entry appear in place, and destructive actions ask first.
  - **Computer use**: Set up computer use does the whole setup (driver, signed
    macOS host when needed, cursor themes, GNOME helper, permission check,
    optional Jev); also Check status,
    Jev keys, and the jev-use recipe with its jobs.
  - **Web search**: Brave Search or SearXNG, a new SearXNG endpoint change, and
    Brave log-out.
  - **Subagents**: enable or disable orchestration; runtime worker inspection
    and control stay in `/subagents`, not extension configuration.
  - **MCP**: a full server manager. A guided form adds a server; each server
    can be shown, refreshed, restarted, stopped, enabled or disabled, edited,
    or removed. Edits are validated like a launch before `~/.octet/mcp.json` is
    replaced, and apply live without restarting the other servers.
  Extensions answer the new `menu/collect` request (`contributes.menu = true`,
  `ext.menu` in the Python SDK); others get an entry per declared command.
  Other extension commands are no longer typed after the slash in the terminal
  UI, and typing one names the extension to open instead. `/subagents` remains
  a runtime command: bare, list and status open the live roster, including
  mid-run; `/subagents stop <name-or-id|all>` uses the bounded owner-bound stop
  queue. Other worker operations wait for idle. Slash completion follows the
  first-party extension's live registration. Menu actions may run for up to 30 minutes instead of the 30-second
  request deadline, so long installs finish.
- Report why model discovery was rejected. A provider's HTTP error now names
  its status and the provider's own message, bounded and with
  credential-shaped tokens masked, instead of only "model discovery request
  was rejected"; an Anthropic key that needs `anthropic-workspace-id` now
  says so (#454).
- Mask the home directory as `~` in the TUI workspace footer on every platform,
  including native Windows. Use the platform home and its canonical path rather
  than only `HOME`, so Windows verbatim paths and symlinked homes do not expose
  the username. This changes display only; workspace and tool paths are unchanged.
- Fix deleting sessions from the `/resume` picker. Its default delete binding
  was Ctrl+D, which closes octet from every surface, so it quit octet instead,
  and Mac keyboards have no forward Delete key. Ctrl+X (or Delete) now asks for
  a confirmation that names the session, then moves it to the store's
  recoverable trash (#458).
- Serialize concurrent conditional writes to the same file on Linux and
  macOS. Parallel writers, such as delegated workers editing one file, could
  have a write land while it reported a conflict, or lose a committed write
  into a leftover `.octet-tmp-*` file. Writers now hold an advisory lock on
  the target from the unchanged check through the exchange. The wait is
  bounded at 2 s, after which the write falls back to the unlocked checks.
  Displacing exactly the observed file now counts as success.
- Raise the minimum supported Rust version from 1.86 to 1.88 for the
  workspace and `octet-serve`. The rust-minor-patch updates that #444 held
  back need it: `image` 0.25.10, `globset` 0.4.20, `ignore` 0.4.33, and
  `wiremock` 0.6.5. CI, the Windows job, the production-panic audit and the
  `octet-serve` release jobs now pin Rust 1.88.0; octet release binaries are
  still built with Rust 1.97.1. Source builds need Rust 1.88 or newer.
- Fix inflated TUI throughput for hidden reasoning and buffered responses. Use
  request-to-completion timing and label the latest-turn rate as end-to-end,
  not server generation speed (#462).
- Attribute built-in OpenRouter requests to `octet coding agent` at
  `https://octet.skaft.org`, with the `cli-agent` category, using fixed public
  request headers. Cover Chat Completions and Batch API requests without adding
  attribution to unrelated providers or changing explicit header overrides.
- Link the npm launcher and native platform packages to the same octet homepage.
- Publish the v0.8.1 npm channel: `@skaft/octet` plus `@skaft/octet-darwin-arm64`,
  `@skaft/octet-darwin-x64`, and `@skaft/octet-linux-x64-gnu`, built from the
  verified immutable release assets with trusted publishing and registry
  provenance. Install with `npm install -g @skaft/octet`, or pin
  `@skaft/octet@0.8.1` to reproduce one exact release.
  README, installation, distribution, getting-started, and release docs now lead
  with the npm lane.
- Omit the redundant `computer_use_` prefix from TUI labels so
  computer-use actions remain distinguishable within the label width cap.
- Computer-use setup builds the Cua Driver runtime from a
  Python 3.10+ interpreter even when the bundle runs on macOS's Xcode Python
  3.9. Setup failures, including a missing compatible Python, are now reported
  as actionable errors instead of `internal error` (#457).
- Fix computer use hanging on native Windows when run inside octet. Its driver
  probes inherited the extension's protocol stdin, and on Windows a child that
  shares that pipe can stall at startup. The options menu then fell back after
  5 s to a bare command list with an argument prompt, and **Check status**
  failed with `internal error`. Every first-party extension subprocess now gets
  its own stdin, and a repository test enforces it. A driver check that cannot
  run is reported as a failing self-check with its reason, a failed
  computer-use action names its error, and a bare `computer-use` command checks
  status as its default.
- The computer-use options menu now shows what each action reported. Setup,
  **Check status**, the Jev actions and the jev-use actions returned a
  tool-shaped result, and the menu's result document renders only a `text`
  field, so a completed action showed just "<label> finished" with no report.
  Every menu command now returns rendered text; the agent tools keep the
  structured content.
- Stop querying the terminal background (OSC 11) on native Windows. The console
  host delivers the reply as key records, and Windows Terminal's reply reached
  the composer as typed text. Auto uses its neutral fallback there.
- The queued-message hint names Alt+Q on Windows and WSL, the key their key set
  binds to restore a queued message, instead of Alt+Up, which does nothing
  there.
- Computer use requires `cua-driver` 0.30.2 or newer. macOS 11
  and 12, which pip used to resolve silently to 0.11.0, and other unsupported
  systems now get a clear error that names the supported platforms.
- Make octet and `octet-computer-use` work on Linux desktops: Omarchy
  (Arch + Hyprland), Sway, GNOME, and KDE on Wayland, and any X11 session. The
  TUI now copies through `wl-copy`, `xclip`, or `xsel` alongside OSC 52, matching
  the existing Linux paste helpers. Extensions may declare the Linux
  desktop-identity and compositor variables (`XDG_CURRENT_DESKTOP`,
  `XDG_SESSION_DESKTOP`, `DESKTOP_SESSION`, `KDE_FULL_SESSION`,
  `HYPRLAND_INSTANCE_SIGNATURE`, `SWAYSOCK`, `XDG_STATE_HOME`). Computer use reads
  Linux display-session readiness instead of macOS grants, enables the driver's
  native Wayland backend, shows the model-colored agent cursor, and provisions
  even without `python3-venv`. See the new [Linux setup](docs/linux.md) guide.
- Build native Windows x64 (`x86_64-pc-windows-gnu`) binaries again: the
  interactive crate did not compile for Windows. A new Windows CI job builds
  `octet.exe` and `octet-host.exe`, checks the host hello handshake, runs the
  Windows terminal, ConPTY, workspace, Python SDK and computer-use suites, and
  uploads an unsigned pull-request test artifact. Nothing is published; see
  [Windows](docs/windows.md).
- Run the interactive frontend in native Windows Terminal and conhost. Both
  export no `TERM`, so PowerShell and cmd sessions always fell back to plain
  output. The console is now detected directly (Windows Terminal: Unicode,
  truecolor, italics, links; conhost: ASCII, 256 colors; mintty pipes and
  redirected streams stay plain). Frames reach the console in one
  `WriteConsoleW` call, delayed end-of-line wrapping keeps full-width rows from
  shifting later frames, and console close, `Ctrl+Break`, logoff and shutdown
  restore the console through the coordinated shutdown path.
- Stop a Windows terminal's background-colour reply from leaking into the
  composer: crossterm reports a key-up for every synthesized key-down, which
  flushed an unconfirmed reply prefix as typed text.
- Start Python extensions on Windows through `py -3` or `python.exe` from
  `PATH`; Windows cannot run a script by its `#!` line. The Python SDK now
  frames stdio as exact UTF-8 with LF on every platform instead of the
  Windows code page and CRLF.
- Run the interactive `!` command through Git Bash on Windows with Job Object
  cleanup, matching the bash tool, instead of requiring `sh` on `PATH`.
- Publish new files on Windows through the directory-relative native rename
  (`NtSetInformationFile`) instead of the Win32 wrapper, which documents its
  name as a NUL-terminated path. New private files failed to publish on the
  Windows runner with `ERROR_INVALID_PARAMETER`.
- Start extensions and open sessions from an elevated Windows session: the
  host's artifact store (its temporary root and per-generation scratch) and the
  workspace session directory are now created owned by the current user,
  instead of adopting directories an elevated process creates owned by the
  Administrators group.
- Replace existing files on Windows through pin, then displace, then publish:
  the target is pinned (deny write/delete sharing while holding `DELETE`),
  re-verified, renamed aside to `.octet-old-*`, and the staged file is
  published with a no-replace rename. This unblocks the `edit` tool and every
  rewrite of existing private state (session metadata, auth stores, model
  caches) beyond first-run writes; see [Windows](docs/windows.md).
- Make the source-only `octet-snap-compact` extension launchable and actually
  tested. Its `extension.py` entrypoint is committed non-executable while every
  released extension's entrypoint is `100755`, so a fresh checkout could not
  start the extension; the mode now matches the README. Its suite also built the
  renderer into `renderer/target/`, which the host's bounded source walk rejects
  as unverified, so the build now targets a temporary directory like the README
  documents. Because the renderer needs Rust 1.96 against octet's 1.86 MSRV, the
  suite skips on older toolchains instead of failing, and CI runs it from a new
  `extensions/source-catalog.txt` under a 1.96 toolchain. The extension remains
  unpublished: only its README ships in a release.
- Condense picker chrome for a denser read: the model picker omits
  `in —`/`out —` when pricing is unknown (subscription/OAuth models now show
  only `272K ctx · vision`), and the resume picker hides `re:<pattern>` /
  `"phrase"` hints until the filter actually uses them.
- Attribute extension source-digest work and startup handshakes separately in
  `OCTET_STARTUP_TRACE`; do not hash disabled or untrusted extension source trees.
  Reuse tool definitions, the initial extension host-state projection, and
  resumed configuration during app construction. Latest-session lookup scans
  candidates without a full sort, and application-owned resume hydrates a
  viewport-scaled tail. Defer OSC 11 background-color detection until after
  the first ready frame, repainting only if the detected theme changes.
- Size the runtime's worker pool to the machine (`available_parallelism()`, at
  least 2) instead of 2 threads, and run parallel read-only tool calls in waves
  of one per CPU, from 4 to 32, instead of 4. `Agent::set_parallel_read_wave_width`
  pins the width.
- Say plainly in the README, `--help` and `/status` that the default full
  access has no sandbox and no approvals, and that `--safe-mode` asks before
  every bash call and file change but is not a sandbox.
- Fix a crash (a reentrant `RefCell` borrow) when hovering over or clicking the
  scrollbar with the threaded renderer (#461).
- Keep `Working` visible while tools run (#463).
- Honour a request's `NO_PROXY` when a lower-case `no_proxy` is also set: a
  proxy overlay now replaces both letter cases of each variable it sets.
- Extend Still's quiet activity grouping to web search/fetch, MCP, and
  computer-use calls, with concise family summaries. Delegations keep their
  existing subagent presentation; `Ctrl+O` still reveals individual call details.
- Split the five largest source files into focused modules, with no change in
  behaviour: `agent.rs` (12.6K lines), `extension_process.rs` (18K) and
  `delegation.rs` (8.6K) in `octet-agent`, and `extensions.rs` (7.9K) and
  `extensions/serve.rs` (9.3K) in `octet-coding-agent`. Public items keep their
  paths through re-exports.

## [0.8.1] - 2026-09-27

See [release notes](docs/releases/v0.8.1.md) for scope, availability, and limits.

- Split startup tracing around the workspace marker and HTTP-client construction;
  skip an unchanged private workspace-marker rewrite while retaining no-follow
  validation and atomic repair. Use indexed model lookup during provider
  registration and check credentials once per endpoint during model pruning.
- Avoid cloning full conversation history for the native-steering continuation
  request, and skip that request entirely for ordinary provider turns.

- Give `Still` the compact model-adaptive startup splash. `splash_model_adaptive`
  with `splash_compact` replace the larger file-theme mark and the fixed `splash`
  colour with the compiled default's 16x4 byte that shades from the active model
  family, matching its model-adaptive prompt chevron and composer marker.

- Group Still's successive exploration and edit/write calls into distinct-file
  summaries. `Ctrl+O` retains paths and commands, and failures stay visible.
- Remove `Still`'s 112-column cap so its transcript, splash, composer text,
  and pickers use the available width instead of sitting in a narrow centered
  column on wide terminals. Its soft prompt and composer fills are retained.
- Reserve the detail row beneath a live `Working` status in file themes and
  beneath Still's in-flight compact tool header, so transitions through
  two-line `Thinking`, tool calls, and responses do not pull the composer up.
- Give custom themes that set `content_max_width` a full-bleed filled surface. Their
  text keeps the centered reading column, but a shaded composer and borderless
  `band`/`rail` surfaces paint to the terminal edges instead of floating as a
  narrow island with unpainted bars beside them. Bordered cards, plain text, and
  themes that only inset their content render as before.
- Fix `failed to save theme: invalid theme selector "Cards"` (and `"Still"`).
  `/theme` offers the compiled-in file themes through the same picker variant as
  a discovered theme, but persistence rejected their reserved stems, so
  confirming one always failed and the selection was lost on the next start.
  A reserved stem is now accepted as the built-in it names, canonicalizing case
  and the `.toml` spelling; the compiled default is still not selectable by name.

- Restore the default theme's full-cell model-adaptive prompt card, lost in
  v0.8.0 when a hardcoded compiled-theme branch replaced the whole-cell wash
  with a per-line text tint. The default theme now declares the wash explicitly
  (`prompt_wash = true`) and the same `prompt_wash` token governs every theme,
  so `prompt_wash = false` still keeps prompt rows on the theme's own fill. The
  card fills the entire cell — marker gutter, padding, trailing canvas, and the
  breathing rows above and below — and is layered under the rich renderer's own
  inline runs, so bold, italic, and inline code keep their styling inside it
  instead of being flattened onto one colour. Unknown-background and no-colour
  terminals stay unpainted, and `Cards` and `Still` are unchanged.

- Render the startup welcome card inside the shared presentation column, at the
  same `content_width` and with the same leading inset as the transcript,
  composer, and pickers. A custom theme that caps its reading column centers
  its splash; themes that request no inset and no width cap are unchanged.
- Rank `@` mention completions with a bounded top-k heap instead of collecting
  and sorting every match, and keep one query-scoped result on the workspace
  file index so a re-render or a selection change no longer rescans the whole
  index. The returned window and its ordering are identical to the previous
  full sort.
- Stop unasserted discovery fallbacks from overwriting configured custom-model
  limits. Endpoint-asserted context and output limits remain authoritative over
  a stale registry pin, but a sparse `/v1/models` response that omits both
  fields now leaves the configured `context_window` and `max_output_tokens`
  intact instead of replacing them with the 262144/16384 defaults. Cached
  inventories are versioned 11 so already-merged caches are rebuilt.

- Compile `examples/themes/Still.toml` into every release as the built-in
  `Still` selector, available after `Cards` in `/theme` and through `--theme`
  and `OCTET_THEME`. Reserve the `Still` file stem so local theme files cannot
  shadow the built-in. Still provides a centered reading column with a softly
  shaded composer and user prompts, compact unboxed tool activity, and aligned
  prompt/prose/activity indents. Its model-adaptive prompt and composer markers
  sit over a restrained blue-and-neutral palette. Validate the embedded theme
  for every supported terminal background.
- Ship the `Cards` example theme (`examples/themes/Cards.toml`): shaded
  rail surfaces with quiet stripes, dark/light fills, an amber accent with
  adaptive model colours, a borderless shaded composer, and a themed splash
  with compact geometry. Copy it into `~/.octet/themes` to use it. A test
  fails the build if the example stops validating for any background
  profile, so the next release that compiles it into the binary keeps the
  example and the built-in identical.
- Bound idle Ctrl+D exit to a 2 s extension-shutdown timeout with force-kill
  fallback (matching the signal path) so a hung extension child cannot make
  exit feel stuck; bound computer-use `end_session` to 3 s on shutdown.
- Drain pending terminal input around Kitty keyboard-mode pop and raw-mode
  exit so a held Ctrl+D repeat/release (`ESC[100;5u`) cannot leak a `00;5u`
  tail into the parent shell.
- Parse trailing Kitty image anchors once per transcript-block finish instead
  of twice, and find compaction cut points with a binary search instead of a
  linear scan per budget step.
- Show a cleanup notice if interactive exit takes longer than 150 ms; fast and
  signal-driven exits remain quiet, and the notice lasts through cleanup.
- Reuse encoded Kitty image placements across frames, keyed by cell size with
  bounded retention and payload-replacement invalidation; avoid rebuilding
  unchanged terminal window titles.
- Walk the complete workspace file index off-thread for `@` completion and
  show a scanning indicator until suggestions arrive; cache folded paths for
  per-keystroke matching instead of silently stopping after 10,000 files.
- Make computer-use window-state screenshots opt-in and shorten inline element
  hints to a 20-row sample with the full addressable count and structured-data
  location; explicitly requested screenshots and complete element data remain
  available. Full desktop captures remain screenshot-based.
- Compile `examples/themes/Cards.toml` into every release as the built-in
  `Cards` selector, so it needs no copy into a discovery root. It appears in
  `/theme` after the terminal-appearance choices, works with `--theme` and
  `OCTET_THEME`, and reserves the `Cards` file stem so a local `Cards.toml`
  can neither shadow nor be shadowed by the built-in.
- Let custom themes opt out of the model-colour prompt wash with
  `prompt_wash = false`: prompt rows then render on the surface's own fill
  while the chevron keeps its prompt colour. The default keeps the wash so
  model provenance stays visible.
- Let custom themes opt into the compiled default's smaller splash geometry
  with `splash_compact = true` (4-tall byte-mark at 16 columns instead of
  the larger file-theme presentation). Unset themes render as before.
- Add `splash_model_adaptive = true` so a custom theme can keep the
  default's model-adaptive splash. It claims the whole splash: the byte-mark
  gradient and the splash text both follow the active model accent, and a
  `splash` colour is ignored. `Cards` uses it.
- Paint `rail` transcript surfaces edge to edge: short rows now pad to the
  full frame like `band` chrome instead of rendering as ragged text-only
  highlights, so shaded fills read as cards.
- Align full-width (`card`/`band`/`rail`) user surfaces with the shared
  marker gutter in file themes, so prompt cards start and end on the same
  columns as tool/shell cards. Plain user text and the compiled default
  keep the historical compact prompt grid.
- Shade the startup byte-mark into a truecolor column gradient derived from
  a custom theme's `splash` colour, keeping the travelling sweep on
  animation-capable terminals. Limited palettes, reduced motion, and themes
  without `splash` render exactly as before.
- Give borderless `band`/`rail` transcript surfaces a vertical cushion row
  above and below their content when the theme sets `padding > 0`, matching
  the breathing room card surfaces already had, so shaded fills never touch
  their own edges. Themes with `padding = 0` and the compiled default
  (all-plain surfaces) render exactly as before.
- Fix custom OpenAI-compatible providers ignoring live context-window changes:
  with `auto_discover: true`, endpoint-asserted limits are now authoritative
  over registry pins (`max_model_len` wins, output is the tighter cap clamped
  to the live window) so vLLM profile switches converge via background refresh
  instead of re-entombing a stale pin in every cache write. `auto_discover:
  false` remains the explicit registry-truth opt-out. Custom model caches
  advance to version 10 to discard entombed pins.
- Align the release-candidate bookkeeping with `0.8.1-rc.1`: the Python and
  TypeScript SDKs, the shell installer, and the four executable-bundle
  manifests follow the candidate, and the install guides, distribution notes,
  and `v0.8.1-rc.1` release notes point at it. Refresh the checked-in
  models.dev pricing, capabilities, and source snapshots and update the
  deterministic OpenRouter DeepSeek price assertion to the reviewed catalog.
- Fix the `octet-computer-use` test bootstrap to use the vendored SDK and
  isolate the ambient TypeSafe key so the Jev suite never reaches the network.
- Fix the active-run thinking-control test to expect the committed footer
  label after the wire update is admitted, and format the composer sources
  for the current toolchain.
- Fix context compaction reporting no prior messages to summarize when a single
  oversized entry alone exceeds the keep-recent budget: the boundary walk now
  cuts at the newest valid turn boundary before the crossing instead of
  collapsing to the oldest visible entry. A re-compact with no new history now
  reports that explicitly instead of claiming there were no prior messages.
- Remove the retired `octet-import-aider` and `octet-import-pi` source
  extensions and their packaged-doc, installer, and test references; the
  built-in `octet migrate import pi` host implementation is unchanged.

- Fix Shift+Tab cycling so exact provider choices always sort from lowest to
  highest and wrap to the lowest; rapid active-run taps advance from the latest
  selected level, repaint immediately, and defer preference I/O until idle.
- Fix Serve’s Ultra slider label and purple transition after rainbow Max.
  Expand the bounded model catalog from 256 to 4,096 entries so large provider
  inventories no longer hide models at the old cutoff; retain capability gates
  and bootstrap byte limits.
- Add the `octet-computer-use` bundle to the official release catalog. It drives
  native desktop applications on macOS, Windows, and Linux through a locally
  installed MIT-licensed Cua Driver, provisioning the driver on request and
  following the active effect-confirmation policy: gated profiles confirm each
  effectful action; full access does not prompt by default.
- Deprecate `octet-browse`. It remains published, installable, and unchanged, and
  is retained because its isolated, Octet-owned Chromium profile with a manual-auth
  boundary is a safety property the computer-use path does not provide.
- Refresh reviewed models.dev metadata to 910 pricing routes, 395 canonical names,
  and 932 capability routes; preserve the [source provenance](crates/octet-ai/models/SOURCES.md).
  The re-freeze is required because the pre-release freshness gate compares the
  checked-in snapshots against live models.dev whenever the workspace version
  changes, so the 0.8.1 bump re-checked a snapshot last reviewed for 0.8.0. All
  nine changed prices and seven changed context/output limits are OpenRouter
  routes; canonical display names are unchanged. Builds and runtime remain
  offline, direct DeepSeek schedule pricing remains excluded/unknown, and public
  metadata does not establish live inference acceptance.
- Restrict the four computer-use `.app`-bundle tests to macOS. They inject a
  `darwin` host candidate and assert `CFBundleExecutable` resolution, which the
  driver only performs on darwin; off darwin `None` is the correct answer, so
  the suite now skips them instead of failing there. The other desktop-host
  tests, which drive the platform-independent `OCTET_CUA_DESKTOP_APP` override,
  keep running on every platform.
- Count a loopback AWS-metadata fixture request when it is read rather than after
  its response is written, so a caller that has already read the final body
  always observes the final count. The four request-count assertions could
  otherwise read one short on a loaded runner.
- Give each `herdr` transport test socket a process-unique path from a counter
  instead of a sub-millisecond clock residue, which repeated across the parallel
  test threads sharing one process and failed the second bind with `EADDRINUSE`.

## [0.8.1-rc.1] — local dogfood only (unpublished)

- Add in-session `/setup` provider configuration while preserving the active
  model and session.
- Discover user-defined TOML themes in `/theme`.
- Show the active session name in the terminal window title and keep it
  synchronized with session changes.
- Prevent Kitty inline images from advancing the cursor into the composer.
- Pin source extension manifests to this RC host; the published 0.8.0 bundles
  remain unchanged and are not compatible with the RC binary.
- Fix Shift+Tab thinking cycling: the idle and active-run paths now share one
  ascending, total rule, so the gesture always advances instead of silently
  doing nothing on a token-budget or no-longer-advertised selection.
- Give the `Working` and `Thinking` activity labels one shared shimmer clock so
  they cannot shimmer differently or freeze mid-row; retry, compaction, and
  provider lifecycle labels stay timer-only.

This candidate is built for local dogfooding; it is not a published release.

## [0.8.0] - 2026-09-25

See [release notes](docs/releases/v0.8.0.md) for changes, availability and current limits.

### Release safety

- Update both Rust lockfiles to rustls 0.23.45 for RUSTSEC-2026-0285, retaining
  dependency security checks rather than suppressing the advisory.
- Keep Python shutdown hooks and their acknowledgements ahead of a subsequent
  stdin EOF, while preserving the bounded drain for a non-cooperative hook.
- Retain Rust 1.86 compatibility in process event-bus response admission.

### Interaction

- Accept owner-bound `/subagents stop <name|all>` during active responses without
  blocking input or mistaking interruption acknowledgement for worker settlement.
  Show compact K/M/B/T token counts, visible stop guidance, and continuation rows
  aligned with Thinking, while preserving exact accounting and native scrollback.

- Offer first-run setup in the order API key, supported OAuth subscription,
  then local/self-hosted models. API keys use masked input and explicit review,
  with recoverable owner-private storage rather than hashes or an encryption
  claim. ChatGPT (Codex) and GitHub Copilot reuse host-owned device login;
  configured launches and noninteractive modes do not reopen onboarding.

- Silence routine session lookup/replay/fork progress during startup, including
  resumed launches. Keep input, setup, errors, and coordinated shutdown live.
  Fresh launches skip the unnecessary session-replay worker and full-config copy.

- Quiet routine reload output: remove the startup arming banner, queued-path
  chatter, and automatic success summaries. Only explicit reload commands get a
  completion summary. Host, worker-deferral, extension, provider-catalog, and
  watch-limit problems appear once per component/condition and reappear after a
  successful check clears them; skipped components retain their diagnostics.
  Resource/bootstrap and keybinding checks now use the same checked-component
  recurrence rule, without hiding explicit command diagnostics.
  Actual work losses are always reported, counting only discarded
  extension host requests. Automatic permission paths never open a picker or
  probe an unconsented replacement; explicit consent remains required.
  Watch/timing details remain available through `/reload --dry-run`.

- Hide the root elapsed clock while retry status is active, including after its
  countdown expires, while keeping the interrupt hint. Normal Working/Thinking
  elapsed clocks are unchanged.

- Ease the resting activity colours off the profile extremes: the dark theme's
  Working/Thinking label now rests at a slightly greyed off-white (about #f9
  instead of #fd) and the light theme's rests at a soft near-black (about #16
  instead of #00), each with the sweep ceiling moved to keep the travelling band
  and the margin dot's pulse at their previously pinned separations. The light
  bound is measured, not chosen: a 0.009 rest fails the pinned 1.7:1 worst-case
  cell separation for a blue identity after ANSI256 quantization, so the rest
  sits at 0.0085 and the light sweep ceiling moves 0.09 -> 0.095 to preserve the
  pinned 0.08 relative-luminance travel.

- Add the hidden `/debug` command (exact name only; absent from the popup and
  `/help`). It writes an owner-private `~/.octet/octet-debug.log` with the
  terminal size, every rendered line plus its visible width and the agent
  messages as JSONL, then reports the path; an in-flight run is read through the
  inspection snapshot, so the dump never disturbs the frame coalescer.

- Add `/session`, reporting the session file, id, title, head, entry and
  active-branch message counts, checkpoints, usage records, the token buckets
  and exact cost, and marking unknown exposure as a known subtotal instead of
  presenting it as exact.

- Add `/settings` (defaults, theme, images and default model/reasoning; the
  transport and editor padding are reported as read-only route/theme facts) and
  `/scoped-models` (ordered Ctrl+P scope persisted as the user `models` key),
  and `!command`/`!!command` local shell escapes that record their result as
  model-visible or explicitly excluded from model context.

- Withdraw the `/tree` and `/checkout` slash commands (maintainer decision) with
  their keybinding: the durable connector tree stays available through
  `octet sessions inspect`, and branch movement stays available through the
  session surfaces that own it. `/quit` is renamed `/exit`; `/quit` is no longer
  parsed.

- Restore the full `/model` picker for a launch that pinned a built-in model.
  Startup narrows the catalog to the selected route for latency, and the picker
  rendered that narrowed catalog, so a DeepSeek launch listed only DeepSeek
  models even with other provider credentials in the environment. The picker now
  completes the deferred provider inventories (and refreshes the scoped-model
  cycle) before it lists, and an active run defers the picker to the next idle
  boundary instead of showing a partial provider list.

- Slash commands now work while a run is active. `/help`, `/cost`, `/cache`,
  `/tree`, `/context`, `/update`, `/name`, `/export`, `/extensions status`,
  `/extensions inspect` and `/thinking` render or open their real surface
  immediately instead of being queued to the next idle boundary; `/model` opens
  its picker inline. Previously these commands were unusable during streaming.
- Add platform-aware discoverability hints, including the queued-follow-up edit
  binding (`option+↑` on macOS, `alt+↑` elsewhere).
- Keep startup silent and the composer editable while extensions initialize;
  defer model-context notices until terminal teardown.
- Let `/goal` mutate and report the durable goal during an active response.
- Give activity shimmer a complete sweep and rest phase. Working and Thinking
  share a model-derived hue family; neutral identities stay neutral in both
  appearances. Physical-terminal appearance remains a separate acceptance gate.
- Display plain-dollar footer costs without weakening durable uncertainty or
  budget accounting.
- Redesign the `/subagents` panel: group by state with counts, collapse terminal
  groups by default, aligned columns with a header row, bounded failure reasons
  and bounded rendering.
- Stop rendering absence as text in subagent rows: `no ceiling`,
  `inherited no ceiling` and every `?` placeholder are gone, elapsed and token
  counts are human-formatted, and no internal Rust API or operation id is
  printed into the transcript.
- Add the interactive live-reload supervisor: the prompt samples the skill,
  prompt, theme, context-file, keybinding, settings, and extension roots in use
  plus the resolved executable (`current_exe()` re-resolved every poll) on a
  bounded 1000 ms metadata poll, and applies a pass only at the idle prompt in
  the order host eligibility/consent → resources → extensions; an admitted host
  replacement supersedes in-process rebuilding. Saves are debounced (200 ms,
  2 s hard ceiling) and coalesced; a pass is never admitted while a run owns the
  session, so evidence queued behind a busy boundary is applied at the next idle
  prompt. It is enabled silently by default; `/reload --dry-run` exposes watch
  settings and per-layer details, while automatic passes omit success summaries;
  `reload`, `reload_poll_ms`, `reload_debounce_ms`, and `reload_max_files` are
  user-level settings, `/reload --dry-run` previews a pass without changing
  anything, and `/reload --force` takes one at the idle boundary while previewing
  possible interruptions from currently pending host requests and workers.
  Plain `/reload` rebuilds resources/extensions without selecting the host layer.
- `/reload` now also reloads the **host binary**. When the executable on disk
  changed, the candidate is validated first with a side-effect-free internal
  probe (`--internal-reexec-probe`, which initializes no provider, extension,
  workspace or network work) and its generation is re-stated immediately before
  the jump, so a build replaced mid-probe is never executed. The reload happens
  at an idle boundary only and refuses while a model turn, tool call, shell
  child, effect approval or in-flight session write is live; live delegated
  workers become an explicit opt-in that flushes their durable records and
  detaches them for reattachment. The new image keeps the same PID and resumes
  exactly the session that was running (`--resume <id>`), extension children are
  stopped through the bounded path, session-lock descriptors are `CLOEXEC` so
  the new image can re-lock its own session, and nothing ever locks the
  executable — so several panes and a running `serve` reload independently.
- Late `providers/register`, `providers/update` and `providers/unregister`
  take effect in the running session without a reload. Credentials,
  endpoints, headers, transports, callbacks and OAuth payloads stay host-owned;
  a late registration never overrides a built-in the user did not opt into;
  pricing and capability validation still run before a model becomes routable;
  and an in-flight request is never mutated — the change applies at the next
  request boundary.

### Codex subscription behaviour

- Make the deliberate 272K Codex request cap visible and overridable instead of
  silent. It remains the default because OpenAI recommends a 272K Codex context
  limit, usage above 272K is double-priced, and oversized long-running sessions
  can drop the Codex websocket.
- Emit at most one bounded clamp notice per session, for the effective model
  only, and only when that model is a Codex route.
- Add an explicit opt-in override (`--codex-context-window`, the equivalent
  environment variable, and the reasoning/effort menu) that requires the
  Pro/ProLite entitlement and a separate acknowledgement naming both
  consequences; un-acknowledged or above-entitlement values fail closed.
- Record cost and usage above 272K as uncertain rather than exact
  (`Session::record_usage_uncertainty`), since the whole request is priced at the
  long-context tier rather than only the excess.
- Keep WebSocket connection-limit retries in the host's physical-attempt budget
  instead of secretly resending inference in the transport. Stored-response
  cursor recovery is bounded; ordinary non-stored Codex responses cannot be
  resumed by inventing a new request after output has begun.

### Subagents

- Keep the live worker roster in bounded pinned chrome above the composer while
  any retained worker is active, independently of root-run activity and app-owned
  history navigation. Hide it after settlement while retaining telemetry,
  accounting, and the `/subagents` inspector. Metric changes do not dirty history;
  raw first-party orchestration calls/results and worker transitions add no
  automatic transcript or copy notices. Ordinary failures and approvals retain
  their existing owners. Native-scroll regression coverage retains interleaved
  commands, results, and answers exactly once, rather than clipping them into a
  pending-tool preview.
- Bound the strip to one third of terminal height and available space; omitted
  rows point to `/subagents`. Native terminal scrollback cannot overlay chrome.
- Stop truncating worker model ids: worker/state/model are mandatory columns
  that never ellipsize, optional metrics drop first, and the compact fallback
  line still prints the full model. The `/subagents` picker header is now the
  stable surface name `Subagents`, with counts in rows/status rather than the
  title.
- Remove the `completed with warnings` state from the TUI: both completed
  variants render the same `✓ completed · <duration> · <rate> tok/s` line and
  the same success role, and the counts stay in the model for exit status and
  telemetry. A completed turn with live workers now shows one subdued
  `subagents are running; inspect them in the /subagents menu.` line.
- Add bounded `/subagents open-all tmux|herdr` planning and multiplexer adapters.
  Product pane execution is disabled for all workers and the parent until the
  host provides atomic writer claim/settlement. An opaque handle or fresh
  launchability snapshot is not a writer lease; adapters are directly tested,
  not qualified live handover.
- Keep API 0.2 child model execution inherited. Requested provider/model/reasoning
  metadata does not establish multimodel execution; unsupported selections fail
  closed rather than silently selecting another model.
- Workers survive the parent turn: durable child records are owned by the
  session rather than the run, with reattachment on a later turn and an explicit
  parent wait. A detached worker parks at the approval boundary in a bounded
  state instead of mutating unattended.

### Providers, codecs and tools

- Refresh reviewed models.dev metadata to 895 pricing routes, 385 canonical names,
  and 916 capability routes; preserve the [source provenance](crates/octet-ai/models/SOURCES.md).
  Builds and runtime remain offline, direct DeepSeek schedule pricing remains
  excluded/unknown, and public metadata does not establish live inference acceptance.
- Decode OpenRouter's per-model `reasoning` object (`mandatory`,
  `default_enabled`, `supported_efforts`, `default_effort`) instead of treating
  the presence of a `reasoning_effort` parameter as proof that reasoning is
  optional. A mandatory model no longer receives a disabling
  `reasoning.effort=none`, which OpenRouter rejected with HTTP 400; it receives
  the endpoint's default effort or an advertised exact effort, and a saved `off`
  is clamped to a supported choice. Advertised effort strings are sent verbatim.

- Share a 64 MiB / 32-file Bash spill budget per owner, including active captures;
  evict oldest files and move disk capture/cleanup off the async path. Keep the
  16 MiB per-stream cap and distinguish expired, partial, and complete spills.
- Advance Responses replay/capacity projections by suffix, retain control-queue
  reservations through delivery, and cap combined extension schemas at 4 MiB.

- Decode the reasoning a provider advertises through accepted request parameters
  (`supported_parameters` containing `reasoning`, `reasoning_effort` or
  `reasoning.effort`). It was read as an undecodable assertion, so a newly
  released gateway model had thinking permanently Off until the pinned metadata
  snapshot was refreshed and the binary rebuilt. An explicit negative assertion
  still wins.
- Name the reasoning-only failure separately: a turn that finishes normally with
  thinking but no answer text now reports "provider returned reasoning but no
  answer text" instead of the generic empty-content message.

- Reserve bounded headroom (1% of the window, 256–4096 tokens) between octet's
  input-token estimate and the provider's own count when deriving a request's
  output cap. Sizing the request as exactly `window − estimate` put real local
  servers one token over the limit, which they reject outright.
- Recover a size rejection instead of failing the turn: local compaction now runs
  and the request is retried for HTTP 400/413/422 responses that carry a numeric
  provider code, which previously took the permanent-failure branch. Named
  policy, auth, quota and rate-limit rejections still fail closed without
  compacting, and a session with nothing reducible reports the limit.

- Add strict JSON-schema and grammar/regex request declarations, and remove a
  declared-but-unimplemented deferred-tool capability claim. Grammar custom-call
  decoding and history/result replay remain incomplete; declarations alone do
  not establish complete codec support.
- Land Anthropic caller-beta merge, refusal fallbacks, and the Mistral
  Conversations finish/base-URL classification fixes that previously failed
  assertions.
- Wire the agent's optional service tier into live `ResponsesOptions`, gated by
  the declared route capability, and select it through `/fast`. Tier-aware
  settlement applies the declared Codex tariffs to the provider's echoed tier,
  and the conservative reservation prices the same worst case; the command
  retains its durable priority-uncertainty marker and does not clear historical
  exposure.
- Let sparse built-in inventories inherit the pinned models.dev **input
  modalities and context/output limits** — but only where the endpoint asserts
  nothing. Direct DeepSeek publishes identifiers only, so `deepseek-flash`
  (V4.1 Flash) previously registered without image input (attachments failed
  closed with "Image input is unsupported") and with the generic 128K/64K
  placeholder instead of its documented 1M context / 384K output, silently
  capping the usable window. An endpoint that asserts any modality or limit —
  including an explicit text-only list or a smaller window — still wins, a
  snapshot entry that declares text-only input keeps that decision
  (`deepseek/deepseek-v4-pro` stays text-only), and a model absent from the
  pinned record keeps the conservative fallback.
- Add an assistant-message frame encoder/reducer and a bounded partial-message
  journal. Partial recovery observations do not replace authoritative usage or
  tool-outcome records.
- Add typed provider declarations covering sampling params, headers,
  `vllmPriority`, `supportsMaxOutputTokens`, thinking-token budget fields,
  `$var` chat-template interpolation and bearer-token credential aliases.
- Tool behaviours: bash output spill, bounded interval checkpoints and the
  session environment contract; original-file non-overlapping multi-edit with
  legacy normalization; preview coalescing and unanimous finalized-result batch
  termination; invocation-memo and deferred-handle primitives; summarization
  retry distinct from compaction failure. The agent tool surface stays
  `read`/`write`/`edit`/`bash` with opt-in ripgrep-backed `search`; `ls`, `find`,
  and `grep` operations use `search` (rg) or `bash`.

### CLI and sessions

- Remove the 64 MiB session-catalog cliff; use indexed substring search,
  targeted lookups, and bounded maintenance batches without capping total disk.

- Add `--powershell` (additive Windows `powershell` opt-in, reported inert on
  other hosts, conflicting with an exclusive `--tools`/`--no-tools` list) and
  ordered `--models` patterns that resolve a literal `provider/model`/bare-id
  reference exactly before globs, preserve the requested order and keep each
  `:level` reasoning suffix.
- Add `--mode json` session-event JSONL, `--list-models` search,
  `--session-id`/`--name`, sequential positional prompts with bounded `@file`
  and media expansion, and piped stdin in every mode.
- Keep `--no-session` transcripts ephemeral while preserving usage from every
  RPC session; failed accounting appends retain private accounting-only recovery
  and retry idempotently without restoring conversation data.
- Add incremental session/entry search backed by a disposable SQLite projection
  that re-reads only changed transcripts, and fail-closed catalog publish gates
  (checksum, count, schema, minimum client version, required providers) with an
  immutable install path.
- Add single-file HTML session export with a script-free escaping CSP.
- Accept a session name when starting `octet serve`.
- Split configuration diagnostics into a dedicated module without behaviour
  change.

### Telemetry

- Move optional JSONL writes to an ordered 256-record / 1-MiB worker with
  observable loss/failure and bounded lifecycle drain; durable accounting stays
  authoritative and unchanged.

- Add a callback-based, vendor-neutral telemetry substrate
  (`TelemetryContext`/`TelemetrySpan`, NOOP and InMemory implementations,
  serializable typed schema and a span-assertion harness) with no global and no
  exporter, and wire run, turn, provider-request, provider-stream, tool,
  compaction, summary and delegation span boundaries.
- Keep accounting independent of observation: the JSONL observer and the
  fail-closed uncertain-usage path are unchanged, and identical business outcomes
  are asserted under NOOP and InMemory.
- Fold tool and summary usage into totals with a cache-hit rate and a distinct
  `cacheWrite1h` bucket, preserving uncertainty.

### Editor and TUI

- Make Option+Up/Alt+Up recall the newest editable pending message instead of
  only local follow-ups: a Ctrl+S live-steering submission is now withdrawn from
  the agent before persistence, releasing its reserved control budget, so the
  recalled text is never also delivered. Recall is arbitrated by that
  submission's own receipt rather than the delayed delivery event, so it fails
  closed once the agent has claimed the input for the session. Sticky `/answer`
  input remains deliberately non-retractable, and the pending-state hint only
  advertises the affordance while an editable entry exists.

- Render Pi-style `$…$`, `$$…$$`, `\(…\)`, and `\[…\]` math through the
  existing LaTeX engine, preserving currency, code, incomplete expressions, and
  unsupported source rather than displaying partial art. Broaden bounded Mermaid
  flowcharts with upstream layout, subgraphs, reverse directions, node lists,
  and labelled links. Unsupported diagrams retain source with a visible reason;
  diagrams wider than the viewport show source rather than cropped art. Explicit
  grok-mermaid oracle fixtures do not establish full Pi renderer equivalence.

- Keep active inspection and consent panels inside the polling run loop. Use
  shared semantic snapshots and renderer-private layout, with post-write
  revision-fenced geometry and consent receipts instead of a long-held input lock.
- Bound undo and redo to 64 snapshots / 4 MiB each, with oversized-edit barriers.

- Add reusable keybinding parsing/conflict detection, editor undo/redo,
  kill/yank, word/line actions and OSC 133 zone primitives. Product key dispatch,
  user-binding loading and viewport prompt jumps remain incomplete.
- Preserve focus reporting with focus-out interaction reset.
- Port LaTeX rendering (symbol tables, parser, fraction/operator/matrix layout)
  and add a bounded, self-contained Mermaid box-drawing engine; both fail closed
  on unsupported syntax instead of misrendering it.

### Herdr integration

- Report octet's agent lifecycle to [Herdr](https://herdr.dev) from the
  interactive frontend, so a pane running octet appears as a first-class agent
  in the sidebar, agent list, state rollups, notifications, and waits. The
  implementation follows Herdr's official Pi integration (integration version
  9) on the documented custom-agent surface: `pane.report_agent` for semantic
  `idle`/`working`/`blocked` state, `pane.report_agent_session` for session
  identity, and `pane.release_agent` on exit, with a strictly increasing `seq`
  seeded from wall-clock milliseconds so a restarted process cannot report
  stale sequence numbers.

  State comes from the run stream itself — ready at startup, `working` from the
  moment a prompt is accepted, `blocked` while an approval or input prompt is
  on screen, and `idle` when the run settles — so the pane cannot disagree with
  octet. Reporting is bounded to one 500 ms socket attempt plus one 1500 ms
  retry, silent on every failure, and active only when `HERDR_ENV=1` with a
  pane id and transport, so it is a complete no-op outside Herdr. A slow Herdr
  server can add at most the bounded delivery attempts at a report boundary.
  Only the opaque session id (never a
  transcript path) and the bounded on-screen approval prompt leave the process;
  display-only presentation stays with `herdr pane report-metadata`.

  Transports are direct socket IPC on `HERDR_SOCKET_PATH` (Linux/macOS) and the
  documented `HERDR_BIN_PATH` CLI wrapper (Windows), each as an argv list with
  no shell.

- Resume octet sessions in restored Herdr panes without a Herdr-side change,
  using Herdr's documented plugin surface instead of a native agent kind. While
  octet runs in a pane it keeps one small owner-private record (`pane id`,
  Herdr session scope, session id, cwd, session-store root, workspace, pid) under
  `~/.octet/herdr/panes/`, and
  `octet herdr install-plugin` generates a manifest whose single `[[startup]]`
  hook is `octet herdr restore` — no events, actions, panes, build steps, or
  state outside octet's own directories. Herdr runs startup hooks after it
  restores the session and the API socket is ready, so that pass reopens each
  recorded pane with `octet --resume <id>` and the recorded `--session-dir` and
  `--workspace` scope.

  The pass is bounded and fails closed: records are capped, size-limited,
  owner-private, written atomically, pruned after 14 days or when their pane is
  gone, and only resumed when the record belongs to this Herdr session, the pane
  still exists, it currently hosts no agent, and its directory still matches the
  recorded one. At most 16 panes per start; every skip is reported with its
  reason; session ids are token-validated and absolute paths are bounded and
  shell-quoted. The pane-list response is drained concurrently and capped. The
  record survives a Herdr server stop (`SIGHUP` to the pane, measured against
  Herdr 0.9.0) and is dropped on a deliberate exit, so a later restore never
  resurrects a session the user closed. See [`docs/herdr.md`](docs/herdr.md).

### Repository tooling and docs

- Add `scripts/changelog.py` (release extraction and link repair) and
  `scripts/create-source-archive.py` (deterministic source artifact).
- Add Codex context documentation; remove obsolete planning, comparison, and
  task-handoff records from public documentation. Remove accidentally tracked
  Swift build caches and reject them in deterministic source archives.
- Keep benchmark measurement verdicts separate from release approval: an external
  PID snapshot cannot satisfy inference attribution or cross-platform review.

### Candidate limits

This candidate is not published or live-qualified. Native companion apps remain
source-only; signed installation, real-terminal behavior, Windows PowerShell,
and live-provider acceptance are not established by deterministic fixtures.
The [candidate notes](docs/releases/v0.8.0.md) describe remaining scope limits.

## [0.7.6]

- Fix stale models.dev integration for discovered names, context/output limits
  and advertised thinking, preserving explicit provider API assertions.
- Recognize DeepSeek V4.1 Flash correctly; refresh and review the checked-in rich
  metadata snapshot before release, without network access during compilation.
- Remove the duplicate working-directory line from the TUI splash while keeping
  the footer path and narrow-terminal behavior.
- Align active first-party distribution versions to 0.7.6; independent API,
  protocol, example and historical-release identities remain unchanged.
- See [hotfix notes](docs/releases/v0.7.6.md) and the exact GitHub release for
  validation, signed publication and public-install results. No all-provider
  or live-inference qualification is claimed.

## [0.7.5] - 2026-09-11

- Repair stale startup splash rows after model switches.
- Keep activity and compaction shimmer on a monotonic clock, skipping missed
  frames instead of replaying them in bursts.
- Group live model, reasoning, context and cumulative cost in the default footer,
  with the working directory right-aligned and safe narrow-terminal fallbacks.
- Add honest installer/updater progress on stderr; success requires the exact
  installed version, not just a completed download.
- Add a quiet, non-blocking startup update notice and a rich, scrollable
  `/changelog` command with a splash reminder.
- Avoid misleading project-skill warnings when the same root is already a user
  skill directory, without relaxing actual project trust checks.
- Correct stale publication-status documentation shipped with 0.7.4, and retain
  linked public reference files across documentation package layouts.
- Bound installer-script downloads independently of curl's version, and bound
  both pre- and post-install version checks by time and output size.

## [0.7.4] - 2026-09-11

- Add host-qualified Codex interrupted-inference recovery and separately paced,
  cancellable pre-send waiting without restarting tasks or replaying committed tools.
- Preserve accepted controls and durable failed-attempt usage uncertainty across
  resume, including truthful known subtotals and hard-budget enforcement.
- Keep API-wait presentation responsive with attempt-owned rollback,
  operation-scoped recovery status and length-adaptive activity shimmer.
- Fix three medium-severity Vitest alerts and scoped maintenance (#411).
- Overlap independent provider discovery in bounded batches and reduce usage
  reconciliation allocation; retain [matched measurement limits](docs/benchmarks/v0.7.4-performance.md).
- Improve ANSI256 Light added-diff contrast and limited-colour splash logos.
- Publish signed native/Serve packages and four exact-host executable bundles.
  Independent APIs and schemas remain unchanged; registry channels are separate.
- See [release verification and the documentation correction](docs/releases/v0.7.4.md).
  A subsequently reported Ghostty-to-SSH model-switch regression is addressed
  in 0.7.5; weeks-long endurance and full Codex parity are not claimed.

## [0.7.3] - 2026-09-09

See the [release notes](docs/releases/v0.7.3.md) for scope and remaining terminal
qualification limits.

### Fixed

- Apply contrast-aware 256-colour approximation across the full TUI while
  preserving true-colour, ANSI16, and no-colour behavior.
- Preserve streaming work and stabilize Markdown rendering, compact subagent
  presentation, model picker navigation, and composer chip editing.
- Enforce safe tool ordering and repair the Browse setup-test readiness race.

### Added

- OpenRouter batch handling and Luna model support.

### Changed

- Align product, SDK, Serve, and first-party bundle versions to 0.7.3.
- Resize and genuine historical updates can still clear/replay scrollback;
  full terminal qualification remains open.

## [0.7.2] - 2026-09-08

See the [release notes](docs/releases/v0.7.2.md) for verified signed Native/Serve
publication, public-install checks, and known terminal-protocol limits.

### Fixed

- Defer the first branded startup frame until model/setup state, workspace, and
  appearance are resolved; start its animation at readiness.
- Consume late terminal background replies and recognized fragmented OSC 11
  bodies before editor input; retain bounded Escape-key ambiguity handling.
- Navigate path and `@` file completions with Up/Down and accept the selected
  result with Tab, preserving editor movement when no menu is visible.
- Indent subagent transcript rows beneath their heading and omit per-worker
  call counts without removing state, tokens, cost, or underlying telemetry.
- Limit terse Bash command previews to three visual lines plus a hidden-line
  count; Ctrl+O reveals the full retained command.

### Changed

- Full access implicitly trusts explicitly enabled executable extensions;
  installed bundles remain disabled by default. Safe mode retains its
  permission/execution boundaries and does not inherit implicit trust.
- Align product, SDK, Serve, and first-party distribution versions to 0.7.2;
  independent extension APIs and native-host protocol versions are unchanged.

## [0.7.1] - 2026-09-07

See the [release notes](docs/releases/v0.7.1.md) for publication and
verification evidence.

### Added

- Added first-install Auto/Light/Dark appearance selection with preview and
  persistence, plus `/theme` for later changes; configured and non-TUI launches
  do not reopen onboarding.

### Fixed

- Made startup presentation capability-safe and refreshed the welcome prefix
  across animation, overlays, theme/model changes and redraws (#367).
- Suppressed duplicate prompt replay in interactive plain mode while preserving
  explicit one-shot and piped prompt output (#368).
- Added bounded retries across trusted release GET headers and streamed bodies
  for transient timeouts and HTTP 408/429/5xx; validation failures are never
  retried (#369).
- Preserved application-viewport scroll anchors across coalesced tool and
  subagent updates, including worker-roster insertion and reflow.
- Selected model-supported reasoning for local compaction and auxiliary requests
  instead of forcing unsupported Off; explicit user selections remain unchanged
  (#370).

### Changed

- Use the canonical `skaft-software/octet` source/release repository after #366;
  immutable v0.7.0 assets retain their original signing identity.
- Aligned product, SDK and first-party extension distribution versions to 0.7.1;
  extension API and native-host protocol versions are unchanged.

## [0.7.0] - 2026-09-06

### Added

- Added repository documentation, usage examples, and the octet Brand Kit.

### Changed

- Adopted the octet product name, `octet` and `octet-host` binaries,
  `octet-*` packages, `OCTET_*` settings, and `.octet` directories.

### Fixed

- Fixed provider-start retry handling, typed-media validation, and local
  compaction replay.
- Fixed narrow terminal layouts and duplicate startup rendering.

## [0.6.7] - 2026-09-02

### Fixed

- Keep remote Streamable HTTP MCP unavailable by default: only the one-shot
  process-owner CLI opt-in `--experimental-streamable-http-mcp` can pass the
  gate to the first-party bridge. Project/global/session configuration,
  environment, and manifests cannot activate it; stdio MCP is unaffected.
- Fully neutralize Git clean/process filters from repository, worktree, and
  included configuration during Serve status refresh. Filter names that cannot
  be represented safely, including invalid UTF-8 and delimiter-bearing names,
  now fail closed before Git status can execute them.

## [0.6.6] - 2026-09-02

### Added

- Add deterministic, no-lifecycle npm packages for the native launcher and
  supported runtimes, with exact global-update detection and protected trusted
  publishing/recovery gates.
- Add an offline Homebrew formula generator and protected tap handoff driven by
  signed immutable release metadata.
- Print an exact, shell-quoted session resume command after clean interactive
  exits and shutdown signals.

### Changed

- Cap authenticated Codex model and request budgeting at 272,000 tokens by
  default, matching Pi. Smaller provider windows remain authoritative while
  larger advertised maxima remain available as discovery metadata.
- Preserve DeepSeek V4 discovery context and modality metadata instead of
  replacing it with placeholder defaults.
- Reuse the durable session catalog and an incremental transcript-search index
  when Serve lists and searches session history.

### Fixed

- Bound environment credential reads, validate repaired tool arguments against
  their selected schemas, and retry transient remote reads with bounded
  backoff.
- Detect half-open Responses WebSockets, report closure progress, and prevent
  search subprocesses from stalling on saturated output pipes.
- Classify subagent turn-limit settlement as bounded completion and preserve the
  parent result.
- Isolate ordinary repository-local clean filters from status refreshes and
  serialize durable goal updates across independently opened processes.
- Resolve and launch Serve's GitHub CLI through a hardened environment and clean
  up its complete descendant process tree after cancellation or timeout.
- Keep web session selection and stalled resynchronization monotonic and
  bounded, without replaying stale snapshots over newer live state.
- Reconcile inline terminal scrollback after long transcript shrinkage without
  corrupting retained rows.

## [0.6.5] - 2026-08-30

### Added

- Added `/answer [instruction]` to request an immediate final response from
  gathered evidence. Idle runs begin without tools; active runs persist the
  directive at the next safe boundary and expose no tools thereafter.

### Changed

- Keep authenticated Codex routes on the provider's full advertised context
  window by default for in-context learning; `compaction.max_active_tokens`
  (for example 272000) optionally constrains the active working set.
- Reconcile `/context` with the agent's provider-reconciled semantic context
  breakdown, including adaptive capacity grids for large model windows.
- Raise the default model-visible tool-result cap from 16 KiB to 50 KiB while
  retaining bounded output capture and policy enforcement.

- Keep the Responses Lite wire contract serial while host admission overlaps
  explicitly parallel-safe pure/workspace-read calls; arbitrary shell and
  mutating effects remain ordered.
- Reduce Codex tool-loop latency by enabling provider-advertised tool-call
  batching without relaxing host-side effect ordering.

### Fixed

- Correct GPT-5.6 Luna and Terra pricing across the OpenAI, OpenCode, and Codex
  pricing tables to OpenAI's published standard costs ($0.20/$1.20/$0.02/$0.25
  and $2/$12/$0.20/$2.50 per million tokens, with matching long-context tiers);
  previously displayed session costs were about 5x too high for Luna.
- Preserve GPT-5.4 Pro and GPT-5.5 Pro long-context pricing on Codex routes,
  exclude OpenAI's rejected base `gpt-5.6` alias, and refresh the checked-in
  models.dev pricing and display-name snapshots together.
- Retire a preferred Responses WebSocket before publishing a pre-generation
  connection-lifetime failure and retry that request through the HTTP fallback;
  post-generation disconnects remain terminal.
- Use the active model's adaptive accent consistently for slash completion,
  model selection, session resume, and other picker focus controls.

## [0.6.4] - 2026-08-30

### Added

- Added the optional extension `runtime_commands` negotiation feature so a
  compatibility process can expose its bounded initialization-time slash-command
  catalog without duplicating those names in a generated manifest.
- Added source-fingerprinted, version-pinned Pi compatibility links for the
  supported Pi 0.84.4 profile. Generated links remain disabled and untrusted
  until explicitly activated, reject changed source before import, and report
  unsupported compatibility surfaces instead of silently accepting them.

### Changed

- Estimate the complete next provider request before each model turn. The default
  compaction threshold now uses the context window with a fixed 16K coding-turn
  reserve (or larger advertised reasoning floor), while the provider's advertised
  output maximum remains the request ceiling and is reduced only by actual
  remaining context capacity.
- Reworked the default terminal presentation around one responsive horizontal
  grid for transcript blocks, prompt cards, composer, footer, and pickers.
  Submitted prompts retain their original model provenance colour, queued
  steering stays bounded, and narrow approval and picker layouts preserve the
  action or identity needed to use them safely.
- Keep one authoritative `Working` activity row until the owning run settles,
  including after public assistant text, and distinguish normal completion,
  completion with warnings, interruption, and failure after animation stops.

- Advance context-capacity estimates incrementally across appended session
  messages and re-anchor them to authoritative provider usage, avoiding repeated
  whole-history reconstruction during ordinary multi-turn and tool-heavy runs.
- Reduced avoidable transcript reflow and kept active status invalidation local
  while preserving the complete retained-frame renderer contract.

### Fixed

- Never execute a tool call whose arguments may have been cut off by the
  provider's output-token limit. Ygg retains the call envelope, discards partial
  arguments, persists a paired failure result, and asks the model to reissue the
  complete call.
- Serialize durable goal-store transactions across independent handles and
  revalidate the lock identity before publication so concurrent goal turns and
  revisions cannot overwrite one another.
- Keep bounded actionable reasons visible for collapsed run and tool failures,
  preserve warning outcomes instead of painting them as success, and prevent an
  approval from being confirmed when its selected action is not visible.
- Bound nested Pi extension-manifest traversal and fail closed on incomplete or
  replaced migration inputs.

## [0.6.3] - 2026-08-28

### Added

- Added opt-in owner-only `ygg.telemetry.v1` JSONL measurements for model
  latency/TTFT, disjoint usage buckets, retries, tool timing, compaction, and
  bounded run outcomes without recording prompts, arguments, results, or
  provider payloads.
- Added `ygg doctor` for read-mostly local prerequisite, provider, and model
  visibility diagnostics.
- Added reproducible systems-benchmark and Harbor analysis tooling and a compact
  checksummed Terminal-Bench 2.1 evidence package with explicit
  surrogate-adjudication limits.

### Changed

- Render active `Working` and fixed `Thinking` headers with a bounded,
  model-adaptive shimmer. The latest explicit reasoning heading and plain
  `Ctrl+O` hint now stay on the subdued detail row without changing geometry.
- Reconciled provider, session, telemetry, and Harbor token accounting around
  disjoint uncached/cache-read/cache-write input buckets. Harbor totals now
  include every durable usage operation and cache writes without adding the
  cache-hit detail twice.
- Gave every physical retry its own request timing/TTFT lifecycle and labeled
  telemetry usage as request, operation, or cumulative scope.

### Fixed

- Fail normal terminal responses that contain no visible text, media, or tool
  call instead of silently reporting success.
- Keep a truthful TUI lifecycle for every active run: open with `Working`,
  promote to `Thinking` only on actual reasoning deltas, use visibly streaming
  assistant text as the liveness signal, restore `Working` after a completed
  turn when the run continues, and settle only at the authoritative run
  boundary.
- Replace transient tail activity with an incoming tool row without invalidating
  and reflowing long transcript history, keeping renderer animation and composer
  input responsive at the reasoning-to-tool boundary.
- Removed timer-only repaint from otherwise static transcript markers;
  intentional `Working`/`Thinking` animation invalidates only its active status
  block and leaves tool/shell dot cadence unchanged.
- Decode observed Codex Responses error envelopes with a nullable provider code
  while still rejecting a missing or nullable error message.
- Retire a preferred Responses WebSocket before publishing a provider
  connection-lifetime error, then allow a bounded pre-generation retry through
  the HTTP fallback instead of racing the poisoned socket. Never replay a
  request after generated output has been observed, and close the active socket
  when its owning response is dropped.
- Keep repeated-call diagnostics out of same-response batches and preserve
  machine-readable JSON tool results.
- Run Harbor's Docker adapter in an independently cleanable process group,
  perform TERM→KILL descendant cleanup before artifact conversion/finalization,
  and fail closed if process death cannot be verified.

## [0.6.2] - 2026-08-27

### Fixed

- Preserved bounded subagent summaries, fatal errors, usage, and the complete
  sibling roster when owning-run cleanup removes the live host tree. Missing
  active workers now settle as explicit `orphaned` diagnostic records instead
  of making every worker disappear; an identical explicit retry can replace its
  matching orphaned cache entry.
- Skipped Apple Foundation Models `/v1/models` discovery when the optional
  `fm serve` health probe says the local server is absent, eliminating the
  routine loopback connection warning without hiding errors from other custom
  providers.
- Kept the interactive composer's hardware cursor visible in both retained-frame
  renderers, including after panels, resize replays, renderer resumes, and
  extreme narrow-width fallback rendering.
- Rendered every bounded subagent in the persistent TUI transcript event and the
  compatibility activity path regardless of ordinary `Ctrl+O` tool disclosure.
- Added the one-time v0.6.2 managed-package migration so v0.6.0/v0.6.1
  first-party bundles and Ygg Serve refresh to the exact hotfix version during
  startup.

## [0.6.1] - 2026-08-27

### Changed

- Replaced Ygg's experimental semantic-commit/native-scrollback renderer with a
  direct Rust port of Pi's retained-frame algorithm. First render, changed-range
  updates, append/shrink, resize and offscreen-change replay, cursor bookkeeping,
  CSI 2026 framing, and Kitty cleanup now follow the pinned Pi control flow;
  terminal-owned resume eagerly materializes its complete active branch.
- Removed dead public API surface across the workspace and obsolete unused TUI
  compatibility adapters in `sexy-tui-rs`.
- Flattened the internal tool trait stack: `Tool` became the object-safe
  `ErasedTool` form and `TypedTool` was removed.
- Retired the `show_turn_cost` flag.
- Disabled `/theme` and theme-file discovery; v0.6.1 exposes only the compiled
  default theme in terminal and graphical Serve surfaces.
- Hoisted protocol helpers that were duplicated across crates into `ygg-ai`.
- Fixed concurrency bottlenecks so `Session::persist` and `DelegationManager`
  journal writes no longer block under load.
- Removed roughly 19 GB of accumulated workspace junk from the repository tree.
- Fixed documentation drift: subagent worker tool-scope and limit wording in
  `README.md`, `docs/extensions.md`, and `docs/design/ygg-agent.md`, plus the
  supported-fixes target in `SECURITY.md`.
- Added a zero-token `ygg migrate pi --dry-run` inventory that resolves bounded
  Pi user/project packages and resources, hashes source and lock inputs, parses
  JavaScript/TypeScript API use without executing package code, and emits human
  or schema-versioned JSON compatibility classifications.
- Added `ygg pi install`/`list` and the persistent `ygg-pi-compat` subprocess for
  explicitly trusted local Pi tools, commands, lifecycle, notification, input,
  and confirmation compatibility.

## [0.6.0] - 2026-08-23

### Added

- Deferred tool-schema loading, ported from Pi's deferred-tools design:
  tool results can now carry `added_tool_names` — the set of tools that became
  available as a consequence of that execution (for example an extension or
  MCP server registering tools on first use). Models advertising the new
  `deferred_tool_loading` capability stop re-sending announced schemas in the
  static request tool set; providers without the capability are unaffected.
  This is the load-bearing prerequisite for lazily registered extension and
  MCP tool schemas.
- Delegated workers now expose a bounded rolling `recent_tools` activity ring
  (last six tool calls with flattened argument summaries, host timestamps, and
  an error flag) on every `agent/list` record, and the `/subagents` picker rows,
  inspect detail, and headless list render the latest action live — so each
  worker answers "what is it doing right now" without opening its transcript.
- Terminal-gate rejections (`CandidateRejected`) now carry cumulative session
  cost alongside run cost, so delegated-worker spend tracks token usage between
  accepted turns instead of jumping at settlement.

- Added checksum-verified, bounded, atomic executable-extension bundles for the
  small first-party release catalog and offline/local archives. `ygg extension
  install`, `list`, `update`, and `remove` now manage API `0.2` bundles without
  enabling or trusting them; managed nested skills are discoverable but remain
  inactive until explicitly loaded.
- Added deterministic release packaging, complete tracked-file inclusion checks,
  local install/remove smoke coverage, and post-publication install/update smoke
  coverage for every catalog bundle.
- Added an interactive `/extensions` installed-bundle activation menu that
  persists only enablement, never trust, rebuilds the extension host at the idle
  boundary, and becomes read-only when a higher-precedence activation source is
  authoritative.
- Added a native `/subagents` worker browser with arrow-key selection,
  owner-bound authoritative live refresh, stable-ID focus, and scrollable,
  owner-authorized read-only delegated transcripts.
- Added `subagent_continue` to the first-party `ygg-subagents` extension:
  it steers an active worker through `agent/message` or resumes a settled
  worker as a new run of its durable session through `agent/follow_up`. A
  resumed worker keeps its conversation context, and the host clears the
  stale completion timestamp and re-anchors an elapsed wall deadline so the
  new run owns a fresh budget.
- Workers in `ygg-subagents` can now be granted `edit`, `write`, and `bash`
  per spawn through the spawn `tools` list. The host's scoped tool snapshot
  is the enforcement boundary; the default remains the read-only
  `read`/`search` pair.
- Cargo-installed binaries now embed the text documentation and materialize a
  versioned `share/ygg/` tree that refreshes after Cargo-channel updates.

### Changed

- Subagents now inherit the parent's full standard tool scope (`read`, `search`,
  `edit`, `write`, `bash`) by default, matching Claude Code's Task workers;
  pass `tools: [read, search]` to keep a worker read-only. Worker prompts,
  profiles, the spawn schema, and skill guidance were updated accordingly.
- The interactive composer's slash-command list refreshes when extension
  contributions change, and an unknown slash command now names enabled
  extensions that are not ready instead of failing with a bare error.
- Kept the target-specific Ygg Serve `package.toml` application archive distinct
  from generic executable-extension `extension.toml` bundles.
- Removed ambient executable-extension header/status/footer and presentation
  activity from TUI chrome; extension and worker state now appears only in
  explicit interactive views.
- Routed the coding product's in-harness child sessions through the trusted,
  owner-bound `ygg-subagents` extension. Ultra is now unavailable without its
  live observation service, and the root no longer receives a parallel native
  collaboration tool surface.
- Removed the fixed aggregate subagent token/cost reservation pool. First-party
  children now have fresh contexts and inherit the parent's context/output and
  optional session-token settings exactly; an unlimited parent remains unlimited.
- Raised first-party subagent limits from 2 active/16 retained children per
  owner to 8 active/32 retained, with explicit ceilings of 256 turns,
  50,000,000 microdollars, and 24 hours of wall time.
- Made per-child turn, cost, and wall-time ceilings optional in
  `agent/spawn.policy`: `null` (or omitted) inherits the parent session's
  ceiling, so an unlimited parent policy with no ceiling produces an unlimited
  child.
- Reworked the TUI composer-adjacent subagent activity strip: it appears only
  while workers are pending or running, uses `•`/`└` glyphs with model-matched
  colours, and `Ctrl+O` expands it from the two to the five most recent
  workers while it is visible (falling back to the verbose tool-output toggle
  only when no strip is shown).
- `agent/follow_up` on a settled extension child is now a resume instead of a
  rejection: the child's persistent worker task and durable transcript survive
  between runs, the stale completion timestamp is cleared, and an elapsed wall
  deadline is re-anchored from the child's requested timeout.

### Fixed

- Clear composer-adjacent subagent activity when hydrating a different session,
  so worker telemetry cannot persist across a session switch.

## [0.5.0] - 2026-08-19

### Added

- Added executable-extension API `0.2` with exact feature negotiation,
  host-capped concurrency, one serialized writer, cooperative cancellation with
  bounded tombstones/escalation, request-scoped progress, and correlated child
  requests, including ephemeral text/secret input that is never logged or
  persisted.
- Added typed text/image/audio tool-result parts, declared output schemas,
  validated structured content and retained metadata, plus generation-scoped
  artifact publication from bounded inline data or a verified scratch path.
- Added best-effort session, turn, and tool lifecycle observations; host-owned
  policy-intent classification (currently default-deny without a domain
  adapter); optional original-intent-bound single-use approval redemption;
  manifest-allowlisted, owner-scoped secret brokerage; host-derived
  session/process ownership; inspectable process health; and deadline-bounded
  drain/reload.
- Added transactional `tools/register` and `tools/unregister`, per-process
  catalog epochs, provider-turn schema/implementation snapshots, and stable
  owner ordering so long-lived extensions can publish changing tool catalogs
  without rebuilding the agent.
- Added optional extension-to-host child-agent sessions with scoped spawn,
  message, follow-up, list, wait, and interrupt operations. Ownership is derived
  from the parent request rather than trusted child JSON.
- Added automatic supervision after a successful extension handshake: crashes
  withdraw live tools, restart with bounded jittered exponential backoff, and
  remain fenced from explicit shutdown and stale generations.
- Added dependency-free Python SDK `0.2` support for negotiation, concurrent
  dispatch, cancellation tokens, progress, artifacts, lifecycle handlers,
  policy/approval requests, secret lookup, live tool catalogs, child-agent
  sessions, and graceful drain while retaining API `0.1` wire support.
- Migrated the Caffeinate example to a supervised API `0.2` lifecycle
  extension that reference-counts active turns. Sleep inhibition is no longer
  native agent-kernel behavior.
- Added durable, provider-neutral session goals: a per-session objective with a
  bounded turn budget, stored owner-only in the session's private `.serve/goals`
  directory, that the agent continues toward after each settled turn until it
  reports explicit completion or becomes blocked. `/goal <objective>`, `/goal
  status`, `/goal pause`, `/goal resume`, and `/goal clear` manage the goal in
  the terminal, and the graphical Serve shows the same goal with a badge and a
  composer command.
- Added metadata-gated Ultra reasoning with automatic bounded V2 task
  delegation, including spawn, follow-up, peer messaging, race-free waiting,
  interruption, descendant cancellation, and inheritance of the root's approved
  tools and execution policies.
- Added isolated child sessions and descriptor-relative private team storage
  with synced `provenance.jsonl`; delegation fails closed if provenance cannot be
  persisted.
- Added `ygg update` with install-method detection, release checks, and pinned
  installer or Cargo execution outside the running process.

### Changed

- Froze executable-extension API `0.1` as a backward-compatible,
  text-oriented contract. Its optional tool metadata remains accepted but is
  discarded, and its `after_response` hook remains completion-only.
- Superseded the earlier host-owned capability design with a tiny agent kernel:
  JSON-RPC subprocess extensions own MCP, browser, web-search, computer-use,
  memory, LSP, subagent-orchestration, and caffeinate domain behavior.
- Replaced the obsolete OpenAI Codex Pro wire mode with provider-advertised
  `ultra` effort. Persisted Pro selections remain readable and migrate only when
  the route advertises Ultra plus V2 collaboration and the host can execute it;
  no codec emits `reasoning.mode`.
- Updated authenticated Codex discovery to parse advertised reasoning levels,
  `use_responses_lite`, and `multi_agent_version: "v2"` using cache schema 2 and
  client version `0.147.0`. Offline or incomplete metadata does not infer those
  capabilities from model names or OAuth plans.
- Matched current Responses Lite ordinary and compact requests, including
  explicit `parallel_tool_calls: false`, developer-message instructions,
  input-item `additional_tools`, `reasoning.context: "all_turns"`, and narrow
  removal of image-detail hints.
- Swapped the safety default to full host access (`UnsafeHost`) and replaced
  `--safe` with canonical `--safe-mode` for approval-required execution. The
  obsolete `--yolo` flag and its configuration/environment aliases are no longer
  accepted; `--safe` remains a hidden compatibility alias for `--safe-mode`.
- Rewrote bash safety classification with tree-sitter parsing: a strict
  word-only command allowlist joined by `&&`, `||`, `;`, and `|`, with bounded
  recursion through shell wrapper invocations, decides which `bash` commands
  the `Controlled` profile auto-approves as read-only; anything else still
  requires one-shot approval.
- Reworked local compaction around Pi-style token retention: the most recent
  20,000 tokens of conversation are kept verbatim
  (`compaction.keep_recent_tokens`), a turn that crosses the retention boundary
  is split so its older prefix is summarized with its own bounded output budget
  while its recent suffix is retained, and the checkpoint summary uses a
  bounded output budget. Legacy compaction mode and policy settings remain
  accepted.
- Moved TUI liveness out of the composer: the composer now sits in a static
  model-accent frame while the transcript owns animation, inline slash, file,
  and `@` completions render below the composer, and reasoning presentation
  uses compact `Working` and `Compacting context` labels instead of animated
  shimmer.

- Session discovery now keeps JSONL transcripts authoritative while caching
  bounded title projections in a disposable workspace SQLite catalog, and
  streams transcript replay and metadata scans so large sessions no longer need
  a second whole-file buffer.
- Reduced per-frame rendering work in long TUI sessions.

### Fixed

- Kept slash/path completion, panel, and report surfaces out of
  terminal-owned history, and reconciled streamed-layout contractions without
  punching blank rows into the live grid. `/context` now repaints completely
  after a tall slash popup while finalized transcript rows remain exactly once
  in native scrollback.
- Made delegated mailbox delivery transactional: bounded UTF-8 pages remain
  leased until an untruncated tool result is durably appended, and failed or
  truncated persistence restores the page.
- Preserved accepted steering, queued prompt messages, and follow-ups across
  interruption, backpressure, and failed runs, retaining queue reservations until
  durable prompt delivery; rejected oversize or overflowing durable tasks and
  messages instead of truncating, evicting, or silently releasing them.
- Rejected stale, future-dated, malformed, incomplete, and inconsistent Codex
  cache metadata before capability activation, stripped dynamic capabilities
  offline, and applied legacy-Pro migration precedence consistently at rebuilds.
- Propagated the effective current root prompt to newly spawned children and
  securely rolled failed delegation activation back to its exact empty private
  team directory.
- Omitted `tool_choice` from OpenAI Chat requests when no tools are enabled so
  the field is no longer sent without a tool list.
- Made the signed binary installer portable across supported macOS and Linux
  targets, and added replacement coverage proving a v0.4.0 installation upgrades
  both binaries and packaged documentation without touching user data.
- Surfaced terminal provider failures in the Serve web UI and retained
  failed-provider diagnostics after later work.
- Bounded the Caffeinate example's sleep inhibition: if Ygg cannot report a
  terminal outcome, such as an interrupted run, the inhibitor now expires after
  30 minutes.

The `0.2` foundation does not yet ship first-party MCP, browser, web-search,
computer-use, memory, LSP, or subagent-orchestration packages. Supervision
begins only after a successful initial handshake and does not yet detect a hung
but still-open child; a full application rebuild still recreates extension
processes. The coding product does not yet configure an approval UI adapter or
secret provider. OS-level CPU/RSS/FD/PID quotas also remain future kernel work.

## [0.4.0] - 2026-08-10

### Added

- Added an exception-driven Fleet command center that aggregates active and
  attention-needed sessions, supports task and project search, and returns
  directly to focused work.
- Added live session activity indicators in the Serve sidebar and line numbers
  in the graphical source-file viewer.
- Added checkout-aware self-documentation guidance to `/help` and the system
  prompt when Ygg is run from its own source tree.
- Added the `ygg_sdk` Rust library and `ygg-host` protocol-v1 NDJSON process
  interface for native integrations, including bounded streaming, inline and
  configured providers, typed media, seeded history, and durable sessions.
- Added first-use, owner-only Codex credential migration from legacy Codex and
  Hamr stores without modifying the source credentials.
- Added both Ygg binaries to deterministic, Git-tracked release archives,
  binary/source installers, containers, and release smoke tests.
- Added credential-free configured-provider acceptance for Serve covering
  authentication/model selection, streaming, tool replay, retries, explicit
  compaction, restart/resume, cancellation, and secret-safe failures, plus an
  optional protected live-provider acceptance workflow.
- Added a deeply nested PDF regression proving Ygg's bounded iterative preflight
  rejects hostile nesting before parser entry.

### Changed

- Made graphical prompts submitted during an active run steer by default instead
  of becoming queued follow-ups.
- Removed the redundant interactive `/tool`, `/docs`, `/sessions`, and
  `/cycle-model` commands. The top-level `ygg sessions` command remains
  available.
- Rounded compact TUI footer costs to three significant figures.
- Made headless native-host interactions fail closed: tool confirmations are
  denied, typed input requests are cancelled, protocol frames are bounded, and
  session/image/provider inputs are validated at their system boundaries.
- Enforced dependency review, high-severity npm audit, plus `cargo audit` and
  `cargo deny` for both lockfiles on pull-request and release paths.

- Replaced broad Serve session inventory replays with bounded, targeted catalog
  scans and carried an authorized resume session into the worker by descriptor.
  On the isolated 891-transcript fixture, startup improved from 43.01 seconds to
  19.41 seconds; direct resume of the large-session fixture completed in 2.39
  seconds.

### Fixed

- Made `/overview` bootstrap from session inventory without creating or opening
  a provisional task, including anchorless reconnect refresh and cancellation of
  stale session-selection navigation.
- Kept the focused session surface and route mode stable when session selection
  or creation fails or is retried.
- Hardened Serve catalog and resume selection against corrupt or symlinked
  transcripts, trashed sessions, unsafe sidecar metadata, inactive-branch
  configuration, and pathname replacement after resume authorization.
- Prevented no-color line truncation from appending an ANSI reset sequence to
  otherwise plain terminal output.
- Preserved launcher configuration, model selection, and provider credential
  environment variables when `ygg serve` dispatches to the exact-version
  first-party package runtime.

## [0.3.2-alpha] - 2026-08-01

### Added

- Added target-specific prebuilt Ygg binaries and a version-pinned installer for
  GNU/Linux x86-64, macOS x86-64, and macOS Apple silicon; compiling with Cargo
  remains an explicit `--from-source` option.
- Added the minimal first-party application-extension workflow: `ygg extension
  install`, `list`, `update`, and `remove` download or accept a local package,
  verify its checksums and exact Ygg compatibility, and install it atomically.
- Added external `ygg serve` dispatch to the separately installed, loopback-only
  Ygg Serve runtime. The ordinary Ygg binary does not include the Serve backend
  or web application.
- Added source-located diagnostics for unknown global and trusted-project config
  keys, typo suggestions, and strict rejection through `--strict-config`,
  `strict_config`, or `YGG_STRICT_CONFIG`.
- Added `SessionRunOutcome` persistence, root-head checkpoints, durable run
  terminal state, and independent display metadata for steering and follow-up
  inputs.

- Added the loopback-only Ygg Serve backend under `extensions/ygg-serve/`, with
  bounded host/session contracts, deterministic snapshots and replay,
  authenticated HTTP/WebSocket transport, session supervision, evidence and
  attachment storage, document and test-result ingestion, repository context,
  project files, terminals, transcript search, runtime status, and prompt
  context.
- Added the React 19 and TypeScript web client under `apps/web/`, with responsive
  session navigation, transcript and activity views, composer controls,
  attachments, completion review, branching, project files, terminal access,
  usage and context views, settings, search, local themes and fonts, and
  Playwright acceptance coverage.
- Added the feature-gated adapter in
  `crates/ygg-coding-agent/src/extensions/serve.rs`, boundary enforcement,
  deterministic embedded assets, installed-runtime smoke coverage, and
  target-specific release packaging for GNU/Linux x86-64 and both supported
  macOS architectures.

- Added architecture, current-state, lifecycle safety, LAN pairing, native
  delivery, P0/P1 delivery, and web acceptance documentation for Ygg Serve.

### Changed

- Kept `auto` on the terminal-owned renderer so native scrollback, drag
  selection, logical-height chrome, stable-prefix suffix updates, and full
  retained-transcript replay on resize remain the defaults. `--mouse app`
  explicitly opts into the bounded, anchored semantic viewport.
- Preserved the `0.3.1-alpha` default prompt alignment, event rows, transcript
  surfaces, and composer spacing; this experiment changes terminal behavior,
  not the visual layout.
- Reconciled the vendored renderer's `0.3.1` package metadata and provenance with
  the Ygg workspace while keeping its unsynchronized standalone baseline explicit.

- Added PTY coverage proving only explicit `app` mode negotiates mouse ownership;
  `auto`/`terminal`/`off` leave it to the terminal, and all four restore terminal
  state.
- Added regressions that grow one live Markdown block while scrolled above the
  tail and reflow cached transcript rows across consecutive wide and narrow
  renders.

### Fixed

- Replaced line-based configuration updates with structural, comment-preserving
  TOML editing so multiline values, similarly prefixed keys, and table sections
  cannot be corrupted when Ygg persists model or reasoning selections.
- Rebuilt cached transcript rows whenever the requested width changes, even when
  no content block is dirty, preventing wide rows from leaking into a narrow
  render.
- Sanitized user input before Markdown parsing so terminal protocols cannot
  expand differently from the semantic copy projection or destabilize prompt
  geometry.
- Prevented OpenAI Responses, OpenAI Chat Completions, and Anthropic Messages
  POSTs from being replayed after full transport timeouts or ambiguous failures
  while sending the request or awaiting response headers; replay-safe connection
  failures remain visible, cancellable, and bounded.

- Enforced `#![forbid(unsafe_code)]` across the vendored `sexy-tui-rs` crate.

## [0.3.1-alpha] - 2026-07-26

### Changed

- Replaced width-dependent commit row bookkeeping with stable semantic cursors
  that remap after reflow, including list-item and table-row boundaries for
  large streamed Markdown blocks.
- Kept deferred-history prepends and cancelled streaming retries on the same
  append-only semantic tape without renumbering retained commit identities.
- Added terminal-emulator regressions for streaming layout shrink, nonzero
  scrollback offsets, synchronized output, and width changes during generation.

### Fixed

- Prevented finalized streamed output from being duplicated, omitted, or
  overwritten in native terminal scrollback when Markdown rows shrink, the
  terminal is resized, or scrolling and resizing overlap with generation.
- Preserved terminal-owned history across theme and disclosure repaints without
  clearing scrollback or replaying the committed transcript.

## [0.3.0-alpha] - 2026-07-25

### Added

- Failed interactive runs now retain the compact lifecycle row and show a
  bounded, terminal-safe diagnostic that can be copied for troubleshooting.

### Changed

- Made fenced Markdown code copy-safe with borderless, terminal-adaptive shading.
- Kept the default prompt composer unfilled and restrained at rest, with a
  model-colored perimeter shimmer only while work is active; explicit themes
  retain their authored chrome.
- Refreshed the project identity, terminal demo, installation references, and
  release documentation for the `0.3.0-alpha` line.

### Fixed

- Redacted request credentials and terminal controls from bounded provider and
  transport diagnostics before they can be persisted or printed.
- Serialized ChatGPT credential refresh across processes and bounded OAuth
  responses, update metadata, and other remote discovery inputs.
- Prevented atomic new-file publication from replacing a concurrently created
  target and retired extension RPC connections after interrupted framed writes.
- Centralized terminal-safe human-facing command output and strengthened session
  export redaction without changing provider-visible conversation context.

## [0.2.0-alpha] - 2026-07-25

### Added

- Added durable Responses replay and multimodal reads.
- Added persistence for authoritative raw output.
- Added native compaction transport and pro mode.
- Added support for labeled custom OpenAI providers, including tool calling
  with macOS 27 Apple Foundation Models: system and private cloud compute (PCC).
- Added PDF attachment handling that resolves to a workspace path for file tools
  instead of pretending PDFs are supported as multimodal payloads.

- Showcased sexy-tui-rs themes and added the ygg demo to the README.
- Updated installation, security-support, and release references for this alpha.

### Changed

- Refined reasoning status presentation and hardened native Responses integration.
- Improved bounded, sanitized tool-output projections and edit/write diffs in the
  TUI while keeping raw evidence out of transcript copy.
- Updated the startup identity and release metadata for the `0.2.0-alpha` line.

### Fixed

- Prevented the shell prompt from overwriting the inline composer or leaving a
  stale footer after exiting the interactive TUI.
- Fixed release-blocking image/audio ingestion and media capability handling.

## [0.1.1-alpha] - 2026-07-24

### Added

- Restored the animated, model-tinted braille-tree startup identity. The startup
  card reports the package version, selected model, reasoning configuration, and
  workspace without taking over the terminal background.
- Added entitlement-gated GPT-5.6 Pro reasoning mode for ChatGPT OAuth Pro
  routes, with independent CLI, configuration, session persistence, picker, and
  OpenAI Responses wire support.
- Added `shell_path`, `--shell-path`, and `YGG_SHELL_PATH` for explicit
  Bash-compatible shell selection.
- Added syntax-aware inline Bash command rendering, including distinct command
  names, strings, operators, flags, and arguments.

### Changed

- Renamed the model command tool from `exec` to `bash`. Every command is now
  passed intact to one Bash-compatible shell with `-c`, matching Pi's Unix
  semantics: explicit `shell_path`, `/bin/bash`, `bash` on `PATH`, then `sh`.
  Ygg does not consult `$SHELL`.
- Renamed the primary execution limit to `bash_timeout_secs`,
  `--bash-timeout-secs`, and `YGG_BASH_TIMEOUT_SECS`. The prior configuration,
  CLI, and environment spellings remain compatibility aliases.
- Reworked the transcript hierarchy around a fixed two-row live reasoning
  status that exposes only the model's latest explicit Markdown heading, uses a
  blinking model-colored dot beside a plain model-colored label, and falls back
  to `Thinking`, alongside in-place activity, bold neutral tool names, restrained metadata, quieter
  collapsed-output hints, consistent spacing, and model-provenance user prompts.
- Tool lifecycle dots now blink in lockstep while work is active, settle dimly,
  use green only for successful Bash commands, and reserve red for failures.
- Completed reasoning disappears by default and remains available through the
  global verbose disclosure mode.
- Active context telemetry now accounts for newly persisted tool results before
  the next provider usage report, so an imminent auto-compaction no longer
  appears to trigger against a stale pre-tool token count.
- Ported Pi-compatible terminal input, selection, paste, key-repeat, and overlay
  behavior while preserving native terminal selection and scrollback.
- Long-session rendering now hydrates a bounded tail, caches stable transcript
  rows, and avoids replaying or repainting committed native scrollback.
- Simplified tool output presentation: Bash output remains neutral, file tools
  expose diffs when relevant, and completed tool evidence stays collapsed unless
  explicitly expanded.

- Existing sessions containing historical `exec` calls continue to render as
  Bash events; new provider schemas advertise only `bash`.
- Command cancellation and timeouts retain process-group cleanup, bounded
  stdout/stderr capture, live progress, and detached-descendant supervision.
- Added regression coverage for shell selection and Bash expansion, Pro-mode
  entitlement and persistence, synchronized event-dot animation, startup version
  display, reasoning cleanup, command syntax styling, and transcript lifecycle.
- Reduced development-profile codegen units to limit incremental artifact
  accumulation without disabling incremental compilation.

## [0.1.0-alpha] - 2026-07-22

### Added

- Interactive TUI, chronological plain mode, and response-only print mode.
- OpenAI Chat, OpenAI Responses, and Anthropic Messages protocol support.
- Local OpenAI-compatible endpoint configuration and cloud/provider discovery.
- Branchable append-only sessions, usage/cost records, checkpoints, resume, and compaction.
- Bounded `read`, `search`, `edit`, `write`, and `exec` tools plus skill discovery/activation tools.
- Complete CLI tool allowlist/deny controls, offline startup, context-file disable switch, workspace trust gate, and `--version`.
- Deterministic checked-in model metadata and Unix containment profile.

- Project configuration/resources are ignored unless the workspace is explicitly trusted; project settings cannot relax global authority floors.
- Disabled tools are absent from provider schemas and execution dispatch.
- `--no-edit` disables both mutation tools.
- Descriptor-relative no-follow file operations close parent-symlink replacement races and compare target state immediately before rename.
- File/context/config/credential/session/discovery/provider-stream inputs have hard byte/count limits; special files are rejected.
- Arbitrary process and shell execution use one truthful authority gate.
- Unresolved mutating calls are never replayed after a crash.
- Session appends use interprocess locking, stale-generation detection, private permissions, and synced writes; listing is read-only.
- Cancellation propagates through autonomous compaction and prevents post-cancel summary/usage commits.
- TTY print output neutralizes terminal control sequences.

- Added root installation/security documentation, MIT and third-party notices, checked-in architecture docs, reproducible release gates, dependency policy, a fuzz target, and complete package metadata.
- Release builds enable ThinLTO, one codegen unit, symbol stripping, and abort-on-panic to reduce startup work and binary/RSS footprint.
- The alpha release target is macOS and Linux; command execution is explicitly Unix-only.

### Changed

- Session resume hydrates and paints only a bounded tail instead of cloning, parsing, and rendering the entire transcript; older history materializes on demand for PageUp/PageDown, wheel navigation, selection, and semantic copy.
- Session discovery uses bounded lightweight metadata scans, and direct resume-by-id avoids parsing unrelated session bodies.
- TUI redraws emit exact changed rows, clear stale Kitty images, coalesce composer border colour runs, anchor scrolled readers while output arrives, and repeat only editing/navigation keys (never submit, close, or toggle actions).
- Provider model inventories use private, scoped cache-first startup. Built-in inventories refresh in the background; stale custom inventories refresh before catalog construction so the current launch sees server changes while retaining last-known-good models on failure.
- Connection setup and response headers have separate bounds. Custom endpoints have a configurable cold-start header allowance, while non-timeout network loss retries visibly and cancellably up to five times; a full transport timeout is not multiplied automatically.
- Ordinary final answers no longer trigger a hidden second completion-confirmation inference.
- Request sizing and transformation avoid temporary whole-history buffers and redundant context reconstruction during resume and send.
- Codex Responses requests use zstd compression, low text verbosity, and capability-gated parallel tool-call declarations without changing generic OpenAI-compatible routes.
- Streaming parsers use bounded linear scans and aggregate response budgets, including adversarial one-byte compatibility streams, pre-ID tool arguments, and Anthropic signatures.
- Interactive shell commands drain stdout and stderr concurrently under a fixed output budget, enforce the execution timeout, and terminate the complete process group on cancellation.
- Native terminal selection and scrollback are the default again; stable-prefix frame updates avoid redrawing committed history, while application-owned semantic mouse behavior remains available through `--mouse app`.
- Semantic transcript blocks use one consistent breathing row between actions without separating a tool header from its result or diff.
- Custom hlid/llama.cpp discovery reads the active nested `meta.n_ctx` context window instead of falling back to training limits or a generic default.
- Custom endpoint reasoning controls are authoritative: off-only, binary, and level-based metadata produce exactly the corresponding picker choices and wire values.
- Reasoning is collapsed by default into a stable two-line, model-colored status that surfaces only explicit model-emitted Markdown headings, falls back to `Thinking`, disappears on completion, and expands with `Ctrl+O`.
- Every bundled theme retains its authored palette, while the compiled default follows the selected model lab and resets cleanly after theme switches.
- Batched tool results retain independent bounded output allowances so a large early result cannot starve later calls in the same turn.

[0.7.6]: https://github.com/skaft-software/octet/releases/tag/v0.7.6
[0.7.5]: https://github.com/skaft-software/octet/releases/tag/v0.7.5
[0.7.4]: https://github.com/skaft-software/octet/releases/tag/v0.7.4
[0.7.3]: https://github.com/skaft-software/octet/releases/tag/v0.7.3
[0.7.2]: https://github.com/skaft-software/octet/releases/tag/v0.7.2
[0.7.1]: https://github.com/skaft-software/octet/releases/tag/v0.7.1
[0.7.0]: https://github.com/skaft-software/ygg/releases/tag/v0.7.0
[0.6.7]: https://github.com/skaft-software/ygg/releases/tag/v0.6.7
[0.6.6]: https://github.com/skaft-software/ygg/releases/tag/v0.6.6
[0.6.5]: https://github.com/skaft-software/ygg/releases/tag/v0.6.5
[0.6.4]: https://github.com/skaft-software/ygg/releases/tag/v0.6.4
[0.6.3]: https://github.com/skaft-software/ygg/releases/tag/v0.6.3
[0.6.2]: https://github.com/skaft-software/ygg/releases/tag/v0.6.2
[0.6.1]: https://github.com/skaft-software/ygg/releases/tag/v0.6.1
[0.6.0]: https://github.com/skaft-software/ygg/releases/tag/v0.6.0
[0.5.0]: https://github.com/skaft-software/ygg/releases/tag/v0.5.0
[0.4.0]: https://github.com/skaft-software/ygg/releases/tag/v0.4.0
[0.3.2-alpha]: https://github.com/skaft-software/ygg/releases/tag/v0.3.2-alpha
[0.3.1-alpha]: https://github.com/skaft-software/ygg/releases/tag/v0.3.1-alpha
[0.3.0-alpha]: https://github.com/skaft-software/ygg/releases/tag/v0.3.0-alpha
[0.2.0-alpha]: https://github.com/skaft-software/ygg/releases/tag/v0.2.0-alpha
[0.1.1-alpha]: https://github.com/skaft-software/ygg/releases/tag/v0.1.1-alpha
[0.1.0-alpha]: https://github.com/skaft-software/ygg/releases/tag/v0.1.0-alpha

[0.8.0]: docs/releases/v0.8.0.md
