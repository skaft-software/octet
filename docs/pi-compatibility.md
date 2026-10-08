# Optional Pi extension compatibility

Octet can load explicitly reviewed Pi extension factories through the optional
`extensions/octet-pi-compat` Node package. **Rust remains the agent and terminal
host.** The fallback loads installed Pi helpers in the extension process, not a
second Pi CLI or agent loop. Ordinary octet use and native executable extensions
do not need Node.

**Source preview:** unchanged Doom, drawing, and powerline-footer factories have
passed actual octet binary/PTY acceptance as well as synthetic-host tests. The
original rainbow editor now passes paced and immediate-burst rescue on a fresh
native binary, including retained editable draft, resize and shutdown. Full Pi API
parity and a startup-speed comparison are not established.

The contract is **Pi 1.0.2's public extension API**, replicated under Octet's
subprocess JSON-RPC protocol. octet 0.9.0 is pinned to exactly Pi 1.0.2: the
adapter ships the dependency versions Pi 1.0.2 ships (jiti 2.7.0, TypeBox
1.3.27, pi-tui 1.0.2) and resolves imports through the same module table as Pi
1.0.2's extension loader. The [27-row ledger](pi-extension-api.md) is the
bounded scope and distinguishes implementation from real-host acceptance.
Arbitrary third-party extensions, private Pi internals, the Pi CLI and child SDK
are not targets. The [package README](../extensions/octet-pi-compat/README.md)
describes the adapter; historical package acceptance is not public API parity.

<a id="explicit-local-setup"></a>

## Bring your Pi extensions

Requires Node 22.19+ or Bun and an existing managed **Pi 1.0.2** installation. From a
source checkout, install the adapter's dependencies once:

```bash
cd /absolute/path/to/octet/extensions/octet-pi-compat
npm ci --ignore-scripts --no-audit --no-fund
```

Then import your enabled Pi extensions with one setup command:

```bash
node configure.mjs --reviewed --from-pi \
  --output "$HOME/.octet/pi-import/octet-pi-compat"
octet --extension-dir "$HOME/.octet/pi-import" --enable-extension octet-pi-compat
```

Setup uses Pi's resolver, tries each factory on octet's emulated path, then tries
the installed-Pi fallback if loading fails. It prints the chosen route or a skip
reason for every entry. Broken factories and conflicting registrations are
skipped. Reviewed builtin overrides receive exact manifest grants and require
native `builtin_tool_overrides_v1` admission; first-party ownership conflicts
remain refused. Unsupported tool-schema constraints cause an explicit skip, not
constraint removal. Supported patterns are enforced. Loading is not proof that an extension's commands, hooks or UI work.
Failing lifecycle observation callbacks are reported and isolated; permission/veto
hooks and failed host mutations still fail closed. Replace `node` with `bun` to
reuse Bun; generated configuration records the executable.

`--reviewed` accepts executing the enabled factories and their imports, including
installed Pi code on the fallback route, with your OS permissions. Setup does not
import credentials, change Pi settings, or enable the bridge. It snapshots enabled
Pi palettes as native `pi-*` themes and makes the selected palette this session's
startup preference; explicit `--theme`/`OCTET_THEME` wins. Saved settings are not
rewritten. Pi helper/remote-component colors use the imported snapshot; later
native theme changes are not yet synchronized to those components. Missing
packages are not installed. The Pi install location is recorded, so a different
HOME at runtime does not hide it. A Pi upgrade requires reconfiguration.

<a id="mirror-your-pi-setup"></a>

### Mirror your Pi setup

To have an enabled `octet-pi-compat` activate this machine's Pi setup instead of
one frozen import, configure the reviewed mirror opt-in once:

```bash
node configure.mjs --reviewed --from-pi --mirror \
  --output "$HOME/.octet/pi-import/octet-pi-compat"
octet --extension-dir "$HOME/.octet/pi-import" --enable-extension octet-pi-compat
```

