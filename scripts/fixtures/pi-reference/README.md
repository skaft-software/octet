# Pi reference fixture (test-only)

`@earendil-works/pi-ai` **1.0.2** — the supported-reference floor of the Pi range
octet accepts (`>=1.0.2 <1.5.0`) — pinned by `package-lock.json`.

Rust acceptance tests that exercise an extension's explicit `pi-ai` provider
streams (for example `streamOpenAICompletions`) run the *installed* Pi
implementation, by design: the request a Pi package builds is the request Pi
builds. They therefore need a supported `@earendil-works/pi-ai` that Node can
resolve from `extensions/octet-pi-compat/shims/`. Without one the shim refuses
(`-32031 installed_pi_unavailable`, no silent fallback), the extension provider
reports an error, and tests such as
`pi_custom_http_delegate_uses_process_fetch_and_current_host_hooks` fail.

This directory is never shipped and is not a dependency of octet or of
`extensions/octet-pi-compat`. CI installs it from the lockfile with scripts
disabled and links the package where the shim resolves it:

```sh
npm ci --prefix scripts/fixtures/pi-reference --ignore-scripts --no-audit --no-fund
mkdir -p extensions/octet-pi-compat/node_modules/@earendil-works
ln -s "$PWD/scripts/fixtures/pi-reference/node_modules/@earendil-works/pi-ai" \
  extensions/octet-pi-compat/node_modules/@earendil-works/pi-ai
```

Run the same two steps once after `npm ci --prefix extensions/octet-pi-compat` to
run those tests locally. (`npm ci` in the adapter directory removes the link.)
CI reuses that prepared adapter installation for both the Rust and Node tests;
it must not reinstall the adapter between those suites and remove the fixture.

Change the pinned version only together with the supported Pi range and the
shim inventory; Dependabot does not manage this directory.

## Reduced long-session crash probe

After installing both locked Node fixtures (and the adapter's native editor peer
as in CI), run from the repository root:

```sh
python3 -B scripts/pi-eval/crash_probe_selftest.py
python3 -B scripts/pi-eval/crash_probe.py --out /absolute/new/private-evidence
```

The supervisor records host PID, observed exit code or signal, its own timeout or
output-limit intervention, sampled Linux host RSS (unavailable elsewhere), and a
64 KiB stderr tail. Each leg retains bounded stdout/stderr and `result.json`.
Node diagnostics record oracle/adapter child PID, wait status, RSS and live heap,
plus snapshot begin/end or applied revision/head transitions. EOF alone never
establishes OOM. RSS samples are evidence, not a memory-growth acceptance bound.
The reduced corpus uses Pi 1.5.0's published SessionManager: 1,200 entries,
compaction, branch checkout, reopen, new/switch, and six routed adapter revisions
including mid-transfer cancellation and later successful hooks. No providers,
user factories, user sessions or user configuration are loaded.

The existing `session-driver.mjs` accepts optional per-case `checkpoints` (zero-based
operation indexes) and `diagnostics: true`. Every operation executes, but only the
selected observations are retained. Default paired tests still observe every
operation. Retaining all large snapshots can itself exhaust the harness's JSON
output limit; that is not evidence of an adapter crash. CI runs the oracle probe;
the adapter leg also runs in the normal Node suite.

Full-inventory acceptance on the owner's real setup is **NOT RUN**. These probes
neither reproduce nor diagnose a historical full-inventory process exit.
