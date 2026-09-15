# `octet-voice` — local text-to-speech for assistant messages

**Status:** design brief, pre-build. **Target:** octet v0.7.x extension, hardening against the v0.8/v0.9 Pi-parity surface.

---

## TL;DR

**Verdict: build it now. It does not need to wait for v0.8/v0.9.** The load-bearing
primitive already exists in shipped v0.7.3 code:

> `after_response` is a typed, manifest-declared hook that fires **once per completed
> assistant turn** and hands the extension the **final assistant text, with reasoning
> and tool calls already stripped**.

That is *exactly* the payload a TTS feature wants, and the host did the hard part for
us. The one thing missing is **token-level streaming**, so today you hear the answer
after it finishes rather than as it is written. Everything else on the wish list
(interruption, voice choice, code-skipping, progress narration, mute indicator) is
buildable today.

Second headline: **the extension runs as a resident child process for the session**, so
a warm 82M-parameter model costs nothing per turn. That single fact is what makes this
feel instant rather than sluggish, and it is the reason this design is *simple*.

**Recommended shape:** Python extension, API `0.2`, two hooks (`after_response`,
`before_prompt`), in-process warm Kokoro-82M with a silent auto-fallback to macOS
`say`, and a queue that never runs audio work on the host's hook thread.

---

## 1. What the platform gives you today (verified against v0.7.3 source)

### 1.1 The hook surface — `crates/octet-agent/src/extension_process.rs:1947`

| Hook | Since | Payload | Host deadline | Deny-capable |
| --- | --- | --- | --- | --- |
| `before_prompt` | 0.1 | `{ prompt }` | **5 s** | no |
| `after_response` | 0.1 | `{ response }` | **2 s** | no |
| `before_tool_call` | 0.1 | tool call | — | yes |
| `after_tool_call` | 0.1 | tool result | — | no |
| `provider_retry` | 0.2 | retry advice | — | no |
| `before_persistence` | 0.2 | turn metadata | — | no |
| `post_mutation` | 0.2 | affected resources | **250 ms** | no |
| `session_start` / `session_end` | 0.3 | binding + outcome | **250 ms** | no |

Deadlines from `crates/octet-coding-agent/src/extensions.rs:104-115`.
Renderer calls get 500 ms. Every one of these is a hard budget: the host `await`s the
call and treats overrun as a diagnostic, not a retry.

### 1.2 The `after_response` contract, precisely

Two facts make this a gift rather than a compromise:

**It already strips the noise.** The payload is built from `latest_assistant_text()`
(`crates/octet-coding-agent/src/extensions.rs:4789`), which walks the session to the
newest assistant message and returns `assistant_text()` — and there is a test asserting
the exact behaviour we want:

```rust
// crates/octet-coding-agent/src/extensions.rs:7659
fn assistant_text_excludes_reasoning_and_tool_calls() {
    // Reasoning + Text("final ") + ToolCall + Text("answer")  =>  "final answer"
}
```

So no reasoning traces, no tool-call JSON, no tool results. We never have to strip
those ourselves.

**It already fires at the right cadence.** Call sites are the terminal boundary of a
completed turn, guarded by `allows_after_response()`, which is **success-only** —
`modes/mod.rs:63`: *"API 0.1's compatibility hook is success-only; terminal failures,
cancellation, stream loss, and shutdown must never invoke it."* So a 40-tool-call
agentic run produces **one** utterance (the answer), not forty. Designers of noisy TTS
features fight for this property; we get it for free.

### 1.3 Residency — the quiet superpower

`[runtime]` defaults to `lifecycle = "legacy_resident"`, `sharing = "isolated"`, and it
**starts with the admitted session binding** (`docs/extensions/legacy-authoring.md:192`).
The process is long-lived, not per-call.

Consequence: load the model once at session start, keep it warm, and every turn is pure
inference. Cold-start cost is paid once, off the user's critical path. This is what
separates a delightful feature from an annoying one.

### 1.4 The three platform constraints that shape the design

**Constraint A — 2 seconds, and the host is waiting.**
`AFTER_RESPONSE_RPC_DEADLINE = 2s` and the interactive loop `await`s the hook
(`modes/interactive.rs:5836`). Synthesising audio inside the hook would burn the budget
and stall the prompt. **Therefore: the hook does no audio work at all.** It sanitises,
enqueues, and returns `continue` in single-digit milliseconds. A worker thread owns
synthesis and playback.

**Constraint B — CLI flags require API `0.3`, and API `0.3` is deliberately crippled.**
`extension_process.rs:1750` rejects `commands`, `context`, `ui`, `notifications`,
`tool_renderers`, `presentation`, and every non-session hook on API `0.3`:

