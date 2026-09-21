# `octet-pi-compat`

> **Archived Pi bridge evidence — non-shipping, not a release gate.**
> Preserved from the local pre-reduction 0.8.0 candidate. The Pi execution bridge
> and `octet pi` command family are removed; commands, tests, “current” claims,
> and release requirements below describe the historical implementation only.
> Referenced bridge source/fixtures are no longer installed or executable here.
> The JSON profiles are preserved byte-for-byte as inert evidence, not runtime
> configuration. No receipt here qualifies the reduced RC or a published release.
> Current [Pi inventory/import](../../pi-migration.md) and native providers are
> separate from Pi extension execution.

Run a bounded subset of reviewed Pi extension source through Pi's public loader.
octet still owns the model loop, JSON-RPC transport, trust gates, persistence,
and process cleanup. See [Pi migration](../../pi-migration.md) to inspect or
import a setup first; a generated link is not proof of compatibility.

## Create a local link

With [octet 0.8.0 installed](../../installation.md) and a separately reviewed
local Pi installation:

```console
octet pi plan ./extension.ts --pi-package /reviewed/pi-coding-agent \
  --output /private/review/pi-plan.json
octet pi preflight --plan /private/review/pi-plan.json
octet pi publish --plan /private/review/pi-plan.json
octet pi list
```

`publish` creates a **local** aggregate link, not a public release.
`octet pi install SOURCE` is the local one-command shorthand;
none of these commands installs npm dependencies or imports source. Plans are
inert and preflight/publish revalidate their pins. Generated links remain inert
until separately enabled and trusted. Package code then runs with your OS
authority under octet's executable-extension trust model.

