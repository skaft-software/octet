# Offline robustness targets

Install the pinned `nightly-2026-08-01` toolchain and `cargo-fuzz` 0.13.2.
Run from the repository root; keep build output on disk, not a RAM-backed `/tmp`.

| Target | Exercises / assertions | Input cap |
| --- | --- | --- |
| `session_record` | Saved-session deserialization | 2 MiB |
| `terminal_input` | Production stdin decoder, byte/chunk boundaries, escape timeout, replacement decoding; bounded retained bytes | 4 KiB |
| `text_utils` | Visible width, clipping with varied ellipses, ANSI/DCS/image-anchor stripping, wrapping; bounded clipped width, idempotent stripping, literal grapheme boundaries | 4 KiB |
| `extension_wire` | Production LF framing with negotiated and small limits, CRLF, invalid UTF-8/JSON, host message types and API 0.4 provider events | 64 KiB |
| `markdown_render` | Assistant Markdown, code, tables, math and diagrams at widths 0/1/2 and a selected width, plain and truecolor; every row fits | 4 KiB |

Harnesses perform no terminal, network, file or process operations. The decoder
and framing harnesses include the production source modules directly, avoiding a
public fuzz-only API or a second implementation. Input caps keep mutation runs
fast; regular unit tests cover exact native frame ceilings. Seed files under
`corpus/<target>/` are deliberately small and synthetic. Keep discovered bulk
corpora and `artifacts/` local; promote minimized findings to owning-crate unit
tests, so ordinary CI catches regressions without a fuzzer.

```sh
export CARGO_HOME="$HOME/.cargo" RUSTUP_HOME="$HOME/.rustup"
HOME=$(mktemp -d) TMPDIR=$(mktemp -d) \
  cargo +nightly-2026-08-01 fuzz run terminal_input -- \
  -max_total_time=1200 -rss_limit_mb=4096 -jobs=2 -workers=2
# Substitute any target from the table. max_total_time is per worker.
cargo +nightly-2026-08-01 fuzz tmin terminal_input fuzz/artifacts/terminal_input/crash-<hash>
```

The weekly security workflow runs short campaigns, not an exhaustive proof.
Coverage and execution counts are libFuzzer's observed counters, not percentages
of the product or a claim about live providers or real terminal qualification.
