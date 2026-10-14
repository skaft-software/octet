# octet v0.9.0 — Claude Cloud release handoff

## Start here

The user requests that Claude in the cloud take over and finish delivering stable
v0.9.0. Publication authorization covers scoped fixes/commits, push, PR, normal
protected merge, immutable tags, workflow execution and publication for this
version. **This branch is handoff material, not release source. Do not tag its
HEAD or merge it into main as part of publication.**

1. Read this handoff, [state.json](state.json),
   [maintainer conventions](../../conventions.md), and the
   [release skill](../../skills/release/SKILL.md).
2. Reconcile the recorded GitHub run, tags and release before doing any mutation.
3. Use a separate clean worktree at the exact release source below. Read the
   native and extension release workflows in full before invoking them.
4. Finish the remaining release stages, or report the exact reviewer/CI blocker.

No local publishing continuation was launched. Local automation is stopped;
Claude Cloud is the sole coordinator for subsequent release mutations. Do not
run the original Mac-specific scratch continuation. Everything required to
build/sign/publish is tracked at the release source; private scratch logs and
uncommitted experimental Pi repairs are not needed or uploaded.

## Verified checkpoint

- Repository: `skaft-software/octet` (public).
- Phase: **MERGED**, not PUBLISHED or PUBLIC_SMOKE.
- [PR #581](https://github.com/skaft-software/octet/pull/581) merged normally using
  the only enabled method, squash. No override, force-push or rebase.
- Exact merged source: `d2425ccfa70b1da6293dac7e786816e328b22370`.
- Source tree: `a8bf13b1d479984c1ccddac385d06255551a12b7` — identical to the
  fully qualified PR tree.
- Qualified PR head: `b0266bbf616dc2ad18e6d2fd0c02f8ad277cb778`.
- [PR CI 38467780009](https://github.com/skaft-software/octet/actions/runs/38467780009):
  completed **success**, including Windows GNU and all nine app-pinned checks.
- [PR CodeQL 38467773944](https://github.com/skaft-software/octet/actions/runs/38467773944):
  completed **success**.
- [Exact-main CI 38477515288](https://github.com/skaft-software/octet/actions/runs/38477515288):
  completed **failure** during handoff preparation. Both blockers below must be
  investigated and fixed/requalified before tagging. Do not publish this source
  because its earlier PR passed.
- Remote main was still the merged source at handoff. No canonical `v0.9.0`,
  `octet-binaries-v0.9.0` or `octet-serve-v0.9.0` tag existed. Canonical release
  lookup returned HTTP 404. **Nothing has been tagged or publicly released.**

Local validation on the PR tree passed: Windows surface 6, Python scripts 94,
identity 12, packaged docs 549 files/55 references, source qualification 4,
format/diff, workspace all-target/all-feature clippy denying warnings and live
model-metadata freshness. Full predecessor coding validation was 3159 passed,
0 failed, 3 existing ignored; SDK 202 tests/1 existing skip; five shipped adapters
passed. These are retained source/predecessor receipts, **not final public
artifact qualification**. Historical default-parallel failures remain recorded;
no assertions, deadlines or skips were changed to hide failures.

## Frozen scope and safeguards

Ship exactly these five separate optional bundles:

- `octet-codemode`
- `octet-computer-use`
- `octet-mcp`
- `octet-subagents`
- `octet-web-search`

`octet-pi-compat` is **source-only experimental work**, excluded from
`extensions/release-catalog.txt` and binary/extension inventory expectations.
Do not re-enable it, ship it, publish a six-bundle artifact, or claim cold-start,
installed-Pi lifecycle/workflow or post-mutation acceptance. Built-in Pi-themed
palettes and migration utilities are separate features.

- The developer's original `octet-l18` worktree, branch
  `release-stack/0.9.0/18-pi-full-setup`, retains **14 dirty paths**. Never clean,
  reset, restore, stash, switch, rebase or sweep that worktree. Those uncommitted
  repairs stay local and do not need to ship.
- Explicit staging only; no `git add -A`, force-push, rebase or broad cleanup.
  Fetch/reconcile main before a push; never silently change the frozen source or
  reuse gates when its tree changes. Do not move existing tags or replace assets.
- Never weaken assertions, introduce skips, disable tests/checks, or relax test
  deadlines. For local Rust checks use `ulimit -n 8192`,
  `--profile ci-test --locked`, `RUST_TEST_THREADS=2`, and an explicitly built
  native-editor test host where required. Resource-concurrency control is not a
  test waiver.
- Isolate HOME for acceptance. Never inspect the real `~/.pi`, enable an
  extension, grant trust, install optional dependencies or provision a driver as
  a side effect of verification.
- Live provider/subscription/audio and attended UI/driver acceptance are **not
  run**. Windows Codex HTTP 400 issue
  [#572](https://github.com/skaft-software/octet/issues/572) remains unresolved.
  GNU CI does not qualify MSVC; automated ConPTY checks are not physical-display
  acceptance. Do not invent performance comparisons.
- npm, Homebrew, crates.io, SDK and container registries are independent. Do not
  enable npm publication or claim any independent channel without its own
  receipts. Documentation explicitly says v0.9.0 is not yet published to npm.
- Comments end `---` followed by
  `_Generated by [Claude Code](https://claude.ai/code)_`; PR bodies end
  `🤖 Generated with [Claude Code](https://claude.com/claude-code)`.
  Commits include the actual model coauthor.

## Protection changes already authorized and completed

**Do not change protection further.** The user separately authorized only:

1. Removal of retired `web` from required main checks. Strict/up-to-date remains
   enabled; all other checks and app bindings were preserved.
2. Addition of only `octet-binaries-v0.9.0` and `octet-serve-v0.9.0` to the
   `stable-release-publish` deployment-tag policies. Reviewer protection,
   deployment mode and every existing policy were preserved.

Required checks: `dependency-review`, `dependencies`, `quality`,
`first-party-extension-tests`, `test (ubuntu-24.04)`, `test (macos-15)`, `msrv`,
`smoke-install` (app 15368), and `CodeQL` (app 57789). Main also requires linear
history and conversation resolution. Squash merging is enabled; merge commits
and rebase merging are disabled. Publishing still requires its configured
reviewer. **Do not bypass or automatically approve that reviewer gate.** Report
pending-deployment links when human approval is needed.

Two CodeQL conversations blocked the otherwise-green PR. They were reviewed and
resolved with explanations, not suppressed or dismissed: `JSON.stringify`
encodes test-owned temporary paths as complete Node JavaScript string literals
in `pi-mirror-static.test.mjs:127` and `pi-setup.test.mjs:385`. Seven adversarial
strings round-tripped in both generated-code shapes without injection. Both
threads are resolved; CodeQL and its rules remain enabled, and alerts were not
dismissed. The assessment is posted in the PR review threads.

## Current release blockers — exact-main CI failed

Source `d2425ccfa70b1da6293dac7e786816e328b22370` is **not MAIN_GREEN**.
[Run 38477515288](https://github.com/skaft-software/octet/actions/runs/38477515288)
finished with two failed jobs:

1. **Quality / live metadata freshness**, job
   [115571758505](https://github.com/skaft-software/octet/actions/runs/38477515288/job/115571758505):
   `stale models.dev snapshot: crates/octet-ai/models/models-dev-pricing.json`.
   The existing 60-second freshness gate failed; it did not update source.
   Regenerate through `scripts/refresh-models-dev-pricing.py`, review names,
   pricing, capabilities and source receipt together, and preserve strict price
   goldens. Earlier local/PR freshness success does not qualify the new main run.
2. **Windows GNU / Python SDK**, job
   [115571758972](https://github.com/skaft-software/octet/actions/runs/38477515288/job/115571758972):
   `test_A07_unnegotiated_progress_is_refused` failed **during initialization**,
   before its unnegotiated-progress assertions. At
   `sdk/python/tests/test_typed_process.py:167`, `self.initialize(progress=False)`
   calls line 71 `self.receive()["result"]`; line 61
   `self.messages.get(timeout=4)` raises `_queue.Empty`; line 64 reports
   `AssertionError: process did not respond:` with empty stderr. Run summary:
   **202 tests, 1 failure, 18 existing Windows skips**. No causal diagnosis or
   fix is established. Investigate process startup/handshake/read synchronization;
   do not label it harmless flakiness, change the four-second deadline, suppress
   the test or add skips.

Other main lanes succeeded: dependencies, smoke-install, first-party extensions,
msrv, extension-api-v03, macOS and Ubuntu. Dependency review is PR-only and
skipped on main; release candidates were skipped after the upstream failure.
These partial successes do not qualify publication.

Create a narrowly scoped repair PR, qualify its exact head, merge normally and
qualify its **new exact merged SHA** before proceeding. Update source/tree/run
identities throughout this handoff; the original source archive and predecessor
receipts do not qualify that replacement. Do not merely retry the old stale
source or reuse its previously green PR.

Push also reported an open default-branch Dependabot alert:
[alert #4](https://github.com/skaft-software/octet/security/dependabot/4),
`GHSA-59cr-6r3x-644w` (medium), GitPython submodule-update path traversal.
`docs/benchmarks/swe-bench-live-lite-v0.6.3/requirements.txt:9` pins
`GitPython==3.1.61`; first patched version is 3.1.62. This is separate from the
CodeQL test-literal findings; do not dismiss it as the same false positive. The
main dependencies lane succeeded. Record it honestly, and avoid unrelated
benchmark cleanup unless release policy makes it a blocker or the user expands
scope.

## Remaining execution

Advance only **MERGED → MAIN_GREEN → PUBLISHED → PUBLIC_SMOKE**.

### 1. Qualify exact-main CI

```sh
gh api repos/skaft-software/octet/actions/runs/38477515288 \
  --jq '{id,head_sha,status,conclusion,html_url}'
```

The recorded run is now failed; first repair/requalify it as described above.
For subsequent recorded runs, a portable read-only fallback is included. From
the handoff branch root, substitute the new run ID and exact source SHA:

```sh
bash docs/maintainers/handoffs/v0.9.0-cloud/read-only-ci-watch.sh 38477515288 \
  d2425ccfa70b1da6293dac7e786816e328b22370 /tmp/octet-main-watch 240
```

Its default bounded observation is 240 × 30 seconds; `.snapshot.json` and `.exit`
are durable receipts. Exit 124 means observation exhausted, not CI failure.
`gh run watch` and the jobs API previously returned HTTP 502: transport errors
are not CI conclusions. Reconcile the same run instead of rerunning work.

Confirm full run success, exact source SHA, and successful, non-skipped lanes:
`dependencies`, `quality`, `test (ubuntu-24.04)`, `test (macos-15)`,
`windows (x86_64-pc-windows-gnu)`, `first-party-extension-tests`,
`extension-api-v03`, `msrv`, `smoke-install`. The native workflow itself enforces
these and refuses an older success after a newer CI failure.

If CI fails, inspect only the failed gate, fix the actual blocker in a separate
scoped PR, and requalify the new exact merged SHA. Do not publish the old source
just because the PR passed. Recorded model integer goldens are
`412380/824760/34365`; do not relax them to mask bad pricing projections.

### 2. Create the canonical source release only after MAIN_GREEN

Use the **new exact MAIN_GREEN merged source after both repairs**, not the failed
`d2425ccfa70b1da6293dac7e786816e328b22370` or this handoff branch. Set `SOURCE` to
that validated SHA and record its tree and CI identity. Reconcile tags/releases
again immediately before mutation. Stop on uncertainty or unexpected existing
state; never replay a possibly-completed mutation.

Read `scripts/create-source-archive.py` before executing it. It requires Python
3.11+, creates only the requested commit, checks version, bounds unsafe archive
paths/build outputs, and refuses an existing output. Example from a clean
release-source worktree:

```sh
mkdir -p /tmp/octet-v0.9.0-source-assets
python3 scripts/create-source-archive.py --version 0.9.0 \
  --ref "$SOURCE" \
  --out /tmp/octet-v0.9.0-source-assets/octet-source-v0.9.0.tar.gz
```

A local archive was prepared (2835 tracked entries, correct five-bundle catalog),
SHA-256 `4a9d28411fd089ee772a163298cde9fb816a7fd84cc722f980e841a908bf23c8`.
It was **not uploaded or signed** and belongs to the now-failing original source.
Do not reuse it for the repaired source. Regenerate in the cloud and verify its
actual bytes, tar commit header and catalog; record the produced digest rather
than pretending the local receipt qualifies a public object. Upload its checksum
too.

Create/push an annotated `v0.9.0` tag at the qualified source using full refs and
normal non-force push. Create one stable, non-draft canonical GitHub release
with `--verify-tag --latest=false`, source archive/checksum and honest notes.
The native/extension workflows require an already-published stable canonical
release and **do not create that release record themselves**; they attach their
assets. Explain that only source is available while signed assets and public
verification are pending. Preserve the previous latest release until completion.

### 3. Start the tracked signed publishers once

Read both workflows in full:

- `.github/workflows/release-octet.yml` (native):
  push `octet-binaries-v0.9.0` at the exact source; npm is not enabled.
- `.github/workflows/release-serve.yml` (five executable extensions; the old
  filename does **not** mean a Serve application ships):
  push `octet-serve-v0.9.0` at the same source.

There is no current `.github/workflows/release-octet-extensions.yml` file.
Use annotated immutable tooling tags. Record both run IDs, expected source SHA,
workflow path and tag; query those runs rather than redispatching duplicates.
Do not set `require_provider_acceptance=true`: live acceptance is explicitly
unrun and documented, not a passed gate. Preserve reviewer approval. If jobs
wait on `stable-release-publish`, report their links and request the configured
reviewer's approval; do not edit/bypass protection or approve on their behalf.

### 4. Qualify published artifacts and official install/update

Do not call publication complete until both runs and all intended publish and
verify jobs have actually succeeded. Fetch failed logs only on terminal failure.

- Verify remote canonical/tooling tags peel to the intended source commit.
- Native release expects three Linux/macOS tar archives, one Windows MSVC zip,
  `install-octet.sh`, `install-octet.ps1`, `OCTET_SHA256SUMS` and corresponding
  `.sigstore.json` bundles. Verify SHA-256, exact source/version metadata and
  Sigstore identities/workflow commit, not just asset names.
- Extensions expect **exactly five** bundles, their `.sigstore.json` files,
  `SHA256SUMS` and `SHA256SUMS.sigstore.json`. No Pi bundle. Catalog is the source
  of names; read actual artifact naming and verification in the packaging/workflow
  scripts rather than guessing.
- Expected keyless issuer: `https://token.actions.githubusercontent.com`.
  Expected identities are the exact workflow paths at their tooling-tag refs;
  certificate workflow SHA is the tooling commit (initially identical to source).
  Never use a wildcard identity or skip provenance validation.
- Native workflow checks published, version-pinned install and host hello on
  Linux, Intel macOS and ARM macOS. MSVC has its own release build/native/ConPTY
  checks; GNU CI alone is insufficient.
- Extension workflow installs, updates, lists and removes each public bundle in
  isolated HOME and asserts installation does not create activation config.
  There is no shipped Serve app/Serve smoke in this release.
- Independently download/run the **published** installer in a new isolated HOME,
  with `OCTET_INSTALL_DIR` in that root and `OCTET_NO_MODIFY_PATH=1`. Version must
  be exactly `octet 0.9.0`; run help and the installed host hello request:
  `{"protocol_version":1,"request_id":"published-smoke","command":"hello"}`.
  Preserve signature verification, never fall back silently to a source build.
- A startup/version/help check alone is not coding-task behavioral acceptance.
  Keep physical-display, real-driver and paid-provider/audio journeys explicitly
  unqualified unless separately performed with appropriate authorization.

After genuine public qualification, update the release page with immutable
source, CI/publication run links, checksum/signature and observed public-install
receipts plus independent-channel/unrun limitations. Only then promote latest
and verify the public latest installer path resolves to the same qualified
version. Finish with exact identifiers, observations and remaining exceptions.

## Known prior blockers (already fixed; do not redo)

- An MCP reconnect test raced server headers; header event barriers and its
  original bounded `wait_for` fixed it. No assertions/deadlines removed.
- Predecessor PR CI quality failed because installation docs lacked the literal
  npm notice. Current source adds **“v0.9.0 is not yet published to npm.”** and
  honest Windows GNU/MSVC boundaries, preserving all tests and adding assertions.
- GitHub `gh pr edit` failed on a retired Projects-classic GraphQL field. REST
  PR PATCH worked. Do not infer failed transport mutated state.
- Metadata freshness previously changed upstream; reviewed provider-scoped
  projections were regenerated, not hand-edited. Final local freshness passed.
- Historical unsigned RC verifier/install/Pi-acceptance scripts were specific to
  older run IDs and six-bundle inventory. They are not qualified for this final
  source/publication and must not be reused unchanged.
