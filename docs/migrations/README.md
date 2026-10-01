# External product migration — staged, not cut over

The seven extension projects and four application projects from
[PR #480](https://github.com/skaft-software/octet/pull/480) have public source
snapshots under `skaft-software`. Octet remains the source of truth while the
integration gates below are open. **This is not a completed migration.** No
source directories, builtins, examples, SDKs, core crates, existing releases,
or install/update routes have been removed or replaced.

## Pinned destinations

[external-products.json](external-products.json) records the original commit,
source directory, destination repository, and verified published commit.
Each destination's `MIGRATION-SOURCE.json` records original files, SHA-256 hashes,
and executable bits; `MIGRATION.md` identifies remaining qualification gaps.
These are new snapshot histories, not filtered copies of the original history.
The original Git history remains in octet.

| Source | Public repository |
| --- | --- |
| `extensions/octet-browse` | [octet-browse](https://github.com/skaft-software/octet-browse) |
| `extensions/octet-computer-use` | [octet-computer-use](https://github.com/skaft-software/octet-computer-use) |
| `extensions/octet-mcp` | [octet-mcp](https://github.com/skaft-software/octet-mcp) |
| `extensions/octet-subagents` | [octet-subagents](https://github.com/skaft-software/octet-subagents) |
| `extensions/octet-web-search` | [octet-web-search](https://github.com/skaft-software/octet-web-search) |
| `extensions/octet-snap-compact` | [octet-snap-compact](https://github.com/skaft-software/octet-snap-compact) |
| `extensions/octet-serve` | [octet-serve](https://github.com/skaft-software/octet-serve) |
| `apps/web` | [octet-web](https://github.com/skaft-software/octet-web) |
| `apps/apple-shared` | [octet-apple-shared](https://github.com/skaft-software/octet-apple-shared) |
| `apps/macos` | [octet-macos](https://github.com/skaft-software/octet-macos) |
| `apps/ios` | [octet-ios](https://github.com/skaft-software/octet-ios) |

For local copies checked out at the pinned destination commits, verify the full
source inventory without fetching or executing product code:

```sh
python3 scripts/verify-product-extraction.py --products-root /path/to/product-checkouts
```

This checks committed bytes, SHA-256 records, inventory completeness and modes,
not working-tree modifications or standalone functionality. The source checkout
must contain the pinned original commit. Destination checkouts must be at the
pinned published revision, not a moving branch tip.

## Observed checks

- All 11 destination commits were checked through the GitHub API after pushing.
- All 615 original source files were checked byte-for-byte, including modes.
- Five official executable bundles packaged from external checkouts twice with
  a fixed `SOURCE_DATE_EPOCH`; each pair was identical. These local staging
  bundles include migration metadata and were **not** published or installed.
- Standalone Web Search: 54 tests passed.
- Browse: 122 tests, one failure and four skips; the failing synchronization
  assertion reads the former monorepo's SDK source directory.
- MCP: 102 tests, one error; its release test reads the former parent catalog.
- Subagents: 110 tests, one failure and one error; SDK and parent-catalog references.
- Computer Use: 341 tests, one failure and seven skips. The failing menu-status
  test assumes no Cua Driver is installed. The same test fails in the original
  source checkout on this machine, where a driver is installed.
- No standalone Rust, web, Swift, native-signing, registry-publishing or full
  workspace qualification is claimed by this staging change.

## Required cutover gates

1. Port each project's CI and deterministic release packaging. Preserve upstream
   license notices and replace old relative documentation links. SDK registry
   publication is not assumed: the SDK remains source-distributed in octet.
2. Replace extension test dependencies on the sibling SDK/catalog with pinned
   upstream contract checks. Keep host-side extension conformance in octet.
3. Replace the compiled catalog's monorepo source path with a core-owned catalog
   mapping official IDs to pinned repositories/releases. Keep SHA-256 validation,
   inert install, explicit activation and source-bound authority checks unchanged.
   Existing installed records and older canonical release URLs must still work.
4. Preserve exact `requires_octet` checks. Neither extraction nor separate Git
   histories authorizes a compatibility-range change. Publish matching artifacts
   and prove install, update, list and removal before switching download origins.
5. Move Serve's optional Cargo dependency to a pinned external source without
   creating two incompatible `octet-agent` type identities. Its adapter into the
   private coding-agent `App` stays core-owned. Pass default and all-feature builds.
6. Move the browser-sign-in font to core-owned assets; the CLI currently embeds
   it from `apps/web`. Replace the web bundle sync's monorepo path with an explicit,
   bounded destination and validate coordinated client/backend bundles.
7. Pin the Apple shared package in macOS SwiftPM and iOS XcodeGen definitions.
   Preserve the existing source-only limitations: macOS is documented as missing
   shared client types; iOS has no qualified live-host pairing path. Do not claim
   those pre-existing gaps are fixed by moving files.
8. Update the documentation inventory and release/Windows/extension/web lanes;
   run packaged-docs checks and preserve release provenance. Only then delete
   product sources from this repository, leaving examples and builtins intact.

Keep this migration draft until these gates have evidence. No new repository has
been declared the authoritative release source yet.
