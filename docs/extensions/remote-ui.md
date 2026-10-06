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
it. A host with an already constructed native `InteractiveShell` supplies that
consumer before process initialization; it need not infer the embedded terminal
from its own stdout. Ordinary terminal bootstrap retains its detected-terminal
gate. Supplying an inactive shell never falls back to another terminal, and
noninteractive modes do not advertise this feature. Existing
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

### `ui/chrome` — desktop notification intent

A foreground, owner-fenced request can ask the host terminal to display a
desktop notification without granting raw terminal access:

```json
{"jsonrpc":"2.0","id":"notify-1","method":"ui/chrome","params":{"parent_request_id":7,"resource_owner":{"session_id":"host-issued-owner","extension_instance_id":"demo","process_generation":1},"chrome":{"kind":"desktop_notification","title":"Ready","body":"The run finished."}}}
```

This requires negotiated `remote_ui` and the same owner admission as other chrome
requests. Title/body must be control-free UTF-8, at most 1024/4096 bytes; a title
cannot contain a semicolon. Ceded/unavailable terminals and a full 16-intent
mailbox are refused. The ordinary chrome receipt (`tools_expanded`) acknowledges
native admission, not OS delivery. Only the host renderer encodes OSC 777; the
terminal must support that desktop-notification protocol. Headless/plain output
does not advertise this frontend service. No escape bytes are accepted in frames.

### `ui/chrome` — native component editor service

The adapter's synchronous `Editor` facade uses `chrome.kind:"editor"` on an
already admitted surface, under the existing `remote_ui` negotiation and complete
owner triple. It sends `surface_id`, an optional opaque `editor_id`, and a closed
`operation` object. Composer placement also requires the exact host-issued
`mount_id`; another placement must not supply one. A ceded terminal, retired
surface/process/owner, wrong mount or disposed handle refuses the call.

`create` returns an opaque handle; `bind` selects the primary component editor.
Reads, input, replacement, history, render projection and disposal all use that
surface's native sexy-tui-rs model. The native autocomplete service issues query
IDs and revisions, bounds incoming suggestions, owns selection/navigation and
checks the originating revision before completion. Completion coordinates are
UTF-16 line/column observations validated at the native boundary, including
scalar/grapheme boundaries; stored native carets are UTF-8 byte offsets.

Bounds are 16 editors per surface, 256 KiB raw/expanded draft, 100 history entries
and 256 KiB aggregate history, and 64 recovery paste entries / 4 MiB. Local
provider menus have at most 128 suggestions, 4096 bytes per string and 512 KiB
aggregate; controls are rejected. Registry completions retain their existing,
narrower wire profile. Retirement drops the service, with no Pi/JS editing-engine
fallback. This is editing, not a terminal or synchronous native-render RPC.

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

### Native composer admission

An editor occupies only the composer slot. Octet's terminal owner reserves
submit/follow-up, clear, close, application bindings and its slash popup before
routing ordinary editing keys to the component. There is no `composer/submit`
reverse RPC and no custom-editor `session/send_user_message` submission path.
Native admission reads the last acknowledged, expanded composer draft, parses
slash commands through the same registry, steers busy Enter, and queues Ctrl+S
follow-up. Refusal leaves the component's draft/pastes/undo untouched.
If issued input is still unacknowledged, draft-sensitive host actions wait for its
completed checkpoint. Later keys/pastes keep their order in a mount-bound buffer
of at most 128 events and 256 KiB paste bytes; overflow is reported without
admitting that event. Clear, cancel and close bypass the wait. Retirement drops
the buffer; an explicit fullscreen view never receives buffered composer events.

An accepted submission clears the native draft and sends a `ui/editor-text`
replacement to the component. Completion and restored drafts use the same seam.
The answering checkpoint is an ordinary, exact-pair `composer/set` receipt;
only the acknowledged empty clear may answer while a command's panel owns focus.
That receipt does not grant general panel writes.

### `composer/history` — native prompt-history seed

With negotiated `composer`, an owner-scoped request may seed older native prompts:

```json
{"jsonrpc":"2.0","id":"history-1","method":"composer/history","params":{"parent_request_id":7,"resource_owner":{"session_id":"host-issued-owner","extension_instance_id":"host-issued-instance","process_generation":1},"entries":["newer prompt","older prompt"]}}
```

Entries are newest-first, at most 100, and at most 256 KiB UTF-8 in aggregate.
Plain-text validation permits newline/tab but rejects terminal controls; unknown
fields and malformed entries are refused. The normal composer feature, generation,
foreground-owner and cancellation fences apply. Success returns `{}` after the
shell prepends the older entries, deduplicates them against native history and
retains the latest 100 prompts. The current draft, history navigation and native
attachment payloads are preserved. This seed is UI-only, not session persistence
or model context.

Pi factories are invoked exactly once on the admitted component surface. Both
settings-only and input/render-overriding editors occupy the composer slot;
the native slash popup remains available around their projected rows.

## Host-to-extension notifications

