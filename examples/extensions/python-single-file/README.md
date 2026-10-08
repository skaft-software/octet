# Python single-file local authoring

`extension.py` is a 12-line API 0.4 tool example (the SDK provides framing,
negotiation, scheduling and cancellation). Create a local package directly; the
tool vendors the reviewed source SDK and its license/provenance, so the result
runs without a global install:

```sh
python3 scripts/extension-author.py examples/extensions/python-single-file/extension.py /tmp/wait-extension --name wait-extension --tool wait
```

`--tool` names must exactly match literal static `@ext.tool(name=...)`
annotations with a non-empty literal `description`; dynamic declarations are
rejected. The tool never imports or runs author code. Optional `--skill` and
`--asset` inputs are copied to the package root (`SKILL.md`) and `assets/`;
input symlinks and hidden files are refused. Inspect `extension.toml` and
explicitly select this local package using the host's extension
directory/enablement options. Discovery does not execute it; the tool does not
enable or trust it.

The generated directory is local package source; use the archive step below for a
validated local release-format bundle. It is not a published/catalog-qualified
asset. Installation is separate and never enables or trusts it. The generated
manifest pins `requires_octet` to the current workspace host version. To build
the deterministic-format archive without publishing, use the existing packager
and pass that exact host version:

```sh
bash scripts/package-octet-extension-release.sh wait-extension /tmp/bundles v0.9.0 /tmp/wait-extension
```

Substitute the current `[workspace.package].version` from the root `Cargo.toml`
for `v0.9.0`. The archive is still not a published/catalog-qualified asset.
Installation remains separate and never enables or trusts it. To exercise host
discovery, explicit test enablement and the real process lifecycle on an unpacked
package:

```sh
env -u PYTHONPATH CARGO_TARGET_DIR=/tmp/python-single-file-host-target \
  CARGO_BUILD_JOBS=1 RUST_TEST_THREADS=2 cargo run --offline --locked --profile ci-test \
  --manifest-path examples/extensions/python-single-file/host-smoke/Cargo.toml -- /path/to/unpacked/wait-extension
```

The host smoke selects the package directory supplied on the command line, including
both `wait-extension` and `wait-tool` names. It checks disabled-by-default refusal,
explicit enablement, API 0.4 negotiation, dropped-waiter cancellation with restart
supervision disabled, subsequent same-generation tool use and acknowledged shutdown.
It uses only the repository Rust host and local extension; it does not load provider
configuration or make model/network calls. After building the probe, run the generator
regression suite against that real host binary:

```sh
OCTET_PYTHON_HOST_SMOKE=/tmp/python-single-file-host-target/ci-test/python-extension-host-smoke \
  python3 scripts/test_extension_author.py -v
```

Without `OCTET_PYTHON_HOST_SMOKE`, the real-host package-name regression is explicitly
skipped; the other generator tests include only a raw subprocess lifecycle check.

The package contains your `author.py`, a generated launcher and bounded vendored
SDK source under `vendor/`; the SDK is not an independent installation. Only the
authored source is your one file; extra modules and directories must be added
deliberately as assets and wired manually. Dynamic declarations, commands, hooks,
and schema inference are not generated.
