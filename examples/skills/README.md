# Skill examples

Review a skill before copying or activating it.

Copy a reviewed skill directory into `.octet/skills/` for a trusted project or
`~/.octet/skills/` globally. octet discovers metadata but does not inject
instructions automatically.

Use `/skills`, `/skills search <query>`, `/skills load <name>`, and
`/skills off <name>`. Supporting text under `references/` and `templates/` is
loaded lazily and only for an active skill.

<a id="legacy-repro-first"></a>

## Repro First

Before activating [Repro First](repro-first/SKILL.md), explicitly select
`read,search,bash` as the session's tool set. `search` is opt-in; loading the skill
does not replace that separate tool-selection choice.

This selection provides neither a read-only guarantee nor an OS sandbox.
`bash` executes commands under the host's permissions and approval policy.

```sh
octet --tools read,search,bash
```

Then use `/skills` to select the reviewed skill.
