# Remote terminal UI (API `0.4`)

`remote_ui` is an optional, frontend-bound process-extension feature. An
extension opens a surface, publishes styled line snapshots, and receives focused
input without acquiring the terminal. Rust composes cached frames through the
existing `sexy-tui-rs` renderer; painting never makes an extension RPC.

The host owns terminal output, focus, resize, approvals, process/session lifetime,
and restoration. The extension owns its component objects, render logic, timers,
and optional dependencies. Native extensions need no Node or JavaScript runtime.
The optional [Pi adapter](../pi-compatibility.md) uses this same language-neutral
transport; it never starts Pi's agent or terminal runtime.

## Negotiation and ownership

Select API `0.4` and negotiate `remote_ui` only when offered in
`initialize.params.protocol.optional_features`. APIs `0.1`–`0.3` do not acquire
it. The host must configure the frontend wake/consumer binding before advertising
it; unavailable or noninteractive frontends do not invent UI success. Existing
extension enablement, trust, and effect-policy gates still apply.

Open/close requests carry a numeric `parent_request_id` and may carry a previously
issued `resource_owner`. A live parent's owner takes precedence. After normal
parent settlement, the same issued owner can authorize retained-context calls.
Foreign, stale, cancelled, or replaced owners are refused. Keep the complete
`{session_id, extension_instance_id, process_generation}` triple, not a session
name manufactured by the extension.

A surface outlives its opening command, not its process generation or foreground
session. Return the command response after opening; do not keep an ordinary
request pending throughout a game. The Pi adapter retains the original JS
`ui.custom()` promise and handler continuation until `done()` or host dismissal,
while settling the Rust command once the mounted component publishes its first
frame. Subsequent host effects remain owner-validated, not ambient authority.

## Extension-to-host messages

Each envelope is one UTF-8 JSON object plus LF on protocol-only stdout.
Diagnostics go to stderr.

### `ui/open` — request

```json
{"jsonrpc":"2.0","id":"open-1","method":"ui/open","params":{"parent_request_id":7,"surface_id":"demo","title":"Demo","placement":"fullscreen","mouse_capture":false}}
```

Success returns `{"columns":80,"rows":24}` after the host admits the mount.
`placement` defaults to `fullscreen`; other placements are `header`, `footer`,
`above_editor`, `below_editor`, and `editor`. Header/footer/editor replace their
corresponding host surface; widgets compose around the composer. Restoring a
mount restores the ordinary host surface, not an extension-owned terminal.
Surface IDs are scoped to extension instance, generation, and foreground owner.
There are at most 16 live mounts. Focus conflicts, occupied exclusive placements,
host panels, or a ceded terminal are refusals, not successful opens.

An `editor` open additionally returns a host-issued `editor_mount_id`, for example
`{"columns":80,"rows":24,"editor_mount_id":"host-editor-mount"}`. Other
placements omit it. Treat this identity as opaque: it remains stable across
resize but changes when an editor is closed and reopened, even with the same
`surface_id` and owner. It authorizes no access without the matching live owner.

`mouse_capture` defaults to false and requests host-owned mouse capture for a
focused fullscreen surface. Close, crash, rescue, and owner replacement restore
the configured host mouse mode. Raw terminal mode writes are not frame content.

### `ui/frame` — notification

```json
{"jsonrpc":"2.0","method":"ui/frame","params":{"resource_owner":{"session_id":"host-issued-owner","extension_instance_id":"host-issued-instance","process_generation":1},"surface_id":"demo","revision":1,"columns":80,"rows":24,"lines":["\u001b[38;2;255;128;0mhello\u001b[0m"]}}
```

A frame completely replaces the previous line snapshot. `revision` increases
within a surface lifetime; zero is valid. Geometry identifies the host size used
to render it. The final frontend checks owner/generation, mount, geometry, and
revision. Stale geometry is rejected **before** advancing revision. Accepted
content is clipped to its rectangle; fullscreen retains a host rescue affordance.

Each mount has a bounded latest-frame mailbox, not an unbounded FIFO. Frames
wake the frontend independently of ordinary background polling. Key, mouse,
lifecycle, and effect messages do not use this replaceable-frame lane. Cached
component frames and focused input are UI-only, never provider context or durable
conversation history.

Bounds:

- surface ID: 1–64 safe ASCII identifier bytes;
- title: 1–128 terminal-safe UTF-8 bytes;
- dimensions: positive `u16` values;
- snapshot: at most 256 lines, 16 KiB per line, 512 KiB aggregate text;
- the existing 1 MiB JSON-line bound still applies.

Lines contain printable UTF-8 and checked SGR only: basic styles, 16-color,
indexed-color, and RGB foreground/background. Cursor movement, erase controls,
OSC, queries, embedded newlines, and other control bytes are rejected. The host
preserves approved styling; it does not feed ANSI through a plain-text widget.

### `ui/close` — request

```json
{"jsonrpc":"2.0","id":"close-1","method":"ui/close","params":{"parent_request_id":7,"resource_owner":{"session_id":"host-issued-owner","extension_instance_id":"host-issued-instance","process_generation":1},"surface_id":"demo"}}
```

Success returns `{}` after closing the matching mount. Another extension or owner
cannot close it. Refusals use existing typed `unsupported_feature`,
`invalid_request`, `bounds_exceeded`, and `not_foreground_owner` errors.

### `composer/set` — editor recovery checkpoint

Editor recovery checkpoints require API `0.4` with both `remote_ui` and
`composer` negotiated. They use the existing composer request, not a second
input or replay channel:

