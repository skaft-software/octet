# Built-in Pi system palette

Rust port of Pi 1.0.2 `system-theme.ts`, reviewed contract
`cd32f7725fdbddbaecdff5b1e68491563394e0ca` (MIT; see LICENSE).
The recipe/solver supplies 56 semantic palette tokens; Octet retains its own
renderer, identity, command controls and geometry. The existing perceptual-color
port is shared with imported Pi palettes. There is no runtime Node dependency.

## Independent offline oracle

`oracle.mjs` verifies the package versions and exact SHA-256 hashes of the three
upstream TypeScript files before executing them in isolated data modules. It
does not execute Octet or the Rust solver, fetch dependencies, or change Pi.
`vectors.json` records 119 independent expected palettes: dark/light, indexed
fallbacks, background-only, unreadable foregrounds, midgray, saturation bounds,
colored backgrounds and palette mutations including Catppuccin Frappe.

```sh
node crates/octet-coding-agent/src/tui/theme/pi/oracle.mjs /reviewed/pi --check
# Explicit fixture/recipe regeneration, then format and re-run the Rust tests:
node crates/octet-coding-agent/src/tui/theme/pi/oracle.mjs /reviewed/pi --write
cargo fmt --all
cargo test -p octet-coding-agent --lib tui::theme::pi::tests
```

Requires Node with `stripTypeScriptTypes` (qualification used Node 22.20).
`--check` compares oracle vectors; `--write` regenerates both vectors and recipe
constants. It never regenerates the independently ported solver. The recorded
commit is the review contract, not a claim that this script verified Git HEAD.
Rust tests require byte-exact token values and faint flags across every vector,
and check native projection, readability, model independence and no-color output.
Passing these is palette qualification, not desktop, provider or release proof.
