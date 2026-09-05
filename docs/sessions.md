# octet sessions

[Documentation](README.md) · [Context](context.md) · [Commands](commands.md)

Continue the latest conversation in the current workspace:

```sh
octet --continue
```

Sessions are bounded, append-only JSONL, namespaced by workspace under the
configured session directory (`--session-dir PATH`). Complete semantic boundaries
are durable; provisional streaming deltas are not. Parent links preserve branches
without rewriting history. Names/tags live in `.metadata/` sidecars so the
conversation remains readable without changing its format.

## Resume and branch

```sh
octet --resume
octet --resume SESSION_ID
octet --fork SESSION_ID
octet --fork
```

Resume restores model/reasoning, prompt identity, tool panels, branches, and
historical prompt colors. Explicit `--model`/`--reasoning` override recovered
values. `--fork <id|path>` creates a new session from the selected head before
startup; bare `--fork` opens the picker.

| Interactive action | Result |
| --- | --- |
| `/resume [id]` | Resume directly or open the picker. |
| `/fork` | Pick an active-branch user message or the whole conversation for a new session. |
| `/clone` | New session from the current head without a picker. |
| `/tree` | Complete parent-linked history. `+` traces the selected branch; `*` marks its exact durable head. |
| `/checkout <entry-id>` | Move the durable head and branch from there without deleting ancestry. |
| `/name [name]` | Show or change the readable name. |
| `/export [path]` | Redacted portable export. |

The resume picker supports fuzzy, quoted-phrase, and `re:` regex filtering,
named-only filtering, recent/title/message-count sorting, and optional paths.
Tab toggles current/all-workspace scope; Ctrl+S cycles ordering, Ctrl+N filters
named sessions, Ctrl+P toggles paths, Ctrl+R renames, and Delete moves to trash.
All-workspace browsing is not a cross-workspace transcript index; a differently
scoped session cannot be resumed into the same live App.

## Commands

```sh
octet sessions list
octet sessions list --query parser
octet sessions inspect SESSION_ID
octet sessions rename SESSION_ID "parser hardening"
octet sessions tag SESSION_ID rust local-model
octet sessions export SESSION_ID
octet sessions export SESSION_ID --output ./handoff.octet-session.json
octet sessions delete SESSION_ID
octet sessions repair SESSION_ID
octet doctor
```

Listing/search is read-only and uses bounded metadata scans or the disposable
catalog. `list` searches IDs, names, derived titles, tags, internal JSONL paths,
and dates encoded in IDs, scoped to the selected workspace store. Modified times
appear as relative ages. `inspect` validates read-only and reports derived
active-branch title, size, entries, head, checkpoints, usage, and branch roots/leaves;
its connector tree and `/tree` include abandoned forks without treating them as
active context. `doctor` performs read-mostly prerequisite/provider/model checks
without constructing an Agent or starting executable extensions.

## Recovery

Delete moves JSONL and metadata to `.trash/`, not permanent unlinking. Repair
first writes an owner-private backup, then removes only an interrupted final
append. Corruption in a completed record is diagnosed, never automatically
rewritten. A dropped run never silently replays an unresolved mutating call;
its outcome is indeterminate. Cancellation cannot undo an already completed
action. [Network and effect recovery](tools.md#recovery-and-security).

## Portable export and redaction

Export validates and writes an owner-private `octet-session-export` version 1
JSON package with source identity, readable metadata, and records. Existing
paths are not replaced without `--force`.

Redaction is on by default: credential-like keys and string content in prompts,
tool arguments, and results are scanned. The bounded deterministic scanner
covers authorization/cookie headers, common API-token prefixes, credential
assignments and URL queries, URL userinfo, private-key blocks, and JSON objects
or arrays serialized inside strings. It preserves surrounding prose and UTF-8
and reports replaced values/fragments. This is a safety filter, **not proof that
arbitrary prose or media contains no secret**. `--include-secrets` requests raw
values with an explicit warning; use only for a trusted destination.

HTML export, hosted viewers, and cloud sharing are outside this local-first
session boundary. [Media/privacy](media.md#privacy-and-remote-reads).

## Discovery catalog

Each workspace store may contain `.catalog/sessions-v1.sqlite3`, a private,
disposable SQLite projection of bounded titles, active-branch message counts,
and transcript size/mtime fingerprints. JSONL and `.metadata/` remain
authoritative. Listing/resume need not scan all transcript bytes: only missing
or stale entries are streamed, the active row refreshes on normal app close,
and missing sessions lose their rows. The picker enumerates workspace-key
directories under the shared root and displays their `.workspace` markers.

Unavailable, locked, corrupt, oversized, or newer-version catalogs never block
access: octet falls back to bounded JSONL scans and rebuilds corrupt SQLite
contents. Removing `.catalog/` is safe; it is recreated on demand.

## JSONL schema

Each physical line is one JSON object with a `type` discriminator:

| Record | Meaning |
| --- | --- |
| `entry` | Immutable parent-linked conversation/state. |
| `head` | Active branch selection and cumulative cost. |
| `checkpoint` | Completed prompt and exact restorable head. |
| `usage` | Provider/model/token/cost accounting for one operation. |

Stable entry envelope:

```json
{
  "type": "entry",
  "id": "entry-id",
  "parent": "previous-entry-id",
  "metadata": {
    "prompt_model": "local-model-id",
    "prompt_model_source": "local",
    "prompt_color": "#5a36d6"
  },
  "value": { "type": "message" }
}
```

`parent: null` marks a root. Entry values are `message`, `compaction`, `config`,
`prompt_template_selected`, `skill_activated`, `skill_resource_read`, and
`skill_deactivated`. Template name/hash stay outside model-visible context.
Skill activation/resource events are resumable; compaction snapshots active
skills and cumulative Pi-compatible `details.readFiles` / `details.modifiedFiles`.
Older records missing those fields receive empty lists.

`metadata.prompt_color` is normalized sRGB, assigned at the original user append,
inert, and never provider input. It remains authoritative through resume,
checkout, branching, compaction, model switches, and theme reloads. Legacy prompts
without it may derive a deterministic fallback from their historical
`prompt_model`, never the currently selected model.

`usage` kinds include assistant turns, rejected Responses turns, compaction,
terminal gates, and `delegated_agent`. A delegated record names its host-created
child, completed turn/tool-call counts, aggregate disjoint token buckets,
route/model, exact category cost, and picodollar remainder. Child JSONL remains
the detailed transcript; a root mirror is written **once before the owning
checkpoint**. Session cost, `/cost`, footer, export, resume, and subsequent
cost-limit checks therefore include child spend without reopening private paths.

The head record is the only branch-selection mutation:

```json
{
  "type": "head",
  "id": "entry-id",
  "total_cost_microdollars": 0,
  "total_cost_picodollars_remainder": 0
}
```

Branching/compaction never rewrites entries. Context walks parents from the
selected head and applies compaction boundaries. Completed records are strict
UTF-8 and strict JSON; only a final unterminated record is a recoverable torn
append. Reads are bounded by bytes and record count.
[Persistence invariants](design/octet-agent.md#sessions).
