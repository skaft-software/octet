# word-count-python: one tool and one slash command

The smallest real API `0.4` Python extension. [`extension.py`](extension.py) is
20 lines: it registers the `word_count` model tool and the `wordcount` slash
command, both of which return `<path>: <count> words` for a file. The
[Python SDK](../../../sdk/python/README.md) owns the JSON-RPC process loop,
cancellation and shutdown; the author writes no protocol code.

Requires Python >=3.9 on `PATH` and the reviewed source SDK from this checkout.
No provider, credentials, network, pip install or registry publication is
needed for the example itself.

## Vendor the SDK

[`run-extension`](run-extension) is the launcher the local package generator
writes: the host stages the launcher into private storage, so it reads
`OCTET_EXTENSION_DIR`, puts `vendor/` on `PYTHONPATH`, suppresses bytecode and
execs `python3 -B extension.py`. Copy the SDK into that directory (the same
closure the generator vendors):

```console
mkdir -p vendor
cp -R ../../../sdk/python/octet_extension vendor/octet_extension
chmod +x run-extension
```

`vendor/` is ignored by Git. The checked-in
[`extension.toml`](extension.toml) declares API `0.4`, the launcher entrypoint,
`filesystem = "workspace"` and the exact `word_count`/`wordcount` catalog.

## Enable and run

With a source-built API `0.4` host:

```console
octet --extension-dir ./examples/extensions --enable-extension word-count-python
```

Check `/extensions status`, then let the model call `word_count`, or run the
command directly:

```text
/wordcount README.md
```

Both return:

```text
README.md: <count> words
```

Discovery never executes or enables anything. `--extension-dir` grants
invocation-only source authority for that run; full-access startup trusts the
explicitly enabled extension without persisting a grant, and safe/controlled
policies need an explicit source-bound grant. Capability metadata is host
consent metadata, **not** an OS sandbox: the child reads the file with ordinary
OS authority. See the [host guide](../../../docs/extensions.md).

The Python and TypeScript recipes register the same `word_count`/`wordcount`
names, so enable one of them at a time when both live under the same discovery
root.

## Provider-free acceptance

[`../word_count_examples_test.py`](../word_count_examples_test.py) copies this
example into a scratch extension directory, serves a loopback scripted
OpenAI-compatible endpoint that answers with a `word_count` tool call, runs the
real binary in print mode, and types `/wordcount sample.txt` into the real TUI:

```console
OCTET_BIN="$HOME/src/octet-release/bin/octet-latest" \
  python3 examples/extensions/word_count_examples_test.py
```

Observed on this checkout with the real `bin/octet-latest` (API `0.4`):

```text
test_python_example_slash_command ... ok
test_python_example_tool_call ... ok
...
Ran 4 tests in 3.823s

OK
```

The print-mode half asserts the scripted model saw the tool result
`sample.txt: 18 words`; the PTY half asserts the real TUI rendered
`sample.txt: 18 words` for `/wordcount sample.txt`.

The launcher is Unix-only, matching the existing Python/TypeScript local
recipes. This is a local source example, not an installable bundle: it has no
host-version pin and is not a catalog or PyPI publication.
