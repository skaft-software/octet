# CHANGELOG policy

`CHANGELOG.md` is the single source of release notes. `scripts/changelog.py`
extracts releases and repairs their links for display, so the format below is a
contract, not a style preference.

## Format

- One top-level `# Changelog` heading, then release sections in newest-first
  order.
- A release section starts with `## [x.y.z]`, optionally followed by
  ` - YYYY-MM-DD`:
  `## [0.7.5] - 2026-09-11`.
- Only `x.y.z` triples are releases. Non-versioned headings such as
  `## [Unreleased]` are markers: the extractor stops the previous section there
  and does not return them as releases.
- Each release body is the text between its heading and the next `##` heading,
  trimmed.

## Links

Links inside a release body are repository-root relative.
`scripts/changelog.py extract` rewrites them to tag-pinned source links:

- `blob` for files, `tree` for directories (trailing `/` or a dotless basename);
- floating `blob|tree/main|master` GitHub URLs are re-pinned to the release tag;
- legacy `https://github.com/skaft-software/{ygg,octet}/…` URLs are canonicalized
  to `https://github.com/skaft-software/octet`;
- external, protocol-relative and in-page (`#anchor`) links are left unchanged;
- a target that escapes the repository root (`../…`) is left unchanged rather
  than guessed.

## Commands

```sh
# Normalized body of one exact release.
python3 scripts/changelog.py extract --version 0.8.0

# Normalized bodies of every release newer than a known version, in file order.
python3 scripts/changelog.py since --last 0.7.6
```

`--changelog PATH` overrides the default `CHANGELOG.md`. An absent release exits
non-zero with no stdout. Behavioral tests live in
`scripts/tests/test_changelog.py`.
