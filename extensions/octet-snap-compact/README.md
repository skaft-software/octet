# octet-snap-compact (local API 0.4 extension)

Replaces **local** parent-model compaction with deterministic PNG frames from
[`oxi-snapcompact` 0.64.0](https://docs.rs/oxi-snapcompact/0.64.0/oxi_snapcompact/)
when the *active* model accepts image input. It is not a model-callable tool.
Octet still selects the retained history boundary, validates the output and
context budget, persists the checkpoint, and replays the images as an ordinary
synthetic user message. On text-only models without a bitmap checkpoint, the
normal parent-model summarizer remains in use. `native-responses` is not
replaced. This is source-checkout functionality for octet 0.8.0, not a
published bundle or API 0.3 process.

## Build and enable

The renderer is a separate Rust package because `oxi-snapcompact` 0.64.0
requires Rust **1.96+** (octet's main workspace declares 1.86). Build it once:

```sh
cargo build --release --locked --manifest-path extensions/octet-snap-compact/renderer/Cargo.toml
python3 -m pip install ./sdk/python  # if the source SDK is not importable
cargo build -p octet-coding-agent --bins
./target/debug/octet --extension-dir ./extensions --enable-extension octet-snap-compact
```

`extension.py` is executable and finds the SDK directly when run from this
checkout. For a copied standalone directory, install the checkout's source SDK
in the Python interpreter used by the entrypoint and bring the built renderer
with it. Extensions are disabled by default, and safe mode never starts them.
Executable extensions run with your OS authority; `filesystem = "none"` and
`network = false` are consent metadata, not isolation.

Keep `[compaction] mode = "local"` (the default) for automatic compaction, or use
`/compact` at a safe boundary. `/extensions status` should show the extension
running with the `compaction_strategy` negotiated feature. For this source
extension, omit `--enable-extension` on the next launch to disable it.

## Boundaries

- The host sends only text serialized from discarded turns and any earlier
  bitmap source. Tool results use the existing 2,000-character summarization
  limit; pre-existing non-text media are represented by placeholders.
- The renderer uses the crate's model-selected shape. Every source character
  must be covered. Up to 256 PNGs / 8 MiB and 256 KiB source are accepted;
  batches carry at most 2,048 characters and 32 PNGs. A protocol, render,
  cancellation or context-budget failure **does not commit a checkpoint** and
  does not silently retry the parent model.
- Inline PNGs are persisted with the source text in the append-only session.
  Later compactions rerender that source plus new discarded turns. The old
  history is not erased from disk. Switching a session *with an active bitmap
  checkpoint* to a text-only model fails explicitly rather than silently
  replacing the images with placeholders; switch back to a vision route.
- Each image consumes provider context. This strategy is useful only when the
  rendered frame count actually fits the selected model's working window.
  It is not a guarantee of cheaper tokens or accurate visual recall.

## Checks

```sh
cargo test --manifest-path extensions/octet-snap-compact/renderer/Cargo.toml --locked
python3 -m unittest discover -s extensions/octet-snap-compact -p 'test_*.py'
cargo test -p octet-agent --lib compaction_strategy
cargo test -p octet-agent --lib vision_compaction_bypasses_parent_summary_and_bad_frames_keep_history
cargo test -p octet-agent --lib text_only_compaction_keeps_parent_summary_path
cargo test -p octet-agent --test snapcompact_session
```