> *"API 0.3 currently implements only its negotiated initial tool catalog, secret-free
> provider catalogs, manifest-declared CLI flags, and declared session_start/session_end
> hooks; commands, other hooks, context, UI, renderers, notifications, confirmations,
> and presentation are deferred."*

So `0.3` buys flags but costs us the mute indicator, `/voice` commands, and the
`after_response` hook. **Use API `0.2`** — it has every hook and every contribution
surface except `session_start`/`session_end`, which TTS does not need.
Settings therefore come from a config file plus a `/voice` command surface, not from
`--voice-speed`. That is a deliberate, correct trade.

**Constraint C — no keybinding surface yet.** That is issue **#260** (v0.8). Until then,
interruption is via `/voice stop` and — better — via `before_prompt`, see §4.4.

### 1.5 What v0.8/v0.9 parity actually adds

Pi `0.84.4` is the pinned parity target, under epic **#190** ("install and use unchanged
Pi extensions end to end"). The relevant leaves:

| Issue | What it unlocks for voice |
| --- | --- |
| #163 hooks epic | the place a streaming/text-delta hook would land |
| #259 semantic UI + renderer transport | status/widget surfaces for a richer speaking indicator |
| #260 editor/focus/input handoff | **a real keybinding: tap space to stop** |
| #264 bounded tool-progress presentation | narrated progress with host-sanctioned decoration |
| #270 provider stream proxy w/ backpressure | the sanctioned streaming pattern |

The prize is #260 (interruption affordance) and a **streaming observation hook**. Note
that the parity promise is behavioural, not literal: *"A bounded host-side transport or
different internal implementation is fine"* (#259). So we should design our core to be
adapter-shaped and swap the adapter later rather than betting on Pi's exact wire.

---

## 2. The one real gap: it doesn't speak *while* writing

Today's UX for a 60-second agentic answer is: generate for 60 s, then a wall of speech.
For the "read me the answer while I'm away from the screen" story that is *fine* — you
were not watching anyway. For the "pair with me, narrate as you go" story it is a miss.

**Do not tail the session JSONL.** It is tempting (sessions are plain parent-linked
JSONL at `~/.octet/sessions/<id>/<ts>.jsonl`) but it buys nothing: entries are written
per *completed message*, so granularity is identical to `after_response` — while adding
fragility (locating the live session, honouring head/branch selection, compaction,
`.delegation/*` child transcripts, torn final appends). **Rejected for v1.**

**The right ask is a new declared hook.** Filed against #163:

> A declared, bounded `assistant_stream` observation hook (or an equivalent bounded
> `text_delta` notification with explicit backpressure and coalescing) that delivers
> incremental assistant text to opted-in extensions, with the same discipline the other
> hooks already have: manifest-declared only, off the hot path unless declared, never
> able to veto or delay generation.

Every voice agent in the world is built on this primitive (sentence-boundary chunking
during token streaming is the standard architecture), and octet is the only one of these
agents where the host has *already* solved the hard part of message ownership.

