# Migrating from Pi

Inspect your Pi setup before importing anything:

```console
octet migrate pi --dry-run
```

This scanner reads local files without running package code, starting a model,
or changing either setup. It always runs dry, even without `--dry-run`; it is
not an apply command or a compatibility promise. The commands here describe the
octet `0.7.0` source build, not a public installation channel.

## Current command

The scanner reads `~/.pi/agent/settings.json` and the selected project's
`.pi/settings.json`. `PI_CODING_AGENT_DIR` or `--pi-home` selects another user
directory; `--project` selects another project:

```console
octet migrate pi --dry-run --project /path/to/project
octet migrate pi --dry-run --json > pi-migration.json
```

It exits before normal octet configuration, provider discovery, session or
extension startup, and model bootstrap, so it uses no model tokens. See the
[report classifications](#classification) before treating anything as compatible.

## Import portable setup data

Import is a separate, opt-in command. Preview it first:

```console
octet migrate import pi --dry-run
octet migrate import pi --source /path/to/pi/agent --dry-run --json
octet migrate import pi --source /path/to/pi/agent
# Explicitly accept destination conflicts:
octet migrate import pi --source /path/to/pi/agent --yes
```

Without `--source`, import checks `PI_CODING_AGENT_DIR`, then the standard Pi
agent locations. It uses octet's built-in read-only API `0.3` adapter, not Pi
package code or a user-selected adapter command. The host owns destinations and
writes.

| Portable data | Import behavior |
| --- | --- |
| Model selection | Selects a Pi provider/API-model pair only if it has exactly one match in octet's built-in catalog, then persists the canonical catalog ID. Custom, unknown, and ambiguous pairs are skipped, not guessed. |
| Skills | Copies to `~/.octet/skills/` with host-authored `disable-model-invocation: true` frontmatter. Review before explicitly enabling. |
| Local stdio MCP declarations | Adds to `~/.octet/mcp.json` with `enabled: false` and `required: false`. Review before explicitly enabling. |

Credentials, MCP environment values, headers, working directories, and Pi
permission decisions are **never copied**. Unsupported models/transports are
reported as skipped; model skips have bounded text details and JSON
`model_diagnostics`. Import never writes the Pi setup, uses a network service,
starts an imported MCP server or extension, or invokes a model.

Owned hashes are tracked in `~/.octet/migrations/pi-state.json`. A changed
destination is a conflict requiring interactive confirmation or `--yes`.
`--dry-run` validates the same inputs without writing. Before changing a
destination, import creates a private backup under `~/.octet/backups/migrate/`
and prints its path.

### Restore an import

Restore normally requires the current destination to still match the import:

```console
octet migrate restore ~/.octet/backups/migrate/IMPORT-DIRECTORY
# Explicitly overwrite a destination changed after import:
octet migrate restore ~/.octet/backups/migrate/IMPORT-DIRECTORY --yes
```

Review the backup and later local edits before using the overwrite option. This
restore is separate from the generated-extension rollback below.

## Plan, preflight, and publish a compatible extension

After reviewing local Pi sources and a separately installed Pi runtime, create
an inert aggregate plan:

```console
octet pi plan ./first.ts --with ./second-package --with ./third.ts \
  --name pi-compat-0-84-4 --pi-package /reviewed/pi-coding-agent \
  --output /private/review/pi-aggregate-plan.json
octet pi preflight --plan /private/review/pi-aggregate-plan.json
octet pi publish --plan /private/review/pi-aggregate-plan.json
octet pi list
```

`publish` writes a **local** generated wrapper under `~/.octet/extensions/`, not
a public release. `octet pi install ...` combines compile, preflight, and publish.
Neither path downloads/installs dependencies, imports package code, runs lifecycle
scripts, copies the Pi runtime, nor enables/trusts the link.

- `--with` is ordered. All sources share one persistent Pi process, real
  `ExtensionRunner`, event bus, `globalThis`, and registry set.
- Compilation requires exactly `@earendil-works/pi-coding-agent@0.84.4`, selected
  by `--pi-package` or bounded local discovery. Prefer an explicit path in
  automation. The bridge distribution `0.7.0` targets Pi `0.84.4` and Node 22.19+.
- `--output` requires an existing non-symlink parent and a new file; it never
  replaces a plan. Without it, stdout is canonical JSON and the inertness note
  goes to stderr, so stdout can be redirected.
- Plans pin ordered canonical source paths and bounded SHA-256 fingerprints,
  supported adjacent dependency locks (`package-lock.json`, npm shrinkwrap,
  pnpm, Yarn, or Bun lock files), the canonical Pi runtime, and its exact
  `package.json` bytes plus reviewed `dist/` tree. They also pin bridge/Pi/octet
  versions, `pi_aggregate` lifecycle profile, and explicit-enable/explicit-trust.
- Preflight rereads every pin without imports. Publish repeats it before writing
  and rolls back a partial package on write failure. Changed source, lock,
  package, plan digest, or runtime requires a replacement plan.

[Schema-v3 link identity and schema-v2 aggregate locks](../extensions/octet-pi-compat/COMPATIBILITY.md#aggregate-publication-and-api-03-evidence-seam)
bind source order, integrity, manifest path, and trust requirements. The bridge
checks source/runtime integrity before and after loading and fails closed on
changes. Review the generated name, then separately enable and trust it:

```console
octet --enable-extension pi-extension-name --trust-extension pi-extension-name
```

`octet pi list` reports freshness, not enablement or trust. To remove a link from
discovery reversibly:

```console
octet pi rollback pi-extension-name
```

Rollback moves only a validated generated package to a private rollback directory
beside the extension root. It leaves reviewed sources and enable/trust policy
intact. Review its records before manually restoring it.

The live bridge defaults to API `0.2`. Explicit `--api-version 0.3` selects the
[constrained provider mode](../extensions/octet-pi-compat/README.md#api-03-provider-mode),
not an in-place upgrade or lifecycle/dynamic-command support. Its provider
coverage is fake-Pi fixture evidence, not real-runtime parity. The static
`pi-runtime-evidence.json` sidecar uses the generated API `0.3` canonical JSON
helper; it does not add runtime-manager behavior.

## Scanner reference

The scanner resolves configured local, managed npm, and managed git packages
without installing missing ones. It applies Pi manifests, conventional resource
directories, package filters, and top-level resource overrides; records installed
names/versions; and hashes bounded source/configuration separately from locks.
It parses JavaScript, TypeScript, and TSX with tree-sitter, follows bounded
relative imports inside each package, and inventories Pi events, registrations,
UI calls, mutations, and runtime imports. Filesystem, process, network, secret,
native-module, and dynamic-import signals are conservative inventory, not proof
of runtime effects. Malformed, missing, oversized, linked, or unsupported input
becomes a diagnostic; one bad package does not stop the rest.

### Classification

| Path | Meaning |
| --- | --- |
| `direct` | Pi skills or Markdown prompts have a deterministic octet resource path; the scanner does not copy them. |
| `replace` | Reserved for an exact package/version/source-hash native replacement recipe. No replacement recipes ship. |
| `bridge` | Uses only surfaces implemented by the pinned process. Still needs a successful generated-link runtime handshake; this is a candidate, not runtime availability or exact fidelity. |
| `native_port` | Uses a known Pi `0.84.4` mutation/registration requiring a native port or a future bounded host primitive. |
| `manual` | Arbitrary Pi TUI/editor components, custom providers, or deep session/compaction internals need redesign. Pi JSON themes also need manual conversion to octet's different semantic schema. |
| `blocked` | Could not resolve/read/parse completely, or uses names outside the pinned Pi `0.84.4` public profile. |

Unsupported calls are never silently classified as no-ops.

### Machine-readable report

`--json` emits schema version `1`, with explicit safety fields:

```json
{
  "schema_version": 1,
  "source": "pi",
  "mode": "dry_run",
  "model_usage": "disabled",
  "package_code_executed": false
}
```

Package entries include configured source/scope, resolved root, name/version,
source and lock hashes, discovered resources and Pi-filter enablement, extension
analyses, analyzed file/byte/node counts, unresolved internal imports, and
diagnostics. Source hashes cover the manifest, discovered resources, reachable
relative modules, and bounded source/configuration; lock hashes cover supported
npm/pnpm/Yarn lockfiles. A hash is omitted if its complete selected input cannot
be read within bounds. Any future recipe must match identity, version, source
hash, and lock hash, never package name alone.

## Safety and bounds

The scanner does not execute/import extensions, run npm lifecycle scripts,
install packages, trust/start executable extensions, send data to a model/network,
copy/rewrite/delete either setup, or read Pi authentication/model credential
stores. Settings, manifests, source, locks, resources, relative import closure,
and aggregate hashing have fixed limits. Selected files use descriptor-bound,
no-follow regular-file reads; linked package roots/resources are rejected.
`--npm-root` only adds an explicit legacy `node_modules` search root. The scanner
never executes a configured Pi `npmCommand`.

The compatibility host, once enabled and trusted, does run third-party npm code
with your OS authority. Static scanning is not a sandbox. See
[executable extensions](extensions.md#kernel-boundary) and [security](../SECURITY.md).

## Deliberate compatibility boundary

Portable import, inventory, and pinned local links are separate capabilities,
not universal Pi source or UX compatibility. The default bridge supports bounded
tools, transformed details/error/usage, live tool catalogs, dialogs,
notifications, basic lifecycle/context, and the local event bus. Initial commands
become native slash names when `runtime_commands` is negotiated;
`/<name> COMMAND ...` is the fallback. Later command registration still needs a live command
catalog protocol. `registerFlag` is diagnosed, not converted to a pre-trust API
`0.3` manifest flag by executing source during CLI construction.

Exact replacement recipes, automatic whole-setup selection, transparent custom
provider/OAuth/stream handlers, session/tree/compaction or agent-control mutation,
and arbitrary TUI/editor/header/footer/widget parity are not provided. Pi themes
and those components may need redesign. MCP, search, browser, LSP, memory, and
subagent capabilities belong in replaceable extension processes, not an arbitrary
Pi component ABI in the kernel.

The [compatibility ledger](../extensions/octet-pi-compat/COMPATIBILITY.md) retains
all 118 public surfaces, 78 examples, 33 TUI audit rows, and six plan-mode journeys;
its [machine form](../extensions/octet-pi-compat/profiles/0.84.4.ledger.json) is
canonical. `conformance.py --check --json` validates fixtures and profile integrity,
not a real Pi run. The [full gate](../extensions/octet-pi-compat/COMPATIBILITY.md#integrity-verified-unchanged-source-full-gate)
requires local integrity-verified tarballs, a clean pinned checkout, fresh
allowlisted environment, and Linux network isolation. Neither prose nor load-only
smoke proves Pi parity. The separate [provider ledger](pi-provider-compatibility.md)
records native provider route assumptions and unsupported surfaces.

## Project and earlier section links

Future migration work is tracked in the [project](https://github.com/orgs/skaft-software/projects/5),
not promised by these commands. Model-assisted porting is not an automatic
fallback; no current scanner invocation silently starts model use.

- <a id="migration-architecture"></a>[Migration architecture](#deliberate-compatibility-boundary).
- <a id="deterministic-scannercompiler"></a>[Deterministic scanner/compiler](#scanner-reference).
- <a id="compatibility-process"></a>[Compatibility process](../extensions/octet-pi-compat/COMPATIBILITY.md#aggregate-publication-and-api-03-evidence-seam).
- <a id="exact-recipes"></a>[Exact recipes](https://github.com/orgs/skaft-software/projects/5): none ship; see [classification](#classification).
- <a id="agentic-fallback"></a>[Agentic fallback](https://github.com/orgs/skaft-software/projects/5): not an implemented automatic migration path.
- <a id="product-promise"></a>[Current scope](#deliberate-compatibility-boundary) and [project](https://github.com/orgs/skaft-software/projects/5).
