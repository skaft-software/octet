# Architecture

Maintainer reference for the experimental source snapshot, not octet 0.7.0
qualification. Start with the [Serve guide](README.md) for local use.
[LAN pairing](lan-pairing.md), native shells, and production live previews remain
design-only. The graphical protocol described here is separate from extension
API 0.3 and native-host protocol 1.

## Shape

```text
apps/web
  └─ transport-neutral client and deterministic reducer
       └─ octet-serve protocol
            └─ host service
                 └─ session supervisor
                      └─ one session actor per graphical session
                           └─ feature-gated coding-agent adapter
                                └─ one private App/Agent owner
```

The frontend knows only the versioned protocol. It must not know whether its
transport is same-host HTTP/WebSocket, a future native bridge, or an
authenticated LAN connection.

The shared React client lives in `apps/web/`; future native shells belong under
`apps/`. The client uses the compiled default theme and canonical octet
`01101111` byte mark. It does not host or synchronize a TUI.

## Extension packages

The first-party extension owns:

- bounded wire identifiers and payloads;
- host bootstrap and session catalog;
- authoritative session snapshots;
- typed item lifecycle events;
- command validation and idempotency;
- replay cursors and replay-gap recovery;
- session actor and supervisor orchestration;
- loopback HTTP/WebSocket transport;
- preview and artifact capability handles;
- production web assets;
- future device identities and LAN transport.

It must not expose `App`, provider credentials, unrestricted paths, raw process
handles, or internal TUI state over the wire.

## Coding-agent adapter

`App` is private to the binary package. A standalone extension crate cannot
truthfully create real octet sessions without an adapter at that ownership
boundary.

The adapter:

- is feature-gated;
- lives at `crates/octet-coding-agent/src/extensions/serve.rs`;
- constructs a new `App` for a requested session;
- translates agent/session lifecycle into renderer-neutral extension events;
- accepts only validated typed commands;
- preserves exactly one mutable owner per session;
- performs no presentation work;
- depends on the extension's host-facing trait rather than making the extension
  depend on coding-agent internals.

The feature-enabled package runtime keeps the internal dispatch into this
adapter tiny. The ordinary octet binary instead keeps a tiny external `octet serve`
dispatch into the installed runtime. Source-level extraction behind a stable
Runtime API is deferred. The default TUI, agent, AI, and `sexy-tui-rs` must not
depend on the web surface.

The adapter and client are presentation-only boundaries. They must not add
presentation instructions to the model, alter the system prompt or active tool
schemas, insert frontend state into session content, or ask another model to
summarize work for the interface. Existing broad local authority remains the
default; client authentication and agent authority are separate controls.

## Session ownership

One supervisor owns the graphical session catalog. Every active graphical
session has one actor and one `App`/Agent owner. Multiple clients may observe or
control that actor, but they may not create competing owners.

Each client keeps private presentation state such as:

- selected session;
- scroll position;
- open inspector and pane size;
- unsent composer draft.

Commands that affect shared execution carry stable command IDs. Repeating the
same command returns the original acknowledgement and never executes twice.

## Bootstrap modes

`GET /api/v1/bootstrap` creates and selects a provisional session by default.
`selectedSessionId` restores an explicit session. `inventoryOnly=true` returns
catalog state without creating, opening, or selecting a session; its
`selectedSessionId` and `selectedSession` fields are both `null`. The inventory
and explicit-selection query modes are mutually exclusive. Bootstrap catalog
cursors are anchored before asynchronous project/session listing, so a catalog
change racing the snapshot always retains a newer replayable revision instead of
being hidden behind stale list data.

## Item lifecycle

The protocol distinguishes provisional streaming state from durable committed
entries:

1. an item is created with a stable item ID;
2. bounded deltas update that item;
3. completion replaces provisional fields with authoritative content;
4. durable commit attaches the exact session entry identity;
5. reconnect either replays missing ordered events or replaces the state with a
   complete authoritative snapshot.

The client reducer must be deterministic and idempotent. Tool state, sources,
outputs, and changed files are derived from typed evidence, never from assistant
prose or command-shaped text.

## Pull-request projection

The coding-agent adapter discovers a session-associated pull request only from
structured `gh pr view --json number,url,state,isDraft` output. First-time
association remains disabled until the session admits user work; a bounded,
independent host refresher then keeps both hosted and inventory-only sessions
current without delaying agent commands. CLI execution is non-interactive and
shell-free, with null stdin, bounded output and concurrency, a four-second
timeout, and strict HTTPS pull-request URL validation. Hosted refreshers wait
fairly for detached query permits, while inventory work uses only immediately
available capacity so a large inactive catalog cannot starve live sessions.
Inventory evidence is attempted oldest-first in bounded rounds, so a temporarily
unavailable record cannot indefinitely pin every later session behind it.

