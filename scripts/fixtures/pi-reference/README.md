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

Change the pinned version only together with the supported Pi range and the
shim inventory; Dependabot does not manage this directory.
