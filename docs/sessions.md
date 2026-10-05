# octet sessions

[Documentation](README.md) · [Context](context.md) · [Commands](commands.md)

```sh
octet --continue
```

Sessions are append-only JSONL files, grouped by workspace under the session
directory (`--session-dir PATH`). Only completed steps are saved, not streaming
deltas. Parent links keep branches without rewriting history. Names and tags
live in `.metadata/` sidecars, so the conversation file stays unchanged.

## Resume and branch

```sh
octet --resume                # pick from a list
octet --resume SESSION_ID
octet --fork SESSION_ID       # new session from that head
octet --fork                  # pick, then fork
```

Resume restores the model, reasoning, prompt identity, tool panels, branches and
prompt colors. An explicit `--model` or `--reasoning` overrides the recovered
value.

Inside a session, `/resume`, `/fork`, `/clone`, `/name` and `/export` do the
same ([commands](commands.md)). In the interactive TUI, a name you give a
session shows in the terminal window title as `octet · <name>`, and unnamed
sessions use `octet`. Renaming, resuming or starting a session updates the
title. Plain, print and RPC modes don't set a window title.

In the resume picker, type to filter by fuzzy match, `"quoted phrase"` or `re:`
regex. Tab switches between this workspace and all workspaces. Ctrl+S cycles the
sort (recent, title, message count), Ctrl+N shows named sessions only, Ctrl+P
toggles paths, and Ctrl+R renames. Ctrl+X (or Delete) asks to move the selected
session to trash, naming it: Enter confirms and Esc cancels. Trash is
recoverable. The session stays on disk, hidden from the picker, and the web UI's
trash can restore it. You can't trash the current session. A session from
another workspace can't be resumed into the same live app.

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

`list` is read-only. It searches IDs, names, titles, tags, JSONL paths and dates
in this workspace's store, and shows modified times as relative ages. `inspect`
reports title, size, entries, head, checkpoints, usage, and branch roots and
leaves, including abandoned forks (which never count as active context).
`doctor` runs read-mostly checks without starting an agent or an extension.

## Recovery

