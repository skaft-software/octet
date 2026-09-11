# `octet-pi-compat`

Run a bounded subset of reviewed Pi extension source through Pi's public loader.
octet still owns the model loop, JSON-RPC transport, trust gates, persistence,
and process cleanup. See [Pi migration](../../docs/pi-migration.md) to inspect or
import a setup first; a generated link is not proof of compatibility.

## Create a local link

With [octet 0.7.5 installed](../../docs/installation.md) and a separately reviewed
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

The bridge distribution remains `0.7.0`, independently of octet `0.7.5`;
Pi `0.84.4` and the live API version are also independent contracts. It requires exactly
`@earendil-works/pi-coding-agent@0.84.4` and Node 22.19 or newer, validated before
importing extension code. It never silently adopts a newer Pi runtime from
`PATH`. `octet pi plan --pi-package DIR` records a canonical nonstandard package
location without relying on ambient extension-environment inheritance.

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
  preparation, transformed result details/error/usage, and live tool catalogs;
- initial Pi commands as native octet slash commands when `runtime_commands` is
  negotiated, with the generated multiplexed route as a compatibility fallback;
- notifications, confirmations, text input, and a plain-text compatibility theme;
- basic lifecycle events, prompt/context contributions, and a local Pi event bus;
- host session-name and reasoning snapshots where supplied by octet.

Unknown Pi APIs fail closed. Session/tree mutation, compaction control, root-agent
messaging, active-tool policy mutation, arbitrary components/editors/widgets, and
terminal input are not silently emulated. The [per-surface ledger](COMPATIBILITY.md#public-extension-surface)
is authoritative; an example that loads is not evidence of behavioral parity.

## API 0.3 provider mode

`octet pi install SOURCE --api-version 0.3` explicitly selects the constrained
provider bridge. API `0.2` remains the default; existing links are never upgraded
in place. The `0.3` link contributes only host-owned `providers` and one fixed
aggregate Pi-tool dispatcher. It omits legacy commands, UI, context,
notifications, confirmation, process, and network contributions.

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
load plus `/todos` smoke, not plan-mode behavioral parity. Flags, shortcuts,
active-tool overlays, session entries, root messages, editor/widget transport,
and durable custom entries remain explicit blockers. Fake-Pi provider cases cover
catalog, authorization, hooks, streaming, cancellation, mutation, and cleanup only.

The separate [unchanged-source full gate](COMPATIBILITY.md#integrity-verified-unchanged-source-full-gate)
uses `conformance.py --full --network-isolated` with local coding-agent/TUI tarballs,
`--pi-package`, and `--source-root`. It verifies npm SRI and both resolved package
roots, uses fresh `HOME` and an allowlisted environment with Linux `unshare --net`,
and loads all 78 unchanged sources through Pi's public loader. It performs no
download and fails rather than accepting fake fixtures, a package directory
alone, or a smoke test as real-runtime proof.
