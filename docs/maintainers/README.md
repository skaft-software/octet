# Maintainer prompts and skills

This directory holds the tracked maintainer-facing prompt templates and skills
for octet: prompt templates, `release`, `add-provider`, and
`interactive-testing` skills, and agent conventions in
[`conventions.md`](conventions.md).

[AGENTS.md](../../AGENTS.md) is the tracked source of repository agent
conventions, available in every clone and worktree. `CLAUDE.md` imports it;
[`conventions.md`](conventions.md) points to it rather than duplicating the rules.

Main is always releasable. Maintainer work skips PRs; only gated, linear
fast-forward commits land on `main`. Outside contributions arrive as PRs. Follow [AGENTS.md](../../AGENTS.md) and run the full
`scripts/gate.sh` on the exact commit before any push.
Quick mode is for iteration only. Tags, releases, installs and publishing remain
explicit owner actions.

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
