# Local octet 0.8.2 test RC

This is an unsigned, unpublished **macOS ARM64** combined test build. Its binary
reports `octet 0.8.2`, but it is not interchangeable with any published 0.8.2
artifact. Source tree: `a78080e39d6e17a68afd879cff43850f33ed15b5`.

## Run without installing or replacing your daily driver

```sh
RC=~/octet-rc/pr480-20261003-030139
"$RC/run.sh"
```

The wrapper defaults `OCTET_CACHE_WARMING=off` without persisting a setting:
cache-warming cancellation/abort regression tests currently fail. Ordinary
provider turns still use your existing account/configuration and can be billed.
Existing enabled/trusted extensions may start normally. No binary or extension
was installed/activated in your real Octet configuration during this preparation;
ordinary Cargo/npm dependency caches may have been updated.

For a fresh configuration, set `HOME` to a new private directory before running;
your normal credentials and settings will then be unavailable. To explicitly
experiment with extra **billable** warming despite the failures:

```sh
OCTET_CACHE_WARMING=streaming "$RC/run.sh"
# or: OCTET_CACHE_WARMING=idle "$RC/run.sh"
```

`bin/octet-host` is also built. `--version`, `--help`, and its protocol-1 hello
were verified with isolated HOME and no provider calls.

## What's included

- #501 base preserving #480, #495, and the combined #491/#498 UI/history work.
- #493 headless Responses prewarming, #494 inference metrics, #496 prompt-cache
  warming, #497 codemode, #499 startup work, and #500 optional Pi/remote UI.
- Recovered uncommitted Working-dot shimmer patch with regression coverage.
- Integration repairs and reviewed, current models.dev metadata projections.
- Both native binaries, finite public documentation, six local release-catalog
  extension bundles, exact-source archive and verification evidence.

The stock release command builds default features (`default=[]`). Embedded
Serve CLI support and web/desktop apps are **not** in this native binary;
standalone Serve backend tests were run separately. All-feature workspace
compilation/tests cover the feature-gated Rust code. This RC is not signed,
tagged, merged, uploaded or qualified across operating systems.

## Optional extensions

`extension-bundles/` contains local bundles for browse (deprecated), codemode,
computer-use, MCP, subagents and web-search. They are not automatically installed
or enabled. Do not fetch published bundles assuming they contain this combined
source. For example, unpack codemode into a private extension directory:

```sh
mkdir -p "$RC/local-extensions"
tar -xzf "$RC/extension-bundles/octet-codemode-0.8.2.tar.gz" -C "$RC/local-extensions"
"$RC/run.sh" --extension-dir "$RC/local-extensions" --enable-extension octet-codemode
```

Codemode needs Node >=22.19.0 and Python for its launcher; its VM and vendor
licenses are bundled. Real offline WASM/transport and deterministic bundle tests
passed earlier; the final VM suite had one short-deadline/output assertion failure,
which passed in isolation. See `VERIFICATION.md`; do not treat it as fully green.
Enabling/trusting an extension is an explicit user decision.

Pi compatibility remains an optional **source package**, not an installable
release-catalog bundle. Its dependencies are installed only inside
`source/extensions/octet-pi-compat/node_modules` for synthetic tests. Follow
`source/extensions/octet-pi-compat/README.md` and
`source/docs/pi-compatibility.md` for reviewed factory configuration. No user/third-party factory,
Doom WAD, user Pi installation or third-party extension was enabled/imported by
this preparation. Node dependencies are not part of the immutable source archive.

The archive also includes optional Pi source under `extras/octet-pi-compat`,
without npm dependencies; running its setup is a separate explicit choice. When
using the archive set `RC` above to its extracted root. The full repository source
archive is a sidecar; Git/rebuild instructions below refer to the retained
candidate worktree, not an assumed Git checkout inside the runtime archive.

## Evidence and known gaps

Read `VERIFICATION.md` for actual gate results, observed failures, inherited
reported findings, skipped live/platform/manual acceptance and preserved local
work. `evidence/*.log` and matching `*.exit` retain the command results; earlier
failed or superseded attempts are intentionally retained and clearly separated.

`bin/SHA256SUMS` verifies the two runnable binaries. The local package's
`LOCAL_RC.json` records exact source, toolchain, hashes and scope. Its custom
archive is for local testing, not the canonical installer or a signed release.
The canonical packager's clean-commit/signature requirements were not bypassed.

## Rebuild

```sh
cd "$RC/source"
cargo build --release --locked --target aarch64-apple-darwin -p octet-coding-agent --bins
```

This source is staged in an isolated detached worktree, not committed. To inspect
integration changes use `git diff --cached HEAD`; the same binary patch is saved
as `evidence/combined-candidate.patch`. The source `.tar.gz` is immutable tree
export; older tree exports beside it are superseded, not alternative qualified
builds. Do not apply this patch to the dirty daily-driver checkout.