**Fallback that needs no host change:** an extension acting as a *provider* stream proxy
at v0.8 (#269/#270) could observe deltas by wrapping the configured provider. Heavy,
requires being the configured model, and not v1. Noted as a possible v2 bridge.

---

## 3. State of the art in local TTS (2025-26)

### 3.1 The field, honestly

| Engine | Size | License | CPU/Apple perf | Voice clone | Verdict |
| --- | --- | --- | --- | --- | --- |
| **Kokoro-82M** | 82M, ~310 MB fp32 / ~90 MB q8 | **Apache-2.0** | faster than realtime on plain CPU; 5× RT on 32-core; MLX path on M-series | no (54 baked voices) | **the default** |
| Piper (VITS/ONNX) | tiny, <1 GB | MIT repo archived → **GPL-3.0** fork | realtime on a Pi 4 | no | fallback for tiny machines; audibly synthetic |
| XTTS v2 | ~1.8 GB | **CPML, non-commercial** | slow on CPU | yes, best-in-class | avoid: licence + weight |
| Chatterbox | ~0.5-1 GB | **MIT** | heavier | yes; emotion control | the upgrade path when expressiveness matters |
| Qwen3-TTS / Voxtral / Dia / CSM | 1-5 B | mixed | needs MLX/GPU | varies | only via `mlx-audio`; overkill for reading answers |
| **macOS `say`** | 0 bytes | OS-provided | instantaneous | no | **the always-works floor** |

Kokoro also has a genuinely clean provenance story — trained on permissive and
synthetic audio, $1000 of A100 time, Apache-licensed weights, ~11.6 M downloads/month —
which matters for an extension that will be installed by strangers. Prior art agrees:
the Claude Code "Narrator" plugin and the OpenCode voice plugin both reach for Kokoro
first.

### 3.2 The recommendation: an engine ladder, not an engine

Bulletproof means **the feature cannot be broken by a missing dependency**. So:

- **Tier 0 — `say` (floor).** `/usr/bin/say` + `/usr/bin/afplay` are present on every
  Mac. Zero install, zero network, instant, no Python. If Kokoro is absent or broken,
  speech still happens. This tier is why the extension *never* fails.
- **Tier 1 — Kokoro-82M (default).** Via `kokoro-onnx` for portability, or `mlx-audio`
  on Apple Silicon for speed. Warm in-process, ~300-500 MB RSS.
- **Tier 2 — Chatterbox (opt-in).** Same adapter, richer voices, for users who want it.

Selection is automatic with an explicit override: probe, pick the best available,
degrade silently, report the choice in `/voice status`.

### 3.3 Two environment landmines (this machine, and probably many others)

1. **Python 3.14.7.** The ML/ONNX ecosystem lags new interpreters; `kokoro-onnx` wheels
   for 3.14 are unlikely to exist yet. **Mitigation:** never import third-party TTS at
   module import time — probe lazily inside a `try`, cache the verdict, fall back to
   Tier 0. Optionally document a pinned 3.11/3.12 venv for Tier 1.
2. **`espeak-ng` is not installed**, and Kokoro's G2P (misaki) wants it for fallback and
   non-English. `kokoro-onnx` bundles `espeakng-loader`, but this must be *verified at
   build time*, not assumed. If unavailable → Tier 0, no exception.

Also worth noting: this machine has **9.4 GB free disk**. Kokoro fp32 is ~310 MB —
comfortable — but the extension must never do a surprise multi-hundred-MB download.
Weight fetching belongs in an explicit `/voice setup` with progress output.

---

## 4. Architecture: bulletproof by construction

```
   octet host (Rust)
        │  spawns once per session, supervises, restarts on crash
        ▼
┌───────────────────────────── octet-voice process (resident) ─────────────────────────────┐
│                                                                                          │
│  hook thread ──── after_response ─┐                                                      │
│  (2 s budget)      before_prompt ─┤   ┌──────────────┐   ┌───────────────────────────┐    │
│                                   ├──▶│  sanitizer   │──▶│  bounded queue (epoch-tagged)│ │
│  JSON-RPC stdio                   │   │  (pure fn)   │   └─────────────┬─────────────┘    │
│  ◀── notifications / status ──────┘   └──────────────┘                 │                 │
│                                                                        ▼                 │
│                                              ┌──────────────────────────────────────┐    │
│                                              │  synth worker (own thread)           │    │
│                                              │  chunk → synthesize → prefetch → play │    │
│                                              │  Kokoro warm │ MLX │ `say` fallback   │    │
│                                              └──────────────────────────────────────┘    │
└──────────────────────────────────────────────────────────────────────────────────────────┘
```

Five invariants. Every one exists because breaking it produces a specific, predictable
user-facing failure.

**I1 — No audio work on the hook path.** The `after_response` handler sanitises,
enqueues, returns. Measured in single-digit ms against a 2 s budget — a 100× margin.
*Failure prevented:* the agent feeling laggy; hook timeouts; dropped notifications.

**I2 — Bound everything.** Max queued chunks, max characters per utterance, max total
backlog, bounded log line. On overflow: drop oldest and surface a one-line notice rather
than growing without limit or silently truncating the newest (the newest is what the
user wants to hear).
*Failure prevented:* a 20k-token answer pinning memory and speaking for ten minutes.

**I3 — Ladder, never a cliff.** `kokoro-onnx` → `mlx-audio` → `say` → log-once-and-
disable. Every synthesise call is wrapped; any exception demotes a tier and continues.
The hook result is **always** `{"disposition":{"action":"continue"}}`.
*Failure prevented:* a TTS problem becoming an agent problem.

**I4 — Epoch-tagged preemption.** A monotonic epoch increments on every new utterance
set and on every stop. Chunks carry their epoch; the worker drops any chunk from a stale
epoch and aborts in-flight playback.
*Failure prevented:* stale audio talking over the answer you actually asked for.

**I5 — The sanitizer is pure and golden-tested.** `str -> str`, no I/O, no clock, no
config beyond a passed-in options struct. Goldens are cheap and this is the code most
likely to regress.

### 4.1 Why in-process, not a sidecar daemon

A sidecar (model server on a Unix socket) buys process isolation and multi-frontend
sharing (TUI *and* Serve companion). It costs a second lifecycle to supervise, a socket
protocol, and a restart story. The host already supervises the extension and restarts it
on crash, so the isolation gain is small. **Start in-process.** Revisit only if the Serve
companion needs to share a voice queue — and note that workspace sharing requires
manifest opt-in, a trust partition, and a matching digest, so it is not a free move.

---

## 5. The product *is* the sanitizer

Unpolished markdown read aloud is unbearable. This is where the feature is won or lost —
more than in the choice of model.

| Input | Spoken |
| --- | --- |
| ```` ```rust … ``` ```` fence | skipped (default) / "…code block, 12 lines…" / read (opt-in) |
| `` `kokoro_onnx` `` | "kokoro onnx" |
| `**bold**`, `# heading`, `- bullet` | plain prose, no punctuation noise |
| `[text](https://…)` | "text" — never the URL |
| `/Users/a/b/parser.py:42` | "parser dot py, line 42" (or skipped when the reply is link-dense) |
| `+42 −17` diff hunk | "…diff, 42 additions…" |
| tables | "…table, 5 rows…" |
| `snake_case`, `kebab-case`, `HTTPServer` | "snake case", "kebab case", "HTTP server" |
| emoji, ANSI escapes, box-drawing | stripped |
| very long answer | first N sentences + "…and N more lines" |

Also: split into sentence-bounded chunks of ~2 sentences / ~300 chars. Small chunks are
what make first-audio fast, and sentence boundaries are what keep prosody sane.

---

## 6. User stories → design decisions

Each story is stated with the decision it forces and how we know it worked.

**S1 — "Read me the answer while I'm away from the screen."**
`after_response` → sanitise → speak. *Acceptance:* answer is spoken in full; tool calls,
reasoning, and diffs are never spoken.

**S2 — "Start speaking quickly; don't make me wait."**
Warm resident model + sentence chunking + next-chunk prefetch while the current one
plays. *Acceptance:* first audio within ~1 s of the hook firing, after a warm session.

**S3 — "Don't read me code, paths, or diffs."**
The sanitizer, default `code = skip`. *Acceptance:* golden fixtures; a Rust-heavy answer
produces no identifier soup.

**S4 — "Stop talking when I send my next message."**
`before_prompt` hook → bump epoch, flush queue, abort playback. Elegant: the host fires
this exactly when the user has moved on. *Acceptance:* silence begins before the new
turn's output; no stale audio ever overlaps a new answer. *(This is also the pre-#260
substitute for "tap space to stop".)*

