# octet identity assets

Canonical repository-owned export of the approved native `01101111` byte and
original outlined wordmark. These are identity assets, **not product captures**.
The intended product version is 0.7.0; this package does not establish a released
binary, installation, performance result, or terminal/browser qualification.

## Use

- [Primary symbol](marks/mark-gradient.svg): transparent public blue–cyan–green mark.
- [Black](marks/mark-black.svg) / [white](marks/mark-white.svg): monochrome symbol.
- [Black wordmark](marks/wordmark-black.svg) / [white wordmark](marks/wordmark-white.svg): original outlines, no font dependency.
- [Light lockup](marks/lockup-light.svg) / [dark lockup](marks/lockup-dark.svg): symbol, outlined wordmark and descriptor.
- [Light badge](marks/badge-light.svg) / [dark badge](marks/badge-dark.svg): square icon slots.
- `marks/icon-{light,dark}-{16,24,32,48,64,128,256,512,1024}.png` and
  [favicon.ico](marks/favicon.ico): pinned original raster exports.
- `marks/mark-{provider}-{light,dark}.svg`: historical provider-color samples;
  not provider endorsements, capability claims, or a runtime color registry.
- [Download package](octet-identity.zip), [manifest](manifest.json),
  [file checksums](SHA256SUMS), [ZIP checksum](octet-identity.zip.sha256).

Use the public gradient for directories and general identity. In the product,
keep the existing model/theme color pipeline, foreground balancing, monochrome
and custom overrides; static samples must not replace runtime theme behavior.
Status colors still mean status, not provider identity.

## Fixed construction

Eight contiguous positions encode lowercase ASCII `o` (`0x6f`). Positions **1
and 4** are half-height; the other six are full-height, all on one baseline.
The primary master is 256 × 128: each column is 32 units wide. Zero columns have
`y=64, height=64`; one columns have `y=0, height=128`. No gaps, reordering,
ninth cursor-like column, bevels, or changed proportions. Give free-standing
lockups at least one column of clear space. At small sizes omit the wordmark.

Two-row terminal equivalents (the leading blank is significant):

```text
 ██ ████
████████
```

```text
 ## ####
########
```

These are construction examples, not captured terminal output. Keep terminal
input immediate and use existing capability/theme fallbacks; no image protocol,
Nerd Font, forced background or mandatory animation is required.

## Provenance and reproduction

The assets are byte-for-byte imports from the founder-approved native handoff
`octet-identity-20260904-02/octet-byte-brand-draft`, not redrawn approximations.
The [manifest](manifest.json) records the approved plan and source README hashes,
individual file hashes, roles and geometry. The source README's draft-selection
status predates approval. `palettes.json` retains its historical Ygg source
attribution deliberately. Historical/third-party names are not product aliases.

Run from the repository root with Python 3.10+ (standard library only):

```sh
python3 docs/assets/octet/export.py --check
python3 docs/assets/octet/test_export.py
python3 docs/assets/octet/export.py --write
```

`--check` validates pinned hashes, SVG construction/outlined text, PNG structure
and dimensions, ICO sizes, and exact reproducibility of the checksums and ZIP.
`--write` rebuilds only the export checksums and deterministic, uncompressed ZIP
from those validated inputs. It does **not** bless modified masters or recreate
rasterization; the original PNG/ICO bytes are pinned source exports. Raster
regeneration/optical changes require separate reviewed provenance. Exporting
requires no network, installed fonts, image libraries, private handoff path or
product build. The ZIP has fixed timestamps, permissions and sorted paths.

No concept social card, terminal glyph proof, staged product screenshot,
OpenRouter placement mockup or peer logo is included. Actual branded product
captures and exact-candidate integration evidence must be qualified separately.
The external website consumes a pinned export; this repository does not own its
implementation, public URL/redirect policy or deployment.