The private `pull-requests-v1.json` sidecar stores the session ID, validated URL,
number, state, and refresh time. It is size/count bounded, opened without following
symlinks, and replaced atomically. Invalid or ambiguous persisted evidence fails
host startup. Evidence persistence and inactive transcript projection run on the
blocking pool rather than the agent runtime path; command-side projection
replacements read the actor's in-memory PR summary instead of contending with
that persistence lock. Temporary GitHub or CLI failure preserves the last valid
state; authoritative closure removes open evidence, while merged evidence is
terminal. Permanent deletion fences the session before removing evidence, so an
already-finishing discovery cannot recreate the sidecar record.
`session.pullRequestChanged` advances the actor's catalog projection and replay
sequence, never the durable conversation transcript. Bootstrap overlays retain
the host catalog's durable PR projection when an active actor view is waiting on
a backpressured evidence event. The web store replaces sidebar/command-center PR
evidence only from the catalog stream, so a delayed session envelope cannot
regress a newer hosted or inventory projection.

## Incomplete provider accounting

A durable session `usage_uncertainty` record does not move its head or create a
usage ledger row. The lightweight catalog/index replay accepts and validates it,
so resumed sessions remain listable and searchable and retain every known usage
record. Idle snapshots project `context.usageUncertain` directly from session
evidence, even if the host stopped before a run outcome was committed. Live
uncertainty publishes the same flag through `context.updated`; it never invents
a completed outcome.

The host retains a separate owner-private, bounded append-only
`usage-v1/usage-uncertainty.jsonl` log containing JSON session-ID strings. One
synced marker per session is enough to make accounting incomplete; duplicate
live publications and startup backfill are idempotent. A torn final write is
truncated on reopen, while complete malformed records (including blank lines)
fail closed. Like known inference accounting, these content-free markers survive
permanent session deletion. Known request ordinals, token buckets, model rows, timestamps, and
request counts are unchanged; unknown attempts are never zero-token requests.

An append, sync, quota, or record-consistency failure latches the host store
unavailable for further writes until reopen/replay; no later append may follow
a potentially partial record. APIs continue to expose only known subtotals with
`usage_uncertain: true`, even when the failed marker never reached disk. Reopen
repairs torn tails and syncs both replayed logs before trusting idempotent
retries. Permanent deletion requires available accounting and successfully
copies the quiesced session's final known rows and uncertainty marker before
writing deletion intent. Persistence failure therefore retains the transcript
as the recovery source.

Startup accounting also inspects archived projects. A genuinely missing session
is distinct from a corrupt or unreadable existing transcript; an inaccessible
registered project root is not proof that its separately stored transcripts
were deleted. Incomplete source inspection conservatively flags all accounting
queries and blocks deletion, while readable sources still contribute their known
rows. Reopen retries inspection without inventing unknown request ordinals or
persisting synthetic session IDs.

Live context uncertainty is published independently of host-marker success.
Idle manual compaction reconciles accounting on both success and failure, even
when only uncertainty evidence changed and no conversation entry was appended.
The open composer receives `context.updated` without waiting for another run;
a persistence error remains a command error, never a fabricated completion.

The usage stats, lifetime, and activity APIs expose optional `usage_uncertain`
flags (camel-case `usageUncertain` in the web projection). Unknown evidence has
no reliable timestamp, so **every period conservatively warns** while any marker
is retained; no date or period is invented. The web usage page labels numbers as
known subtotals and explains incomplete activity. The session composer keeps
usage/cost unknown visible after resume, independently of completion-review UI
and independently of the next-turn context cost estimate.

These are additive default-false fields, omitted on the wire when false. Existing
payloads and known-usage JSONL remain unchanged; graphical protocol and accounting
store versions are unchanged. Updated readers accept absent flags. As with other
strict DTO additions, older clients that reject unknown fields need the matching
web bundle to consume a flagged payload. Serve is experimental and its package
pins the exact host version; hosts and clients must use the matching bundle,
not an older strict client.

## Local transport

The described host binds only to IPv4 loopback and retains strict host/origin
validation, request/frame limits, security headers, and sanitized errors. A
one-use launch capability is exchanged for an ephemeral HttpOnly,
SameSite=Strict cookie before API or event-stream access. It must not gain LAN
access by binding the same server to `0.0.0.0`.

## Local terminal

When sandbox policy permits process execution, `octet-serve` owns a bounded
in-process PTY manager and exposes it only through the authenticated,
same-origin loopback WebSocket. A browser owner key reattaches to a retained
shell after disconnect, while the manager limits retained sessions to four and
bounds input, replay, and dimensions. A terminal is rooted at the configured
workspace; it is not a general path or remote-shell API. Server shutdown stops
all retained shells. [Process cleanup](../../design/serve-lifecycle-safety.md#owned-subprocesses)
is bounded and does not contain deliberately escaped processes.

## Preview isolation

Production advertises `previews: false`; fixture UI is not a registered live
preview. The following remains an isolation requirement, not implemented
production preview support:

Generated HTML and live previews use a separate, capability-limited surface.
They cannot access the main application DOM, provider credentials, arbitrary
host files, process APIs, or unrestricted navigation. Preview closing changes
presentation only; it does not stop a session or development server.