**S5 — "Shut up right now."**
`/voice stop` (and `/voice mute` to persist). *Acceptance:* immediate silence; mute
survives restart via config.

**S6 — "Let me pick a voice and speed."**
`/voice voice <name>`, `/voice speed <0.5-2.0>`, `/voice test`, persisted to
`~/.octet/octet-voice.toml`. Not CLI flags — Constraint B. *Acceptance:* settings
survive a session restart; `/voice test` speaks without consuming a model turn.

**S7 — "Don't talk over my terminal when I'm in a script."**
Default `interactive_only = true`; `after_response` fires in print/plain/rpc/serve modes
too, and speaking during `octet -p` or the Serve companion would be wrong.
*Acceptance:* `octet -p "…"` stays silent.

**S8 — "Narrate what you're doing, not just the final answer."**
Optional mode using `after_tool_call` → "editing parser dot rs", "running tests" — a
progress-narration mode that is genuinely delightful AND is available **today**.
*Acceptance:* off by default; bounded; never speaks tool *output*.

**S9 — "Let the agent choose to speak something."**
A `speak` tool, so "read me that file aloud" / "read the test failures out" works.
Prior art: the OpenCode voice plugin ships exactly this. *Acceptance:* agent-invoked
speech respects the same queue and mute state.

**S10 — "Show me it's working without stealing my screen."**
`ui = ["status"]` → `🔊 af_heart · 2 queued` / `🔇 muted`, plus a `notifications`
contribution on degradation ("voice: falling back to system speech"). *Acceptance:*
status is transient and never enters model context or session history (host-guaranteed).

**S11 — "It must be private and offline."**
`network = false`. No telemetry. Weight download is an explicit, visible `/voice setup`.
*Acceptance:* the extension functions with the network fully down.

