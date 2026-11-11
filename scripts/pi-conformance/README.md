# Pi conformance harness

Test-only, not wired into CI. It runs Pi's own unchanged example extensions through native Pi
(interactive mode on a PTY) and through the octet binary with the `octet-pi-compat` adapter, both
against a loopback mock provider (nothing leaves the machine), and compares what the user sees
and what the model is offered.

Setup (once):

```sh
npm install --prefix scripts/pi-conformance      # Pi 1.5.0 + @xterm/headless
cargo build --release -p octet-coding-agent --bin octet
```

Run, with an isolated HOME and Node 22.19.0 first on PATH:

```sh
export HOME=/tmp/conformance-home PI_OFFLINE=1
python3 scripts/pi-conformance/conformance.py --all-examples --out /tmp/conformance-out
```

Environment: `E2E_OCTET` (binary, default `target/release/octet`), `E2E_NODE_BIN` (Node bin dir),
`E2E_PI_ROOT` (a directory whose `node_modules` holds Pi 1.5.0 and `@xterm/headless`; default this
directory), `E2E_TMP`, `E2E_WIRE=<prefix>` (tap the extension wire into `<prefix>.current`).
Options: `--only a,b`, `--hosts pi,current`, `--all-examples`, `--skip-core`.

Output: `conformance.json` and `conformance.md`. A row passes when `current` equals Pi. An example
passes when every row passes and the migration-safety check below holds. `blocked` marks a known
difference with its reason in `BLOCKED`; `skipped` is an example that cannot run offline (reason
in `SKIP`) or that native Pi itself cannot load offline.

## Scenarios

* Core scenarios (`SCENARIOS`): hello, commands, status-line, custom-footer, widget-placement,
  session-name, custom-header, rainbow-editor, question, overlay-test, project-trust, and the
  offline scenarios for examples the generic one cannot judge: `auto-commit-on-exit` (a temp git
  repo with an uncommitted file; asserts the exit commit), `with-deps` (the example copied with its
  `ms` dependency linked from the Pi install, no network), `rpc-demo` (input dialog, notify,
  prefill), `custom-provider-*` (registration only, no inference).
* `--all-examples` adds a generic scenario for every other example: it loads, shows the same
  startup text and offers the model the same extension tools.
* Cannot run offline: `gondolin` (imports `@earendil-works/gondolin`, which is not installed, and
  needs a VM).

## Migration safety

For every scenario on both hosts the isolated Pi agent directory is seeded with an `auth.json` and
a saved session, fingerprinted (path and sha256 of every file, symlinks by target) before the run
and after the process exits. For `current` the set of added, changed and deleted paths must be a
subset of what the native Pi run of the same scenario changed, and `auth.json` and `sessions/` must
be byte-identical. A violation fails the example (`agent dir writes` row).
