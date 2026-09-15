# Cline migration adapter

This directory contains a bounded, read-only Cline setup import adapter for
extension API 0.3. It treats every source file as inert JSON or UTF-8 text and
returns the shared migration shapes; the host owns persistence, conflict
handling, and idempotence.

The manifest requests no filesystem, process, or network capability beyond the
host-authorized `source_root`. The adapter never imports source modules, expands
settings, launches commands, executes Cline configuration, or writes to either
the source or the destination. Non-secret model selections, safe project
instructions, and local stdio MCP declarations are imported; credentials,
secret-bearing environment values, unsafe or non-portable MCP data, symlinked
sources, and unsupported settings become bounded value-free diagnostics or are
omitted.

## Run the executable directly

The protocol is canonical LF-delimited JSON-RPC. A host must first negotiate
API `0.3`, then call `migration/detect` and pass its returned `config_paths` to
`migration/import`:

```sh
python3 extensions/octet-import-cline/extension.py
```

The adapter accepts no command-line configuration and never writes to the source
tree.

## Tests

The standard-library test suite covers protocol negotiation, fixtures, malformed
and duplicate JSON, secret filtering, symlink rejection, bounded diagnostics,
and source immutability:

```sh
python3 -m unittest discover -s extensions/octet-import-cline -p 'test_*.py'
python3 -m py_compile extensions/octet-import-cline/extension.py extensions/octet-import-cline/cline_import.py
```

Run from the repository root. The suite uses only local fixtures and performs no
network access. Raw/private runtime captures are deliberately kept outside
tracked qualification documentation.
