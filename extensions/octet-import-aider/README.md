# Aider migration adapter

This directory contains a bounded, source-only Aider import adapter for extension
API 0.3. It reads only the host-authorized `source_root` and returns the shared
migration shapes; the host owns persistence, conflict handling, and idempotence.

The manifest requests workspace filesystem access only. The adapter does not
request secrets or environment values, launch processes, access the network, or
execute Aider configuration. It imports non-secret model selections, the
project instruction file when safe, and local stdio MCP declarations. History,
ignore rules, remote MCP transports, metadata/cache files, credentials, and
unsupported settings become bounded diagnostics or are omitted.

## Run the executable directly

The protocol is canonical LF-delimited JSON-RPC. A host must first negotiate
API `0.3`, then call `migration/detect` and pass its returned `config_paths` to
`migration/import`:

```sh
python3 extensions/octet-import-aider/extension.py
```

The adapter accepts no command-line configuration and never writes to the
source tree.

## Tests

The standard-library test suite includes protocol, fixture, YAML-adversarial,
secret-filtering, symlink, and source-immutability checks:

```sh
python3 -m unittest discover -s extensions/octet-import-aider/tests -p 'test_*.py'
python3 -m py_compile extensions/octet-import-aider/extension.py
```

Keep raw/private runtime captures outside tracked documentation.
