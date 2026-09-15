# Pi 0.84.4 current-candidate qualification

**Issues:** #257, #258  
**Claim:** `dogfood_conformance` / source-only candidate  
**Pi target:** `@earendil-works/pi-coding-agent@0.84.4` and `@earendil-works/pi-tui@0.84.4`, MIT  
**Pi source revision:** `b79e4cc834970cca69daebffab7df1da7d1e52c4` (`v0.84.4`)  
**Node minimum:** `22.19.0`  
**Bridge identity:** `0.7.0`

This record documents source inspection and the checked-in real-runtime plans. It does not claim that an installed Pi package, an unchanged-source full run, or a live provider has passed. Static fixture presence is not real-runtime evidence.

## Implemented contract

- `bridge.mjs` validates package/runtime, source and dependency-lock fingerprints, aggregate digest, manifest/link identity, explicit enablement/trust, and Octet version before loading. It uses Pi's public `DefaultResourceLoader` with ordered `additionalExtensionPaths`, `noExtensions`, and one in-memory event bus. Partial aggregate loading is rejected before the `ExtensionRunner` is constructed (`extensions/octet-pi-compat/bridge.mjs:3192-3229`).
- The real path constructs Pi's actual `ExtensionRunner` once for the loaded aggregate (`bridge.mjs:3228-3235`). The source-owned tests/fixtures cover ordered registration, shared `globalThis`/event bus, lifecycle settlement, cancellation, restart, stale-source rejection, trust binding, and rollback safeguards.
- `real_runtime.py` is a bounded JSON-RPC peer. Its strict command carries every selected source, source-lock fingerprint, runtime integrity, aggregate digest, manifest, link identity, and Octet version (`real_runtime.py:160-201`). `_run_once` checks registration, command order, lifecycle, execution, cancellation, and shutdown; `run_real_aggregate` repeats the journey and checks restart, trust, and stale-source rejection (`real_runtime.py:219-304`, `332-456`).
- `conformance.py` now validates both real-runtime fixture files and cross-links them from the 0.84.4 profile. The ledger records `real-runtime-aggregate` as **unrun**, rather than as covered. The aggregate metadata remains explicitly `unrun_until_explicit_real_package_and_source_root_are_supplied`.

## Checked-in evidence inventory

| Evidence | Source | Status in this candidate |
| --- | --- | --- |
| Ordered aggregate plan | `tests/fixtures/conformance/real-runtime-aggregate.json` | Declared and statically validated by source; not executed |
| Concrete real-runtime source set and assertions | `tests/fixtures/conformance/real-runtime.json` | Declared and statically validated by source; not executed |
| Focused aggregate metadata test | `tests/test_conformance.py::ConformanceHarnessTests::test_real_runtime_aggregate_fixture_is_ordered_and_explicitly_unrun` | Authored; not run |
| Profile/fixture cross-link and raw-byte digest | `profiles/0.84.4.json`, `profiles/0.84.4.integrity.json` | Synchronized in source; not verified by a command here |
| Ledger gate | `profiles/0.84.4.ledger.json` (`real-runtime-aggregate`) | Explicitly `unrun` |

## Exact central-verifier commands

These are the pending commands for the central non-Rust verifier; none was run in this source-only lane:

```console
python3 -m unittest discover -s extensions/octet-pi-compat/tests -p 'test_*.py'
python3 extensions/octet-pi-compat/conformance.py --check --json
```

With the exact local artifacts available, the real gate is:

```console
python3 extensions/octet-pi-compat/conformance.py --full --network-isolated --json \
  --coding-agent-tarball /local/pi-coding-agent-0.84.4.tgz \
  --tui-tarball /local/pi-tui-0.84.4.tgz \
  --pi-package /local/unpacked/pi-coding-agent \
  --source-root /local/pi-source-at-b79e4cc834970cca69daebffab7df1da7d1e52c4
```

The full gate itself launches the runtime through Linux `unshare --net`; `--network-isolated` is not a substitute for that namespace. It must be run on Linux with `unshare` available. Record the JSON result and stderr separately; do not infer a real-runtime pass from `--check` or the focused fixture test.

## Artifact, cache, and dependency prerequisites

Before the isolated run, the package owner must provide, without downloading during the gate:

1. Coding-agent and TUI tarballs whose SRI, names, versions, and MIT licenses match the 0.84.4 profile.
2. An unpacked coding-agent package root whose resolved `@earendil-works/pi-tui` root matches the verified TUI tarball and whose runtime integrity is stable.
3. A clean Pi checkout at the exact revision above, with `packages/coding-agent/examples/extensions` present and its dependency-bearing sources/lock material already prepared in the local cache/package environment.
4. Node `22.19.0` or newer and Linux `unshare --net` capability.

The gate performs no npm install, dependency resolution, source rewrite, network fetch, or credential import. Any missing dependency or lock/cache input is an artifact-readiness failure, not permission to substitute a toy source or newer Pi checkout.

## Remaining blockers and separate gates

- The real aggregate journey has not been run because the exact package tarballs, unpacked package root, clean pinned source checkout, prepared dependency locks/cache, and Linux namespace are not supplied in this lane.
- Native Windows behavior, live/provider/OAuth behavior, install/update/remove/rollback acceptance, physical terminal behavior, endurance/soak, and release approval remain separate gates.
- No #258 closure, unchanged-source parity claim, installed-candidate claim, or live-provider claim is made from these fixtures.
