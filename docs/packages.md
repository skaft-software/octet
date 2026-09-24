# Extension packages

[Documentation](README.md) · [Extensions](extensions.md) · [Serve](experimental/octet-serve/README.md)

octet installs four executable bundles—Browse, MCP, subagents, and web search—
and the separate Serve application through `octet extension`. See
[executable bundle setup](installation.md#optional-packages) for the tool
integrations. The application-package format on this page is specifically for
`octet-serve`; Serve is not an executable-extension activation target.
The local 0.8.0 RC does not imply matching published packages.

## Commands

```sh
octet extension list
octet extension install octet-serve
octet extension update  octet-serve
octet extension remove  octet-serve

# Install or update from a local release archive instead of downloading
octet extension install --path ./octet-serve-<version>-<target>.tar.gz
octet extension update  --path ./octet-serve-<version>-<target>.tar.gz
```

- `install`/`update` by name download
  `octet-serve-<version>-<target>.tar.gz` from the matching GitHub release and
  verify it against that release's `SHA256SUMS` before linking it into place.
- A local `--path` archive is hashed and installed directly; it must still carry
  a valid manifest for this host target.
- Installs, updates, and removals take an exclusive package lock, so a second
  concurrent operation fails rather than racing.
- The replacement is atomic: a failed install or checksum mismatch leaves the
  previous package in place.
- `remove` deletes the installed package but never Serve sessions or other user
  data.

Installing a package makes the runtime *available*; it does not create a trust
grant. Enablement and trust stay separate ([resource discovery](resources.md),
[extensions](extensions.md)).

## Package archive layout

An archive declares a closed manifest (`deny_unknown_fields`):

```toml
schema_version = 1
id = "octet-serve"
version = "0.7.6"
requires_octet = "=0.7.6"
target = "aarch64-apple-darwin"

[entrypoint]
path = "bin/octet-serve-runtime"
args = ["serve"]
sha256 = "<hex sha256 of the entrypoint binary>"

[capabilities]
network = "loopback"
process = true
filesystem = "workspace"
```

Validation rules (`crates/octet-coding-agent/src/extension_package.rs:901`):

- `schema_version` must be `1`.
- `target` must equal this binary's target triple.
- `id` must match the requested package name.
- the entrypoint `sha256` is verified before the package is linked.
- `capabilities` must declare `network = "loopback"`, `process = true`, and
  `filesystem = "workspace"`; anything else is rejected.

## Source extensions vs. executable bundles

Import adapters such as
[`octet-import-aider`](../extensions/octet-import-aider/README.md) and
[`octet-import-pi`](../extensions/octet-import-pi/README.md) are **source
packages**: a manifest plus a script or module, discovered through the normal
resource roots and enabled/trusted explicitly. They are not installed by
`octet extension` and not distributed as release archives.
[Extension authoring](extensions.md) owns the manifest and API contract; the
[Extension API 0.4 and retained wire reference](extensions/API-0.4-REFERENCE.md) owns the
protocol.

## Pi packages

Pi npm packages are not octet extension packages and cannot run unchanged here.
[Pi inventory and portable import](pi-migration.md) are separate read/convert
workflows, not a compatibility host or dependency installer.