```json
{"jsonrpc":"2.0","id":"checkpoint-1","method":"composer/set","params":{"parent_request_id":7,"resource_owner":{"session_id":"host-issued-owner","extension_instance_id":"host-issued-instance","process_generation":1},"text":"draft","editor_checkpoint":{"surface_id":"demo","mount_id":"host-editor-mount","input_revision":4,"checkpoint_revision":5}}}
```

Success acknowledges the exact committed pair, not merely a queued request:

```json
{"jsonrpc":"2.0","id":"checkpoint-1","result":{"input_revision":4,"checkpoint_revision":5}}
```

`editor_checkpoint` has exactly four required fields:

- `surface_id`: the extension's current editor surface ID.
- `mount_id`: the `editor_mount_id` returned by its successful open. Both IDs
  contain 1–64 ASCII letters, digits, `_`, `-`, or `.`.
- `input_revision`: the highest **completely handled** host-issued input revision.
  Zero denotes the initial seed, before any input is acknowledged. This revision
  cannot move backward or exceed the highest revision the host issued.
- `checkpoint_revision`: strictly increasing within the mount, starting at one.
  Both revisions are integers no greater than `9007199254740991` (`2^53 - 1`).

Text uses the existing bounded plain-text composer contract (256 KiB maximum).
Unknown checkpoint fields, malformed identities, invalid types, and out-of-range
revisions are refused. Checkpoints on `composer/insert` are invalid. Plain
`composer/set` and `composer/insert` are refused while a custom editor owns the
composer; they retain their existing behavior when no editor is mounted.

The host resolves the authoritative owner at admission and rechecks foreground
ownership, process generation, live surface, mount identity, input/checkpoint
revisions, native composer revision, and input focus **at the shell mutation**.
A valid queued request can still be refused if rescue, replacement, a native edit,
or another input owner intervenes. The commit updates the hidden native composer
and its acknowledgement synchronously. Retired mounts cannot write into a later
native draft or a replacement editor; even same-text native mutations change the
native revision and prevent an old checkpoint from overwriting them.

Extensions must finish each input handler before acknowledging that input,
including release events and keys that do not change the text. A mid-handler
`onChange` is not evidence that the input finished. Serialize text checkpoints,
verify the exact returned revision pair, and publish a captured editor frame only
**after the checkpoint for that same captured draft is acknowledged**. Do not
render newer text after awaiting an older checkpoint. Frame publication or
painting alone does not establish a recovery checkpoint.

## Host-to-extension notifications

- `ui/key`: `{surface_id, key, kind, modifiers}`. Kinds: `press`, `repeat`,
  `release`; modifiers: `shift`, `alt`, `control`, `super`. Printable keys carry
  characters; named keys include `Enter`, `Escape`, `Tab`, `Space`, `ArrowUp`,
  `ArrowDown`, `ArrowLeft`, and `ArrowRight`. Editor keys additionally carry
  `editor_input: {mount_id, input_revision}`. The mount is the identity returned
  by open; the host issues a monotonically increasing revision starting at one
  before enqueueing each key. Press, repeat, and release all participate.
  Non-editor keys omit `editor_input`.
- `ui/mouse`: `{surface_id, kind, button, x, y, modifiers, wheel_delta}`. Kinds:
  `press`, `release`, `drag`, `move`, `wheel`; buttons: `left`, `middle`, `right`,
  `none`. Coordinates are zero-based terminal cells. Positive wheel delta is
  down. Only the focused capture lease receives these gestures.
- `ui/resize`: `{surface_id, columns, rows}`. Render future snapshots for these
  dimensions. Initial geometry comes from `ui/open`.
- `ui/closed`: `{surface_id, reason}`. Stop timers and release held input. A dead
  process cannot receive this observation; host restoration does not wait for it.
- `context/updated`: `{resource_owner, host}`. For negotiated remote UI only, this
  carries the host's bounded, secret-free session/model state to retained
  components. A complete current owner is mandatory. It grants no extra service
  authority and must not replace the state of another retained session.

Focused component keys/mouse gestures do not also edit the composer or reach
ordinary terminal-input observers. Host confirmation/input panels retain
priority. `Ctrl+G` is host rescue; `Ctrl+D` remains coordinated application close.
`Escape` belongs to the focused component, so Doom can open its own menu.

## Immediate rescue and recovery limits

`Ctrl+G` retires the focused mount immediately, without waiting for a final
checkpoint, extension response, or grace period. An unresponsive extension cannot
hold the native composer hostage. Close, crash, reload, session replacement, and
shutdown also discard stale mounts. Editor recovery reveals the host's last
committed draft (the initial native draft if no checkpoint committed); legitimate
newer native edits take precedence. Late checkpoints are refused even if their
owner or extension-local surface ID is still otherwise valid.

If issued input remains unacknowledged, the host reports:

> Custom editor closed; N input events were not acknowledged and may contain edits that were not recovered. The native draft retains the last host checkpoint.

`N` counts events, not lost characters: an unfinished event may be a no-op or a
release. The host does not pretend those edits were recovered and does not replay
raw keys. Unsupported terminal paste while an editor is mounted is explicitly
reported rather than silently applied to the hidden native composer. On voluntary
close, an extension may await accepted work before requesting `ui/close`; observed
host rescue must instead stop queued effects and frame publication immediately.

These are protocol and host recovery semantics, not a claim of full native Pi
editor qualification or stable-Pi API parity. Native binary/PTY qualification
must separately establish the integrated adapter's checkpoint/frame ordering.
The underlying draft and semantic transcript remain host-owned. Process
separation and capability declarations are **not an OS sandbox**.