`--mirror` records the opt-in (the Pi agent directory and one advertised hook
set) rather than capturing the current entrypoints. At session startup the
adapter discovers the Pi setup **read-only** and activates its equivalents
through Octet's native machinery: the enabled Pi factories load through the
adapter, and Pi skills, prompt templates, themes, `keybindings.json`, the global
context file and the selected model become native session resources. Discovery
is bounded; missing, linked, oversized or pattern-filtered inputs are reported,
never guessed at, and packages are only used if they are already installed —
discovery never writes the Pi setup, and it never rewrites Octet configuration
(the generated mirror stays in the import directory you chose above).

Everything the mirror contributes is session state owned by the process that
enabled the extension, so disabling `octet-pi-compat` returns exactly to the
user's Octet-only setup, and re-enabling or `/reload` is idempotent. Mirror mode
runs the Pi setup's enabled factories with your OS permissions whenever the
extension starts, so it is the same explicit-review decision as `--from-pi`.
The mapping is item-by-item: Pi `settings.json` (theme, default
provider/model, package/resource path lists), the user and project
`extensions|skills|prompts|themes` directories, installed managed npm package
resources, `keybindings.json` and the global context file. Deliberate
boundaries: Pi `models.json` HTTP providers and Pi `defaultThinkingLevel` are
not applied, Pi resource pattern filters are refused with a diagnostic, and
`auth.json`, sessions and trust state are never read. Mirror mode refuses a
factory that needs a hook outside the advertised set instead of silently
leaving it inert.

After matching release assets are published, `octet extension install octet-pi-compat`
installs the adapter with its locked npm dependencies. Run its
`~/.octet/extensions/octet-pi-compat/configure.mjs` with the same arguments above.
Keep the generated output separate from the installed bundle; don't overwrite
its integrity-checked files.

To select factories manually instead, replace `--from-pi` with absolute paths to
reviewed entrypoints.

The helper captures registrations and writes a manifest plus a bridge
configuration containing entrypoint hashes. It does **not** enable or trust the
extension, change HOME, or download extension packages. It refuses existing
configuration without `--overwrite`. Registrations must still match when the
host starts it; changed entrypoints require fresh review and configuration.
Transitive imports are not sandboxed or made immutable by an entrypoint hash.
Resource discovery is reserved only if a reviewed factory registered
`resources_discover` during capture. Ordinary factories do not require
`resource_paths_v1`; captured resource factories still refuse hosts without that
consumer. Late resource subscriptions require reconfiguration, not silent
activation. Capture alone does not qualify native resource loading.

In that workspace, inspect `octet extensions list`, then explicitly enable and
trust the generated extension with the normal [extension
commands](extensions.md). Alternatively use octet's interactive
extension manager. Keep the Node package at its configured absolute path;
the generated manifest references it. Node dependencies are extension-local.

## Message observations

