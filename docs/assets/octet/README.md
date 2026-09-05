# Brand Kit

The octet symbol encodes lowercase `o` as the byte `01101111`.

[Download Brand Kit](octet-identity.zip) · [Checksums](SHA256SUMS) ·
[ZIP checksum](octet-identity.zip.sha256) · [Asset manifest](manifest.json)

## Logos

- Symbol: [gradient](marks/mark-gradient.svg), [black](marks/mark-black.svg), [white](marks/mark-white.svg)
- Wordmark: [black](marks/wordmark-black.svg), [white](marks/wordmark-white.svg)
- Logo and descriptor: [light background](marks/lockup-light.svg), [dark background](marks/lockup-dark.svg)
- Square badge: [light](marks/badge-light.svg), [dark](marks/badge-dark.svg)
- [Favicon](marks/favicon.ico); PNG icons from 16 to 1024 pixels are in the download.

## Usage

Use the gradient symbol for general branding. Use black or white when color is
not appropriate. Keep the name **octet** lowercase and leave at least one column
of clear space around a free-standing logo. Omit the wordmark at small sizes.

Keep the eight columns, their proportions, and their shared baseline. Do not
add gaps, extra columns, bevels, or stretch the mark. In the 256 × 128 master,
each column is 32 units wide; columns 1 and 4 are half-height.

The application's colors follow its model and theme settings. Static
provider-colored samples are not endorsements and should not replace that
runtime behavior. These files are brand assets, not product screenshots.

## Rebuild the download

From the repository root, with Python 3.10+:

```sh
python3 docs/assets/octet/export.py --check
python3 docs/assets/octet/test_export.py
python3 docs/assets/octet/export.py --write
```

The export uses pinned, approved source files. It rebuilds checksums and the
ZIP deterministically; it does not redraw the logos or regenerate raster images.
The manifest records asset origins, geometry, and hashes.
