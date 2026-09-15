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
| `usage` | Usage for one assistant turn or compaction; does not affect model-visible context. |
| `usage_uncertainty` | An accepted attempt whose usage is unknown. |

Unknown record types are ignored on read so a newer file stays openable by an
older binary. A torn or non-UTF-8 trailing line is dropped rather than failing
the whole session; `octet sessions repair SESSION_ID` rewrites a file whose
damage is not confined to the tail.

## Entries

An `entry` has `id`, `parent` (`null` marks a conversation root), optional
presentation `metadata`, optional `timestamp_unix_ms`, and a `value` object that
is itself `type`-tagged (`crates/octet-agent/src/session.rs:479`):

| `value.type` | Meaning |
| --- | --- |
| `message` | A complete user/assistant/tool-result message that is model-visible. |
| `compaction` | Manual compaction: history older than `first_kept` is replaced by `summary` when context is reconstructed. |
| `responses_turn` | Opaque OpenAI Responses output stored beside its canonical assistant message. Not model-visible. |
| `responses_compaction` | Opaque `POST /responses/compact` checkpoint that covers the active branch through `covered_through`. |
| `config` | Model/reasoning marker; not model-visible. |
| `prompt_template_selected` | Recorded template name and full-file hash for provenance. |
| `skill_activated` / `skill_resource_read` / `skill_deactivated` | Explicit skill activation lifecycle and bounded resource reads. |

Only complete semantic boundaries are durable. Provisional streaming deltas are
never written, so a process kill loses at most the in-flight turn.

## Branching

`parent` links make the file a tree, not a list. `octet --fork SESSION_ID`,
`/fork`, and `/checkout <entry-id>` append a new branch without rewriting
ancestry; the `head` record tracks the active leaf. Legacy append times cannot
be recovered, so `timestamp_unix_ms` is omitted for historical entries.

## Sidecars and privacy

Names, tags, and other presentation metadata live in a `.metadata/<id>.json`
sidecar (written with owner-only permissions) so the transcript stays readable
without a format change. `octet sessions export` produces a redacted portable
form; the raw JSONL is not a sharing format.