The adapter supplies Pi `message_start/update/end` payloads and run-local
`agent_end.messages` from the prompt, text stream and subscribed model-turn hooks.
Committed assistant/tool messages replace the partial observation before
`turn_end`; cancellation or a failed projection settles the streamed partial.
Message observers request the corresponding hooks. Final model/usage/cost/stop
facts come only from the assistant's linked persisted record, never aggregate
session usage or the currently selected model. Missing facts remain explicitly
unavailable. Exact tool-execution boundary parity remains incomplete; see the
[adapter contract](../extensions/octet-pi-compat/README.md#implemented-surface).
A complete, bounded OSC 777 desktop-notification write is translated to an
owner-fenced native notification intent. The host renderer emits it; terminal
support and OS delivery remain terminal-dependent. Other direct terminal control
writes stay refused.

## UI execution model

Pi components render inside the adapter, using selected pinned Pi TUI utilities
and a headless component/editor driver. A Pi `ui.custom()` mount settles the
Rust command after its first frame while preserving the JS handler's pending
promise until completion/dismissal. Rust composes cached styled frames via
[`remote_ui`](extensions/remote-ui.md), routes focused keys and host-captured
mouse gestures, and owns terminal resize/restoration. There is no second stdin
reader, terminal renderer, or synchronous render RPC.

Header/footer/widgets are distinct generic host placements. The original footer
receives secret-free host model/session/context snapshots instead of fabricated
usage or OAuth data. The host publishes secret-free OAuth provenance from its
actual authentication resolver (including Codex and subscription resolvers),
without reading or refreshing credentials. Unclassified dynamic resolvers still
refuse that query; endpoint names never establish authentication kind.
Unavailable capabilities fail explicitly. Custom editor
state uses the host composer handoff rather than a separate agent input loop.
Every custom editor occupies the composer slot, leaving native transcript,
chrome and the single slash popup visible. `Ctrl+G` rescues only explicit fullscreen
views; it never unmounts an editor. `Ctrl+D` remains host-owned shutdown.
`onTerminalInput` listeners run after native reserved actions, the open slash popup
and native search-query ownership, but before the slot editor, in registration
order over the raw terminal spelling. They may consume or replace one event. The
whole chain is bounded to 50 ms per event: a listener that stops answering is
latched off and its input is delivered unchanged, because Pi would block typing
indefinitely. Input past the 256-byte wire bound bypasses the chain. Editor
submissions use native idle/busy admission; refusals preserve component paste and
undo state. The adapter's `Editor` and `CustomEditor` now share a synchronous
native-backed facade: sexy-tui-rs owns editing state, caret, undo/history/paste
recovery and autocomplete state. JS supplies presentation and callback hooks,
not a second editing engine. Registry argument menus use the bound native caret
and mount/handle/revision/owner fences; Up/Down and Tab stay host-owned.
The 193 immutable Pi 1.0.2 Editor/history cases and separate ownership guard pass
against the compiled production Rust service. Real App/adapter tests separately
qualify admission and custom-editor registry completion/clear/retirement.

Raw extension terminal writes are not allowed into frames. The adapter recognizes
allowlisted mouse-mode writes as capture intent and bounded complete OSC 777
writes as desktop-notification intent, both subject to host refusal. Do not use unrelated Pi terminal patches, private session internals, or
session-file writers as compatibility assumptions.

<a id="installed-pi-fallback-experimental-opt-in"></a>

## Installed-Pi fallback (experimental)

`--from-pi --reviewed` authorizes automatic fallback during setup and records a
route for each accepted factory. Manual entrypoint setup still defaults to the
emulated path. Its `pi_compat_fallback_eligible` diagnostic names missing Pi
exports; explicitly selected `--pi-runtime installed` or
`"pi_runtime": "installed"` remains available for reviewed manual configurations.
Only managed Pi 1.0.2 is accepted.

This runs code from your installed Pi 1.0.2 (`@earendil-works/pi-coding-agent`,
`pi-agent-core`, `pi-ai` and `pi-tui`) inside the extension process:

- Pi's built-in tool factories run commands and file reads/writes in that
  process, outside Octet's per-command tool policy, other extensions'
  `tool_call` hooks, output spill and process-group supervision.
- Importing Pi installs process-wide signal handlers, an HTTP dispatcher and
  `fs` patches in that process.
- Octet still owns the `pi` API object, sessions, model calls, settings and the
  terminal. Pi functions that would start Pi sessions or agents, call models,
  read credentials, write Pi config or trust, start MCP servers, or own the
  terminal or clipboard are refused, as are names Octet has not classified.
- A missing Pi install, or any version other than 1.0.2, fails loudly; nothing
  downloads or switches Pi versions silently.

## Qualification and reproduction

The automated adapter suite uses a synthetic host. A run with all four original
entrypoints explicitly supplied passed **22 tests, zero skips**, including Doom
WASM animation/resume, drawing mouse export, owner-retained powerline snapshots,
and rainbow-editor animation. Synthetic-host results alone do **not** establish
terminal painting. A separate actual octet binary/PTY run passed unchanged Doom,
drawing, the powerline footer, and terminal restoration on macOS. That run is a
one-off acceptance, not a retained timing or speed comparison.

Historical rainbow-editor probes verified paced/burst recovery through Ctrl+G
retirement before the composer-slot architecture. They do not qualify the current
editor contract: Ctrl+G now retains the slot. Use `scripts/test-pi-composer-slot.py`
with reviewed unchanged footer/custom-editor factories to check one draft, one
native slash popup, Ctrl+G retention, native clear, resize and Ctrl+D restoration.
Arbitrary editor overrides and the wider extension corpus remain unqualified.
The bounded native Editor corpus is independently reproduced using the
[test-peer instructions](../extensions/octet-pi-compat/test/pi-v1.0.2/README.md);
its pure `wordWrapLine` cases are library evidence, not native wrapping proof.

The Pi bridge withholds `modelRegistry.getApiKeyAndHeaders` by default. Configure
with `--provider-credentials` only after reviewing the loaded factories and imports;
the generated capability is negotiated explicitly, and the host resolves each
request against exactly one native provider/model. Values are returned only to the
requesting factory and omitted from status/diagnostics. OAuth request signers that
cannot be represented as Pi key/header results remain refused; builtin provider
inheritance and the original Anthropic-attribution provider remain unsupported.
See the [adapter README](../extensions/octet-pi-compat/README.md) for configuration
and security limits.

```bash
export CARGO_TARGET_DIR=/absolute/existing-target
CARGO_PROFILE_DEV_DEBUG=0 cargo build -p octet-coding-agent --example native-editor-test-host --locked -j3
node --test extensions/octet-pi-compat/test/adapter.test.mjs
# Original-factory tests require explicitly reviewed local paths:
PI_DOOM_PATH=/absolute/pi-doom/src/index.ts \
PI_DOOM_WAD=/absolute/pi-doom/doom1.wad \
PI_DRAW_PATH=/absolute/draw.ts \
PI_FOOTER_PATH=/absolute/powerline-footer/index.ts \
PI_RAINBOW_PATH=/absolute/rainbow-editor.ts \
node --test extensions/octet-pi-compat/test/*.test.mjs
python3 scripts/test-pi-compat-pty.py --help
# Actual native UI acceptance (review the original sources/assets first):
python3 -B scripts/test-pi-compat-pty.py /absolute/path/to/octet \
  --doom /absolute/pi-doom/src/index.ts \
  --draw /absolute/draw.ts \
  --footer /absolute/powerline-footer/index.ts \
  --capture /absolute/native-ui.pty
```

Without those environment variables, original-factory probes skip; the suite
never searches HOME or developer temporary directories for executable factories.

The PTY script creates an isolated HOME/workspace, installs only supplied reviewed
entrypoints, and uses an offline model (no inference/credentials). Available
probes cover original `pi-doom`, Ben Vinegar's `/draw`, the installed powerline
footer, and Pi's example rainbow editor. It tests rendering/input/resize/rescue
and terminal restoration, not full Pi API parity. Editor acceptance runs
separately. `--pi-cli /absolute/pi/dist/cli.js` adds repeated
spawn-to-first-synchronized-frame measurements
for comparison; this is a startup metric, **not** an inference or throughput
benchmark. Consult the recorded test output for which probes actually passed.

The originals and their assets are not bundled into octet. Install them
separately and preserve their upstream [licenses and
notices](../THIRD_PARTY_NOTICES.md). Doom declares GPL-2.0; its WAD has separate
terms.

## Real-package matrix (recorded 2026-10-06)

A compatibility matrix ran 34 published Pi packages against the merged 0.9.0
candidate adapter with isolated CLI configuration and a scratch workspace (the
user's `~/.octet` was never modified). The recorded classification:

| Result | Packages | Meaning |
| --- | --- | --- |
| pass | 7 | the factory loaded and a real command or tool executed in the TUI or a headless run |
| blocked by an Octet defect | 12 | the package exposed an Octet-side defect, for example a refused completion registration, an adapter process exit, or a provider-context projection refusal |
| blocked by prerequisites | 11 | the package needed a language server, extra runtime, VCS checkout, or an installed Pi that the test machine did not have |
| not an extension factory | 4 | the package ships resources only (themes, skills or prompts) |

The matrix is a bounded sample, not a parity claim. It was recorded before the
adapter shim and crash repairs that landed later in the candidate, so some rows
may already be repaired; the matrix has not been re-run. Recorded failures
include a refused completion registration, an `extension stdout closed` adapter
exit in `pi-mcp-adapter`, a PTY footer timeout in `pi-powerline-footer`, and
`session_append_process_retired` during provider-context preparation in
`pi-web-access`.
