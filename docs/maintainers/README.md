# Maintainer prompts and skills

This directory holds the tracked maintainer-facing prompt templates and skills
for octet. It mirrors the parity item `6.2` requirement (maintainer prompts plus
`release`, `add-provider`, and `interactive-testing` skills; agent conventions in
[`conventions.md`](conventions.md)).

[`conventions.md`](conventions.md) is the tracked mirror of the repository-root
agent conventions. The working `AGENTS.md` at the root stays local-only
(`.gitignore` excludes it with the other agent-instruction and runtime-state
files), so the mirror is what a fresh clone reads. Update both together.

`.octet/` is gitignored (it is local agent state), so these files are the source
of truth. Activate a copy for your checkout:

```sh
mkdir -p .octet/prompts .octet/skills
cp docs/maintainers/prompts/*.md .octet/prompts/
cp -R docs/maintainers/skills/release .octet/skills/
```

Then invoke a prompt with `/cl`, `/is ISSUE`, `/pr PR-URL`, or `/wr`, and a skill
with `/skills load release`. Global activation uses `~/.octet/prompts` and
`~/.octet/skills` instead. See
[Instructions, prompt templates and skills](../instructions.md) for discovery,
trust, and bounds.

## Prompts

| File | Purpose |
| --- | --- |
| [`cl.md`](prompts/cl.md) | Audit `## [Unreleased]` changelog entries against the commits since the last release. |
| [`is.md`](prompts/is.md) | Analyze a GitHub issue and propose a fix without implementing it. |
| [`pr.md`](prompts/pr.md) | Review a pull request from a URL without checking out its branch. |
| [`wr.md`](prompts/wr.md) | Wrap up a task end to end with changelog, doc, and evidence checks. |

## Skills

| Skill | Purpose |
| --- | --- |
| [`release`](skills/release/SKILL.md) | Prepare, package, publish, verify, and recover an octet release. |
| [`add-provider`](skills/add-provider/SKILL.md) | Add or change a provider/model codec, catalog entry, and tests. |
| [`interactive-testing`](skills/interactive-testing/SKILL.md) | Drive the TUI in a controlled tmux session for behavioral checks. |

Maintainer prompts and skills never bypass workspace trust, tool policy, or the
"no persisted trust default" rule. They are ordinary bounded resources.
