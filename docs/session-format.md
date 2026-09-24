# Session file format

[Documentation](README.md) · [Sessions](sessions.md) · [Commands](commands.md)

octet stores a session as one append-only **JSONL** file: one JSON object per
line, newline-terminated, UTF-8. Sessions are namespaced by workspace under the
session directory — `~/.octet/sessions/<workspace-key>/` by default, overridable
with `--session-dir PATH` or `OCTET_SESSION_DIR`. A new session file is named
`<timestamp>-<suffix>.jsonl`; imported or legacy files keep their `*.jsonl` stem.

The format is internal but stable enough to read and repair by hand. It is not a
Pi session format, and octet does not import arbitrary Pi transcripts
([Pi import](pi-migration.md)).

## Record envelope

Every line is a `type`-tagged object (`crates/octet-agent/src/session.rs:617`):

| `type` | Meaning |
| --- | --- |
| `entry` | One appended conversation/config/skill entry (see below). |
| `head` | Durable head update: current head `id` plus cumulative cost. |
| `root_head` | Durable checkout before the first entry; later appends start a new root branch. |
| `checkpoint` | Restore point for a completed prompt (`prompt`, `head`, optional `usage`, `run_cost_microdollars`). Does not move the head. |
| `usage` | Completed provider operation (including turns, compaction, terminal gates and delegated accounting); never model-visible context. |
| `usage_uncertainty` | An accepted attempt whose usage is unknown. |
| `tool_invocation` | Bounded auxiliary memos/latest tool progress for one unresolved invocation; never model context or proof of completion. |

Unknown record types are ignored on read so a newer file stays openable by an
older binary. A torn or non-UTF-8 trailing line is dropped rather than failing
the whole session; `octet sessions repair SESSION_ID` rewrites a file whose
damage is not confined to the tail.

## Accounting certainty

An explicit zero price is known free usage. Missing both the exact `cost` and
legacy `cost_microdollars` fields means **unpriced**, not free, even after
switching to a priced model. Cost scalars are only known subtotals while any
unpriced operation or `usage_uncertainty` record remains; hard cost ceilings
fail closed. Branch checkout does not erase session-global spending/exposure.

## Entries

An `entry` has `id`, `parent` (`null` marks a conversation root), optional
presentation `metadata`, optional `timestamp_unix_ms`, and a `value` object that
is itself `type`-tagged (`crates/octet-agent/src/session.rs:479`):

| `value.type` | Meaning |
| --- | --- |
| `message` | A complete user/assistant/tool-result message that is model-visible. |
| `compaction` | Local compaction: history older than `first_kept` is replaced by `summary`, or by its optional `snapcompact` inline PNG frames plus lead-in on vision routes. |
| `responses_turn` | Opaque OpenAI Responses output stored beside its canonical assistant message. Not model-visible. |
| `responses_compaction` | Opaque `POST /responses/compact` checkpoint that covers the active branch through `covered_through`. |
| `config` | Model/reasoning marker; not model-visible. |
| `prompt_template_selected` | Recorded template name and full-file hash for provenance. |
| `skill_activated` / `skill_resource_read` / `skill_deactivated` | Explicit skill activation lifecycle and bounded resource reads. |

An API 0.4 bitmap compaction retains `snapcompact.source_text` alongside the
base64-encoded inline `snapcompact.frames`, for later re-compaction without
losing earlier source. Only frames and the bounded lead-in enter vision-model
context. A text-only model refuses an active bitmap checkpoint instead of
silently omitting it. Existing text-only compaction records omit this field.

Conversation entries commit complete semantic boundaries, not provisional
streaming deltas. Auxiliary observations below and the assistant sidecar are
separate; neither proves completion or an exactly-once external effect.

## Auxiliary invocation state

```json
{"type":"tool_invocation","scope":{"operation_id":"1","invocation_id":"0"},"record":{"generation":0,"state":"EffectPending","values":{}}}
```

The operation is the immutable assistant entry ID; invocation is the zero-based
source call index within that assistant batch. A provider's reusable tool-call
ID is **not** invocation identity. Each record replaces the invocation's bounded
live state. `generation` fences handles; state spelling is `EffectPending` or
`OutcomeReady`. String-valued addresses are:

- `pi.pending.tool_output:<operation>:<invocation>`: latest bounded raw progress.
- `pi.op.tool_memo:<operation>:<invocation>:<name>`: serialized JSON memo text.

Defaults bound a value to 64 KiB, values per invocation to 256 and live
invocations to 64, within the session's 256 MiB/1,000,000-record envelope.
The product's Bash checkpoint sink retains its existing 50 KiB snapshot and
interval bounds. These records do not move the head or affect usage/cost.

A synced paired tool result removes live state and fences retained handles;
replay applies the same cleanup. Earlier append-only bytes are **not securely
erased**. Read-only handles cannot mutate invocation state. Safe recovery keeps
memos but clears obsolete progress; unsafe unresolved effects are not repeated,
and interruption reports distinguish the last observation from an unknown
external outcome. A crash before memo persistence can repeat work: this is not
an exactly-once guarantee. Raw memos/checkpoints can contain private tool output.

## Branching

`parent` links make the file a tree, not a list. `octet --fork SESSION_ID`,
and `/fork` append a new branch without rewriting
ancestry; the `head` record tracks the active leaf. Legacy append times cannot
be recovered, so `timestamp_unix_ms` is omitted for historical entries.

## Sidecars and privacy

Names, tags, and other presentation metadata live in a `.metadata/<id>.json`
sidecar (written with owner-only permissions) so the transcript stays readable
without a format change. `octet sessions export` produces a redacted portable
form; the raw JSONL is not a sharing format.

One more sidecar exists only while an assistant attempt is streaming:
`<session>.partial-assistant-frames` (owner-only) journals the compact
`AssistantMessageFrame` sequence for the in-flight turn, bounded to 8192 frames
or 1 MiB. It is never a session record and never enters provider-visible
context, usage, or cost. Terminal stream events contribute no frame, so the
journal holds partial progress only. The next start reduces the surviving
prefix into the in-progress assistant message and republishes it once (a torn
final line is dropped); terminal settlement removes the journal, so a completed
turn is never replayed as partial progress.
