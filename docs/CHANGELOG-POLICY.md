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

## Current entry policy

`python3 scripts/check-changelog.py` checks Unreleased and every version from
0.10.4 onward, regardless of local tags. Earlier sections, including their
historical heading formats, are frozen and exempt. CI quality and the full local
`scripts/gate.sh` run this offline check.

Use only Breaking Changes, Added, Changed, Fixed and Removed subsections, in that
order, omitting empty subsections. Each bullet is one line of positive,
present-tense, user-facing text, at most 160 characters before an optional credit:
` ([#n](https://github.com/skaft-software/octet/pull/n) by [@user](https://github.com/user))`.
Known limitations, engineering chores and test chatter do not belong here.
Conservative lint catches common prose problems; human review remains necessary.
A prepared release must have a real calendar date; an unfinished version may say
`Release candidate` until preparation is complete.

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

## Generated release text

`python3 scripts/release-notes.py sync` renders only the current workspace version
into `docs/releases/vX.Y.Z.md` and the in-agent bundled copy. Its GitHub body
(`render`) preserves the changelog section verbatim beneath a product heading;
it does not normalize links. `artifact --commit SHA` emits the same text and dated
source identity as `release.json` for a separately approved website deployment.
`check` compares the two Markdown copies without writing. CI and the full local
gate run it. Historical notes are never regenerated, deleted or checked for drift.
There is no generation marker for the in-agent renderer to display.

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
