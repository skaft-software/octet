# Pi compatibility

**v0.10.0 is octet's first Pi-compatible release.** Use your existing Pi
extensions, skills, prompt templates, themes and custom UI with a native Rust
agent. Enable the optional `octet-pi-compat` adapter to bring your Pi setup.

We're working toward full compatibility with existing Pi setups. The adapter
follows **Pi 1.4's public extension API**, works with managed Pi installs from
1.0.2 through 1.4.x, and needs Node 22.19+. Rust still owns the agent loop,
sessions, permissions and terminal; the adapter loads extension code, not a
second Pi agent. Ordinary octet use does not need Node.

See the [v0.10.0 release notes](releases/v0.10.0.md) for downloads and launch scope.
Pi support requires octet 0.10.0 or later; v0.9.6 does not ship the adapter.

For API coverage and known gaps, see the [API ledger](pi-extension-api.md).
For implementation details, see the [adapter README](../extensions/octet-pi-compat/README.md).
The adapter uses the jiti 2.7.0 and TypeBox 1.3.27 that Pi 1.0.2 through 1.4
ship, and pi-tui 1.0.2's components.
Compatibility concerns public extensions, not Pi's private internals or complete CLI.

<a id="use-your-pi-setup"></a>

## Use your Pi setup

With octet **0.10.0+**, Node **22.19+** and your existing Pi setup:

```bash
octet extension install octet-pi-compat
# Installation is inert. Enable only after reviewing your Pi factories:
octet --enable-extension octet-pi-compat
```

For source development, install locked adapter dependencies as described below,
then run `./target/release/octet --extension-dir ./extensions --enable-extension octet-pi-compat`.
To install a downloaded bundle explicitly, use
`octet extension install --path /absolute/path/octet-pi-compat-0.10.0.tar.gz`.

Each time octet starts with the adapter on, it reads this machine's Pi setup
(`~/.pi/agent`, or the directory in `PI_CODING_AGENT_DIR`) and mirrors it for
that session: your enabled Pi extensions, skills, prompt templates, themes,
`keybindings.json`, the global context file and the selected model. A Pi footer
extension such as `powerline-footer` draws the footer, so it matches the one in
Pi. Nothing is written to your Pi directory or to octet's saved settings, and
turning the adapter off returns octet to its own setup. Enabling is the review:
it lets the adapter run your enabled Pi extensions with your OS permissions, as
Pi does. Native provider-credential disclosure remains a separate reviewed
opt-in; enabling the stock bundle does not grant it. Native TOML appearance
remains independent. Project `.pi` resources load only in a trusted workspace. It needs
Node 22.19+ on `PATH`.

