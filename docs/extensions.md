# Executable extensions

Write new extensions against **API `0.3`**. An extension is a trusted local
process in any language that can implement the canonical JSON-RPC contract.
The [generated API reference](extensions/API-0.3-REFERENCE.md) is the exact
contract for fields, bounds, errors, methods, and availability.

A manifest can declare options without executing extension code. This is a
**manifest fragment, not a runnable extension**:

```toml
api_version = "0.3"

[contributes]
flags = [
  { name = "index-enabled", type = "boolean", default = true },
  { name = "index-root", type = "string", default = "." },
  { name = "index-limit", type = "integer", default = 500 },
]
```

The host resolves these options before startup and supplies their values in
[`initialize.params.flag_values`](extensions/API-0.3-REFERENCE.md#initializerequest).
See [CLI flags](#api-03-cli-flags) for validation and parsing rules.

No complete runnable API `0.3` example is qualified here. The
[Python `Extension` runtime](../sdk/python/README.md)
is an API `0.1`/`0.2` adapter; generated `octet_extension.api_v03` types do not
implement a complete API `0.3` process runtime. The
[TypeScript example](../sdk/typescript/README.md) demonstrates contract negotiation,
not a running extension. The existing [extension examples](../examples/README.md#legacy-extension-examples)
are legacy references, not API `0.3` quickstarts. Do not retag their manifests.

## Kernel boundary

The host owns model conversations, sessions and tool-result persistence,
permissions and approvals, process supervision and cleanup, and resource limits.
Extensions own domain behavior such as MCP, search, browser use, LSP, or subagent
orchestration. A generic host service does not imply a shipping capability
package or make its domain part of the kernel.

Executable extensions run with the current user's operating-system authority.
Capability declarations are consent metadata, **not a sandbox**. Message, queue,
request, concurrency, artifact, shutdown, and process-tree bounds do not provide
OS CPU/RSS/FD/PID isolation. Use separate OS-level isolation for full-access work.

**Executable extensions are disabled by default, including installed bundles.**
Discovery never executes code. **Full access (`unsafe_host`, the default) trusts
selected extensions implicitly, but never enables them.** An explicitly enabled
extension can start without an extra trust flag, subject to process-policy gates
and the existing source, compatibility, and integrity checks. This implicit
trust is calculated for the current policy; it never writes a persistent trust
grant or an invocation trust flag back into configuration.

`--safe-mode` removes implicit trust and never starts executable extensions,
even with explicit trust and process/shell flags enabled. Executable processes
still require `unsafe_host`: safe mode is not an extension sandbox, and an
approval cannot bypass that floor. Native-host protocol `1` is a
[separate embedding interface](sdk.md); it discovers extensions but does not
start them.

## Layout and discovery

Each direct child directory contains `extension.toml`, and its directory name
must exactly match the manifest `name`:

```text
.octet/extensions/workspace-index/extension.toml
~/.octet/extensions/workspace-index/extension.toml
```

Precedence is global, then trusted project, then explicit `--extension-dir`
directories in command-line order; later definitions win by directory name.
Project resources are ignored until the workspace is trusted. Enablement and
executable trust remain separate. Full-access trust applies to the selected,
validated source, without changing discovery precedence or enabling other
installed extensions. For example, with a reviewed bundle installed:

```sh
octet --enable-extension octet-web-search
```

`--enable-extension NAME` enables a selected extension for that invocation;
`enabled_extensions = ["octet-web-search"]` persists activation in user config.
`--trust-extension NAME` remains an optional explicit invocation-only trust grant,
not activation or permission to bypass safe mode. A bare persistent
`trusted_extensions` name applies only under `~/.octet/extensions`; an exact
`NAME@/absolute/path/extension.toml` grant is required to persist trust for project
or explicit sources. These source-bound grants remain distinct when implicit
full-access trust is absent. Even an explicitly trusted extension stays stopped
under a controlled policy.

A trusted project config can suggest `enabled_extensions` but cannot create a
persistent trust grant. Persistent trust comes from user config or
`OCTET_TRUSTED_EXTENSIONS`, never from that project suggestion.

See the retained [discovery and trust reference](extensions/legacy-authoring.md#layout-and-discovery)
for config examples, bounded manifest reads, diagnostics, and resolver APIs.
`/extensions status` shows discovered, enabled, trusted, and running state;
`/extensions` manages activation without granting trust.

## Manifest

Select `api_version = "0.3"` exactly; an extension's own `version` does not select
the wire. octet 0.7.2 source uses `octet_version`, `requires_octet`, `OCTET_*`, and
`octet_extension`, with no aliases for earlier first-party wire names or imports.
The first-party source distribution version is `0.7.2`, independent of API
`0.3`.

Declare the entrypoint and tools for the implementation you actually supply.
Return the complete tool catalog and a valid selected contract from
[`initialize`](extensions/API-0.3-REFERENCE.md#initializeresponse). Do not copy
legacy command, context, presentation, or broker declarations into a new API
`0.3` implementation and assume the old methods exist. Existing implementations
have a separate [legacy manifest reference](extensions/legacy-authoring.md#manifest),
not a current-API template.

`requires_octet` is optional for an unpackaged local extension, enforced when
present, and mandatory as an exact running-version requirement for an installed
bundle. Installation never enables, records a trust grant, starts, or runs setup
code. Full-access trust is a runtime policy, not an installer side effect. See
[bundle validation and commands](extensions/legacy-authoring.md#installable-extension-bundles).
Published-catalog examples remain publication-gated; use a reviewed source or
local archive when matching publication has not been verified.

### API `0.3` CLI flags

Each `[contributes].flags` declaration needs a globally visible `name`, an exact
`type`, and a typed `default`. Types are exactly `boolean`, `string`, and
`integer`; an optional `description` is short plain text.

- Boolean flags accept `--index-enabled` and `--no-index-enabled`.
- Strings and integers take one value: `--index-root src`, `--index-limit 1000`.
- Integers are signed portable JSON integers; defaults and supplied strings are
  bounded. Names are at most 64 bytes: lowercase letter first, then lowercase
  letters, digits, or hyphens. Each extension may declare at most 64 flags.
- Unknown fields, duplicates, invalid identifiers, unsupported types, wrong
  default types, and oversized values fail manifest validation.
- Only validated metadata is read; the host never starts or imports an extension
  to discover options. Flags register only for selected, enabled, trusted
  extensions. Collisions with built-in options, another selected extension, or
  a generated boolean inverse reject the invocation before startup.
- Flags appear in `octet --help` for ordinary no-subcommand startup.
  Authentication, package/migration/Pi, and other early-exit paths retain their
  static parsing.
- All values, including omitted defaults, are sent as a sorted `{name, value}`
  list in `initialize.params.flag_values`.

Legacy manifests cannot declare these flags and keep their exact initialization
wires. Pi compatibility links cannot turn runtime `registerFlag` calls into this
pre-start manifest surface; see [Pi migration](pi-migration.md).

## Transport contract

Use UTF-8 canonical JSON followed by exactly one LF, with stdout reserved for
protocol and bounded diagnostics on stderr. API `0.3` framing is stricter than
ordinary JSON-lines: duplicate keys, unknown envelope fields, noncanonical
whitespace/escapes, malformed surrogates, nonportable numbers, and excessive
depth are rejected before dispatch. The frame limit excludes LF. A frame at the
bound is accepted; one byte over terminates the stream. Negotiated limits replace
the offered limits atomically for both directions after initialization.

Implement against these exact reference sections rather than adapting a legacy
JSON example:

- [Canonical envelopes](extensions/API-0.3-REFERENCE.md#canonical-framing-and-json-rpc-envelopes)
  and [all bounds](extensions/API-0.3-REFERENCE.md#bounds).
- [Host offer](extensions/API-0.3-REFERENCE.md#contractoffer),
  [selection](extensions/API-0.3-REFERENCE.md#contractselection), and
  [initialization fields](extensions/API-0.3-REFERENCE.md#initializerequest).
- [Capabilities](extensions/API-0.3-REFERENCE.md#capabilities),
  [method directions and terminal semantics](extensions/API-0.3-REFERENCE.md#methods-and-terminal-semantics),
  and [exact error code/message pairs](extensions/API-0.3-REFERENCE.md#errors).
- [Tool arguments](extensions/API-0.3-REFERENCE.md#toolcallparams),
  [results](extensions/API-0.3-REFERENCE.md#toolcallresult),
  [cancellation](extensions/API-0.3-REFERENCE.md#cancelrequestparams), and
  [shutdown](extensions/API-0.3-REFERENCE.md#shutdownparams).

Only foundation methods/capabilities may be negotiated, and optional services
may be omitted when the host cannot safely bind them. `dynamic_tools`,
`tools/register`, `tools/unregister`, and `context/collect` are deferred.
`ContentPart` has foundation text; image and audio variants are deferred.
Legacy artifact, progress, command, presentation, secret, and child-session
methods are not implicit API `0.3` capabilities.

The schema also defines optional migration, provider catalog/auth/stream, and
session-lifecycle services. A schema declaration is not proof that a particular
product host offers the service. Provider acceptance ambiguity never permits
automatic replay; authorization uses opaque host-policy leases, not credentials
or URLs in protocol fields. See the exact generated models and host offer.

## Lifecycle and reload

The host remains responsible for cleanup and persistence. Cancellation is
cooperative, not rollback, and ambiguous unsafe work is not replayed. Preserve
terminal disposition and generation ownership rather than inferring success
from a lost connection. The retained [host lifecycle reference](extensions/legacy-authoring.md#lifecycle-and-reload)
covers candidate-first reload, bounded drain, shutdown, and supervised restart;
its legacy observational methods are not API `0.3` methods.

### Declared API `0.3` session hooks

The optional cleanup hooks are an all-or-nothing pair:

```toml
api_version = "0.3"

[contributes]
hooks = ["session_start", "session_end"]
```

API `0.3` rejects a partial pair or legacy request-path hooks alongside it.
Initialization must select both optional `lifecycle_events` and `hook/run`, or
neither; a declaration/selection mismatch is rejected. These hooks are distinct
from the optional `session_lifecycle` service.

One `hook/run` with `hook: "session_start"` is delivered when the coding product
activates its durable session owner, and one with `hook: "session_end"` when that
binding settles. [`SessionBinding`](extensions/API-0.3-REFERENCE.md#sessionbinding)
contains only an opaque SHA-256-derived owner key, a host-created extension
instance fence, and process generation. `SessionEnd` adds a generated outcome,
`shutdown`, `reload`, `crash`, or `cancelled` reason, and duration in milliseconds.
It exposes no session path, mutable `Session`, prompt, host-state snapshot, or
request-path context.

Ownership is recorded before start to prevent duplicate pairs. Each dispatch is
capped at 250 ms; malformed results, timeouts, and remote errors become bounded
diagnostics. Valid `deny` or `defer` dispositions are recorded but cannot veto
host lifecycle ownership. Settlement is idempotent and precedes child shutdown.
On accepted reload, the old generation receives `interrupted`/`reload` end before
the replacement's start. After a detected crash, the replacement receives the
old `interrupted`/`crash` end with the old binding generation before its own start.
These hooks do not enter the prompt/tool hot path and are not legacy
`session/started` or `session/settled` observations.

The separate optional `session_lifecycle` capability is offered only when the
interactive product configures a safely bound active-session driver.
`session/create` and `session/fork` return durable IDs without switching;
`session/switch` accepts an existing workspace session ID; `session/reload`
rereads the active durable session at an idle boundary. See
[the generated request models](extensions/API-0.3-REFERENCE.md#sessioncreateparams).

## Retained reference topics

The following anchors preserve links from the former combined guide. The linked
maintenance references retain API `0.1`/`0.2` details without teaching them as
current authoring interfaces.

- <a id="runtime-lifecycle-and-sharing"></a>[Runtime lifecycle and sharing](extensions/legacy-authoring.md#runtime-lifecycle-and-sharing)
- <a id="live-tool-catalogs"></a>[Legacy live tool catalogs](extensions/legacy-authoring.md#live-tool-catalogs)
- <a id="semantic-presentation-snapshots"></a>[Legacy semantic presentation](extensions/legacy-authoring.md#semantic-presentation-snapshots)
- <a id="child-model-session-service"></a>[Legacy child model-session service](extensions/legacy-authoring.md#child-model-session-service)
- <a id="optional-kernel-services"></a>[Legacy brokers, correlation, input, and approvals](extensions/legacy-authoring.md#optional-kernel-services)
- <a id="python-sdk"></a>[Python SDK status](../sdk/python/README.md) and [legacy runtime](../sdk/python/legacy-runtime.md)
- <a id="capability-boundaries"></a>[Capability ownership boundaries](extensions/legacy-authoring.md#capability-boundaries)
- <a id="installable-extension-bundles"></a>[Bundle installation, update, removal, and validation](extensions/legacy-authoring.md#installable-extension-bundles)
- <a id="first-party-application-packages"></a>[Separate Serve application packages](extensions/legacy-authoring.md#first-party-application-packages)

See also the [legacy wire reference](extensions/PROTOCOL-REFERENCE.md) and
[project tracking](https://github.com/orgs/skaft-software/projects/5).
