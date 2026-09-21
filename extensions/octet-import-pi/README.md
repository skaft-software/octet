# Pi migration adapter

A thin source package for API `0.3` `migration/detect` and `migration/import`.
The launcher replaces itself with `octet migrate adapter pi`; it does not
reimplement parsing, protocol negotiation, credential filtering or ingestion.
Requires macOS/Linux, `/bin/sh`, and **octet 0.7.6** on the launcher's `PATH`.
Use the same reviewed host installation for both the parent and the launcher.
There are no npm/Python runtime dependencies and no install scripts.

## Use

For normal import, use the host-owned command; installing this package is **not**
required and cannot override the built-in adapter:

```sh
octet migrate import pi --source /reviewed/pi/agent --dry-run
octet migrate import pi --source /reviewed/pi/agent
```

For a typed protocol client, start `./extension.sh`, negotiate the optional
`migration.adapter.v1` capability and its two methods using the
[API 0.3 contract](../../docs/extensions/API-0.4-REFERENCE.md), then send an
absolute `source_root` to `migration/detect`. Pass the returned `config_paths`
to `migration/import`. The result is bounded non-secret models, skill content,
local stdio MCP declarations and diagnostics, not destination writes.

The manifest may be discovered as a reviewed source extension using normal
[enablement and trust](../../docs/extensions.md). Discovery never executes it;
there are no model tools. This is not a new public adapter-selection flag, a
catalog publication or automatic migration registration in ordinary chat.

The executable reads only the explicitly selected source setup. Filesystem
consent is `unrestricted` because that source can be outside the workspace;
this metadata is not an OS sandbox. It does not execute Pi/package/MCP code,
read authentication stores, import environment/header credentials or permission
grants, write either setup, or use the network. The host separately owns
normalization to `MigratedSetup`, disabled outputs, conflict review, idempotence,
backups and restore. See [Pi migration](../../docs/pi-migration.md).

## Local verification

Build in the existing Cargo target, then run the package's process-level tests:

```sh
cargo build --locked -p octet-coding-agent --bin octet
OCTET_PI_IMPORT_TEST_BINARY="$PWD/target/debug/octet" \
  python3 -m unittest discover -s extensions/octet-import-pi/tests -p 'test_*.py'
cargo test --locked -p octet-coding-agent --test migration_import --test migration_host_full
```

Tests stage only the launcher, bind a reviewed local binary through a temporary
`PATH`, and use a fresh `HOME` and no ambient credentials. They check canonical
negotiation/detect/import/shutdown, rejection boundaries, source immutability
and absent destination writes. Missing local octet or non-POSIX platforms skip
with an explicit reason. These are local fixtures, not installed-release or Pi
runtime parity evidence. Results: [execution record](../../docs/swarm-audit/EXECUTION-pi.md).