`delete` moves the files to `.trash/`. `repair` makes an owner-private backup,
then removes only an interrupted final append. A corrupted completed record is
reported, never rewritten. A dropped run never silently replays a mutating call,
so its outcome is indeterminate. Cancelling can't undo what already ran. See
[network and effect recovery](tools.md#recovery-and-security).

## Portable export and redaction

`export` writes an owner-private `octet-session-export` version 1 JSON file. It
won't replace an existing path without `--force`. Redaction is on by default and
scans prompts, tool arguments and results for credential-like keys and strings.
It's a safety filter, **not proof that arbitrary prose or media has no secret**.
`--include-secrets` asks for raw export-eligible values, with a warning. Use it
only for a trusted destination. Private extension metadata (including
annotations with no `public` flag) is always left out, in both JSON and HTML,
even with `--include-secrets`. That filtering doesn't change the private source
transcript. Private extension JSON string values may contain ordinary LF, CR,
and TAB; JSONL stores them escaped. Object keys, namespaces and entry types
remain control-free, and NUL, ESC and other controls remain rejected. Public
metadata retains its control-free contract. Existing metadata byte, depth,
node-count and key-size limits still apply.

For a shareable page, run
`octet sessions export SESSION_ID --format html --output ./session.html`. It
writes one owner-private, script-free file with formatted Markdown, bounded code
highlighting, branch links and validated inline image previews. Author HTML,
external links and terminal control characters stay inert, and audio and remote
media aren't fetched. Metadata keys named `Image` or `Audio` stay literal data,
not previews. Hosted viewers and cloud sharing are separate from this local
export. See [media privacy](media.md#privacy-and-remote-reads).

<details>
<summary>What the scanner covers</summary>

Authorization and cookie headers, common API-token prefixes, credential
assignments and URL queries, URL userinfo, private-key blocks, and JSON objects
or arrays serialized inside strings. It keeps surrounding prose and UTF-8 intact
and reports what it replaced. The scan is bounded and deterministic.

</details>

## Discovery catalog

Each workspace store keeps a disposable SQLite index,
`.catalog/sessions-v1.sqlite3`, so listing is fast. JSONL and `.metadata/` stay
authoritative, and deleting `.catalog/` is safe. It's recreated on demand.

<details>
<summary>How the index is kept</summary>

It holds titles, active-branch message counts and transcript size and mtime
fingerprints. Only missing or stale entries are streamed. The active row
refreshes on normal app close, and sessions that no longer exist lose their
rows. The picker lists the workspace-key directories under the shared root and
shows their `.workspace` markers. If the index is unavailable, locked, corrupt,
oversized or from a newer version, octet scans the JSONL instead and rebuilds a
corrupt one.

</details>

## JSONL schema

Session files are JSONL with `entry`, `head`, `checkpoint`, `usage` and
`usage_uncertainty` records, plus optional creation metadata in a leading
`header`. The schema is here for tools that read them.

<details>
<summary>Record types, entry and head examples, and rules</summary>

Each line is one JSON object with a `type`:

| Record | Meaning |
| --- | --- |
| `header` | Optional immutable creation identity, workspace, timestamp and parent-session reference; not a conversation entry. |
| `entry` | Immutable, parent-linked conversation or state. |
| `head` | The active branch and cumulative cost. |
| `checkpoint` | A completed prompt and its exact restorable head. |
| `usage` | Provider, model, token and cost accounting for one operation. |
| `usage_uncertainty` | Accepted-attempt exposure whose usage and cost are unknown. |

An entry, where `parent: null` marks a root:

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

Entry values are `message`, `compaction`, `config`, `prompt_template_selected`,
`skill_activated`, `skill_resource_read` and `skill_deactivated`. Template name
and hash stay outside the model's context. Compaction snapshots the active
skills and the cumulative Pi-compatible `details.readFiles` and
`details.modifiedFiles`. Older records without those fields get empty lists.

`metadata.prompt_color` is a normalized sRGB value set when the prompt is first
appended. It's never sent to the provider, and it survives resume, checkout,
branching, compaction, model switches and theme reloads. A legacy prompt without
it may get a fallback from its historical `prompt_model`, never the currently
selected model.

`usage` kinds include assistant turns, rejected Responses turns, compaction,
terminal gates, `cache_warm` and `delegated_agent`. Cache refresh usage counts
toward exact session totals and ceilings, never assistant-turn statistics or
model-visible context. Separate payload-free `cache_warm` lifecycle records
store attempt, route/model, timestamp, state, optional prefix `anchor`, and
`extension_override`; old records default missing metadata. An unsettled
`started` record remains uncertain on reopen. `/session` also reports the live
user-selected warming mode, economic decision and known refresh subtotal.
See [cache warming](cache-warming.md).

A delegated record names the child, its
turn and tool-call counts, token totals (counted separately), route and model,
and exact cost. The child's JSONL stays the detailed transcript. A root mirror
is written **once before the owning checkpoint**, so session cost, `/cost`,
export, resume and cost limits all include child spend.

An accepted inference attempt that's interrupted before authoritative usage is
available is recorded as `usage_uncertainty`, **not** as zero tokens or zero
cost. Its `record` holds only trusted `endpoint`, `model` and `operation`
identifiers, each at most 128 ASCII identifier bytes. No endpoint URLs, request
or response bodies, prompts, credentials or error text go in it. Unknown
exposure uses the same owner-private, locked, synced append path as known usage,
and a failed append must stop automatic replacement. The writer records each
failed physical attempt once, not each retry notification, so the records are
evidence, not provider-confirmed billable-attempt counts.

`Session::has_uncertain_usage()` stays true after later successful turns,
checkpoints, compaction, checkout (to another branch or the root) and reopening.
It's session-wide accounting, never model-visible context or head state.
Existing usage and cost totals are **known subtotals**, not complete spend. If
`usage_uncertainty.bound` is present, the ledger includes its recorded token
and worst-case cost exposure when checking limits, including after resuming.
A historical bounded record does not authorize a new provider request. Current
transports lack a trusted input-token admission bound, so hard token or cost
ceilings refuse new inference before dispatch, even when an output cap exists.
Planning estimates, prior usage and model context windows are not input bounds.
New ambiguous attempts therefore retain unbounded uncertainty; missing bounds
fail both ceilings, and missing prices fail the cost ceiling.
`usage_uncertainty_records()` exposes the bounded identifiers separately from
the optional admission bounds, and neither invents
provider-confirmed usage. Delegated exposure is mirrored into the owning root
ledger by child, recording only increases.

Older sessions without these records keep their known ledger, and the reader
doesn't invent evidence about past failures. Fork and clone start a new
accounting session: like known usage telemetry, uncertainty isn't copied by the
active-branch conversation projection. Restoring within the original session
never clears its exposure.

The head record is the only thing that changes the selected branch:

```json
{
  "type": "head",
  "id": "entry-id",
  "total_cost_microdollars": 0,
  "total_cost_picodollars_remainder": 0
}
```

Branching and compaction never rewrite entries. Context walks parents from the
selected head and applies compaction boundaries. Completed records are strict
UTF-8 and strict JSON, and only a final unterminated record counts as a
recoverable torn append. Reads are bounded by bytes and record count.
[Persistence invariants](design/octet-agent.md#sessions).

</details>