**S12 — "Never break my session."**
Every path returns `continue`; failures degrade a tier and log once. *Acceptance:* kill
the model files mid-session → assistant still speaks via `say`, agent unaffected.

---

## 7. Proposed extension surface

```toml
name = "octet-voice"
version = "0.1.0"
api_version = "0.2"                 # NOT 0.3 — see Constraint B
description = "Read assistant responses aloud with a local neural TTS engine"

[entrypoint]
command = "extension.py"

[capabilities]
filesystem = "read"                 # own config; never the workspace
process = true                      # afplay / say / engine helper
network = false                     # local only

[contributes]
commands = ["voice"]
tools = ["speak"]
hooks = ["after_response", "before_prompt", "after_tool_call"]
ui = ["status"]
notifications = true
context = false
confirmations = false
```

Commands: `voice` (status), `on|off|mute`, `stop`, `voice <name>`, `speed <n>`,
`mode answer|progress|all`, `code skip|announce|speak`, `test`, `setup`.

Status: engine tier, voice, speed, queue depth, epoch, muted state, last error.

Structure that keeps the core portable to v0.8:

```
octet-voice/
  extension.toml
  extension.py            # thin octet adapter: hooks -> core
  voice/
    sanitize.py           # pure, golden-tested (I5)
    chunk.py              # sentence-bounded segmentation
    queue.py              # bounded, epoch-tagged (I2, I4)
    engines/
      base.py             # synth(text, voice, speed) -> PCM/file
      kokoro.py  mlx.py  system_say.py
    config.py
  vendor/octet_extension/ # vendored SDK, dependency-free entrypoint
  tests/
    goldens/*.md → *.txt
```

---

## 8. Risks

| Risk | Severity | Mitigation |
| --- | --- | --- |
| Python 3.14 wheels missing for ONNX/ML | **high** | lazy probe only; Tier 0 floor; document a pinned 3.11/3.12 venv |
| `espeak-ng` absent → Kokoro G2P fails | medium | verify `espeakng-loader` bundling at build time; else Tier 0 |
| Hook overrun (2 s) | medium | I1: no audio work on hook path; assert via timing test |
| Audio device contention / Bluetooth latency | low | `afplay` for Tier 0; document `sounddevice` device selection |
| Model RAM (~400 MB) on a constrained laptop | low | q8 (~90 MB) variant; `/voice status` shows RSS |
| Long answers speaking for minutes | medium | I2 bounds + "N more lines" tail |
| API 0.2 → 0.3 migration churn as parity lands | medium | adapter-shaped core (§7); hooks/contributions are the only octet-aware layer |
| Streaming hook never ships | low | v1 is useful without it; the JSONL-tail hack stays rejected |

---

## 9. Build plan

**M1 — Skeleton that talks (½ day).** Manifest + vendored SDK + `after_response` →
Tier 0 `say`. Proves the wire, the cadence, and the 2 s budget. Silent-fail wrapper.

**M2 — Sanitizer + queue (1 day).** I2/I4/I5 with goldens; `before_prompt` preemption;
`/voice stop|mute`. This is the milestone where it stops being annoying.

**M3 — Kokoro tier (1-2 days).** Lazy probe, warm load at session start, chunked
synthesis with prefetch, tier ladder + degradation notification. `/voice setup`.

**M4 — Product polish (1 day).** `/voice status` UI surface, `/voice test`, config
persistence, `speak` tool, `interactive_only`.

**M5 — Progress narration (optional, ½ day).** `after_tool_call` mode.

**M6 — Parity adapter (deferred).** Re-point at the v0.8 surface: streaming hook when it
lands, keybinding for stop (#260), richer status (#259).

**File the feature request now** (§2) so the streaming hook has a real consumer arguing
for it during v0.8 grooming — this design is the argument.

---

## 10. Open questions

1. **Should Tier 1 live in-process at all** given 3.14 risk, or should the extension be
   pure-stdlib + `say` with optional Kokoro? (Leaning: ship M1-M2 pure-stdlib, add
   Kokoro as opt-in for exactly this reason.)
2. **Whose voice is "the assistant's"?** A fixed house voice, or per-project override?
3. **Should `before_prompt` interrupt, or merely duck?** Interrupt is simpler and
   matches intent; ducking is friendlier if the user is mid-sentence on a long answer.
4. **Serve companion**: share one voice queue across TUI and companion, or keep them
   independent? (Workspace sharing has real trust/digest costs.)
5. **Does Pi already have a TTS extension** whose behaviour we should inherit rather
   than invent? If so, #190 makes matching it worth more than our own taste.