- `ui/key`: `{surface_id, key, kind, modifiers}`. Kinds: `press`, `repeat`,
  `release`; modifiers: `shift`, `alt`, `control`, `super`. Printable keys carry
  characters; named keys include `Enter`, `Escape`, `Tab`, `Space`, `ArrowUp`,
  `ArrowDown`, `ArrowLeft`, and `ArrowRight`. Editor keys additionally carry
  `editor_input: {mount_id, input_revision}`. The mount is the identity returned
  by open; the host issues a monotonically increasing revision starting at one
  before enqueueing each key. Press, repeat, and release all participate.
  Non-editor keys omit `editor_input`.
- `ui/editor-text`: `{surface_id, text, paste, editor_input?, native_changed?}`. A bounded native
  replacement/clear has `paste:false`; a paste has `paste:true` and the same
  host-issued mount/input fence as a key. Paste enters the component at its
  actual caret through bracketed-paste handling, preserving undo and payloads.
  An unchanged replacement is an echo, not a reset. Reply with a newer ordinary
  checkpoint before publishing the changed draft frame. For a bound native
  facade, a non-paste decision is already committed to its native model;
  `native_changed` signals the callback intent without replaying `setText` and
  relocating that caret. Unchanged echoes and obsolete observations are not edits.
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
priority. `Ctrl+G` rescues only an explicit fullscreen view; it never unmounts
an editor or its footer. `Ctrl+D` remains coordinated application close.
`Escape` belongs to a fullscreen component, so Doom can open its own menu;
in the composer, native application and popup bindings remain authoritative.

## Pre-native input interception

`terminal_input_intercept_v1` is an optional API `0.4` feature offered only with
`remote_ui` and a bound frontend. A frontend that negotiates it asks each
intercepting extension about one editor input event after the host-reserved grammar
and open native slash popup, but **before** the slot editor. A Pi extension's
`onTerminalInput` listeners see the spelling the terminal produced:

```json
{"jsonrpc":"2.0","id":"input-1","method":"ui/terminal-input/intercept",
 "params":{"data":"\u001bOA","resource_owner":{"session_id":"host-issued-owner","extension_instance_id":"demo","process_generation":1}}}
```

The reply is the whole replacement string, decoded by the host into ordinary
input events:

```json
{"jsonrpc":"2.0","id":"input-1","result":{"data":""}}
```

- `data` in either direction is at most 256 UTF-8 bytes, the existing terminal
  input bound; larger input bypasses the chain and stays native. Escape bytes are
  legitimate here (they are the input), so this one string is exempt from the
  usual control-free rule.
- An empty reply consumes the event. A reply that is not a complete, decodable
  input sequence — a partial escape, a malformed sequence, or a terminal *reply*
  such as a cursor-position or device-attributes report — is refused; the host
  never turns protocol traffic into user input.
- The frontend times the whole chain and delivers the original event unchanged if
  the budget expires, so an unresponsive extension cannot freeze typing. It may
  then stop asking that process until reload, and it reports one bounded
  diagnostic. Throwing handlers are reported without losing their subscription.
- Native reserved actions (including submit, busy steering, follow-up, clear,
  close and live user keybinding overrides), open slash-menu keys and an active
  native transcript-search query bypass the lane. The stream consults the same
  read-only host policy as the composer slot; it does not dispatch native actions.
  `Ctrl+G` bypasses consumers only as fullscreen rescue; in the composer it remains
  ordinary consumer/editor input. `Ctrl+D` always remains coordinated shutdown.
- Only the raw spelling of decoded input is offered. Host-derived events (resize,
  focus, terminal replies, Tern protocol input) have no terminal bytes and stay
  out of the lane. Unix input is decoded by octet's single byte decoder, so the
  spelling is exactly what the terminal sent; on Windows, where octet receives
  console key records instead of bytes, a key carries the console's legacy
  spelling when one exists (text, `Esc`, `Alt+]`, `Alt+_`, `Alt+\\`, `Ctrl+G`) and
  keys without one stay native rather than gaining an invented spelling.
- The request is deliberately **not** owner-routed: it carries
  `resource_owner` as an ordinary parameter, because a listener that only
  decides how one keystroke is dispatched must not acquire session-view or
  history-transport work. The frontend still validates the owner (and its
  liveness) on both sides.

This is a frontend-owned ordering guarantee, not an extension-scheduled hook: the
request is answered while the user is typing, and a slow reply delays only input,
never rendering, resizing or shutdown.

## Immediate rescue and recovery limits

`Ctrl+G` retires the focused **fullscreen** mount immediately, without waiting for a final
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
raw keys. Terminal/clipboard paste is fenced component input, never a silent
write into a hidden native draft. On voluntary
close, an extension may await accepted work before requesting `ui/close`; observed
host rescue must instead stop queued effects and frame publication immediately.

These are protocol and host recovery semantics, not a claim of full native Pi
editor qualification or stable-Pi API parity. Native binary/PTY qualification
must separately establish the integrated adapter's checkpoint/frame ordering.
The underlying draft and semantic transcript remain host-owned. Process
separation and capability declarations are **not an OS sandbox**.
