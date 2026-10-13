# word-count-typescript: one tool and one slash command

The same example as the Python recipe, in TypeScript.
[`extension.ts`](extension.ts) is 18 lines: it registers the `word_count` model
tool and the `wordcount` slash command, both of which return
`<path>: <count> words` for a file. Argument types are inferred from the literal
schema; the [process SDK](../../../sdk/typescript/process/README.md) owns the
JSON-RPC process loop, cancellation and shutdown.

Requires Node **>=22.19.0** and the reviewed **0.9.0 source SDK**. No provider,
credentials, network, global install or registry package is needed. From the
repository root:

```console
npm run pack:sdk --prefix examples/extensions/word-count-typescript
npm install --offline --ignore-scripts --no-package-lock \
  --prefix examples/extensions/word-count-typescript
npm run manifest --prefix examples/extensions/word-count-typescript
```

The local package installs the reviewed SDK tarball produced by `pack:sdk`, not a
mutable checkout symlink. Manifest generation reads registrations without
invoking handlers and writes the exact API `0.4` manifest and a staging-safe
`.octet-launcher.sh` which execs the installed Node in place; the host stages the
script, never the dynamically linked interpreter. The generated manifest,
launcher and `node_modules/` are ignored by Git because those paths are
machine-local; [`package.json`](package.json) is the portable recipe. The
generator refuses to overwrite an existing manifest; after moving the checkout or
changing registrations, remove the generated `extension.toml` and
`.octet-launcher.sh`, then rerun `npm run manifest`.

Review the source before generation: importing the module executes its top-level
registration code. The example only registers the tool and command, and exports
the Extension; it does not call `run()`.

## Enable and run

With a source-built API `0.4` host:

```console
octet --extension-dir ./examples/extensions --enable-extension word-count-typescript
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
example into a scratch extension directory, runs the three local build commands,
serves a loopback scripted OpenAI-compatible endpoint that answers with a
`word_count` tool call, runs the real binary in print mode, and types
`/wordcount sample.txt` into the real TUI:

```console
OCTET_BIN="$HOME/src/octet-release/bin/octet-latest" \
  python3 examples/extensions/word_count_examples_test.py
```

Observed on this checkout with the real `bin/octet-latest` (API `0.4`):

```text
test_typescript_example_slash_command ... ok
test_typescript_example_tool_call ... ok
...
Ran 4 tests in 3.823s

OK
```

The print-mode half asserts the scripted model saw the tool result
`sample.txt: 18 words`; the PTY half asserts the real TUI rendered
`sample.txt: 18 words` for `/wordcount sample.txt`.

Launcher generation is Unix-only; Windows native launch is unqualified and
generation refuses it. This is a local source example, not an installable
bundle: the generated manifest has no host-version pin and is not an npm
publication.
