# Terminal images full qualification

Status: source candidate only; not qualified. This document records the bounded Kitty/iTerm2 image foundation and its verification boundary. No Cargo command, test, build, or terminal probe was run in this lane.

## Scope

The foundation accepts caller-owned bytes for PNG, JPEG, GIF, and WebP containers. It validates the source before retaining it, keeps payload bytes private, and exposes only safe format, dimensions, metadata, and byte-count summaries through `TerminalImage`.

- Kitty emits PNG only.
- iTerm2 emits PNG, JPEG, and GIF; WebP remains unsupported.
- Unsupported terminals and unsupported protocol/format combinations produce deterministic semantic fallback rows.
- Semantic rows and copy text never contain protocol controls, base64 payloads, filenames, or source paths.
- Kitty has targetable replacement and deletion. iTerm2 replacement and deletion return `UnsupportedOperation` rather than claiming lifecycle support it cannot provide.

## Bounded safety contract

`ImageLimits` applies caller-selected lower bounds beneath fixed hard ceilings. The current defaults and hard ceilings cover payload bytes, encoded output, protocol chunk size/count, dimensions, pixel count, container items, parser headers, filenames, terminal replies, live IDs, and query timeout. Zero and over-ceiling limit values are rejected.

Container validation requires exact framing and bounded records:

- PNG validates chunk framing and CRCs, requires IHDR/IDAT/IEND structure, and rejects APNG animation chunks.
- JPEG validates SOF dimensions, bounded headers, SOS structure, and exact EOI framing.
- GIF validates fixed extension block lengths, sub-block boundaries, image geometry, and the trailer; Netscape/ANIMEXTS loop application extensions and other animated forms are rejected.
- WebP validates RIFF length/padding, VP8/VP8L/VP8X dimensions, and rejects animation flags/chunks.

Filename metadata is validated as display-safe ASCII and is independently rechecked against the active planner/encoder limit. Capability replies are strict, bounded, query-specific, and reject prefixes, suffixes, concatenated replies, unknown fields, and mismatched Kitty IDs.

## Rendering and lifecycle boundary

`ImageLayout` uses a validated cell-pixel measurement when available and checked arithmetic for aspect-preserving fitting. Without a measurement it uses the conservative one-cell fallback. `ImageRenderPlan` returns blank semantic rows plus an opaque `ImageTerminalCommand`; fallback plans return safe text only. `ImageAnchor` is a bounded DCS marker for an adapter that owns command emission and contains no image payload.

`ImageRegistry` allocates monotonic IDs, caps concurrent live IDs, and never reuses retired IDs. A Kitty replacement emits delete-then-transmit, and delayed deletes cannot target a newer image. iTerm2 has no equivalent targetable cleanup operation.

`ImageProtocolEncoder::write_to` streams bounded output with fixed-size base64 buffers. It checks complete output and chunk limits before emission. `TerminalCapabilities::detect()` remains conservative and performs no query I/O; callers may supply validated, correlated capability overrides or replies.

## Focused source coverage

- `crates/sexy-tui-rs/src/images.rs`: validators, limits, protocol matrix, capability queries, layout, semantic separation, opaque commands, anchors, registry, and lifecycle behavior.
- `crates/sexy-tui-rs/src/capabilities.rs`: conservative terminal capability and cell-pixel primitives.
- `crates/sexy-tui-rs/tests/images_current.rs`: hermetic PNG/JPEG/GIF/WebP fixtures, malformed/polyglot and animation rejection, fallback revalidation, strict capability replies, bounded protocol output, semantic separation, and lifecycle/anchor checks.
- `docs/qualification/terminal-images-full.md`: this qualification boundary.

## Verification proposal

The verifier should run the focused image test target and the surrounding `sexy-tui-rs` tests, then inspect the diff and exact emitted bytes:

```text
cargo test -p sexy-tui-rs --test images_current
cargo test -p sexy-tui-rs
cargo fmt --all -- --check
```

The verifier should additionally exercise Kitty and iTerm2 in real terminals where available, confirm that unsupported combinations remain text-only, inspect replacement/deletion bytes, and verify that semantic selection/copy paths never receive protocol controls. No command or result from these checks is claimed here.

## Integration handoff and acceptance limits

Coding-agent media delivery, `ToolFinished` media retention, hydration, and broader TUI projection are outside this foundation’s ownership and remain required before end-to-end qualification. The current source candidate has not passed Rust compilation, tests, formatter checks, protocol capture, terminal compatibility checks, or media/resume integration. Do not mark terminal-image support qualified until those checks are independently observed.
