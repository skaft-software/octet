# Mermaid oracle fixtures

These are **captured upstream outputs**, not self-captured octet goldens.

## Reference and licensing

Pi reference: `earendil-works/pi` at
`890f920884f6d21fc7617d236ef9e1cc5d7a0ef8`,
`packages/coding-agent/src/modes/interactive/components/mermaid.ts`.
Its lockfile pins `grok-mermaid` **0.2.3**.

The public package tarball was fetched for development-time inspection only:
`https://registry.npmjs.org/grok-mermaid/-/grok-mermaid-0.2.3.tgz`
(SHA-256 `41d02063f53261fe869acb6da0b01838d6ece095d73a7063645ce0442f0d8c88`).
Its dependency-free `dist/index.js` is the oracle, run under Node. No install,
package scripts, network access, JavaScript engine, or subprocess is required
by octet's build, tests, or renderer.

The package README identifies the original **Rust** implementation:
`https://github.com/xai-org/grok-build/blob/main/crates/codegen/xai-grok-markdown/src/mermaid.rs`.
The inspected original has SHA-256
`e53c81fc02cddc78a3f7f3729237f83acd7c2678535a830210d2b1320f60529f`.
Rather than inventing another grid or adding a JavaScript runtime, octet adapts
that original's flowchart layout/label cleanup in
`src/rich_text/mermaid/{layout,labels}.rs`. Both are Apache-2.0, **not MIT**;
copyright notices and the full license are retained in
`src/rich_text/mermaid/LICENSE-APACHE`. Changes are prominently noted in each
adapted file. The Pi port's grapheme-cell painting and outer-blank-row trimming
are also applied: the older Rust original split emoji/combining sequences and
retained a leading blank row in LR diagrams.

## Corpus

`pi-flowcharts.rs` contains 67 byte-for-byte oracle outputs: baseline flowcharts,
all four directions, nested/group-to-group/frame links, local `direction`
(which upstream accepts but ignores), node lists, cycles, self-links,
non-adjacent edges, dotted/thick strokes, label wrapping, quoted/class labels,
HTML formatting/entities/breaks, and CJK/combining/emoji labels in all directions.

The original octet dotted/hyphenated-id case is intentionally kept as an octet
extension in `mermaid_render.rs`, not claimed as upstream-equivalent: Pi parses
`a.b_c-1` as multiple nodes and a dotted link. Likewise, octet's stricter parser
rejects malformed groups/labels rather than publishing upstream's partial art.

## Actual reported architecture graph

`reported-architecture.mmd` is the full user-reported graph, with only copied
terminal wrapping undone: 64 nodes, 7 groups, 82 intended edges, nested Repo /
nix / scripts / reference / themes / env / relay groups, class directives,
HTML breaks, and many converging and skip-layer edges.

Running the **unmodified source** through grok-mermaid 0.2.3 returns art measuring
**496 columns × 87 rows**, plus this warning (81 parsed edges):

```
dropped, link has no target: "ProxyEnv -.PROXY_ENV or ./proxy.env.-> LocalProxy"
```

Pi checks available width **before** warnings. At 80 columns it therefore keeps
the original code fence; at 496+ columns its final renderer keeps the fence and
adds the visible warning. It does not draw this graph into an 80-column screen.

For a complete-diagram oracle, change exactly one equivalent edge spelling:

```
ProxyEnv -.PROXY_ENV or ./proxy.env.-> LocalProxy
ProxyEnv -.->|PROXY_ENV or ./proxy.env| LocalProxy
```

The result is **501 columns × 86 rows**, **zero warnings**, all 82 edges.
`reported-architecture.pi.txt` is that upstream result. Octet renders the
**original source**, including the punctuation-heavy inline edge label, and
matches this equivalent-spelling upstream output byte-for-byte. This is an
intentional parser improvement, not a claim that Pi renders the original
source without a warning. At ordinary terminal widths octet must also retain
the source rather than crop or wrap graph rows.

## Regeneration

With an already extracted grok-mermaid 0.2.3 package (no installation needed):

```
node crates/sexy-tui-rs/tests/fixtures/mermaid/capture.mjs /path/to/package
cargo test -p sexy-tui-rs --test mermaid_parity --test mermaid_render --locked
```

The capture script imports only that provided package and refreshes the
upstream goldens. Ordinary Rust tests read the checked-in fixtures offline.

## Explicit remaining boundary

Only graph/flowchart families dispatch here: state, class, ER, sequence, pie,
etc. remain typed errors. Circle/cross/bidirectional/reversed arrow heads and
other arrow operators beyond `-->`, `->`, `---`, `-.->`, `==>` and their
inline-labelled forms are not implemented. Local directions are ignored as in
Pi; shapes map to rectangular or rounded outlines as in Pi, not SVG outlines.
Semantic color spans and partial-streaming art are not returned by this API.
Bounds: 16 KiB source, 128 nodes, 512 edges, 24 groups / depth 6, 1024-cell input
labels, at most 2^21 canvas cells / 4096 columns / 2048 rows. Pi-style node labels
wrap at 24 cells / 4 rows and edge labels truncate at 28 cells.