Because the setup is read at every start, a change you make in Pi shows up the
next time octet starts, with no step in between. An extension that cannot run
under octet is left out and named in `[Extension issues]` at start; the rest of
your setup still loads. One that is slow is cut off after a few seconds and
named too (see [Slow or stalled extensions](#slow-or-stalled-extensions)). The
sections below are for a frozen, reviewed snapshot instead of the live mirror.

<a id="explicit-local-setup"></a>

## Bring your Pi extensions

Requires Node **22.19.0 or newer** and an existing managed Pi installation,
**1.0.2 through 1.4.x**. The entrypoints reject older Node versions before loading adapter
dependencies or factories. Bun's existing execution path remains available but
is separately unqualified. From a source checkout, install dependencies once:

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
reason for every entry. Broken factories are reported. Registration collisions
require an explicit owner choice and fresh review; setup refuses a partial import
rather than silently discarding a factory's other commands. Installed native
manifests alone do not establish active ownership. Reviewed builtin overrides
receive exact manifest grants and require native `builtin_tool_overrides_v1`
admission. Unsupported tool-schema constraints cause an explicit skip, not
constraint removal. Supported patterns are enforced. Loading is not proof that an extension's commands, hooks or UI work.
Failing lifecycle observation callbacks are reported and isolated; permission/veto
hooks and failed host mutations still fail closed. Generated configuration records
the executable; using Bun does not establish Node or Bun compatibility qualification.

`--reviewed` accepts executing the enabled factories and their imports, including
installed Pi code on the fallback route, with your OS permissions. Setup does not
import credentials, change Pi settings, or enable the bridge. It snapshots enabled
Pi palettes as native `pi-*` themes and makes the selected palette this session's
startup preference; explicit `--theme`/`OCTET_THEME` wins. Saved settings are not
rewritten. Pi helpers and remote components receive the imported palette, and
native theme selection supplies updated palette snapshots and invalidates their
render caches. This implemented synchronization is not qualification of composed
editor/footer lifetimes across theme changes. Native TOML appearance and extension
surface ownership remain separate; enabling extensions does not disable native
customization. While enabled, the bridge also exposes current enabled Pi themes in `/theme` at startup and
`/reload`, even without mirror mode. This read-only palette discovery does not
expand executable grants or change Pi/Octet settings. Invalid or over-budget
palettes are diagnosed; disabling the extension withdraws them. The built-in
`pi` theme is Pi 1.0's default system palette and works without the bridge.
Missing packages are not installed. The Pi install location is recorded, so a different
HOME at runtime does not hide it. A Pi upgrade requires reconfiguration.

<a id="mirror-your-pi-setup"></a>

### Mirror your Pi setup

The stock bundle already uses automatic mirroring on explicit
enablement. The following generated bridge is a distinct fingerprint-bound
review mode, not an extra mandatory setup step for the stock bundle.

To have an enabled `octet-pi-compat` activate this machine's Pi setup instead of
one frozen import, configure the reviewed mirror opt-in once:

```bash
node configure.mjs --reviewed --from-pi --mirror \
  --output "$HOME/.octet/pi-import/octet-pi-compat"
# Review the listed factories and resources, then rerun the printed command,
# which binds the run to that review with --expected-fingerprint <sha256>.
octet --extension-dir "$HOME/.octet/pi-import" --enable-extension octet-pi-compat
```

Without `--expected-fingerprint`, the first command runs nothing: it prints
every factory and resource it would mirror plus the exact command to run. If a
factory cannot load on either route (for example one that needs its own
alternate-screen terminal), it is left out and the rest of the setup is mirrored;
a registration collision still needs your decision and refuses the mirror. Pass
`--strict` to refuse the whole mirror when any factory is unsupported. Excluded
factories are listed when you configure, recorded in the bridge, reported at
every start, and stay excluded until you configure again.

`--mirror` captures reviewed entrypoints, bounded literal-relative import hashes,
routes and static commands/tools/shortcuts/flags, and records the Pi agent directory.
New or changed executable sources/static registrations require fresh configuration
and review; startup never silently expands the executable grant. Resources are
rediscovered **read-only** at startup and activate their equivalents
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
provider/model and `defaultThinkingLevel`, package/resource path lists), user
`extensions|skills|prompts|themes` directories, installed managed npm package
resources, `keybindings.json` and the global context file. Project `.pi` resources
require authoritative host workspace trust; it defaults to false, independently
of factory review. Scoped/versioned npm references resolve only to an existing
matching installed identity. Supported bounded exact/glob selectors and resource
empty lists retain their selection semantics; unsupported patterns are diagnosed
rather than broadened. Explicit invocation model/thinking choices win over these
session-only defaults; unavailable defaults warn without silently lowering effort
or changing half the model/thinking pair. Deliberate boundaries: Pi `models.json`
HTTP providers and managed git packages remain unsupported, and `auth.json`,
sessions and trust state are never read. Mirror mode refuses a
factory that needs a hook outside the advertised set instead of silently
leaving it inert.

`octet extension install octet-pi-compat` installs the adapter with its locked
npm dependencies. Run its
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

## Slow or stalled extensions

Every hook the host sends runs on one ordered queue, so a single factory that
never answers (a network call, a child process, an MCP connection) must not hold
everything else. Each handler gets a time budget: 5 s for lifecycle,
notification, provider and discovery callbacks; 2 s each and 4 s in total for the
prompt hooks (`before_agent_start`, `input`), which share the host's 5 s prompt
deadline. A handler that outlives its budget keeps running but is no longer
waited for. It is named once in `[Extension issues]`, and later hooks skip it
while it is still busy instead of waiting on it again. A tool-call guard that
cannot answer in time blocks the call rather than letting it through. A prompt
sent while an extension is still starting waits up to 12 s for it, then
continues without it. A factory's own setup (the work its default export does
before it returns) gets 10 s, and 20 s across all factories; one that is still
going is left out of the mirror, named in `[Extension issues]`, and tried again
the next time octet starts.

To see which factory is slow, start octet with `OCTET_STARTUP_TRACE=1` and
redirect stderr, for example `OCTET_STARTUP_TRACE=1 octet 2>trace.txt`. Each
handler prints one line, such as
`octet-startup: pi-compat.handler elapsed=… event=session_start factory=pi-lens/dist/index.js took=99ms outcome=ok`
(`outcome` is `ok`, `failed`, `detached`, `late-settled`, `late-failed` or
`skipped-busy`).

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
Managed Pi 1.0.2 through 1.4.x is accepted.

This runs code from your installed Pi (`@earendil-works/pi-coding-agent`,
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
- A missing Pi install, or a version outside 1.0.2–1.4.x, fails loudly; nothing
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