Use `octet pi rollback NAME` to move only a validated generated package out of
discovery into a local rollback directory. It does not delete reviewed sources or
change enablement/trust policy. The [aggregate contract](COMPATIBILITY.md#aggregate-publication-and-api-03-evidence-seam)
details source order, fingerprints, integrity, link identity, and rollback.

## Pinned compatibility profile

The bridge distribution remains `0.7.0`, independently of octet `0.8.0`;
Pi `0.84.4` and the live API version are also independent contracts. It accepts
`@earendil-works/pi-coding-agent` `>=0.84.4 <0.86.0` and Node 22.19 or newer,
validated before importing extension code. The *conformance profile* stays pinned to
`0.84.4`: that is the revision the ledger, fixtures, and release claim are validated
against, while runtime acceptance covers newer patch/minor releases in the same
family instead of refusing them by string equality. It never silently adopts a newer Pi runtime from
`PATH`. `octet pi plan --pi-package DIR` records a canonical nonstandard package
location without relying on ambient extension-environment inheritance.

A directory may declare multiple extension entrypoints. Pi's public package
resolver expands them without importing source; every selected entrypoint must
be a regular file inside that directory's pinned file domain before any factory
loads. Escaping or excluded dependency/cache entrypoints fail closed.
Loading checks the exact resolved entrypoint set, not one extension per directory.

Source fingerprints exclude dependency, build, and cache directories. Supported
adjacent dependency locks and the reviewed runtime installation are pinned
separately. Schema-v3 links and schema-v2 aggregate locks bind the manifest path
and explicit-enable/explicit-trust requirement into link identity; the bridge
verifies that identity before loading and rechecks runtime integrity afterward.
`octet pi list` reports stale/legacy/changed metadata, never that a link is trusted.

The [profile](profiles/0.84.4.json) pins the source revision, npm integrity,
public names, and 78-example corpus. The [machine ledger](profiles/0.84.4.ledger.json)
and its [human view](COMPATIBILITY.md) record exact support and safe divergences.

## Current supported surface

The default API `0.2` bridge supports:

- Pi tools with text/image output, cancellation, bounded progress, argument
  preparation and public Pi schema validation before execution (including hook
  mutations), transformed result details/error, and live tool catalogs;
- initial Pi commands as native octet slash commands when `runtime_commands` is
  negotiated, with the generated multiplexed route as a compatibility fallback;
- notifications, confirmations, text input, and a plain-text compatibility theme;
  select/confirm/input honor Pi `signal` and `timeout` options, cancelling the
  pending host dialog without cancelling its parent operation;
- basic lifecycle events, prompt/context contributions, and a local Pi event bus;
- host session-name and reasoning snapshots where supplied by octet.

The validator is resolved from the selected Pi installation's public `pi-ai`
export, not an ambient workspace package or a vendored schema implementation.
Pi's own coercions are retained (for example number-to-string); non-coercible or
extra arguments fail before tool execution, and every published tool is covered by
an adversarial fixture that observes the absence of its execute effect. API `0.3`
also invokes Pi tool-call interception inside its fixed dispatcher.

Tool definitions: `promptSnippet`/`promptGuidelines` are projected into the
model-facing description with Pi's normalization, `executionMode: "sequential"` is
enforced by a bridge execution lane, and a `constrainedSampling` requirement fails
initialization explicitly while `strict: "prefer"` is accepted with a diagnostic.
The bridge never sends an undeclared tool-definition field.

**Tool-result usage and termination** cross the wire only under two independently
negotiated optional features, `tool_result_usage` and `tool_result_termination`.
When selected, a bridged result carries the kernel's native tool-usage token
counters and/or `terminate`; when a host did not offer the matching feature, a Pi
result field that carries it fails explicitly instead of being dropped or
relabelled as generic metadata. A non-zero Pi cost also fails explicitly, because
the kernel's typed tool usage has no monetary field yet. The shipped octet host
offers neither feature, so this profile refuses those fields rather than claiming
tool-usage accounting or batch termination parity. A busy `waitForIdle` fails
explicitly rather than pretending to wait.

Unknown Pi APIs fail closed. Session/tree mutation, compaction control, root-agent
messaging, active-tool policy mutation, arbitrary terminal components, replacement
editors, and raw terminal input are not silently emulated. The
[per-surface ledger](COMPATIBILITY.md#public-extension-surface) records the baseline
fixtures; an example that loads is not evidence of behavioral parity.

### Optional API 0.2 UI handoff

When the host explicitly offers the corresponding legacy features, the bridge
also supports these bounded operations:

- `semantic_ui`: keyed status, working/hidden-thinking metadata, plain-text
  widgets, and synchronous header/footer components projected as sanitized text.
  Header/footer surfaces must also be declared by the host's manifest snapshot.
- `editor_handoff`: host-acknowledged get/set/paste/focus operations. The synchronous
  Pi getter fails explicitly until an authoritative editor snapshot is available;
  delayed acknowledgements cannot overwrite a newer observation.
- `autocomplete`: bounded suffix suggestions, with UTF-8 cursor validation and
  explicit host registration. Providers belong to the UI owner, not the transient
  request that installed them; a cancelled request cannot poison later disposal.
- `terminal_input`: host resize observations update semantic component widths.
  Raw input hooks and component input delivery remain unsupported.

Settlement, shutdown and replacement fence queued updates, clear contributions,
abort editor waits and dispose component/provider instances. Generated links
include both helper modules before their manifest becomes discoverable. There is
no new credential, process, terminal or API `0.3` authority. The
[qualification record](../../qualification/pi-ui-current-candidate.md)
distinguishes bridge fixtures from still-unrun Rust-host and real-Pi gates.

## API 0.3 provider mode

`octet pi install SOURCE --api-version 0.3` explicitly selects the constrained
provider bridge. API `0.2` remains the default; existing links are never upgraded
in place. The `0.3` link contributes only host-owned `providers` and one fixed
aggregate Pi-tool dispatcher. It omits legacy commands, UI, context,
notifications, confirmation, process, and network contributions. The host's
optional `theme_selection` / `theme/select` offer is validated but never selected;
it does not grant the bridge theme or legacy UI authority. The typed host
`event_bus` offer is likewise validated but not selected: Pi's process-local,
untyped event bus does not authorize cross-process topic declarations.

Bounded secret-free `registerProvider`/`unregisterProvider` declarations synchronize
to octet's `0.3` catalog. After the bounded initial collection window closes and
every serialized registration response arrives, the bridge emits
`providers/complete`, even for an empty catalog. octet projects declarations only
after completion. An older `0.3` extension without that negotiated signal reaches
a bounded timeout with incomplete declarations withheld, not partially published.

octet retains credentials, authorization, and leases. The `octetStream` adapter
receives canonical semantic request JSON, secret-free catalog metadata, and a
generic cancellation signal—not endpoint/base-URL, header, API-key, transport,
callback, or OAuth authority. Safe semantic `before_provider_request` transforms
and reduced `after_provider_response` status hooks are supported;
`before_provider_headers` is rejected. Provider registration and payload hooks
fail explicitly outside this opt-in mode.

Every local aggregate also carries the static `pi-runtime-evidence.json` sidecar.
That API `0.3` evidence file does not upgrade a `0.2` live protocol or enable
lifecycle, lazy activation, workspace/reload, or dynamic-command support.
**Provider coverage is deterministic fake-Pi fixture evidence, not real-runtime
provider parity.** See the [provider boundary](COMPATIBILITY.md#api-03-provider-bridge)
and [release requirements](COMPATIBILITY.md#release-policy).

## Tests

These maintainer commands check fixtures or diagnose a selected local runtime:

```sh
# Hermetic bridge, public-surface, and ledger fixtures.
python3 -m unittest discover -s extensions/octet-pi-compat/tests -p 'test_*.py'
node --test extensions/octet-pi-compat/tests/test_semantic_ui.mjs \
  extensions/octet-pi-compat/tests/test_editor_handoff.mjs
python3 extensions/octet-pi-compat/conformance.py --check --json

# Developer diagnosis only; neither command verifies npm tarball integrity.
OCTET_PI_REAL_PACKAGE=/path/to/@earendil-works/pi-coding-agent \
  python3 -m unittest discover -s extensions/octet-pi-compat/tests \
  -p 'test_bridge_protocol.py'

OCTET_PI_REAL_PACKAGE=/path/to/@earendil-works/pi-coding-agent \
  cargo test -p octet-coding-agent \
  pi::tests::generated_link_runs_the_pinned_real_pi_hello_example_when_selected --lib
```

The real-Pi suite covers the official hello example and an unchanged `plan-mode`
load plus `/todos` smoke, not plan-mode behavioral parity. Flags, full editor/widget
parity, widget transport, session/tree mutation, compaction control, and
model/thinking selection remain explicit blockers. The composer surface, runtime
shortcut registration with `shortcut/trigger` dispatch, session naming, user-message
injection, and the `lifecycle_events_v2` notifications whose host producers exist are
available behind the negotiated Wave-1 features (`composer`, `shortcuts`,
`session_entries`, `message_injection`, `lifecycle_events_v2`, `active_tools`).
`pi.appendEntry`, `pi.setLabel`, `pi.setActiveTools`, and assistant/system
`pi.sendMessage` are bridge-forwarded but still refused by the shipped host with a
typed `unsupported_feature`: it has no durable entry/label API, no live tool policy,
and no assistant/system injection path. Fake-Pi provider cases cover catalog,
authorization, hooks, streaming, cancellation, mutation, and cleanup only.

The separate [unchanged-source full gate](COMPATIBILITY.md#integrity-verified-unchanged-source-full-gate)
uses `conformance.py --full --network-isolated` with local coding-agent/TUI tarballs,
`--pi-package`, and `--source-root`. It verifies npm SRI and both resolved package
roots, uses fresh `HOME` and an allowlisted environment with Linux `unshare --net`,
and loads all 78 unchanged sources through Pi's public loader. It performs no
download and fails rather than accepting fake fixtures, a package directory
alone, or a smoke test as real-runtime proof.

## Selected newer source reference

The separately inspected Pi revision `8a7b0c03dfb702663acafb6dc29f8acaa4ffe391`
identifies coding-agent `0.85.1`. It is **not** a validated conformance profile: the
bridge accepts it as a runtime (it is inside the supported range), but the ledger,
fixtures, and release claim remain validated against `0.84.4`, and no
integrity-verified `0.85.1` runtime campaign has been run. Updating the host requirement to octet `0.8.0` does not change that
runtime identity or remove the ledger's host-primitive and real-runtime blockers.
